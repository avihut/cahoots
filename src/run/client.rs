//! The verbs a caller uses: `run`, `wait`, `status`, `result`, `cancel`.
//! All of them are thin: they read and write the run directory, and the
//! detached supervisor does the work.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::Signal;
use serde_json::{Value, json};

use crate::dirs::Dirs;
use crate::env;
use crate::exit::{Envelope, Exit, Fail, Res};
use crate::explore;
use crate::harness::Progress;
use crate::model::{HarnessId, Role, TaskKindName};
use crate::patch::{self, Commit};
use crate::paths::{self, Workspace};
use crate::pick;
use crate::placement::{self, Placement};
use crate::registry::Registry;
use crate::run::record::{self, RunDir, RunRecord, State, now};
use crate::spawn;
use crate::survival;

const POLL: Duration = Duration::from_millis(150);
/// A `starting` record this young may simply not have its supervisor yet.
const STARTING_GRACE_SECS: u64 = 30;
/// Finished runs keep their content this long; `result` needs it.
const RETENTION_SECS: u64 = 7 * 24 * 3600;
/// What `result` prints inline; beyond it, the caller gets the file's path.
const RESULT_INLINE_BYTES: usize = 64 * 1024;

pub struct RunArgs {
    pub role: Option<Role>,
    pub kind: Option<String>,
    pub brief: PathBuf,
    pub to: Option<HarnessId>,
    pub caller: Option<HarnessId>,
    pub dir: Option<PathBuf>,
    /// A writer works in a fresh worktree cut from the caller's HEAD.
    pub fork: bool,
    /// A writer works in the caller's own tree (config must allow it).
    pub in_place: bool,
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
pub fn refuse_inside_a_sandbox(verb: &str) -> Res<()> {
    if env::in_codex_sandbox() {
        return Err(Fail::policy(crate::install::rules::sandboxed_codex_hint(
            verb,
        )));
    }
    Ok(())
}

/// `pick`: who `run` would ask right now, and why not the others. Read-only.
pub fn pick_target(
    role: Option<Role>,
    kind: Option<&str>,
    caller: Option<HarnessId>,
    to: Option<HarnessId>,
) -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    let registry = Registry::load(&dirs)?;
    let routing = registry.routing(role, kind)?;
    let caller = resolve_caller(caller)?;
    let cwd = std::env::current_dir()
        .map_err(|error| Fail::internal(format!("no working directory: {error}")))?;
    let workspace = Workspace::around(&cwd)?;
    // A preview of ordinary routing: no draw, no id. A run that follows may
    // try the next candidate, if exploration is configured.
    let choice = pick::choose(
        &dirs,
        &registry,
        &routing,
        caller,
        to,
        pick::Selection::Ordinary,
        &workspace.roots(),
    )?;
    Ok(Envelope::new(Exit::Ok, None).with_data(json!({
        "role": routing.role,
        "kind": routing.kind,
        "target": choice.target,
        "harness_version": choice.version.to_string(),
        "gate_notes": choice.admission.notes,
        "skipped": choice.skipped,
        "exploration_share": choice.exploration_share,
    })))
}

pub fn run(args: RunArgs) -> Res<Envelope> {
    refuse_inside_a_sandbox("run")?;
    let dirs = Dirs::resolve()?;
    let registry = Registry::load(&dirs)?;
    let routing = registry.routing(args.role, args.kind.as_deref())?;
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
    let (placement, run_dir) = placement::decide(
        routing.role,
        args.fork,
        args.in_place,
        args.dir.as_deref(),
        &workspace,
        &registry,
    )?;
    let mut roots = workspace.roots();
    roots.push(&run_dir);
    dirs.refuse_inside(&roots)?;
    let brief = paths::read_brief(&args.brief, &workspace)?;

    reconcile(&dirs, &roots);
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

    // The run's id, before any candidate is looked at: it is what the draw is
    // made from, and it is the id the run is recorded under. Nobody supplies
    // it and nothing is drawn again after a target is chosen.
    let id = record::new_id();
    let selection = pick::Selection::Drawn(explore::drawn(&id, routing.exploration_share));
    let choice = pick::choose(
        &dirs, &registry, &routing, caller, args.to, selection, &roots,
    )?;
    let target = choice.target;
    // The commit the run starts from: for a fork, the one it will be cut at;
    // in place, the caller's tree as it is before the writer exists.
    let base_commit = patch::head(&dirs, &run_dir, None, &roots);
    // A fork's repository, pinned here, before the writer exists: what its
    // survival is read against later, whatever the tree's `.git` says then.
    let base_repo = match placement {
        Placement::Fork => survival::pin(&dirs, &run_dir, &roots),
        _ => None,
    };
    let roots = owned(&roots);

    launch(
        &dirs,
        &registry,
        Launch {
            id,
            exploration: choice.exploration,
            role: routing.role,
            kind: routing.kind.cloned(),
            caller,
            target,
            placement,
            base: (placement == Placement::Fork).then(|| run_dir.clone()),
            cwd: run_dir,
            gitdir: None,
            roots,
            base_commit,
            base_repo,
            depth,
            timeout_secs: args.timeout_secs,
            wait_secs: args.wait_secs,
            binary: choice.binary,
            version: choice.version,
            admission: choice.admission,
            resumed_from: None,
            resume_session: None,
            brief,
        },
    )
}

/// Everything decided; what is left is to write the record and start the
/// supervisor. Shared by `run` and `resume`, so a resumed run is recorded,
/// supervised, stopped and reported exactly like any other.
struct Launch {
    id: String,
    /// The private label (`RunRecord::exploration`).
    exploration: bool,
    role: Role,
    kind: Option<TaskKindName>,
    caller: Option<HarnessId>,
    target: crate::model::Candidate,
    placement: Placement,
    base: Option<PathBuf>,
    cwd: PathBuf,
    gitdir: Option<PathBuf>,
    /// The asking client's workspace and the run's directory (`RunRecord::roots`).
    roots: Vec<PathBuf>,
    base_commit: Option<Commit>,
    base_repo: Option<survival::RepoPin>,
    depth: u32,
    timeout_secs: Option<u64>,
    wait_secs: Option<u64>,
    binary: PathBuf,
    version: crate::harness::Version,
    admission: crate::gate::Admission,
    resumed_from: Option<String>,
    resume_session: Option<String>,
    brief: String,
}

fn launch(dirs: &Dirs, registry: &Registry, launch: Launch) -> Res<Envelope> {
    let mut progress = Progress::default();
    if launch.target.harness == HarnessId::Claude && launch.resume_session.is_none() {
        // Preset, so the run is resumable even if it dies before printing.
        progress.session_id = Some(uuid::Uuid::now_v7().to_string());
    }
    let limit = registry.limits.timeout_secs;
    let record = RunRecord {
        v: 1,
        id: launch.id,
        state: State::Starting,
        exit_code: None,
        message: None,
        role: launch.role,
        kind: launch.kind,
        caller: launch.caller,
        target: launch.target,
        blind: registry.review.blind,
        exploration: launch.exploration,
        base: launch.base,
        gitdir: launch.gitdir,
        roots: launch.roots,
        base_commit: launch.base_commit,
        patch: None,
        base_repo: launch.base_repo,
        cwd: launch.cwd,
        placement: launch.placement,
        depth: launch.depth,
        timeout_secs: launch
            .timeout_secs
            .map_or(limit, |asked| asked.clamp(1, limit)),
        int_grace_secs: registry.limits.int_grace_secs,
        term_grace_secs: registry.limits.term_grace_secs,
        binary: launch.binary,
        harness_version: Some(launch.version),
        admission: launch.admission,
        created_at: now(),
        started_at: None,
        finished_at: None,
        supervisor_pid: None,
        callee_pid: None,
        callee_started: None,
        callee_exit: None,
        resumed_from: launch.resumed_from,
        resume_session: launch.resume_session,
        progress,
    };
    let dir = RunDir::create(dirs, &record, &launch.brief)?;
    spawn::spawn_supervisor(&record.id, &dir.log_path())?;

    let wait = launch.wait_secs.unwrap_or(registry.limits.wait_secs);
    let record = wait_for(&dir, Duration::from_secs(wait))?;
    Ok(report(dirs, &dir, &record, true, &[]))
}

pub struct ResumeArgs {
    pub run: String,
    pub brief: PathBuf,
    pub caller: Option<HarnessId>,
    pub wait_secs: Option<u64>,
    pub timeout_secs: Option<u64>,
}

/// Continues a finished run's conversation with a new brief: the same
/// harness, model, role and place, the harness's own session picked up where
/// it stopped. It is a NEW run in every other respect — gated, slotted,
/// depth-checked and recorded like one — so resuming is never a way around
/// anything a fresh run would be refused for.
pub fn resume(args: ResumeArgs) -> Res<Envelope> {
    refuse_inside_a_sandbox("resume")?;
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

    reconcile(&dirs, &workspace.roots());
    let old = RunDir::open(&dirs, &args.run)?.load()?;
    if !old.state.is_terminal() {
        return Err(Fail::new(
            Exit::NotFinished,
            format!(
                "run {} is still going — `cahoots wait` or `cahoots cancel` it first",
                old.id
            ),
        ));
    }
    let Some(session) = old.progress.session_id.clone() else {
        return Err(Fail::new(
            Exit::Usage,
            format!(
                "run {} ended before its harness started a session, so there is nothing to resume",
                old.id
            ),
        ));
    };
    // A run is resumed from where it was started: the same directory, or the
    // same repository. Knowing a run id is not a pass to another workspace.
    let anchor = old.base.clone().unwrap_or_else(|| old.cwd.clone());
    paths::run_dir(Some(&anchor), &workspace).map_err(|_| {
        Fail::policy(format!(
            "run {} belongs to another workspace ({})",
            old.id,
            anchor.display()
        ))
    })?;
    if !old.cwd.is_dir() {
        return Err(Fail::new(
            Exit::Usage,
            format!(
                "the place run {} worked in is gone ({})",
                old.id,
                old.cwd.display()
            ),
        ));
    }
    // A fork whose cut failed never left the caller's tree: its `cwd` is that
    // tree, and a writer resumed there would work in it.
    if old.placement == Placement::Fork && old.base.as_ref().is_none_or(|base| *base == old.cwd) {
        return Err(Fail::new(
            Exit::Usage,
            format!(
                "run {} never got a worktree of its own (its fork was not cut), so there is \
                 nothing to resume — start a new run with --fork",
                old.id
            ),
        ));
    }
    // Both workspaces, the one that started the run and the one resuming it:
    // the supervisor inherits this caller's PATH, and a tool for the run may
    // come from neither.
    let mut roots = workspace.roots();
    roots.push(&old.cwd);
    for root in &old.roots {
        if !roots.contains(&root.as_path()) {
            roots.push(root);
        }
    }
    dirs.refuse_inside(&roots)?;
    let brief = paths::read_brief(&args.brief, &workspace)?;

    if Some(old.target.harness) == caller {
        return Err(Fail::policy(format!(
            "{} is the caller; a harness does not delegate to itself",
            old.target.harness
        )));
    }
    if !registry.harness(old.target.harness).enabled {
        return Err(Fail::new(
            Exit::TargetUnavailable,
            format!("{} is not enabled as a target any more", old.target.harness),
        ));
    }
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
    let (binary, version, admission) =
        pick::eligible(&dirs, &registry, &old.target, old.role, &roots)?;
    // A fork goes back into the worktree that was cut at its base, and its
    // patch is the worktree's whole change since then. Any other tree is
    // live, and may have moved.
    let base_commit = match old.placement {
        Placement::Fork => old.base_commit.clone(),
        _ => patch::head(&dirs, &old.cwd, None, &roots),
    };
    // Its repository too: the pin taken when the chain began.
    let base_repo = match old.placement {
        Placement::Fork => old.base_repo.clone(),
        _ => None,
    };
    let roots = owned(&roots);

    launch(
        &dirs,
        &registry,
        Launch {
            // A resume draws nothing: the session's harness and model are
            // fixed, and its label is false whatever its parent's was.
            id: record::new_id(),
            exploration: false,
            role: old.role,
            kind: old.kind,
            caller,
            target: old.target,
            placement: old.placement,
            base: old.base,
            cwd: old.cwd,
            gitdir: old.gitdir,
            roots,
            base_commit,
            base_repo,
            depth,
            timeout_secs: args.timeout_secs,
            wait_secs: args.wait_secs,
            binary,
            version,
            admission,
            resumed_from: Some(old.id),
            resume_session: Some(session),
            brief,
        },
    )
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
    let invoker = invoker_roots()?;
    reconcile(&dirs, &borrowed(&invoker));
    let dir = RunDir::open(&dirs, id)?;
    let patience = timeout_secs.unwrap_or(Registry::load(&dirs)?.limits.wait_secs);
    let record = wait_for(&dir, Duration::from_secs(patience))?;
    Ok(report(&dirs, &dir, &record, true, &borrowed(&invoker)))
}

pub fn result(id: &str) -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    let invoker = invoker_roots()?;
    reconcile(&dirs, &borrowed(&invoker));
    let dir = RunDir::open(&dirs, id)?;
    let record = dir.load()?;
    Ok(report(&dirs, &dir, &record, true, &borrowed(&invoker)))
}

pub fn status(id: Option<&str>) -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    let invoker = invoker_roots()?;
    reconcile(&dirs, &borrowed(&invoker));
    let outcomes = outcome_ids(&dirs);
    if let Some(id) = id {
        let dir = RunDir::open(&dirs, id)?;
        let record = dir.load()?;
        // `status` answers "how is it going", so it succeeds whatever the
        // run's own fate; `result` and `wait` carry the run's exit code.
        return Ok(Envelope::new(Exit::Ok, None)
            .with_data(summary(&record, outcomes.contains(&record.id))));
    }
    let here = std::env::current_dir()
        .ok()
        .and_then(|cwd| fs::canonicalize(cwd).ok());
    let runs: Vec<Value> = record::all(&dirs)
        .iter()
        .rev()
        .filter(|(_, r)| here.as_ref().is_none_or(|here| r.cwd.starts_with(here)))
        .take(20)
        .map(|(_, r)| summary(r, outcomes.contains(&r.id)))
        .collect();
    Ok(Envelope::new(Exit::Ok, None).with_data(json!({ "runs": runs })))
}

pub fn cancel(id: &str) -> Res<Envelope> {
    refuse_inside_a_sandbox("cancel")?;
    let dirs = Dirs::resolve()?;
    let invoker = invoker_roots()?;
    let invoker = borrowed(&invoker);
    reconcile(&dirs, &invoker);
    let dir = RunDir::open(&dirs, id)?;
    let record = dir.load()?;
    if !record.state.is_terminal() {
        record::write_private(&dir.cancel_path(), b"")?;
        let patience = Duration::from_secs(record.int_grace_secs + record.term_grace_secs + 10);
        let record = wait_for(&dir, patience)?;
        return Ok(report(&dirs, &dir, &record, false, &invoker));
    }
    Ok(report(&dirs, &dir, &record, false, &invoker))
}

/// `outcome`: what became of a run's result. The caller says so right after
/// it used (or dropped) the answer — it is the only ground truth the learning
/// has, which is why a missing outcome stays "unknown" and is never guessed.
///
/// A fork writer's is followed by a first measurement of how much of its
/// diff survived (`survival`), filed in the history. Whatever that finds, or
/// fails to find, the outcome and its envelope are the same.
pub fn outcome(id: &str, outcome: crate::history::Outcome) -> Res<Envelope> {
    refuse_inside_a_sandbox("outcome")?;
    let dirs = Dirs::resolve()?;
    let invoker = invoker_roots()?;
    reconcile(&dirs, &borrowed(&invoker));
    record::validate_id(id)?;
    let stories = crate::history::stories(&crate::history::read(&dirs));
    let story = stories.iter().find(|story| story.run == id);
    let Some(story) = story else {
        // Still going, or never existed: the run directory tells which.
        let record = RunDir::open(&dirs, id)?.load()?;
        return Err(Fail::new(
            Exit::NotFinished,
            format!(
                "run {} has not finished — there is no result to have an outcome yet",
                record.id
            ),
        ));
    };
    crate::history::append(
        &dirs,
        &crate::history::Event::Outcome {
            t: now(),
            run: id.to_string(),
            outcome,
        },
    )?;
    if survival::measurable(story) && !survival::superseded(&stories).contains(id) {
        // The outcome is recorded already: a measurement that cannot be
        // filed is lost, and nothing else.
        let _ = survival::record(&dirs, story, &borrowed(&invoker));
    }
    Ok(Envelope::new(Exit::Ok, None).with_data(json!({ "run": id, "outcome": outcome })))
}

fn outcome_ids(dirs: &Dirs) -> BTreeSet<String> {
    crate::history::stories(&crate::history::read(dirs))
        .into_iter()
        .filter(|story| story.outcome.is_some())
        .map(|story| story.run)
        .collect()
}

fn summary(record: &RunRecord, has_outcome: bool) -> Value {
    let identity = crate::run::visibility::project(record.blind, has_outcome, &record.target);
    let mut data = json!({
        "run": record.id,
        "state": record.state,
        "role": record.role,
        "kind": record.kind,
        "blind": identity.blind,
        "target": identity.target,
        "cwd": record.cwd,
        "placement": record.placement,
        "created_at": record.created_at,
        "finished_at": record.finished_at,
        "tokens": {
            "input": record.progress.tokens_input,
            "cached": record.progress.tokens_cached,
            "output": record.progress.tokens_output,
        },
        "resumable": record.progress.session_id.is_some(),
        "resumed_from": record.resumed_from,
        // Neither says anything of the target, so a blind run shows both.
        "base_commit": record.base_commit,
        "patch": record.patch,
        "notes": record.progress.notes,
        "gate_notes": record.admission.notes,
    });
    if !identity.blind {
        data["model_reported"] = json!(record.progress.model_reported);
        // Until an outcome, a known list order and this label would say which
        // candidate ran: the key is absent, not false and not null.
        data["exploration"] = json!(record.exploration);
    }
    data
}

/// A run as `run`, `wait`, `result` and `cancel` report it: the run's own exit
/// code, its summary, and — when finished — the answer, marked for what it is.
/// `invoker` is the asking process's workspace: no tool run here comes from
/// it either.
fn report(
    dirs: &Dirs,
    dir: &RunDir,
    record: &RunRecord,
    with_result: bool,
    invoker: &[&Path],
) -> Envelope {
    let events = crate::history::read(dirs);
    let stories = crate::history::stories(&events);
    let has_outcome = stories
        .iter()
        .any(|story| story.run == record.id && story.outcome.is_some());
    let mut data = summary(record, has_outcome);
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
    if record.state.is_terminal()
        && let Ok(registry) = Registry::load(dirs)
    {
        let waiting = crate::learn::pending_count(&events, &stories, &registry, record.caller);
        if waiting > 0 {
            // A nudge, not a demand: see the `cahoots-review` skill.
            data["pending_reviews"] = json!(waiting);
        }
    }
    // A writer's place: its own tree in place, or the worktree cut for it. A
    // fork that was never cut has none — its `cwd` is still the caller's
    // tree, which is not the writer's work.
    let place = match record.placement {
        Placement::Caller => false,
        Placement::InPlace => true,
        Placement::Fork => record.base.as_ref().is_some_and(|base| *base != record.cwd),
    };
    if place && record.state.is_terminal() {
        data["worktree"] = json!(record.cwd);
        let mut roots = invoker.to_vec();
        roots.extend(record.tool_roots());
        let read = match (record.placement, record.base.as_deref()) {
            (Placement::Fork, Some(base)) => {
                placement::changes(dirs, &record.cwd, base, record.gitdir.as_deref(), &roots)
            }
            _ => placement::changes_in_place(
                dirs,
                &record.cwd,
                fs::read(dir.git_config_path()).ok().as_deref(),
                &roots,
            ),
        };
        match read {
            Ok(lines) => data["changes"] = json!(lines),
            Err(why) => {
                data["changes"] = Value::Null;
                data["changes_error"] = json!(why);
            }
        }
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
/// drops the content of runs past retention, and the worktree such a run
/// worked in once no run kept on record works there too. `invoker` is the
/// asking process's workspace: no tool run here comes from it.
pub fn reconcile(dirs: &Dirs, invoker: &[&Path]) {
    let runs = record::all(dirs);
    let expired = |record: &RunRecord| {
        record.state.is_terminal()
            && now().saturating_sub(record.finished_at.unwrap_or(record.created_at))
                > RETENTION_SECS
    };
    // Where the runs that stay work: a resumed run shares its worktree with
    // the run it continues, which may age out first.
    let in_use: Vec<PathBuf> = runs
        .iter()
        .filter(|(_, record)| !expired(record))
        .map(|(_, record)| record.cwd.clone())
        .collect();
    for (dir, mut record) in runs {
        if record.state.is_terminal() {
            if expired(&record) {
                if let Some(base) = &record.base
                    && !works_in(&record.cwd, &in_use)
                {
                    let mut roots = invoker.to_vec();
                    roots.extend(record.tool_roots());
                    placement::discard(dirs, base, &record.cwd, &roots);
                }
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
        if let (Some(pid), Some(started)) = (record.callee_pid, &record.callee_started) {
            let mut roots = invoker.to_vec();
            roots.extend(record.tool_roots());
            if spawn::process_started(pid, &roots).as_ref() == Some(started) {
                let orphans = spawn::descendants(pid, &roots);
                spawn::signal_group(pid, Signal::SIGKILL);
                spawn::signal_processes(&orphans, Signal::SIGKILL);
            }
        }
        record.finish(
            State::Crashed,
            Exit::RunFailed,
            Some("the supervisor died before the run finished".to_string()),
        );
        let _ = dir.save(&record);
    }
}

/// Whether one of `in_use` is `worktree`: the same path, or the same
/// directory by another name.
fn works_in(worktree: &Path, in_use: &[PathBuf]) -> bool {
    if in_use.iter().any(|other| other == worktree) {
        return true;
    }
    let Ok(canonical) = fs::canonicalize(worktree) else {
        return false;
    };
    in_use
        .iter()
        .any(|other| fs::canonicalize(other).is_ok_and(|other| other == canonical))
}

/// The asking process's workspace, for the verbs that look a run up: they
/// start `git` and `ps` too. A `git` planted in it is refused, as it is for
/// `run`; a working directory that cannot be read adds nothing.
fn invoker_roots() -> Res<Vec<PathBuf>> {
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(Vec::new());
    };
    match Workspace::around(&cwd) {
        Ok(workspace) => Ok(owned(&workspace.roots())),
        Err(fail) if fail.exit == Exit::Policy => Err(fail),
        Err(_) => Ok(Vec::new()),
    }
}

fn owned(roots: &[&Path]) -> Vec<PathBuf> {
    roots.iter().map(|root| root.to_path_buf()).collect()
}

fn borrowed(roots: &[PathBuf]) -> Vec<&Path> {
    roots.iter().map(PathBuf::as_path).collect()
}
