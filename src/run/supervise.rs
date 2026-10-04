//! The detached supervisor: `cahoots __supervise <id>`. It owns a run from
//! `starting` to a terminal state, and it is the only writer of `run.json`.
//!
//! Synchronous on purpose: reader threads feed one channel, the loop wakes on
//! `recv_timeout` to look at the cancel marker, the deadline and the child.
//! Nothing here waits without a deadline.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::Child;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::Signal;

use crate::dirs::{Dirs, ensure_private_dir};
use crate::exit::{Exit, Fail, Res};
use crate::gate::{self, Watch};
use crate::harness::{self, CalleeText, FAILURE_CHARS, Keep, RunSpec};
use crate::patch;
use crate::placement::{self, Placement};
use crate::registry::Registry;
use crate::run::record::{
    RunDir, RunRecord, State, now, try_lock_file, write_private, write_private_atomic,
};
use crate::spawn::{self, Callee};

const TICK: Duration = Duration::from_millis(200);
/// The callee's stdout is kept verbatim, up to this much.
const EVENTS_CAP: u64 = 64 * 1024 * 1024;
const STDERR_TAIL: usize = 4 * 1024;
/// How long the pipes may stay open after the callee itself has exited.
const DRAIN: Duration = Duration::from_secs(5);
/// How often, at most, a change in the run's activity is saved for `status`.
const ACTIVITY_SAVE_EVERY: Duration = Duration::from_secs(1);

enum Line {
    Out(String),
    Err(String),
    /// A reader reached the end of its pipe. Counted, because the watchdog
    /// holds a sender too and the channel would otherwise never disconnect.
    Eof,
    Reading(Watch),
}

enum Stop {
    Cancelled,
    TimedOut,
    /// The target crossed its `abort_at` while the run was going.
    OverBudget(Option<f64>),
}

/// Consecutive over-threshold readings it takes to stop a run. One could be a
/// blip — or another session of the user's, about to be over.
const OVER_READINGS_TO_STOP: u32 = 2;

pub fn supervise(dirs: &Dirs, id: &str) -> Res<()> {
    // Out of the caller's session first: a harness that kills a timed-out
    // tool call's process group must not reach this process.
    let _ = nix::unistd::setsid();

    let dir = RunDir::open(dirs, id)?;
    let Some(_alive) = dir.try_lock()? else {
        return Err(Fail::internal(format!("run {id} already has a supervisor")));
    };
    let mut record = dir.load()?;
    if record.state != State::Starting {
        return Err(Fail::internal(format!("run {id} is not waiting to start")));
    }
    record.supervisor_pid = Some(std::process::id() as i32);

    let outcome = carry(dirs, &dir, &mut record);
    if let Err(fail) = &outcome
        && !record.state.is_terminal()
    {
        record.finish(State::Failed, fail.exit, Some(fail.message.clone()));
    }
    dir.save(&record)?;
    // The long memory: the run's content ages out in days, this line does not.
    // The sample rate is the rate of THIS moment, and the bit never changes.
    let sample_rate = Registry::load(dirs).map_or(0.0, |registry| {
        if registry.review.enabled {
            registry.review.sample_rate
        } else {
            0.0
        }
    });
    if let Err(fail) = crate::history::append(dirs, &crate::history::finished(&record, sample_rate))
    {
        let _ = writeln!(dir.log(), "[supervisor] {}", fail.message);
    }
    outcome
}

fn carry(dirs: &Dirs, dir: &RunDir, record: &mut RunRecord) -> Res<()> {
    let registry = Registry::load(dirs)?;
    let entry = registry.harness(record.target.harness);

    // The slot is held for as long as this function runs. The client looked
    // first, to fail fast; this is the check that counts.
    ensure_private_dir(&dirs.slots())?;
    let _slot = (0..entry.max_concurrent)
        .find_map(|n| {
            let path = dirs
                .slots()
                .join(format!("{}.{n}.lock", record.target.harness));
            try_lock_file(&path).ok().flatten()
        })
        .ok_or_else(|| {
            Fail::new(
                Exit::Busy,
                format!(
                    "{} is already running as many jobs as it may",
                    record.target.harness
                ),
            )
        })?;

    // A writer's worktree is cut HERE, detached from the caller: a checkout of
    // a large repository can outlast a caller's tool call. A resumed writer
    // goes back into the worktree it had — and one whose fork was never cut
    // has none: its `cwd` is still the caller's own tree.
    if record.placement == Placement::Fork {
        let base = record.base.clone().unwrap_or_else(|| record.cwd.clone());
        if record.resumed_from.is_none() {
            let cut = placement::cut(
                dirs,
                &registry.fork,
                &base,
                &record.id,
                &record.tool_roots(),
                Duration::from_secs(record.timeout_secs),
                record.base_commit.as_ref(),
            )?;
            record.cwd = cut.worktree;
            record.gitdir = Some(cut.gitdir);
            record.worktree_provider = Some(cut.provider);
            dir.save(record)?;
            // Read against the pinned git directory: the worktree is cut at
            // the commit the run recorded, or the writer does not start.
            if let Some(sha) = &record.base_commit
                && patch::head(
                    dirs,
                    &record.cwd,
                    record.gitdir.as_deref(),
                    &record.tool_roots(),
                )
                .as_ref()
                    != Some(sha)
            {
                return Err(Fail::new(
                    Exit::RunFailed,
                    format!("cannot cut a worktree: it was not cut at {sha}"),
                ));
            }
        } else if base == record.cwd {
            return Err(Fail::policy(format!(
                "run {} has no worktree of its own to go back into",
                record.id
            )));
        }
    }

    // In place, the writer gets the caller's own tree, `.git` and all. What
    // git status will read once it is done is recorded now, before it starts,
    // and status runs afterwards only if that is unchanged
    // (`placement::changes_in_place`). A reading that fails records nothing,
    // and then status does not run at all.
    if record.placement == Placement::InPlace {
        let recorded = match &record.resumed_from {
            // A resume is held to the configuration the ORIGINAL run was
            // recorded against, never a fresh reading of its own: a first
            // writer that named a filter in the config would otherwise have
            // set the baseline its resume is compared to, and the resume
            // would run that filter. The snapshot is copied forward from the
            // run this one continues — itself a copy, back to the first run's
            // reading, taken before any writer had touched the tree.
            Some(original) => RunDir::open(dirs, original)
                .ok()
                .and_then(|from| std::fs::read(from.git_config_path()).ok())
                .ok_or_else(|| {
                    format!("the git configuration recorded for run {original} is gone")
                }),
            None => placement::config_listing(dirs, &record.cwd, &record.tool_roots()),
        };
        match recorded {
            Ok(listing) => write_private(&dir.git_config_path(), &listing)?,
            Err(why) => {
                let _ = writeln!(
                    dir.log(),
                    "[supervisor] the git configuration was not recorded: {why}"
                );
            }
        }
    }

    let spec = RunSpec {
        role: record.role,
        target: record.target.clone(),
        session_id: record.progress.session_id.clone(),
        resume: record.resume_session.clone(),
    };
    let argv = harness::command_line(&spec)?;
    let mut child = spawn::spawn_callee(&Callee {
        harness: record.target.harness,
        binary: &record.binary,
        argv: &argv,
        cwd: &record.cwd,
        brief: &dir.brief_path(),
        billing: entry.billing,
        run_id: &record.id,
        depth: record.depth + 1,
        caller: record.caller,
    })?;

    let pid = child.id() as i32;
    // From here the callee's process group is killed on EVERY way out of
    // carry — a normal exit, and any error after the spawn (the save below,
    // the event log, a session save) — before the run is published terminal.
    // `changes` (the in-place config comparison, then `git status`) runs in
    // the reader that reports a terminal run, and a resume waits for one too;
    // so nothing this run left in its group can still be writing the tree, or
    // its `.git/config`, in that window. A process that left the group
    // (setsid, a double fork) is out of reach; that residual is in
    // docs/THREAT-MODEL.md.
    let group = GroupCleanup(pid);
    record.state = State::Running;
    record.started_at = Some(now());
    record.callee_pid = Some(pid);
    let started = spawn::process_started(pid, &record.tool_roots());
    record.callee_started = started;
    dir.save(record)?;

    let watchdog = Watchdog {
        registry: registry.clone(),
        every: Duration::from_secs(registry.limits.watchdog_secs),
    };
    let (stop, stderr_tail) = attend(dir, record, &mut child, pid, watchdog)?;

    let text = record.progress.final_text.clone().unwrap_or_default();
    write_private(&dir.final_path(), text.as_bytes())?;

    // The group goes now, not when this function returns: nothing the writer
    // left in it may change the tree while its patch is read.
    drop(group);
    keep_patch(dirs, dir, record);

    // What the callee said of its failure is its words, not cahoots': it is
    // kept bounded, apart from the message, which only points at it — and
    // only for a run that failed by itself. A stopped run's reason is
    // cahoots', and a run that succeeded has no failure to account for,
    // whatever its stderr said.
    let account = match &record.progress.failure {
        Some(said) => CalleeText::bound(said, FAILURE_CHARS, Keep::Start),
        None => CalleeText::bound(&stderr_tail, FAILURE_CHARS, Keep::End),
    };
    let failure = account
        .as_ref()
        .map(|_| "the callee's own account is in data.failure".to_string());
    let stopped = stop.is_some();
    let (state, exit, message) = match stop {
        Some(Stop::Cancelled) => (State::Cancelled, Exit::Cancelled, None),
        Some(Stop::TimedOut) => (
            State::TimedOut,
            Exit::TimedOut,
            Some(format!("stopped after {}s", record.timeout_secs)),
        ),
        Some(Stop::OverBudget(percent)) => (
            State::Budget,
            Exit::Budget,
            Some(format!(
                "stopped: {} crossed {}% of its plan{} while this run was going — what it \
                 had said so far is kept, and the run can be resumed after the limit resets",
                record.target.harness,
                entry.abort_at,
                percent.map_or(String::new(), |p| format!(" ({p:.0}% used)")),
            )),
        ),
        None if record.progress.budget_stop => (
            State::Budget,
            Exit::Budget,
            Some(match failure {
                Some(failure) => format!("stopped by the callee's own budget limit — {failure}"),
                None => "stopped by the callee's own budget limit".to_string(),
            }),
        ),
        None if record.callee_exit == Some(0) && record.progress.failure.is_none() => {
            (State::Done, Exit::Ok, None)
        }
        None => (
            State::Failed,
            Exit::RunFailed,
            failure.map(|failure| format!("the run failed — {failure}")),
        ),
    };
    if !stopped && state != State::Done {
        record.callee_failure = account;
    }
    record.finish(state, exit, message);
    Ok(())
}

/// A fork writer's patch since the run's base commit, kept in the run
/// directory and summed up in the record — however the run stopped, so a
/// stopped writer's partial work is kept too. Before the run is finished, so
/// the same run always answers the same way. Bookkeeping: a patch that
/// cannot be kept leaves `patch` empty and a line in the log, and never
/// changes how the run ended.
fn keep_patch(dirs: &Dirs, dir: &RunDir, record: &mut RunRecord) {
    record.patch = None;
    // A fork whose cut failed has no worktree: its `cwd` is still the base.
    let Some(base) = record
        .base
        .as_ref()
        .filter(|base| record.placement == Placement::Fork && **base != record.cwd)
    else {
        return;
    };
    let Some(sha) = &record.base_commit else {
        let _ = writeln!(
            dir.log(),
            "[supervisor] patch not kept: the run has no base commit to diff against"
        );
        return;
    };
    let roots = record.tool_roots();
    let kept = placement::fork_git_dir(dirs, &record.cwd, base, record.gitdir.as_deref(), &roots)
        .and_then(|gitdir| {
            patch::capture(dirs, &record.cwd, &gitdir, sha, &roots).map_err(|fail| fail.message)
        })
        .and_then(|bytes| {
            write_private_atomic(&dir.patch_path(), &bytes).map_err(|fail| fail.message)?;
            Ok(patch::summarize(&bytes))
        });
    match kept {
        Ok(summary) => record.patch = Some(summary),
        Err(why) => {
            let _ = writeln!(dir.log(), "[supervisor] patch not kept: {why}");
        }
    }
}

/// Reads the callee until it exits, stopping it if asked to or if it runs out
/// of time. Returns why it was stopped (if it was) and the tail of its stderr.
/// Re-checks the target's usage while a run is going. Only with a usage meter
/// that can watch the target (`gate::watches`): cahoots' own ledger cannot
/// move during a run.
struct Watchdog {
    registry: Registry,
    every: Duration,
}

fn attend(
    dir: &RunDir,
    record: &mut RunRecord,
    child: &mut Child,
    pid: i32,
    watchdog: Watchdog,
) -> Res<(Option<Stop>, String)> {
    let harness = harness::harness(record.target.harness);
    let (sender, lines) = mpsc::channel();
    let mut open_streams = 0u32;
    let readers = [
        child
            .stdout
            .take()
            .map(|pipe| read_lines(pipe, sender.clone(), Line::Out)),
        child
            .stderr
            .take()
            .map(|pipe| read_lines(pipe, sender.clone(), Line::Err)),
    ];
    open_streams += readers.iter().flatten().count() as u32;
    if gate::watches(&watchdog.registry, record.target.harness) {
        let (sender, target) = (sender.clone(), record.target.harness);
        // Ends by itself: once the run is over nobody receives, and `send` fails.
        thread::spawn(move || {
            loop {
                thread::sleep(watchdog.every);
                let Some(reading) = gate::watch(&watchdog.registry, target) else {
                    break;
                };
                if sender.send(Line::Reading(reading)).is_err() {
                    break;
                }
            }
        });
    }
    drop(sender);
    let mut log = dir.log();
    let mut over_readings = 0u32;

    let mut events = File::options()
        .create(true)
        .append(true)
        .open(dir.events_path())
        .map_err(|error| Fail::internal(format!("cannot open the event log: {error}")))?;
    let mut events_written = 0u64;
    let mut stderr_tail = String::new();
    let mut known_session = record.progress.session_id.clone();

    let deadline = Instant::now() + Duration::from_secs(record.timeout_secs);
    let mut stop: Option<Stop> = None;
    let mut ladder: Option<Ladder> = None;
    let mut exited = false;
    let mut exited_at: Option<Instant> = None;
    let mut streams_open = open_streams > 0;
    // What it is doing now reaches `status` within a second, also while the
    // callee is silent. Presentation only: a save that fails is logged, and
    // the run goes on.
    let mut activity_unsaved = false;
    let mut saved_at = Instant::now();

    while !exited || streams_open {
        match lines.recv_timeout(TICK) {
            Ok(Line::Out(line)) => {
                if events_written < EVENTS_CAP {
                    let _ = writeln!(events, "{line}");
                    events_written += line.len() as u64 + 1;
                }
                let before = record.progress.activity.clone();
                harness.parse_line(&line, &mut record.progress);
                if record.progress.activity != before {
                    record.activity_at = Some(now());
                    activity_unsaved = true;
                }
                // The session id is what makes a killed run resumable: on disk
                // the moment it is known, not at the end.
                if record.progress.session_id != known_session {
                    known_session = record.progress.session_id.clone();
                    dir.save(record)?;
                    (activity_unsaved, saved_at) = (false, Instant::now());
                }
            }
            Ok(Line::Err(line)) => {
                let _ = writeln!(log, "[callee] {line}");
                stderr_tail.push_str(&line);
                stderr_tail.push('\n');
                if stderr_tail.len() > 2 * STDERR_TAIL {
                    let cut = stderr_tail.len() - STDERR_TAIL;
                    let cut = (cut..stderr_tail.len())
                        .find(|at| stderr_tail.is_char_boundary(*at))
                        .unwrap_or(stderr_tail.len());
                    stderr_tail.drain(..cut);
                }
            }
            Ok(Line::Eof) => {
                open_streams = open_streams.saturating_sub(1);
                streams_open = open_streams > 0;
            }
            Ok(Line::Reading(Watch::Over { percent })) => {
                over_readings += 1;
                let _ = writeln!(
                    log,
                    "[watchdog] over the abort threshold ({over_readings} in a row)"
                );
                if over_readings >= OVER_READINGS_TO_STOP && stop.is_none() && !exited {
                    stop = Some(Stop::OverBudget(percent));
                    ladder = Some(Ladder::start(pid, record));
                }
            }
            // "In a row" means what it says: a reading that is under, or that
            // is no reading at all, starts the count again.
            Ok(Line::Reading(Watch::Under | Watch::Unknown)) => over_readings = 0,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                streams_open = false;
                if !exited {
                    thread::sleep(TICK);
                }
            }
        }
        if activity_unsaved && saved_at.elapsed() >= ACTIVITY_SAVE_EVERY {
            if let Err(fail) = dir.save(record) {
                let _ = writeln!(log, "[supervisor] activity not saved: {}", fail.message);
            }
            (activity_unsaved, saved_at) = (false, Instant::now());
        }

        if !exited {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exited = true;
                    record.callee_exit = status.code();
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(Fail::internal(format!("waiting for the callee: {error}")));
                }
            }
        }
        if !exited {
            if stop.is_none() {
                if dir.cancel_path().exists() {
                    stop = Some(Stop::Cancelled);
                } else if Instant::now() >= deadline {
                    stop = Some(Stop::TimedOut);
                }
                if stop.is_some() {
                    ladder = Some(Ladder::start(pid, record));
                }
            }
            if let Some(ladder) = &mut ladder {
                ladder.climb();
            }
        } else if streams_open && exited_at.get_or_insert_with(Instant::now).elapsed() > DRAIN {
            // The callee is gone but something it started still holds its
            // pipes open. What it had to say has been read; do not wait on.
            streams_open = false;
        }
    }
    if let Some(ladder) = &mut ladder {
        ladder.sweep();
    }
    for reader in readers.into_iter().flatten() {
        if !streams_open {
            break;
        }
        let _ = reader.join();
    }
    Ok((stop, stderr_tail))
}

fn read_lines<R: Read + Send + 'static>(
    pipe: R,
    sender: mpsc::Sender<Line>,
    wrap: fn(String) -> Line,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) | Err(_) => {
                    let _ = sender.send(Line::Eof);
                    break;
                }
                Ok(_) => {
                    let line = String::from_utf8_lossy(&bytes);
                    let line = line.trim_end_matches(['\n', '\r']).to_string();
                    if sender.send(wrap(line)).is_err() {
                        break;
                    }
                }
            }
        }
    })
}

/// Kills a run's process group when it drops: once the callee is spawned,
/// every path out of `carry` — a normal exit, and any error after the spawn —
/// passes through this, so no process the run left in its group outlives the
/// run into the window where a reader reads `changes` or a resume starts. The
/// kill itself is `spawn`'s (hard rule 3). The group leader may already be
/// gone; its pid is still the group's while any member lives, and `killpg`
/// reaches them.
struct GroupCleanup(i32);

impl Drop for GroupCleanup {
    fn drop(&mut self) {
        spawn::signal_group(self.0, Signal::SIGKILL);
    }
}

/// SIGINT → wait → SIGTERM → wait → SIGKILL, to the callee's process group
/// AND to the descendants snapshotted when the ladder started: Codex puts its
/// tool commands in groups of their own, and a graceful interrupt is the only
/// thing that makes it clean them up itself (docs/SPIKE.md S3).
struct Ladder {
    pgid: i32,
    descendants: Vec<i32>,
    rung: u8,
    next: Instant,
    int_grace: Duration,
    term_grace: Duration,
}

impl Ladder {
    fn start(pgid: i32, record: &RunRecord) -> Ladder {
        Ladder {
            pgid,
            descendants: spawn::descendants(pgid, &record.tool_roots()),
            rung: 0,
            next: Instant::now(),
            int_grace: Duration::from_secs(record.int_grace_secs),
            term_grace: Duration::from_secs(record.term_grace_secs),
        }
    }

    fn climb(&mut self) {
        if Instant::now() < self.next {
            return;
        }
        let (signal, wait) = match self.rung {
            0 => (Signal::SIGINT, self.int_grace),
            1 => (Signal::SIGTERM, self.term_grace),
            _ => (Signal::SIGKILL, Duration::from_secs(2)),
        };
        spawn::signal_group(self.pgid, signal);
        if self.rung >= 1 {
            spawn::signal_processes(&self.descendants, signal);
        }
        self.rung = self.rung.saturating_add(1);
        self.next = Instant::now() + wait;
    }

    /// After the callee is gone: whatever it left behind goes too.
    fn sweep(&mut self) {
        spawn::signal_processes(&self.descendants, Signal::SIGKILL);
    }
}
