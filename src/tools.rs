//! The programs cahoots itself runs — `git` and `ps` — and the PATH every
//! program it starts gets. Each tool runs from the one path a person pinned
//! (`[tools.<name>] binary` in config.toml), never from a PATH lookup: the
//! caller sets PATH, and a `git` it planted in a directory it can write would
//! otherwise run outside every sandbox. A person's verb (`install`,
//! `settings`) pins one; every other verb only locates what is pinned, once,
//! and holds it to the binary policy again at each use (`Tool::at`).
//!
//! Logic only: it runs processes through `spawn`, returns data, and never
//! prints.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::config::edit::{Change, KeyPath};
use crate::exit::{Exit, Fail, Res};
use crate::harness::Version;
use crate::model::HarnessId;
use crate::spawn;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolId {
    Git,
    Ps,
}

impl ToolId {
    pub const ALL: [ToolId; 2] = [ToolId::Git, ToolId::Ps];

    /// The program's name: `git`.
    pub const fn name(self) -> &'static str {
        match self {
            ToolId::Git => "git",
            ToolId::Ps => "ps",
        }
    }

    /// Its pin in config.toml: `tools.git.binary`.
    pub fn key(self) -> String {
        format!("tools.{}.binary", self.name())
    }
}

impl std::fmt::Display for ToolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The oldest git cahoots supports and the first it was not tested against.
/// 2.31 is where `rev-parse --path-format=absolute` came, which finding a
/// repository's top needs (`spawn::git_roots`).
pub const GIT_TESTED: (Version, Version) = (Version(2, 31, 0), Version(3, 0, 0));

/// How long a tool may take to say who it is.
const FINGERPRINT_DEADLINE: Duration = Duration::from_secs(10);

/// A tool cahoots located: its pin, held to the binary policy and to its
/// fingerprint, once.
#[derive(Debug, Clone)]
pub struct Tool {
    id: ToolId,
    pinned: PathBuf,
    canonical: PathBuf,
    version: Option<Version>,
    /// The PATH a person recorded (`tools.path`), for [`Tool::path`].
    recorded: Option<OsString>,
}

impl Tool {
    pub fn id(&self) -> ToolId {
        self.id
    }

    /// The path as pinned: a link a package manager keeps current stays one.
    pub fn pinned(&self) -> &Path {
        &self.pinned
    }

    /// The file it resolved to when it was located: the one fingerprinted.
    pub fn canonical(&self) -> &Path {
        &self.canonical
    }

    /// The PATH a person recorded, which every PATH built for a program
    /// started beside this one ends with.
    pub fn recorded(&self) -> Option<&OsStr> {
        self.recorded.as_deref()
    }

    /// git's version, read when it was located. `ps` has none.
    pub fn version(&self) -> Option<Version> {
        self.version
    }

    /// The binary to run for a use whose workspace is `workspace`: the policy
    /// again, for these roots, and still the file that was fingerprinted.
    /// For git, also the first `git` on the PATH this use gives it
    /// (`Tool::path`), so what git starts for itself finds this one too.
    pub fn at(&self, workspace: &[&Path]) -> Res<PathBuf> {
        let name = self.id.name();
        let resolved = spawn::pinned_system_tool(name, &self.pinned, workspace)?;
        if resolved != self.canonical {
            return Err(Fail::config(format!(
                "cahoots needs `{name}`: {} changed after cahoots checked it",
                self.pinned.display()
            )));
        }
        if self.id == ToolId::Git {
            spawn::git_users_path(self, &[], self.recorded(), workspace)?;
        }
        Ok(resolved)
    }

    /// The PATH it runs with: its own directories, the system's, then the
    /// recorded PATH, none of them inside `workspace` (`spawn::own_path`).
    pub fn path(&self, workspace: &[&Path]) -> Option<OsString> {
        spawn::own_path(&[&self.pinned], self.recorded.as_deref(), workspace)
    }
}

/// Both tools, located: what a verb that starts either passes down.
#[derive(Debug, Clone)]
pub struct Tools {
    pub git: Tool,
    pub ps: Tool,
    /// The PATH a person recorded, which every started program's PATH ends
    /// with.
    pub recorded: Option<OsString>,
}

/// The pins as config.toml holds them (`Registry::tools`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Pins {
    pub git: Option<PathBuf>,
    pub ps: Option<PathBuf>,
    /// `tools.path`: the PATH a person recorded.
    pub path: Option<String>,
}

impl Pins {
    pub fn of(&self, id: ToolId) -> Option<&Path> {
        match id {
            ToolId::Git => self.git.as_deref(),
            ToolId::Ps => self.ps.as_deref(),
        }
    }

    pub fn recorded(&self) -> Option<OsString> {
        self.path.as_ref().map(OsString::from)
    }
}

/// Locates both tools, git first: what every verb that starts one does
/// before anything else. `workspace` is what is known of the caller's
/// workspace so far (`spawn::around`): no tool is run from it, not even to
/// ask its version.
pub fn locate(pins: &Pins, workspace: &[&Path]) -> Res<Tools> {
    let recorded = pins.recorded();
    let git = locate_one(
        ToolId::Git,
        pins.git.as_deref(),
        recorded.as_deref(),
        workspace,
    )?;
    let ps = locate_one(
        ToolId::Ps,
        pins.ps.as_deref(),
        recorded.as_deref(),
        workspace,
    )?;
    Ok(Tools { git, ps, recorded })
}

/// [`locate`], for a verb whose workspace is around `cwd`: what is known of
/// it before any git runs (`spawn::around`).
pub fn locate_around(pins: &Pins, cwd: &Path) -> Res<Tools> {
    let around = spawn::around(cwd);
    let around: Vec<&Path> = around.iter().map(PathBuf::as_path).collect();
    locate(pins, &around)
}

/// The fix a refusal names: where a person pins the tool again.
fn pin_again(id: ToolId) -> String {
    format!(
        "pin it again with `cahoots install` or `cahoots settings` ({})",
        id.key()
    )
}

/// One tool: the pin, the binary policy on it, then its fingerprint, run
/// from `/` on the tool's own PATH. A missing pin, a missing file and a
/// failed fingerprint are the person's setup to fix (34); a pin the policy
/// refuses is a refusal (33), and is never run.
pub fn locate_one(
    id: ToolId,
    pinned: Option<&Path>,
    recorded: Option<&OsStr>,
    workspace: &[&Path],
) -> Res<Tool> {
    let name = id.name();
    let Some(pinned) = pinned else {
        return Err(Fail::config(format!(
            "cahoots needs `{name}`: none is pinned — run `cahoots install` (it pins the {name} on \
             your PATH), or choose one with `cahoots settings` ({})",
            id.key()
        )));
    };
    let canonical =
        spawn::pinned_system_tool(name, pinned, workspace).map_err(|fail| match fail.exit {
            Exit::Policy => fail,
            _ => Fail::new(fail.exit, format!("{} — {}", fail.message, pin_again(id))),
        })?;
    let path = spawn::own_path(&[pinned], recorded, workspace);
    if id == ToolId::Git {
        let unfound = |why: String| {
            Fail::config(format!(
                "cahoots needs `git`: {} {why}, so the programs cahoots starts would not find it \
                 first — {}",
                pinned.display(),
                pin_again(id)
            ))
        };
        programs_find(pinned).map_err(unfound)?;
        // On the PATH this use would give it — a directory inside this
        // workspace left off — the first `git` must be this one.
        if spawn::first_git(path.as_deref()).as_deref() != Some(canonical.as_path()) {
            return Err(unfound(
                "is not the git found first on the PATH cahoots builds here".to_string(),
            ));
        }
    }
    let version = match id {
        ToolId::Git => Some(fingerprint_git_at(&canonical, pinned, path)?),
        ToolId::Ps => {
            fingerprint_ps_at(&canonical, pinned, path)?;
            None
        }
    };
    Ok(Tool {
        id,
        pinned: pinned.to_path_buf(),
        canonical,
        version,
        recorded: recorded.map(OsStr::to_os_string),
    })
}

/// Why the programs cahoots starts would not find `pinned` first when they
/// look `git` up, or `Ok`: it must be named `git`, and its directory — which
/// leads every PATH cahoots builds for them (`spawn::own_path`) — must hold
/// programs (`holds_programs`). Otherwise daft, the callee and their hooks
/// would find another `git`, held to no pin and no floor.
fn programs_find(pinned: &Path) -> Result<(), String> {
    if pinned.file_name() != Some(OsStr::new("git")) {
        return Err("is not named git".to_string());
    }
    let dir = pinned.parent().unwrap_or(Path::new("/"));
    if dir.as_os_str().as_bytes().contains(&b':') {
        return Err(format!(
            "is in {}, a directory no PATH can hold (it has a `:`)",
            dir.display()
        ));
    }
    holds_programs(dir, &fixed_temp_roots())
        .map_err(|why| format!("is in {}, {why}", dir.display()))
}

/// `git version 2.50.1 (Apple Git-155)` → 2.50.1. The first line only, and
/// only git's own words before the number.
pub fn fingerprint_git(version_output: &str) -> Option<Version> {
    let line = version_output.lines().next()?;
    line.strip_prefix("git version ").and_then(Version::find_in)
}

/// git, asked its version: it must say it is git, at a version cahoots
/// supports, that never fetches lazily once told so.
fn fingerprint_git_at(binary: &Path, pinned: &Path, path: Option<OsString>) -> Res<Version> {
    let answer = spawn::run_helper_with_path(
        binary,
        &["--version"],
        Some(Path::new("/")),
        FINGERPRINT_DEADLINE,
        path,
    )
    .ok()
    .filter(|output| output.status == Some(0))
    .map(|output| output.stdout)
    .unwrap_or_default();
    let Some(version) = fingerprint_git(&answer) else {
        return Err(Fail::config(format!(
            "cahoots needs `git`: {} does not identify itself as git — {}",
            pinned.display(),
            pin_again(ToolId::Git)
        )));
    };
    let (oldest, _) = GIT_TESTED;
    if version < oldest {
        return Err(Fail::config(format!(
            "cahoots needs `git`: git {version} is older than the oldest version cahoots supports \
             ({oldest}) — {}",
            pin_again(ToolId::Git)
        )));
    }
    let line = answer.lines().next().unwrap_or_default().trim();
    if !spawn::honours_no_lazy_fetch(line) {
        return Err(Fail::config(format!(
            "cahoots needs a git that never fetches lazily — 2.46 or later, or 2.39.4, 2.40.2, \
             2.41.1, 2.42.2, 2.43.4, 2.44.1 or 2.45.1 or later in its line — refusing to run {} \
             (found `{line}`) — {}",
            pinned.display(),
            pin_again(ToolId::Git)
        )));
    }
    Ok(version)
}

/// Whether `stdout` is ps's answer for this process: its pid, then its
/// parent's, as numbers (procps pads them). The parent may be read before
/// or after ps ran: a supervisor's parent can exit meanwhile.
pub fn ps_answers(stdout: &str, pid: u32, ppids: &[u32]) -> bool {
    let fields: Vec<&str> = stdout.split_whitespace().collect();
    match fields[..] {
        [first, second] => {
            first.parse::<u32>() == Ok(pid)
                && second
                    .parse::<u32>()
                    .is_ok_and(|ppid| ppids.contains(&ppid))
        }
        _ => false,
    }
}

/// ps, asked about this very process: it must answer as ps does.
fn fingerprint_ps_at(binary: &Path, pinned: &Path, path: Option<OsString>) -> Res<()> {
    let pid = std::process::id();
    let before = nix::unistd::getppid().as_raw() as u32;
    let answer = spawn::run_helper_with_path(
        binary,
        &["-o", "pid=,ppid=", "-p", &pid.to_string()],
        Some(Path::new("/")),
        FINGERPRINT_DEADLINE,
        path,
    )
    .ok()
    .filter(|output| output.status == Some(0))
    .map(|output| output.stdout)
    .unwrap_or_default();
    let after = nix::unistd::getppid().as_raw() as u32;
    if !ps_answers(&answer, pid, &[before, after]) {
        return Err(Fail::config(format!(
            "cahoots needs `ps`: {} does not answer as ps — {}",
            pinned.display(),
            pin_again(ToolId::Ps)
        )));
    }
    Ok(())
}

/// The copies of `name` on `path` (a person's PATH) that may be asked who
/// they are, in PATH's order, each with why not when it may not: one in a
/// temp directory, one everyone can write (`holds_programs`), or one inside
/// the person's own repository (`workspace`) — where it is found, or where
/// it resolves — is never run, not even for its version. The binary policy lets a sticky temp directory through, and a
/// repository's `bin` is one the agents working in it can write; a pin made
/// from either would be the planted program this module keeps out.
fn candidates(
    name: &str,
    path: Option<&OsStr>,
    workspace: &[PathBuf],
) -> Vec<(PathBuf, Option<String>)> {
    spawn::find_all_on_path(name, path)
        .into_iter()
        .map(|candidate| {
            let why = pin_unfit(&candidate, workspace);
            (candidate, why)
        })
        .collect()
}

/// Why a program at `pinned` may not be run to be asked who it is — found
/// on a person's PATH, kept from before, or named in `settings` — or `None`:
/// the directory it is in, and the one it resolves into, must hold programs
/// (`holds_programs`) and lie outside the person's own repository
/// (`workspace`). One test, so that every way to a pin is held to it alike.
pub fn pin_unfit(pinned: &Path, workspace: &[PathBuf]) -> Option<String> {
    // Nothing there is nothing to run: the locator says it is gone.
    if pinned.symlink_metadata().is_err() {
        return None;
    }
    let temp = recording_temp_roots();
    let canonical = fs::canonicalize(pinned).unwrap_or_else(|_| pinned.to_path_buf());
    [pinned.parent(), canonical.parent()]
        .into_iter()
        .flatten()
        .find_map(|dir| {
            unfit_dir(dir, &temp, workspace, true)
                .map(|why| format!("{} is in {}, {why}", pinned.display(), dir.display()))
        })
}

/// Why `dir` may not hold a program `install` pins (`programs`), or be in a
/// recorded PATH: it lies inside `workspace`, by name or by where it
/// resolves, or it is not fit (`holds_programs`, `qualifies`).
fn unfit_dir(
    dir: &Path,
    temp: &[PathBuf],
    workspace: &[PathBuf],
    programs: bool,
) -> Option<&'static str> {
    let canonical = fs::canonicalize(dir).ok();
    let inside = workspace.iter().any(|root| {
        let root_canonical = fs::canonicalize(root).ok();
        dir.starts_with(root)
            || root_canonical.as_ref().is_some_and(|r| dir.starts_with(r))
            || canonical.as_ref().is_some_and(|c| {
                c.starts_with(root) || root_canonical.as_ref().is_some_and(|r| c.starts_with(r))
            })
    });
    if dir.is_absolute() && inside {
        return Some("inside the workspace");
    }
    if programs {
        holds_programs(dir, temp).err()
    } else {
        qualifies(dir, temp).err()
    }
}

/// Why a pinned program's own directory may not be trusted as far as the
/// pin is — to hold a copy `install` pins, or to go on the PATH the program
/// runs with — or `Ok`: an absolute, existing directory, not under a temp
/// directory, and not writable by everyone. One its group can write is as
/// trustworthy as the pin in it, which that group could replace anyway (as
/// Homebrew's `admin`-writable `bin` is); one everyone can write, even
/// sticky, lets others add a program of their own beside it.
pub fn holds_programs(dir: &Path, temp_roots: &[PathBuf]) -> Result<(), &'static str> {
    if !dir.is_absolute() {
        return Err("not absolute");
    }
    if temp_roots.iter().any(|root| dir.starts_with(root)) {
        return Err("a temp directory");
    }
    let Ok(canonical) = fs::canonicalize(dir) else {
        return Err("not a directory");
    };
    let Ok(meta) = fs::metadata(&canonical) else {
        return Err("not a directory");
    };
    if !meta.is_dir() {
        return Err("not a directory");
    }
    if temp_roots.iter().any(|root| canonical.starts_with(root)) {
        return Err("a temp directory");
    }
    if meta.permissions().mode() & 0o002 != 0 {
        return Err("writable by everyone");
    }
    Ok(())
}

/// For `install`: the first copy of the tool on `path` (a person's PATH)
/// that locates, or why none did — the first copy's reason, or that there is
/// none.
pub fn discover(
    id: ToolId,
    path: Option<&OsStr>,
    recorded: Option<&OsStr>,
    workspace: &[PathBuf],
) -> Result<Tool, String> {
    let roots: Vec<&Path> = workspace.iter().map(PathBuf::as_path).collect();
    let mut first_why = None;
    for (candidate, unfit) in candidates(id.name(), path, workspace) {
        if let Some(why) = unfit {
            first_why.get_or_insert(why);
            continue;
        }
        match locate_one(id, Some(&candidate), recorded, &roots) {
            Ok(tool) => return Ok(tool),
            Err(fail) => {
                first_why.get_or_insert(fail.message);
            }
        }
    }
    Err(first_why.unwrap_or_else(|| format!("no `{}` on your PATH", id.name())))
}

/// For `install` and `enable`: the first copy of harness `id` on `path` (a
/// person's PATH) that locates as a run would hold it, as found, with its
/// version — or, when none does, why the first copy did not (`None` when
/// there is none).
pub fn discover_harness(
    id: HarnessId,
    path: Option<&OsStr>,
    home: Option<&Path>,
    git: Option<&Tool>,
    recorded: Option<&OsStr>,
    workspace: &[PathBuf],
) -> Result<(PathBuf, Version), Option<String>> {
    let roots: Vec<&Path> = workspace.iter().map(PathBuf::as_path).collect();
    let mut first_why = None;
    for (candidate, unfit) in candidates(crate::harness::harness(id).binary_name(), path, workspace)
    {
        if let Some(why) = unfit {
            first_why.get_or_insert(why);
            continue;
        }
        match crate::pick::locate_pinned(id, &candidate, home, git, recorded, &roots) {
            Ok(located) => return Ok((candidate, located.version)),
            Err(fail) => {
                first_why.get_or_insert(fail.message);
            }
        }
    }
    Err(first_why)
}

/// What `install` did, or would do, with one pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// The pin still locates: nothing is written.
    Kept,
    /// There was none, and the first copy on the person's PATH that locates
    /// is pinned.
    Pinned,
    /// The pin no longer locates, and a copy on the PATH that does replaces
    /// it.
    Repinned,
    /// No copy on the PATH locates: nothing is written.
    NoneFound,
    /// Not asked anything — a harness, while no git is pinned — and left as
    /// it is.
    Unchecked,
}

/// One program's pin, as `install` decided it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pin {
    pub decision: Decision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The pin it replaced, or that no longer locates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub was: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// What `install` did, or would do, with the recorded PATH.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathDecision {
    /// None was recorded, and the person's is now.
    Recorded,
    /// It differed from the person's PATH, which replaces it.
    Rerecorded,
    /// It is the same.
    Kept,
    /// Nothing in the person's PATH qualifies: nothing is written.
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathPin {
    pub decision: PathDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub dropped: Vec<Dropped>,
}

/// What `install` did, or would do, with a harness's settings folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeDecision {
    /// The person's terminal names one, config.toml none: it is recorded.
    Recorded,
    /// config.toml names one already: it is the person's, and stays.
    Kept,
    /// The terminal's does not qualify (`check_home`): nothing is written.
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HomePin {
    pub decision: HomeDecision,
    pub home: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// Every pin `install` decided, and the changes to config.toml that make
/// them so.
#[derive(Debug, Clone)]
pub struct Chosen {
    pub git: Pin,
    pub ps: Pin,
    pub harnesses: Vec<(HarnessId, Pin)>,
    pub path: PathPin,
    /// Only the harnesses whose settings folder the terminal names.
    pub homes: Vec<(HarnessId, HomePin)>,
    pub changes: Vec<Change>,
}

impl Chosen {
    /// `data.pins` in `install`'s envelope.
    pub fn to_json(&self) -> serde_json::Value {
        let mut pins = serde_json::Map::new();
        let mut put = |key: String, value: serde_json::Value| {
            pins.insert(key, value);
        };
        put("git".into(), serde_json::json!(self.git));
        put("ps".into(), serde_json::json!(self.ps));
        for (id, pin) in &self.harnesses {
            put(id.to_string(), serde_json::json!(pin));
        }
        put("path".into(), serde_json::json!(self.path));
        let homes: serde_json::Map<String, serde_json::Value> = self
            .homes
            .iter()
            .map(|(id, home)| (id.to_string(), serde_json::json!(home)))
            .collect();
        put("homes".into(), serde_json::Value::Object(homes));
        serde_json::Value::Object(pins)
    }
}

/// What a person's terminal says that `install` and `enable` read: their
/// PATH, and each harness's settings folder it names.
#[derive(Debug, Clone, Default)]
pub struct Terminal {
    pub path: Option<OsString>,
    pub homes: Vec<(HarnessId, PathBuf)>,
    /// The repository the person is in, if any (`repository_around`):
    /// nothing in it is pinned, run or recorded.
    pub workspace: Vec<PathBuf>,
}

impl Terminal {
    /// This process's, which is a person's: only `install` and `enable` read
    /// it, from a terminal.
    pub fn here() -> Terminal {
        let homes = [
            (HarnessId::Codex, crate::env::codex_home()),
            (HarnessId::Claude, crate::env::claude_config_dir()),
        ]
        .into_iter()
        .filter_map(|(id, home)| Some((id, home?)))
        .collect();
        Terminal {
            path: crate::env::path_var(),
            homes,
            workspace: std::env::current_dir()
                .map(|cwd| repository_around(&cwd))
                .unwrap_or_default(),
        }
    }

    /// The settings folder this terminal names for `id`, if it names one.
    pub fn home(&self, id: HarnessId) -> Option<&Path> {
        self.homes
            .iter()
            .find(|(harness, _)| *harness == id)
            .map(|(_, home)| home.as_path())
    }
}

fn set(key: &str, value: &str) -> Change {
    Change::Set {
        path: KeyPath::of(key),
        value: value.into(),
    }
}

/// The recorded PATH `install` and `enable` would write from the person's,
/// against what is recorded now (`recorded`).
pub fn decide_path(
    person: Option<&OsStr>,
    recorded: Option<&str>,
    workspace: &[PathBuf],
) -> PathPin {
    let now = record_path(person, &recording_temp_roots(), workspace);
    let dirs = |path: Option<&str>| -> Vec<String> {
        path.map(|path| path.split(':').map(str::to_string).collect())
            .unwrap_or_default()
    };
    let (new, old) = (dirs(now.path.as_deref()), dirs(recorded));
    let decision = match (&now.path, recorded) {
        (None, _) => PathDecision::Empty,
        (Some(_), None) => PathDecision::Recorded,
        (Some(path), Some(recorded)) if path == recorded => PathDecision::Kept,
        (Some(_), Some(_)) => PathDecision::Rerecorded,
    };
    PathPin {
        decision,
        added: match decision {
            PathDecision::Rerecorded => new.iter().filter(|d| !old.contains(d)).cloned().collect(),
            _ => Vec::new(),
        },
        removed: match decision {
            PathDecision::Rerecorded => old.iter().filter(|d| !new.contains(d)).cloned().collect(),
            _ => Vec::new(),
        },
        path: now.path,
        dropped: now.dropped,
    }
}

/// A harness's settings folder from the person's terminal, against what
/// config.toml names (`set`).
pub fn decide_home(terminal: &Path, set: Option<&Path>) -> HomePin {
    match (set, check_home(terminal)) {
        (Some(set), _) => HomePin {
            decision: HomeDecision::Kept,
            home: set.to_path_buf(),
            why: None,
        },
        (None, Ok(())) => HomePin {
            decision: HomeDecision::Recorded,
            home: terminal.to_path_buf(),
            why: None,
        },
        (None, Err(why)) => HomePin {
            decision: HomeDecision::Refused,
            home: terminal.to_path_buf(),
            why: Some(why),
        },
    }
}

/// `install`'s pins, decided from config.toml as it is (`config`) and the
/// person's terminal: the recorded PATH first, then git and ps, then each
/// harness (`only`, or all) — kept while it locates, pinned or pinned again
/// from the first copy on the person's PATH that does, and left alone when
/// none does — then each settings folder the terminal names. Nothing is
/// asked: the first copy on a person's PATH is what their own shell runs,
/// and `settings` chooses another. Nothing is written here.
pub fn choose_pins(
    config: &crate::config::UserConfig,
    only: Option<HarnessId>,
    terminal: &Terminal,
) -> Chosen {
    let now = crate::registry::Registry::effective(config);
    let mut changes = Vec::new();

    let path = decide_path(
        terminal.path.as_deref(),
        now.tools.path.as_deref(),
        &terminal.workspace,
    );
    let recorded = match path.decision {
        PathDecision::Recorded | PathDecision::Rerecorded => {
            changes.extend(path.path.as_deref().map(|value| set("tools.path", value)));
            path.path.clone()
        }
        PathDecision::Kept | PathDecision::Empty => now.tools.path.clone(),
    };
    let recorded_os = recorded.as_ref().map(OsString::from);

    // Where install runs: nothing in the person's repository is run, pinned
    // or kept, and a pin kept from before is held to exactly what a copy on
    // their PATH is held to, before it is asked anything.
    let roots: Vec<&Path> = terminal.workspace.iter().map(PathBuf::as_path).collect();
    let mut tool = |id: ToolId| -> (Pin, Option<Tool>) {
        let pinned = now.tools.of(id);
        let kept = pinned.map(|pinned| match pin_unfit(pinned, &terminal.workspace) {
            Some(why) => Err(why),
            None => locate_one(id, Some(pinned), recorded_os.as_deref(), &roots)
                .map_err(|fail| fail.message),
        });
        let (decision, pin) = match kept {
            Some(Ok(tool)) => {
                let pin = Pin {
                    decision: Decision::Kept,
                    binary: Some(tool.pinned.clone()),
                    version: tool.version.map(|v| v.to_string()),
                    was: None,
                    why: None,
                };
                return (pin, Some(tool));
            }
            Some(Err(why)) => (Decision::Repinned, Some((pinned, why))),
            None => (Decision::Pinned, None),
        };
        match discover(
            id,
            terminal.path.as_deref(),
            recorded_os.as_deref(),
            &terminal.workspace,
        ) {
            Ok(found) => {
                changes.push(set(&id.key(), &found.pinned.to_string_lossy()));
                let pin = Pin {
                    decision,
                    binary: Some(found.pinned.clone()),
                    version: found.version.map(|v| v.to_string()),
                    was: pin.as_ref().and_then(|(was, _)| was.map(Path::to_path_buf)),
                    why: pin.map(|(_, why)| why),
                };
                (pin, Some(found))
            }
            Err(why) => {
                let pin = Pin {
                    decision: Decision::NoneFound,
                    binary: None,
                    version: None,
                    was: pin.as_ref().and_then(|(was, _)| was.map(Path::to_path_buf)),
                    why: Some(why),
                };
                (pin, None)
            }
        }
    };
    let (git, located_git) = tool(ToolId::Git);
    let (ps, _) = tool(ToolId::Ps);

    let mut harnesses = Vec::new();
    let mut homes = Vec::new();
    for id in HarnessId::ALL
        .into_iter()
        .filter(|id| only.is_none_or(|only| only == *id))
    {
        let entry = now.harness(id);
        let home = match terminal.home(id) {
            Some(terminal) => {
                let pin = decide_home(terminal, entry.home.as_deref());
                if pin.decision == HomeDecision::Recorded {
                    changes.push(set(
                        &format!("harness.{id}.home"),
                        &pin.home.to_string_lossy(),
                    ));
                }
                let home = (pin.decision != HomeDecision::Refused).then(|| pin.home.clone());
                homes.push((id, pin));
                home
            }
            None => entry.home.clone(),
        };
        let pinned = entry.binary.as_deref();
        // A harness runs git of its own: it is asked nothing until the git
        // it would find first is the located one. Its pin stays as it is.
        let Some(git) = located_git.as_ref() else {
            harnesses.push((
                id,
                Pin {
                    decision: Decision::Unchecked,
                    binary: pinned.map(Path::to_path_buf),
                    version: None,
                    was: None,
                    why: Some("no git is pinned, so it was not asked its version".to_string()),
                },
            ));
            continue;
        };
        let kept = pinned.map(|pinned| match pin_unfit(pinned, &terminal.workspace) {
            Some(why) => Err(why),
            None => crate::pick::locate_pinned(
                id,
                pinned,
                home.as_deref(),
                Some(git),
                recorded_os.as_deref(),
                &roots,
            )
            .map_err(|fail| fail.message),
        });
        let pin = match kept {
            Some(Ok(located)) => Pin {
                decision: Decision::Kept,
                binary: pinned.map(Path::to_path_buf),
                version: Some(located.version.to_string()),
                was: None,
                why: None,
            },
            kept => {
                let broken = match kept {
                    Some(Err(why)) => Some(why),
                    _ => None,
                };
                match discover_harness(
                    id,
                    terminal.path.as_deref(),
                    home.as_deref(),
                    Some(git),
                    recorded_os.as_deref(),
                    &terminal.workspace,
                ) {
                    Ok((found, version)) => {
                        changes.push(set(
                            &format!("harness.{id}.binary"),
                            &found.to_string_lossy(),
                        ));
                        Pin {
                            decision: if broken.is_some() {
                                Decision::Repinned
                            } else {
                                Decision::Pinned
                            },
                            binary: Some(found),
                            version: Some(version.to_string()),
                            was: broken.as_ref().and(pinned.map(Path::to_path_buf)),
                            why: broken,
                        }
                    }
                    Err(why) => Pin {
                        decision: Decision::NoneFound,
                        binary: None,
                        version: None,
                        was: broken.as_ref().and(pinned.map(Path::to_path_buf)),
                        why: Some(why.unwrap_or_else(|| {
                            format!(
                                "no `{}` on your PATH",
                                crate::harness::harness(id).binary_name()
                            )
                        })),
                    },
                }
            }
        };
        harnesses.push((id, pin));
    }
    Chosen {
        git,
        ps,
        harnesses,
        path,
        homes,
        changes,
    }
}

/// The directories every started program searches before the recorded
/// PATH: the system's own.
pub const SYSTEM_DIRS: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// The repository `cwd` is in — `cwd` and every repository top above it
/// (`spawn::around`) — or nothing outside one. A repository's own `bin` on a
/// person's PATH (direnv, mise) is one the agents working in it can write,
/// so `install`, `enable` and `doctor` take nothing from it. A directory
/// outside any repository, the home directory a terminal starts in among
/// them, excludes nothing.
pub fn repository_around(cwd: &Path) -> Vec<PathBuf> {
    let around = spawn::around(cwd);
    if around.len() > 1 { around } else { Vec::new() }
}

/// The temp directories, as they are wherever cahoots runs: a recorded PATH
/// never names one. At use, the caller's `TMPDIR` is not among them: it
/// could otherwise unmark a temp directory of its choosing.
pub fn fixed_temp_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for dir in [
        "/tmp",
        "/var/tmp",
        "/var/folders",
        "/private/tmp",
        "/private/var/folders",
    ] {
        let dir = PathBuf::from(dir);
        if let Ok(canonical) = fs::canonicalize(&dir)
            && !roots.contains(&canonical)
        {
            roots.push(canonical);
        }
        if !roots.contains(&dir) {
            roots.push(dir);
        }
    }
    roots
}

/// The temp directories while a person records a PATH: the fixed ones, and
/// their own `TMPDIR`.
pub fn recording_temp_roots() -> Vec<PathBuf> {
    let mut roots = fixed_temp_roots();
    if let Some(tmpdir) = crate::env::tmpdir() {
        roots.extend(fs::canonicalize(&tmpdir).ok());
        roots.push(tmpdir);
    }
    roots
}

/// Why a directory may not be part of a recorded PATH (`tools.path`), or
/// `Ok`: it must be an absolute, existing directory, not under a temp
/// directory, owned by this user or by root, and not writable by its group
/// or by everyone. Stricter than `holds_programs`: nothing a person pinned
/// vouches for what else such a directory holds.
pub fn qualifies(dir: &Path, temp_roots: &[PathBuf]) -> Result<(), &'static str> {
    if !dir.is_absolute() {
        return Err("not absolute");
    }
    if temp_roots.iter().any(|root| dir.starts_with(root)) {
        return Err("a temp directory");
    }
    let Ok(canonical) = fs::canonicalize(dir) else {
        return Err("not a directory");
    };
    let Ok(meta) = fs::metadata(&canonical) else {
        return Err("not a directory");
    };
    if !meta.is_dir() {
        return Err("not a directory");
    }
    if temp_roots.iter().any(|root| canonical.starts_with(root)) {
        return Err("a temp directory");
    }
    let owner = meta.uid();
    if meta.permissions().mode() & 0o022 != 0
        || (owner != 0 && owner != nix::unistd::geteuid().as_raw())
    {
        return Err("writable by others");
    }
    Ok(())
}

/// Whether `dir` is one of the system's directories, which every PATH
/// cahoots builds already searches.
fn is_system_dir(dir: &Path) -> bool {
    let canonical = fs::canonicalize(dir).ok();
    SYSTEM_DIRS.iter().any(|system| {
        let system = Path::new(system);
        dir == system || canonical.as_deref() == fs::canonicalize(system).ok().as_deref()
    })
}

/// A directory left out of a recorded PATH, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dropped {
    pub dir: PathBuf,
    pub why: &'static str,
}

/// What `install` would record as `tools.path` from a person's PATH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The PATH value to record; `None` when nothing qualified.
    pub path: Option<String>,
    pub dropped: Vec<Dropped>,
}

/// The person's PATH, in order, each directory once, without what does not
/// qualify (`qualifies`), what lies inside their workspace (`workspace`),
/// what is not text, and the system's directories, which come earlier
/// anyway. Pure apart from `stat`.
pub fn record_path(
    person_path: Option<&OsStr>,
    temp_roots: &[PathBuf],
    workspace: &[PathBuf],
) -> Recorded {
    let mut kept: Vec<String> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut dropped = Vec::new();
    for dir in person_path.map(std::env::split_paths).into_iter().flatten() {
        let canonical = fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if seen.contains(&dir) || (dir.is_absolute() && seen.contains(&canonical)) {
            continue;
        }
        seen.extend([dir.clone(), canonical]);
        let why = if !dir.is_absolute() {
            Some("not absolute")
        } else if is_system_dir(&dir) {
            Some("already searched")
        } else {
            unfit_dir(&dir, temp_roots, workspace, false)
        };
        match (why, dir.to_str()) {
            (Some(why), _) => dropped.push(Dropped { dir, why }),
            (None, None) => dropped.push(Dropped {
                dir,
                why: "not text",
            }),
            (None, Some(text)) if text.contains(':') => dropped.push(Dropped {
                dir,
                why: "not text",
            }),
            (None, Some(text)) => kept.push(text.to_string()),
        }
    }
    Recorded {
        path: (!kept.is_empty()).then(|| kept.join(":")),
        dropped,
    }
}

/// `settings set tools.path`: every entry held to `record_path`'s rules,
/// refused rather than dropped. Runs nothing.
pub fn check_path(value: &str, temp_roots: &[PathBuf]) -> Result<(), String> {
    if value.is_empty() {
        return Err("it lists no directory".to_string());
    }
    for entry in value.split(':') {
        let dir = Path::new(entry);
        let why = if entry.is_empty() || !dir.is_absolute() {
            Some("not absolute")
        } else if is_system_dir(dir) {
            Some("already searched")
        } else {
            qualifies(dir, temp_roots).err()
        };
        if let Some(why) = why {
            return Err(format!("{entry:?} is {why}"));
        }
    }
    Ok(())
}

/// Why a harness's settings folder (`harness.<id>.home`) may not be used,
/// or `Ok`: it must be an absolute, existing directory, this user's, and
/// writable by no one else. Runs nothing.
pub fn check_home(dir: &Path) -> Result<(), String> {
    if !dir.is_absolute() {
        return Err("not an absolute path".to_string());
    }
    let meta = fs::metadata(dir).map_err(|error| error.to_string())?;
    if !meta.is_dir() {
        return Err("not a directory".to_string());
    }
    if meta.uid() != nix::unistd::geteuid().as_raw() {
        return Err("not yours".to_string());
    }
    if meta.permissions().mode() & 0o022 != 0 {
        return Err("writable by its group or by everyone".to_string());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// git and ps, located from this test process's own PATH: what a unit
    /// test that runs a real git hands the code under test.
    pub(crate) fn tests_located() -> Tools {
        let path = crate::env::path_var();
        let pins = Pins {
            git: spawn::find_on_path("git", path.as_deref()),
            ps: spawn::find_on_path("ps", path.as_deref()),
            path: None,
        };
        locate(&pins, &[]).expect("git and ps for the unit tests")
    }

    fn script(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{text}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn git_versions_are_read_from_real_strings() {
        assert_eq!(
            fingerprint_git("git version 2.50.1 (Apple Git-155)\n"),
            Some(Version(2, 50, 1))
        );
        assert_eq!(
            fingerprint_git("git version 2.43.0"),
            Some(Version(2, 43, 0))
        );
        for not in ["daft 1.27.9", "2.50.1", "", "version 2.50.1"] {
            assert_eq!(fingerprint_git(not), None, "{not:?}");
        }
    }

    #[test]
    fn ps_answers_only_with_this_process_s_ids() {
        assert!(ps_answers("  123   45\n", 123, &[45]));
        assert!(ps_answers("123 46", 123, &[45, 46]));
        for not in ["123", "123 45 6", "124 45", "123 44", "pid ppid", "", "1 1"] {
            assert!(!ps_answers(not, 123, &[45]), "{not:?}");
        }
    }

    #[test]
    fn a_git_the_programs_cahoots_starts_would_not_find_first_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let temp = fs::canonicalize(temp.path()).unwrap();
        let open = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let open = fs::canonicalize(open.path()).unwrap();
        let real = tests_located().git.canonical;
        let link = |dir: &Path, name: &str| {
            let path = dir.join(name);
            std::os::unix::fs::symlink(&real, &path).unwrap();
            path
        };
        let in_temp = link(&temp, "git");
        let renamed = link(&open, "git-latest");
        for (pin, says) in [
            (&in_temp, "a temp directory"),
            (&renamed, "is not named git"),
        ] {
            let fail = locate_one(ToolId::Git, Some(pin), None, &[]).unwrap_err();
            assert_eq!(fail.exit, Exit::Config, "{says}");
            assert!(fail.message.contains(says), "{}", fail.message);
            assert!(fail.message.contains("find it first"), "{}", fail.message);
        }
    }

    #[test]
    fn a_git_user_s_path_must_find_the_pinned_git_first() {
        // Pinned as `g/git`, a link to `r/git-impl`: a stand-in that is the
        // suite's git under another name.
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let real = tests_located().git.canonical;
        fs::create_dir_all(root.join("g")).unwrap();
        let impl_ = script(
            &{
                fs::create_dir_all(root.join("r")).unwrap();
                root.join("r")
            },
            "git-impl",
            &format!("exec '{}' \"$@\"", real.display()),
        );
        std::os::unix::fs::symlink(&impl_, root.join("g/git")).unwrap();
        let git = locate_one(ToolId::Git, Some(&root.join("g/git")), None, &[]).unwrap();
        let path = spawn::git_users_path(&git, &[], None, &[]).unwrap();
        assert_eq!(
            spawn::first_git(path.as_deref()).as_deref(),
            Some(impl_.as_path())
        );
        // A use whose roots hold the pin's directory leaves it off the PATH,
        // and `r` has no `git`: another one would be found first.
        let hidden = root.join("g");
        let fail = spawn::git_users_path(&git, &[], None, &[&hidden]).unwrap_err();
        assert_eq!(fail.exit, Exit::Config);
        assert!(fail.message.contains("would find"), "{}", fail.message);
        // So does every use of that git itself.
        assert_eq!(git.at(&[&hidden]).unwrap_err().exit, Exit::Config);
    }

    #[test]
    fn a_missing_pin_names_the_fix() {
        for id in ToolId::ALL {
            let fail = locate_one(id, None, None, &[]).unwrap_err();
            assert_eq!(fail.exit, Exit::Config);
            assert!(fail.message.contains("cahoots install"), "{}", fail.message);
            assert!(fail.message.contains(&id.key()), "{}", fail.message);
        }
        let fail =
            locate_one(ToolId::Git, Some(Path::new("/nonexistent/git")), None, &[]).unwrap_err();
        assert_eq!(fail.exit, Exit::Config);
        assert!(fail.message.contains("pin it again"), "{}", fail.message);
    }

    #[test]
    fn a_git_that_is_not_git_or_below_a_floor_is_refused_and_never_run_past_its_version() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let ran = root.join("ran");
        for (name, says, refused) in [
            ("a", "not-git 1.0", "does not identify itself as git"),
            (
                "b",
                "git version 2.20.0",
                "older than the oldest version cahoots supports (2.31.0)",
            ),
            ("c", "git version 2.45.0", "found `git version 2.45.0`"),
        ] {
            let bin = root.join(name);
            fs::create_dir(&bin).unwrap();
            let git = script(
                &bin,
                "git",
                &format!(
                    "if [ \"$1\" = --version ]; then echo '{says}'; exit 0; fi\necho \"$@\" >> '{}'",
                    ran.display()
                ),
            );
            let fail = locate_one(ToolId::Git, Some(&git), None, &[]).unwrap_err();
            assert_eq!(fail.exit, Exit::Config, "{says}");
            assert!(fail.message.contains(refused), "{says}: {}", fail.message);
            assert!(
                fail.message.contains("tools.git.binary"),
                "{}",
                fail.message
            );
        }
        assert!(!ran.exists(), "a refused git ran for more than its version");
    }

    #[test]
    fn a_ps_that_does_not_answer_as_ps_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let ps = script(&root, "ps", "echo '1 1'");
        let fail = locate_one(ToolId::Ps, Some(&ps), None, &[]).unwrap_err();
        assert_eq!(fail.exit, Exit::Config);
        assert!(
            fail.message.contains("does not answer as ps"),
            "{}",
            fail.message
        );
    }

    #[test]
    fn a_pin_in_the_workspace_is_refused_before_it_is_asked_anything() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let ran = root.join("ran");
        let git = script(&root, "git", &format!("touch '{}'", ran.display()));
        let fail = locate_one(ToolId::Git, Some(&git), None, &[&root]).unwrap_err();
        assert_eq!(fail.exit, Exit::Policy);
        assert!(!ran.exists());
    }

    #[test]
    fn locate_refuses_a_changed_file_at_use() {
        let located = tests_located();
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        // A link to the real git, located, then pointed elsewhere.
        let link = root.join("git");
        std::os::unix::fs::symlink(located.git.canonical.clone(), &link).unwrap();
        let git = locate_one(ToolId::Git, Some(&link), None, &[]).unwrap();
        assert!(git.at(&[]).is_ok());
        fs::remove_file(&link).unwrap();
        let other = script(&root, "other", "exit 0");
        std::os::unix::fs::symlink(&other, &link).unwrap();
        let fail = git.at(&[]).unwrap_err();
        assert_eq!(fail.exit, Exit::Config);
        assert!(
            fail.message.contains("changed after cahoots checked it"),
            "{}",
            fail.message
        );
        // And inside the roots of a later use: refused by the policy.
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(located.git.canonical.clone(), &link).unwrap();
        assert!(git.at(&[]).is_ok());
        let parent = located.git.canonical.parent().unwrap().to_path_buf();
        assert_eq!(git.at(&[&parent]).unwrap_err().exit, Exit::Policy);
    }

    #[test]
    fn discovery_never_runs_a_copy_in_a_temp_or_open_directory() {
        // Under the temp directory: the binary policy alone would take it.
        let temp = tempfile::tempdir().unwrap();
        let temp = fs::canonicalize(temp.path()).unwrap();
        let ran = temp.join("ran");
        script(
            &temp,
            "git",
            &format!("touch '{}'\necho 'git version 2.50.1'", ran.display()),
        );
        let found = discover(ToolId::Git, Some(temp.as_os_str()), None, &[]).unwrap_err();
        assert!(found.contains("a temp directory"), "{found}");
        assert!(!ran.exists(), "a copy in a temp directory was run");

        // Writable by everyone: the same. (One its group can write is as
        // trustworthy as a pin there: `holds_programs`.)
        let open = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let open = fs::canonicalize(open.path()).unwrap();
        script(
            &open,
            "git",
            &format!("touch '{}'\necho 'git version 2.50.1'", ran.display()),
        );
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
        let found = discover(ToolId::Git, Some(open.as_os_str()), None, &[]).unwrap_err();
        assert!(found.contains("writable by everyone"), "{found}");
        assert!(!ran.exists(), "a copy everyone can write beside was run");
        fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).unwrap();

        // In the person's own workspace: the agents working there can write it.
        let found = discover(
            ToolId::Git,
            Some(open.as_os_str()),
            None,
            std::slice::from_ref(&open),
        )
        .unwrap_err();
        assert!(found.contains("inside the workspace"), "{found}");
        assert!(!ran.exists(), "a copy in the workspace was run");
    }

    #[test]
    fn record_path_keeps_order_and_drops_with_reasons() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let (a, b, open) = (root.join("a"), root.join("b"), root.join("open"));
        for dir in [&a, &b, &open] {
            fs::create_dir(dir).unwrap();
        }
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
        let temp = root.join("temp");
        fs::create_dir(&temp).unwrap();
        let path = std::env::join_paths([
            a.clone(),
            PathBuf::from("relative"),
            root.join("missing"),
            open.clone(),
            temp.join("x"),
            temp.clone(),
            PathBuf::from("/usr/bin"),
            b.clone(),
            a.clone(),
        ])
        .unwrap();
        let recorded = record_path(Some(&path), std::slice::from_ref(&temp), &[]);
        assert_eq!(
            recorded.path.as_deref(),
            Some(format!("{}:{}", a.display(), b.display()).as_str())
        );
        let why = |dir: &Path| {
            recorded
                .dropped
                .iter()
                .find(|dropped| dropped.dir == dir)
                .map(|dropped| dropped.why)
        };
        assert_eq!(why(Path::new("relative")), Some("not absolute"));
        assert_eq!(why(&root.join("missing")), Some("not a directory"));
        assert_eq!(why(&open), Some("writable by others"));
        assert_eq!(why(&temp), Some("a temp directory"));
        assert_eq!(why(Path::new("/usr/bin")), Some("already searched"));
        assert_eq!(recorded.dropped.len(), 6, "{:?}", recorded.dropped);
        assert_eq!(record_path(None, &[], &[]).path, None);
        // Nothing of the person's own workspace is recorded.
        let inside = record_path(Some(a.as_os_str()), &[], std::slice::from_ref(&root));
        assert_eq!(inside.path, None);
        assert_eq!(inside.dropped[0].why, "inside the workspace");
    }

    #[test]
    fn qualifies_refuses_temp_and_open_dirs() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        assert_eq!(qualifies(&root, &[]), Ok(()));
        assert_eq!(
            qualifies(&root, std::slice::from_ref(&root)),
            Err("a temp directory")
        );
        assert_eq!(
            qualifies(Path::new("/tmp"), &fixed_temp_roots()),
            Err("a temp directory")
        );
        fs::set_permissions(&root, fs::Permissions::from_mode(0o775)).unwrap();
        assert_eq!(qualifies(&root, &[]), Err("writable by others"));
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(qualifies(Path::new("rel"), &[]), Err("not absolute"));
        assert!(check_path(&format!("/tmp/x:{}", root.display()), &fixed_temp_roots()).is_err());
        assert!(check_path(&root.display().to_string(), &fixed_temp_roots()).is_ok());
        assert!(check_path("", &[]).is_err());
    }

    #[test]
    fn check_home_requires_a_private_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(check_home(&root), Ok(()));
        assert!(check_home(Path::new("relative")).is_err());
        assert!(check_home(&root.join("missing")).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(check_home(&root).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    }
}
