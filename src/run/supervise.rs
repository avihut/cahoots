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
use crate::harness::{self, RunSpec};
use crate::placement::{self, Placement};
use crate::registry::Registry;
use crate::run::record::{RunDir, RunRecord, State, now, try_lock_file, write_private};
use crate::spawn::{self, Callee};

const TICK: Duration = Duration::from_millis(200);
/// The callee's stdout is kept verbatim, up to this much.
const EVENTS_CAP: u64 = 64 * 1024 * 1024;
const STDERR_TAIL: usize = 4 * 1024;
/// How long the pipes may stay open after the callee itself has exited.
const DRAIN: Duration = Duration::from_secs(5);

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
        eprintln!("[supervisor] {}", fail.message);
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

    // A writer's worktree is cut HERE, detached from the caller: in a daft
    // repository that runs the repo's setup hooks, which can outlast a
    // caller's tool call.
    if record.placement == Placement::Fork && record.resumed_from.is_none() {
        let base = record.base.clone().unwrap_or_else(|| record.cwd.clone());
        record.cwd = placement::cut(dirs, &base, &record.id)?;
        dir.save(record)?;
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
    record.state = State::Running;
    record.started_at = Some(now());
    record.callee_pid = Some(pid);
    record.callee_started = spawn::process_started(pid);
    dir.save(record)?;

    let watchdog = Watchdog {
        registry: registry.clone(),
        every: Duration::from_secs(registry.limits.watchdog_secs),
    };
    let (stop, stderr_tail) = attend(dir, record, &mut child, pid, watchdog)?;

    let text = record.progress.final_text.clone().unwrap_or_default();
    write_private(&dir.final_path(), text.as_bytes())?;

    let failure = record.progress.failure.clone();
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
        None if record.progress.budget_stop => (State::Budget, Exit::Budget, failure),
        None if record.callee_exit == Some(0) && failure.is_none() => (State::Done, Exit::Ok, None),
        None => (
            State::Failed,
            Exit::RunFailed,
            failure
                .or_else(|| Some(stderr_tail.trim().to_string()).filter(|tail| !tail.is_empty())),
        ),
    };
    record.finish(state, exit, message);
    Ok(())
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

    while !exited || streams_open {
        match lines.recv_timeout(TICK) {
            Ok(Line::Out(line)) => {
                if events_written < EVENTS_CAP {
                    let _ = writeln!(events, "{line}");
                    events_written += line.len() as u64 + 1;
                }
                harness.parse_line(&line, &mut record.progress);
                // The session id is what makes a killed run resumable: on disk
                // the moment it is known, not at the end.
                if record.progress.session_id != known_session {
                    known_session = record.progress.session_id.clone();
                    dir.save(record)?;
                }
            }
            Ok(Line::Err(line)) => {
                eprintln!("[callee] {line}");
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
                eprintln!("[watchdog] over the abort threshold ({over_readings} in a row)");
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
            descendants: spawn::descendants(pgid),
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
