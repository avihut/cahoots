//! `<state>/runs/<id>/` — the source of truth for a run.
//!
//! ```text
//! run.json        the record; ONE writer (the supervisor), replaced atomically
//! brief           what the callee was asked
//! events.jsonl    the callee's stdout, verbatim
//! final.md        the callee's answer
//! supervisor.log  what the supervisor saw, and the callee's stderr
//! lock            held exclusively for the supervisor's lifetime — liveness
//! cancel          a marker the supervisor polls
//! ```

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};

use crate::dirs::{Dirs, ensure_private_dir};
use crate::exit::{Exit, Fail, Res};
use crate::gate::Admission;
use crate::harness::{Progress, Version};
use crate::model::{Candidate, HarnessId, Role};
use crate::placement::Placement;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Starting,
    Running,
    Done,
    Failed,
    TimedOut,
    Cancelled,
    Budget,
    /// The supervisor died without finishing the record; set by reconcile.
    Crashed,
}

impl State {
    pub const fn is_terminal(self) -> bool {
        !matches!(self, State::Starting | State::Running)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub v: u32,
    pub id: String,
    pub state: State,
    /// The cahoots exit code of a finished run. `run`, `wait` and `result`
    /// all return this, so the same run always answers the same way.
    pub exit_code: Option<u8>,
    pub message: Option<String>,
    pub role: Role,
    pub caller: Option<HarnessId>,
    pub target: Candidate,
    /// Where the callee works. For a fork this is the BASE until the
    /// supervisor has cut the worktree, and the worktree from then on.
    pub cwd: PathBuf,
    #[serde(default)]
    pub placement: Placement,
    /// What a fork was cut from.
    #[serde(default)]
    pub base: Option<PathBuf>,
    pub depth: u32,
    pub timeout_secs: u64,
    pub int_grace_secs: u64,
    pub term_grace_secs: u64,
    pub binary: PathBuf,
    pub harness_version: Option<Version>,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub supervisor_pid: Option<i32>,
    pub callee_pid: Option<i32>,
    /// `ps`'s start time for `callee_pid`: the identity reconcile checks
    /// before it signals a pid that may have been recycled.
    pub callee_started: Option<String>,
    pub callee_exit: Option<i32>,
    /// The run this one continues, and the harness session it picks up.
    #[serde(default)]
    pub resumed_from: Option<String>,
    #[serde(default)]
    pub resume_session: Option<String>,
    pub progress: Progress,
    /// Why the gate let this run in.
    #[serde(default)]
    pub admission: Admission,
}

impl RunRecord {
    pub fn finish(&mut self, state: State, exit: Exit, message: Option<String>) {
        self.state = state;
        self.exit_code = Some(exit.code());
        self.message = message;
        self.finished_at = Some(now());
    }

    /// The exit a verb reports for this run.
    pub fn exit(&self) -> u8 {
        match self.exit_code {
            Some(code) if self.state.is_terminal() => code,
            _ => Exit::NotFinished.code(),
        }
    }
}

/// Time-sortable, and safe as a directory name and as an argument.
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

pub fn validate_id(id: &str) -> Res<()> {
    let ok = (1..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Fail::new(Exit::Usage, "a run id is 1–64 of [0-9A-Za-z_-]"))
    }
}

pub struct RunDir {
    pub path: PathBuf,
}

impl RunDir {
    pub fn of(dirs: &Dirs, id: &str) -> Res<RunDir> {
        validate_id(id)?;
        Ok(RunDir {
            path: dirs.runs().join(id),
        })
    }

    pub fn open(dirs: &Dirs, id: &str) -> Res<RunDir> {
        let dir = RunDir::of(dirs, id)?;
        if dir.record_path().is_file() {
            Ok(dir)
        } else {
            Err(Fail::new(Exit::NoSuchRun, format!("no run {id}")))
        }
    }

    pub fn create(dirs: &Dirs, record: &RunRecord, brief: &str) -> Res<RunDir> {
        ensure_private_dir(&dirs.runs())?;
        let dir = RunDir::of(dirs, &record.id)?;
        ensure_private_dir(&dir.path)?;
        write_private(&dir.brief_path(), brief.as_bytes())?;
        dir.save(record)?;
        Ok(dir)
    }

    pub fn record_path(&self) -> PathBuf {
        self.path.join("run.json")
    }
    pub fn brief_path(&self) -> PathBuf {
        self.path.join("brief")
    }
    pub fn events_path(&self) -> PathBuf {
        self.path.join("events.jsonl")
    }
    pub fn final_path(&self) -> PathBuf {
        self.path.join("final.md")
    }
    pub fn log_path(&self) -> PathBuf {
        self.path.join("supervisor.log")
    }

    /// The run's log, to append a line to: what the supervisor saw, for a
    /// person reading it later. The supervisor runs detached and speaks to no
    /// terminal; `spawn_supervisor` points its stdout and stderr here too, so a
    /// panic lands in the same place. A log that can't be opened takes the line
    /// and drops it: not worth failing a run over.
    pub fn log(&self) -> Box<dyn Write> {
        match fs::OpenOptions::new().append(true).open(self.log_path()) {
            Ok(file) => Box::new(file),
            Err(_) => Box::new(std::io::sink()),
        }
    }

    pub fn cancel_path(&self) -> PathBuf {
        self.path.join("cancel")
    }

    pub fn load(&self) -> Res<RunRecord> {
        let text = fs::read_to_string(self.record_path())
            .map_err(|error| Fail::internal(format!("cannot read a run record: {error}")))?;
        serde_json::from_str(&text)
            .map_err(|error| Fail::internal(format!("a run record does not parse: {error}")))
    }

    /// Atomic: a reader sees the old record or the new one, never half of one.
    pub fn save(&self, record: &RunRecord) -> Res<()> {
        let json = serde_json::to_vec_pretty(record)
            .map_err(|error| Fail::internal(format!("cannot encode a run record: {error}")))?;
        write_private_atomic(&self.record_path(), &json)
    }

    /// The liveness lock. `Ok(Some(_))`: nobody held it, and the caller now
    /// does until the guard drops. `Ok(None)`: a live supervisor holds it.
    pub fn try_lock(&self) -> Res<Option<Flock<File>>> {
        try_lock_file(&self.path.join("lock"))
    }
}

pub fn try_lock_file(path: &Path) -> Res<Option<Flock<File>>> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| Fail::internal(format!("cannot open {}: {error}", path.display())))?;
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(guard) => Ok(Some(guard)),
        Err((_, nix::errno::Errno::EWOULDBLOCK)) => Ok(None),
        Err((_, errno)) => Err(Fail::internal(format!(
            "cannot lock {}: {errno}",
            path.display()
        ))),
    }
}

pub fn write_private(path: &Path, bytes: &[u8]) -> Res<()> {
    let mut file = File::options()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| Fail::internal(format!("cannot write {}: {error}", path.display())))?;
    file.write_all(bytes)
        .map_err(|error| Fail::internal(format!("cannot write {}: {error}", path.display())))
}

/// `write_private`, all at once: a reader sees the old file or the new one,
/// never half of one, and a crash leaves the old one. The new one is written
/// beside the file and renamed over it. When `path` is a link, the file it
/// points at is the one replaced, and the link stays a link.
pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> Res<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let name = target
        .file_name()
        .ok_or_else(|| Fail::internal(format!("{} names no file", path.display())))?;
    let tmp = target.with_file_name(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    // One left by a writer that died is not reused: a new file is made 0600.
    let _ = fs::remove_file(&tmp);
    let written = File::options()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&tmp, &target));
    written.map_err(|error| {
        let _ = fs::remove_file(&tmp);
        Fail::internal(format!("cannot write {}: {error}", target.display()))
    })
}

/// Every run on record, oldest first (ids are time-sortable). A directory
/// that does not hold a readable record is skipped, not fatal.
pub fn all(dirs: &Dirs) -> Vec<(RunDir, RunRecord)> {
    let Ok(entries) = fs::read_dir(dirs.runs()) else {
        return Vec::new();
    };
    let mut runs: Vec<(RunDir, RunRecord)> = entries
        .flatten()
        .filter_map(|entry| {
            let id = entry.file_name().into_string().ok()?;
            let dir = RunDir::open(dirs, &id).ok()?;
            let record = dir.load().ok()?;
            Some((dir, record))
        })
        .collect();
    runs.sort_by(|a, b| a.1.id.cmp(&b.1.id));
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_cannot_leave_the_runs_directory() {
        for bad in ["", "..", "../x", "a/b", "a b", "a\0b", &"x".repeat(65)] {
            assert!(validate_id(bad).is_err(), "{bad:?}");
        }
        assert!(validate_id(&new_id()).is_ok());
    }

    #[test]
    fn ids_sort_by_time() {
        let first = new_id();
        std::thread::sleep(std::time::Duration::from_millis(3));
        assert!(first < new_id());
    }

    #[test]
    fn the_lock_is_exclusive_and_dies_with_its_holder() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lock");
        let held = try_lock_file(&path).unwrap();
        assert!(held.is_some());
        assert!(
            try_lock_file(&path).unwrap().is_none(),
            "a second taker got the lock"
        );
        drop(held);
        assert!(try_lock_file(&path).unwrap().is_some());
    }
}
