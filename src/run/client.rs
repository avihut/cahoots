//! The verbs a caller uses: `run`, `wait`, `status`, `result`, `cancel`.
//! All of them are thin: they read and write the run directory, and the
//! detached supervisor does the work.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::Signal;
use serde_json::{Value, json};

use crate::dirs::Dirs;
use crate::env;
use crate::exit::{Envelope, Exit, Fail, Res};
use crate::harness::{self, Progress};
use crate::model::{Candidate, HarnessId, Role};
use crate::paths::{self, Workspace};
use crate::registry::Registry;
use crate::run::record::{self, RunDir, RunRecord, State, now, try_lock_file};
use crate::spawn;

const POLL: Duration = Duration::from_millis(150);
/// A `starting` record this young may simply not have its supervisor yet.
const STARTING_GRACE_SECS: u64 = 30;
/// Finished runs keep their content this long; `result` needs it.
const RETENTION_SECS: u64 = 7 * 24 * 3600;
/// What `result` prints inline; beyond it, the caller gets the file's path.
const RESULT_INLINE_BYTES: usize = 64 * 1024;

pub struct RunArgs {
    pub role: Role,
    pub brief: PathBuf,
    pub to: Option<HarnessId>,
    pub caller: Option<HarnessId>,
    pub dir: Option<PathBuf>,
    pub wait_secs: Option<u64>,
    pub timeout_secs: Option<u64>,
}

/// The harness that is asking. `--caller` wins; otherwise the environment,
/// which is ambiguous when harnesses nest (docs/SPIKE.md S1) — and then
/// cahoots refuses to guess, because the caller is who gets left out.
pub fn resolve_caller(explicit: Option<HarnessId>) -> Res<Option<HarnessId>> {
    if explicit.is_some() {
        return Ok(explicit);
    }
    match env::detected_callers().as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        several => Err(Fail::new(
            Exit::Usage,
            format!(
                "this process is inside several harnesses ({}); say which one is asking with --caller",
                several
                    .iter()
                    .map(|h| h.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

/// The rule a sandboxed Codex needs before a recording verb can work.
fn refuse_inside_a_sandbox(verb: &str) -> Res<()> {
    if env::in_codex_sandbox() {
        return Err(Fail::policy(crate::install::rules::sandboxed_codex_hint(
            verb,
        )));
    }
    Ok(())
}

/// First enabled candidate for the role that is not the caller. `--to` narrows
/// it to one harness. The gate (meters) joins this in `gate::admit`.
pub fn candidates(
    registry: &Registry,
    role: Role,
    caller: Option<HarnessId>,
    to: Option<HarnessId>,
) -> Res<Vec<Candidate>> {
    if let Some(to) = to {
        if Some(to) == caller {
            return Err(Fail::policy(format!(
                "{to} is the caller; a harness does not delegate to itself"
            )));
        }
        if !registry.harness(to).enabled {
            return Err(Fail::new(
                Exit::TargetUnavailable,
                format!(
                    "{to} is not enabled as a target — a person enables it with `cahoots enable {to}`"
                ),
            ));
        }
    }
    let list: Vec<Candidate> = registry.roles[&role]
        .candidates
        .iter()
        .filter(|c| Some(c.harness) != caller)
        .filter(|c| to.is_none_or(|to| c.harness == to))
        .filter(|c| registry.harness(c.harness).enabled)
        .cloned()
        .collect();
    if list.is_empty() {
        return Err(Fail::new(
            Exit::NoEligibleTarget,
            format!(
                "no enabled target for the {role} role — a person enables one with `cahoots enable <harness>`"
            ),
        ));
    }
    Ok(list)
}

fn slot_free(dirs: &Dirs, registry: &Registry, harness: HarnessId) -> bool {
    let _ = crate::dirs::ensure_private_dir(&dirs.slots());
    (0..registry.harness(harness).max_concurrent).any(|n| {
        let path = dirs.slots().join(format!("{harness}.{n}.lock"));
        matches!(try_lock_file(&path), Ok(Some(_)))
    })
}

pub fn run(args: RunArgs) -> Res<Envelope> {
    refuse_inside_a_sandbox("run")?;
    let dirs = Dirs::resolve()?;
    let registry = Registry::load(&dirs)?;
    let caller = resolve_caller(args.caller)?;

    let depth = env::depth();
    if depth >= registry.limits.max_depth {
        return Err(Fail::policy(format!(
            "this is already a delegated run (depth {depth}); delegating further is not allowed"
        )));
    }

    let cwd = std::env::current_dir()
        .map_err(|error| Fail::internal(format!("no working directory: {error}")))?;
    let workspace = Workspace::around(&cwd)?;
    let run_dir = paths::run_dir(args.dir.as_deref(), &workspace)?;
    let mut roots = workspace.roots();
    roots.push(&run_dir);
    dirs.refuse_inside(&roots)?;
    let brief = paths::read_brief(&args.brief, &workspace)?;

    reconcile(&dirs);
    let live = record::all(&dirs)
        .iter()
        .filter(|(_, r)| !r.state.is_terminal())
        .count();
    if live as u32 >= registry.limits.max_active_runs {
        return Err(Fail::new(
            Exit::Busy,
            format!("{live} runs are already active"),
        ));
    }

    let list = candidates(&registry, args.role, caller, args.to)?;
    let target = match list.iter().find(|c| slot_free(&dirs, &registry, c.harness)) {
        Some(target) => target.clone(),
        None => {
            return Err(Fail::new(
                Exit::Busy,
                "every eligible target is already running as many jobs as it may",
            ));
        }
    };
    crate::gate::admit(&dirs, &registry, &target, args.role)?;

    let tool = harness::harness(target.harness);
    let entry = registry.harness(target.harness);
    let binary = spawn::resolve_binary(tool.binary_name(), entry.binary.as_deref(), &roots)?;
    let version = spawn::run_helper(&binary, &["--version"], None, Duration::from_secs(15))
        .ok()
        .and_then(|output| tool.fingerprint(&output.stdout));
    let Some(version) = version else {
        return Err(Fail::new(
            Exit::TargetUnavailable,
            format!(
                "{} does not identify itself as {}",
                binary.display(),
                target.harness
            ),
        ));
    };
    if version < tool.tested().0 {
        return Err(Fail::new(
            Exit::TargetUnavailable,
            format!(
                "{} {version} is older than the oldest version cahoots supports ({})",
                target.harness,
                tool.tested().0
            ),
        ));
    }

    let mut progress = Progress::default();
    if target.harness == HarnessId::Claude {
        // Preset, so the run is resumable even if it dies before printing.
        progress.session_id = Some(uuid::Uuid::now_v7().to_string());
    }
    let limit = registry.limits.timeout_secs;
    let record = RunRecord {
        v: 1,
        id: record::new_id(),
        state: State::Starting,
        exit_code: None,
        message: None,
        role: args.role,
        caller,
        target,
        cwd: run_dir,
        depth,
        timeout_secs: args
            .timeout_secs
            .map_or(limit, |asked| asked.clamp(1, limit)),
        int_grace_secs: registry.limits.int_grace_secs,
        term_grace_secs: registry.limits.term_grace_secs,
        binary,
        harness_version: Some(version),
        created_at: now(),
        started_at: None,
        finished_at: None,
        supervisor_pid: None,
        callee_pid: None,
        callee_started: None,
        callee_exit: None,
        progress,
    };
    let dir = RunDir::create(&dirs, &record, &brief)?;
    spawn::spawn_supervisor(&record.id, &dir.log_path())?;

    let wait = args.wait_secs.unwrap_or(registry.limits.wait_secs);
    let record = wait_for(&dir, Duration::from_secs(wait))?;
    Ok(report(&dir, &record, true))
}

fn wait_for(dir: &RunDir, patience: Duration) -> Res<RunRecord> {
    let started = Instant::now();
    loop {
        let record = dir.load()?;
        if record.state.is_terminal() || started.elapsed() >= patience {
            return Ok(record);
        }
        thread::sleep(POLL);
    }
}

pub fn wait(id: &str, timeout_secs: Option<u64>) -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    reconcile(&dirs);
    let dir = RunDir::open(&dirs, id)?;
    let patience = timeout_secs.unwrap_or(Registry::load(&dirs)?.limits.wait_secs);
    let record = wait_for(&dir, Duration::from_secs(patience))?;
    Ok(report(&dir, &record, true))
}

pub fn result(id: &str) -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    reconcile(&dirs);
    let dir = RunDir::open(&dirs, id)?;
    let record = dir.load()?;
    Ok(report(&dir, &record, true))
}

pub fn status(id: Option<&str>) -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    reconcile(&dirs);
    if let Some(id) = id {
        let dir = RunDir::open(&dirs, id)?;
        let record = dir.load()?;
        // `status` answers "how is it going", so it succeeds whatever the
        // run's own fate; `result` and `wait` carry the run's exit code.
        return Ok(Envelope::new(Exit::Ok, None).with_data(summary(&record)));
    }
    let here = std::env::current_dir()
        .ok()
        .and_then(|cwd| fs::canonicalize(cwd).ok());
    let runs: Vec<Value> = record::all(&dirs)
        .iter()
        .rev()
        .filter(|(_, r)| here.as_ref().is_none_or(|here| r.cwd.starts_with(here)))
        .take(20)
        .map(|(_, r)| summary(r))
        .collect();
    Ok(Envelope::new(Exit::Ok, None).with_data(json!({ "runs": runs })))
}

pub fn cancel(id: &str) -> Res<Envelope> {
    refuse_inside_a_sandbox("cancel")?;
    let dirs = Dirs::resolve()?;
    reconcile(&dirs);
    let dir = RunDir::open(&dirs, id)?;
    let record = dir.load()?;
    if !record.state.is_terminal() {
        record::write_private(&dir.cancel_path(), b"")?;
        let patience = Duration::from_secs(record.int_grace_secs + record.term_grace_secs + 10);
        let record = wait_for(&dir, patience)?;
        return Ok(report(&dir, &record, false));
    }
    Ok(report(&dir, &record, false))
}

fn summary(record: &RunRecord) -> Value {
    json!({
        "run": record.id,
        "state": record.state,
        "role": record.role,
        "target": record.target,
        "model_reported": record.progress.model_reported,
        "cwd": record.cwd,
        "created_at": record.created_at,
        "finished_at": record.finished_at,
        "tokens": {
            "input": record.progress.tokens_input,
            "cached": record.progress.tokens_cached,
            "output": record.progress.tokens_output,
        },
        "resumable": record.progress.session_id.is_some(),
        "notes": record.progress.notes,
    })
}

/// A run as `run`, `wait`, `result` and `cancel` report it: the run's own exit
/// code, its summary, and — when finished — the answer, marked for what it is.
fn report(dir: &RunDir, record: &RunRecord, with_result: bool) -> Envelope {
    let mut data = summary(record);
    if with_result && record.state.is_terminal() {
        let text = fs::read_to_string(dir.final_path()).unwrap_or_default();
        let inline: String = if text.len() > RESULT_INLINE_BYTES {
            let cut = (0..=RESULT_INLINE_BYTES)
                .rev()
                .find(|at| text.is_char_boundary(*at))
                .unwrap_or(0);
            text[..cut].to_string()
        } else {
            text.clone()
        };
        data["result"] = json!({
            // The answer is another agent's claim about the world. It is data
            // to weigh, never instructions to follow.
            "untrusted": true,
            "truncated": inline.len() < text.len(),
            "path": dir.final_path(),
            "text": inline,
        });
    }
    let exit = Exit::ALL
        .into_iter()
        .find(|exit| exit.code() == record.exit())
        .unwrap_or(Exit::Internal);
    let message = match record.state.is_terminal() {
        true => record.message.clone(),
        false => Some(format!(
            "not finished yet — ask again with `cahoots wait {}`",
            record.id
        )),
    };
    Envelope::new(exit, message).with_data(data)
}

/// Every invocation tidies up after supervisors that died: a live-looking
/// record whose lock is free gets marked `crashed`, and the process group it
/// recorded is signalled ONLY if that pid is still the process it was. Also
/// drops the content of runs past retention.
pub fn reconcile(dirs: &Dirs) {
    for (dir, mut record) in record::all(dirs) {
        if record.state.is_terminal() {
            let age = now().saturating_sub(record.finished_at.unwrap_or(record.created_at));
            if age > RETENTION_SECS {
                let _ = fs::remove_dir_all(&dir.path);
            }
            continue;
        }
        if record.state == State::Starting
            && now().saturating_sub(record.created_at) < STARTING_GRACE_SECS
        {
            continue;
        }
        let Ok(Some(_lock)) = dir.try_lock() else {
            continue; // a live supervisor holds it
        };
        if let (Some(pid), Some(started)) = (record.callee_pid, &record.callee_started)
            && spawn::process_started(pid).as_ref() == Some(started)
        {
            let orphans = spawn::descendants(pid);
            spawn::signal_group(pid, Signal::SIGKILL);
            spawn::signal_processes(&orphans, Signal::SIGKILL);
        }
        record.finish(
            State::Crashed,
            Exit::RunFailed,
            Some("the supervisor died before the run finished".to_string()),
        );
        let _ = dir.save(&record);
    }
}
