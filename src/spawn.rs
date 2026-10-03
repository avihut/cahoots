//! The ONLY module that starts processes (hard rule 3; `scripts/guard.sh`
//! holds it). Everything here takes an argv array — there is no shell, no
//! string to split, nothing to quote. It owns the callee's scrubbed
//! environment, binary resolution, process groups and signals.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::{AccessFlags, Pid, access};

use crate::config::Billing;
use crate::env;
use crate::exit::{Exit, Fail, Res};
use crate::model::HarnessId;
use crate::tools::{self, Tool};

/// Holds `binary`, a path a person pinned, to the binary policy: canonical
/// and absolute, not inside the workspace, and not writable by anyone but
/// its owner. A harness binary an agent could have just written is not a
/// harness. Nothing is ever looked up on PATH here: the caller sets PATH.
pub fn resolve_binary(binary: &Path, workspace: &[&Path]) -> Res<PathBuf> {
    let unavailable = |why: String| Fail::new(Exit::TargetUnavailable, why);
    let canonical = fs::canonicalize(binary)
        .map_err(|error| unavailable(format!("{}: {error}", binary.display())))?;
    if !is_executable_file(&canonical) {
        return Err(unavailable(format!(
            "{} is not an executable file",
            canonical.display()
        )));
    }
    if let Some(root) = workspace
        .iter()
        .find(|root| canonical.starts_with(root) || canonical.starts_with(canonical_of(root)))
    {
        return Err(Fail::policy(format!(
            "{} is inside the workspace {} — refusing to run a binary the workspace supplies",
            canonical.display(),
            root.display()
        )));
    }
    let mode = |path: &Path| fs::metadata(path).map(|meta| meta.permissions().mode());
    if mode(&canonical).is_ok_and(|mode| mode & 0o022 != 0) {
        return Err(Fail::policy(format!(
            "{} is writable by its group or by everyone — refusing to run it",
            canonical.display()
        )));
    }
    if let Some(parent) = canonical.parent()
        && mode(parent).is_ok_and(|mode| mode & 0o002 != 0 && mode & 0o1000 == 0)
    {
        return Err(Fail::policy(format!(
            "{} is world-writable — refusing to run a binary from it",
            parent.display()
        )));
    }
    Ok(canonical)
}

/// The first executable `name` in the directories of `path` (a PATH value),
/// as found — not resolved: a symlink a package manager keeps pointing at
/// the current version stays a symlink. Relative entries are skipped. Only a
/// person's verb looks anything up on a PATH, to pin it (`scripts/guard.sh`
/// holds which).
pub fn find_on_path(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

/// Every executable `name` in the directories of `path`, in PATH's order and
/// as found, once each: a second directory that leads to the same file adds
/// nothing.
pub fn find_all_on_path(name: &str, path: Option<&OsStr>) -> Vec<PathBuf> {
    let Some(path) = path else {
        return Vec::new();
    };
    let mut seen = Vec::new();
    let mut found = Vec::new();
    for candidate in std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .filter(|candidate| is_executable_file(candidate))
    {
        let canonical = fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
        if !seen.contains(&canonical) {
            seen.push(canonical);
            found.push(candidate);
        }
    }
    found
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn canonical_of(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Every process started here — each git, the git `daft` starts, the
/// callee's own — is told that git must never fetch a missing object on
/// demand. In a partial clone that
/// fetch follows the repository's remote configuration, which can name a
/// remote helper — a command, run outside every sandbox. A step that needs
/// an object that is not there fails instead, as it would for one that
/// cannot be read. Only a git that honours the variable on every path to a
/// fetch is run at all (`honours_no_lazy_fetch`).
const NO_LAZY_FETCH: (&str, &str) = ("GIT_NO_LAZY_FETCH", "1");

/// The only place a process started here gets its environment: cleared,
/// then `vars`, then `NO_LAZY_FETCH` — last, so that nothing in `vars`, an
/// empty list included, can drop or relax it (`scripts/guard.sh` holds it).
fn scrubbed(command: &mut Command, vars: Vec<(OsString, OsString)>) {
    command.env_clear();
    command.envs(vars);
    command.env(NO_LAZY_FETCH.0, NO_LAZY_FETCH.1);
}

/// What a finished helper command produced.
pub struct Output {
    pub status: Option<i32>,
    /// Its stdout as text, or empty when it is not UTF-8.
    pub stdout: String,
    /// Its stdout as it came, byte for byte.
    pub bytes: Vec<u8>,
}

/// Runs a short-lived helper (`--version`, `git`, `ps`, a usage meter) with
/// a deadline and a minimal environment, on the PATH given — one cahoots
/// built (`own_path`), never this process's, which the caller sets — and
/// returns its stdout.
pub fn run_helper_with_path<S: AsRef<OsStr>>(
    binary: &Path,
    args: &[S],
    cwd: Option<&Path>,
    deadline: Duration,
    path: Option<OsString>,
) -> Res<Output> {
    run_helper_with_env(binary, args, cwd, deadline, path, &[])
}

/// The general form: the PATH given, and `vars` set on top of the minimal
/// environment — for a tool whose own children must be told something too,
/// as the `git` that `daft` starts is told where its hooks are.
pub fn run_helper_with_env<S: AsRef<OsStr>>(
    binary: &Path,
    args: &[S],
    cwd: Option<&Path>,
    deadline: Duration,
    path: Option<OsString>,
    vars: &[(OsString, OsString)],
) -> Res<Output> {
    run_helper_inner(binary, args, cwd, deadline, path, vars, None).map(|(output, _)| output)
}

/// [`run_helper_with_env`], keeping no more than `cap` bytes of stdout: past
/// them the helper is killed, and the `bool` (overflowed) is true. Its
/// output is then not all of it, and the caller must not use it as if it
/// were.
pub fn run_helper_capped<S: AsRef<OsStr>>(
    binary: &Path,
    args: &[S],
    cwd: Option<&Path>,
    deadline: Duration,
    path: Option<OsString>,
    vars: &[(OsString, OsString)],
    cap: usize,
) -> Res<(Output, bool)> {
    run_helper_inner(binary, args, cwd, deadline, path, vars, Some(cap))
}

/// A helper's command: the minimal environment — the PATH given, HOME from
/// passwd, `vars`, then `NO_LAZY_FETCH` (`scrubbed`) — stdin from nothing,
/// stdout piped, stderr dropped. Every helper is started from it, the
/// grouped one included.
fn helper_command<S: AsRef<OsStr>>(
    binary: &Path,
    args: &[S],
    cwd: Option<&Path>,
    path: Option<OsString>,
    vars: &[(OsString, OsString)],
) -> Command {
    let mut env = Vec::new();
    if let Some(path) = path {
        env.push(("PATH".into(), path));
    }
    if let Ok(home) = crate::dirs::passwd_home() {
        env.push(("HOME".into(), home.into_os_string()));
    }
    env.extend(vars.iter().cloned());
    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    scrubbed(&mut command, env);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command
}

fn run_helper_inner<S: AsRef<OsStr>>(
    binary: &Path,
    args: &[S],
    cwd: Option<&Path>,
    deadline: Duration,
    path: Option<OsString>,
    vars: &[(OsString, OsString)],
    cap: Option<usize>,
) -> Res<(Output, bool)> {
    let mut child = helper_command(binary, args, cwd, path, vars)
        .spawn()
        .map_err(|error| Fail::internal(format!("cannot start {}: {error}", binary.display())))?;
    let overflowed = Arc::new(AtomicBool::new(false));
    // Drained on its own thread: a helper that fills the pipe would otherwise
    // block forever while this side waits for it to exit. With a cap, it
    // stops one byte past it and lets the pipe go, and says so.
    let reader = child.stdout.take().map(|pipe| {
        let overflowed = overflowed.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let limit = cap.map_or(u64::MAX, |cap| (cap as u64).saturating_add(1));
            let mut bytes = Vec::new();
            let _ = pipe.take(limit).read_to_end(&mut bytes);
            if cap.is_some_and(|cap| bytes.len() > cap) {
                overflowed.store(true, Ordering::Relaxed);
            }
            bytes
        })
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if overflowed.load(Ordering::Relaxed) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) if started.elapsed() < deadline => {
                std::thread::sleep(Duration::from_millis(15))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Fail::internal(format!(
                    "{} did not finish within {}s",
                    binary.display(),
                    deadline.as_secs()
                )));
            }
            Err(error) => return Err(Fail::internal(format!("waiting for a helper: {error}"))),
        }
    };
    // Something the helper started can hold its stdout open after it has
    // exited. A capped call keeps its deadline while the pipe drains, and a
    // pipe that stays open past it is the helper not finishing; the reader
    // is left to end when the pipe does.
    if cap.is_some() {
        while reader.as_ref().is_some_and(|thread| !thread.is_finished()) {
            if started.elapsed() >= deadline {
                return Err(Fail::internal(format!(
                    "{} did not finish within {}s: its stdout stayed open",
                    binary.display(),
                    deadline.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }
    let mut bytes = reader
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();
    let overflowed = overflowed.load(Ordering::Relaxed);
    if let Some(cap) = cap {
        bytes.truncate(cap);
    }
    Ok((
        Output {
            status: status.and_then(|status| status.code()),
            stdout: String::from_utf8(bytes.clone()).unwrap_or_default(),
            bytes,
        },
        overflowed,
    ))
}

/// How long a grouped helper's stdout may stay open once its group is gone.
const GROUP_DRAIN: Duration = Duration::from_secs(5);

/// What a grouped helper came to, once it exited.
pub enum Grouped {
    /// It exited, and what it printed was read to the end.
    Exited(Output),
    /// It exited, and something it started still held its stdout open after
    /// its group was killed: a process that left the group.
    OutputHeld,
}

/// [`run_helper_with_env`] for a tool that starts others — the tool that
/// cuts a worktree, and the `git` or the hooks it runs. It runs in a process
/// group of its own, and the whole group is killed once it has exited or its
/// deadline has passed, so nothing it started keeps running after it. Its
/// stdout is then read for a few seconds at most: past them, what holds it
/// open left the group, and that is said (`Grouped::OutputHeld`), never
/// waited on.
pub fn run_helper_grouped<S: AsRef<OsStr>>(
    binary: &Path,
    args: &[S],
    cwd: Option<&Path>,
    deadline: Duration,
    path: Option<OsString>,
    vars: &[(OsString, OsString)],
) -> Res<Grouped> {
    let mut command = helper_command(binary, args, cwd, path, vars);
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| Fail::internal(format!("cannot start {}: {error}", binary.display())))?;
    let group = child.id() as i32;
    // Read on a thread of its own, and handed over on a channel, so that the
    // wait for it has a deadline: a pipe something outside the group holds
    // never closes.
    let (sender, read) = std::sync::mpsc::channel();
    match child.stdout.take() {
        Some(mut pipe) => {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes);
                let _ = sender.send(bytes);
            });
        }
        // Nothing to read: nothing to wait for.
        None => drop(sender),
    }
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < deadline => {
                std::thread::sleep(Duration::from_millis(15))
            }
            Ok(None) => {
                signal_group(group, Signal::SIGKILL);
                let _ = child.kill();
                let _ = child.wait();
                return Err(Fail::internal(format!(
                    "{} did not finish within {}s",
                    binary.display(),
                    deadline.as_secs()
                )));
            }
            Err(error) => {
                signal_group(group, Signal::SIGKILL);
                return Err(Fail::internal(format!("waiting for a helper: {error}")));
            }
        }
    };
    // What it started goes with it, before its output is waited for: a
    // process left in the group would otherwise hold the pipe open.
    signal_group(group, Signal::SIGKILL);
    let bytes = match read.recv_timeout(GROUP_DRAIN) {
        Ok(bytes) => bytes,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Vec::new(),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Ok(Grouped::OutputHeld),
    };
    Ok(Grouped::Exited(Output {
        status: status.code(),
        stdout: String::from_utf8(bytes.clone()).unwrap_or_default(),
        bytes,
    }))
}

/// The gits that `NO_LAZY_FETCH` holds on every path to a fetch: from
/// 2.46.0, and the May 2024 security releases of the lines before it, which
/// check it where every lazy fetch starts (promisor-remote.c,
/// `fetch_objects`). 2.45.0 knows the variable but its checkout and diff
/// prefetch go around it. The minimum patch release, by minor version.
const NO_LAZY_FETCH_FIXED: [(u32, u32); 7] = [
    (39, 4),
    (40, 2),
    (41, 1),
    (42, 2),
    (43, 4),
    (44, 1),
    (45, 1),
];

/// Whether the git that printed `version` (`git --version`) never fetches
/// lazily once told so. A version this cannot read is not one. Part of
/// git's fingerprint (`tools::locate_one`): a git below it is a setup
/// problem (34), never run for anything but `--version`.
pub(crate) fn honours_no_lazy_fetch(version: &str) -> bool {
    let Some(number) = version.trim().strip_prefix("git version ") else {
        return false;
    };
    let mut parts = number
        .split(|c: char| !c.is_ascii_digit())
        .map(str::parse::<u32>);
    let (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch))) =
        (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    match (major, minor) {
        (3.., _) | (2, 46..) => true,
        (2, minor) => NO_LAZY_FETCH_FIXED
            .iter()
            .any(|&(line, first)| line == minor && patch >= first),
        _ => false,
    }
}

/// A system tool cahoots itself needs (`git`, `ps`, `daft`), from the one
/// path a person pinned for it, never a PATH lookup — the caller sets PATH,
/// and could put a tool of its own first — held to the same binary policy
/// as a harness. One that is missing is a setup problem (34); one the policy
/// refuses is a refusal (33), and is never run.
pub fn pinned_system_tool(name: &str, pinned: &Path, workspace: &[&Path]) -> Res<PathBuf> {
    as_system_tool(name, resolve_binary(pinned, workspace))
}

fn as_system_tool(name: &str, resolved: Res<PathBuf>) -> Res<PathBuf> {
    resolved.map_err(|fail| match fail.exit {
        Exit::Policy => Fail::policy(format!("refusing to run `{name}`: {}", fail.message)),
        _ => Fail::new(
            Exit::Config,
            format!("cahoots needs `{name}`: {}", fail.message),
        ),
    })
}

/// The PATH a program cahoots starts runs with — every one, the callee
/// included — built here and never taken from the caller: the directories of
/// `binaries`, in the order given (each as pinned, then where it resolves),
/// then the system's (`tools::SYSTEM_DIRS`), then the PATH a person recorded
/// (`tools.path`), each directory once. A pinned program's own directory is
/// left off when everyone can write it or it is a temp directory
/// (`tools::holds_programs`): the binary policy lets a program run from a
/// sticky one, but what others put beside it is no one's choice. A recorded
/// one is held to `tools::qualifies`, without the caller's `TMPDIR`. Nothing
/// inside `workspace` is on it, nor a relative entry, nor one a PATH cannot
/// hold (a `:`). A recorded directory comes last, so it adds tools and never
/// shadows a pinned or a system one.
pub fn own_path(
    binaries: &[&Path],
    recorded: Option<&OsStr>,
    workspace: &[&Path],
) -> Option<OsString> {
    let temp = tools::fixed_temp_roots();
    let holds = |dir: &PathBuf| tools::holds_programs(dir, &temp).is_ok();
    let mut dirs: Vec<PathBuf> = Vec::new();
    for binary in binaries {
        dirs.extend(binary.parent().map(Path::to_path_buf).filter(holds));
        dirs.extend(
            fs::canonicalize(binary)
                .ok()
                .and_then(|canonical| canonical.parent().map(Path::to_path_buf))
                .filter(holds),
        );
    }
    dirs.extend(tools::SYSTEM_DIRS.iter().map(PathBuf::from));
    if let Some(recorded) = recorded {
        dirs.extend(
            std::env::split_paths(recorded).filter(|dir| tools::qualifies(dir, &temp).is_ok()),
        );
    }
    let mut kept: Vec<PathBuf> = Vec::new();
    for dir in drop_workspace(dirs, workspace) {
        if !kept.contains(&dir) && !dir.as_os_str().as_bytes().contains(&b':') {
            kept.push(dir);
        }
    }
    // Nothing left is no PATH at all, never an empty one: an empty entry
    // means the working directory to a shell's lookup.
    std::env::join_paths(kept)
        .ok()
        .filter(|joined| !joined.is_empty())
}

/// `dirs` without the relative ones and without any inside `workspace` —
/// by name or by where it resolves.
fn drop_workspace(dirs: Vec<PathBuf>, workspace: &[&Path]) -> Vec<PathBuf> {
    let roots: Vec<PathBuf> = workspace
        .iter()
        .flat_map(|root| [root.to_path_buf(), canonical_of(root)])
        .collect();
    dirs.into_iter()
        .filter(|dir| dir.is_absolute())
        .filter(|dir| {
            let resolved = canonical_of(dir);
            !roots
                .iter()
                .any(|root| dir.starts_with(root) || resolved.starts_with(root))
        })
        .collect()
}

/// What is known of the workspace around `dir` before any git has run:
/// `dir` itself, canonical, and every directory above it that holds a
/// `.git`, up to the top git would find (`repository_tops`). The tools are
/// located against these: none of them is run from there, not even to ask
/// its version.
pub fn around(dir: &Path) -> Vec<PathBuf> {
    let here = canonical_of(dir);
    let mut roots = vec![here.clone()];
    roots.extend(repository_tops(&here));
    roots
}

/// `git rev-parse` in `dir`: the toplevel and the common dir (which is what
/// two worktrees of one repository share), both canonical. `Ok(None)` outside
/// a repository. The located `git` inside `workspace`, `dir` or the
/// repository around it is refused, and is never run.
pub fn git_roots(git: &Tool, dir: &Path, workspace: &[&Path]) -> Res<Option<(PathBuf, PathBuf)>> {
    let here = canonical_of(dir);
    // The repository's top as its `.git` shows it, before any git is run: a
    // `git` pinned at the top of the repository, above `dir`, is refused
    // here, not after it has answered.
    let tops = repository_tops(&here);
    let mut roots = workspace.to_vec();
    roots.push(&here);
    roots.extend(tops.iter().map(PathBuf::as_path));
    let binary = git.at(&roots)?;
    let path = git.path(&roots);
    let ask = |what: &str| {
        let output = run_helper_with_env(
            &binary,
            &["rev-parse", "--path-format=absolute", what],
            Some(dir),
            Duration::from_secs(10),
            path.clone(),
            &[],
        )
        .ok()?;
        (output.status == Some(0)).then(|| canonical_of(Path::new(output.stdout.trim())))
    };
    let Some(toplevel) = ask("--show-toplevel") else {
        return Ok(None);
    };
    if binary.starts_with(&toplevel) {
        return Err(Fail::policy(format!(
            "refusing to run `git`: {} is inside the workspace {} — refusing to run a binary the \
             workspace supplies",
            binary.display(),
            toplevel.display()
        )));
    }
    Ok(ask("--git-common-dir").map(|common| (toplevel, common)))
}

/// Every directory from `dir` up that holds a `.git`, up to and including the
/// first whose `.git` git takes for a repository — the top git finds. A
/// `.git` git does not take (an empty directory, a file that names nothing)
/// does not end the walk, since git looks past it, and it must not hide the
/// repository around it. This takes a `.git` only where git surely does
/// (`is_repository_marker`), so it never stops below the top git finds; where
/// it is unsure, it walks on, and refuses more.
fn repository_tops(dir: &Path) -> Vec<PathBuf> {
    let mut tops = Vec::new();
    for ancestor in dir.ancestors() {
        let marker = ancestor.join(".git");
        if marker.symlink_metadata().is_err() {
            continue;
        }
        tops.push(ancestor.to_path_buf());
        if is_repository_marker(&marker, ancestor) {
            break;
        }
    }
    tops
}

/// Whether git takes `marker`, the `.git` in `top`, for a repository: a git
/// directory, or a file `gitdir: <path>` that names one, all of it the
/// user's. Mirrors git (setup.c: `read_gitfile_gently`, `is_git_directory`,
/// `validate_headref`, `ensure_valid_ownership`) within git's own limits,
/// and is never more lenient: whatever this takes, git takes too. Whatever
/// it cannot be sure of, it does not take, and the walk goes on up.
fn is_repository_marker(marker: &Path, top: &Path) -> bool {
    let Ok(meta) = fs::metadata(marker) else {
        return false;
    };
    let gitdir = if meta.is_dir() {
        marker.to_path_buf()
    } else if meta.is_file() {
        let Some(text) = whole_small_file(marker) else {
            return false;
        };
        let Some(named) = text.strip_prefix(b"gitdir: ") else {
            return false;
        };
        let named = trim_line_ends(named);
        if named.is_empty() {
            return false;
        }
        top.join(OsStr::from_bytes(named))
    } else {
        return false;
    };
    // git uses a repository only if the user owns it (`safe.directory`,
    // which this does not read, aside).
    is_git_directory(&gitdir)
        && [top, marker, &gitdir].into_iter().all(owned)
        && fs::canonicalize(&gitdir).is_ok_and(|real| owned(&real))
}

/// `path`, canonical, if it is a git directory and this user's — the path
/// and what it resolves to — as git asks before it uses a repository. For a
/// git directory cahoots pins and later passes to git as `--git-dir`, which
/// skips git's own ownership check: this is that check.
pub fn owned_git_dir(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let canonical = fs::canonicalize(path).ok()?;
    (is_git_directory(&canonical) && owned(path) && owned(&canonical)).then_some(canonical)
}

/// The line in a small file git reads whole (a `commondir`), its line end
/// trimmed — read without blocking, so a FIFO in its place holds nothing up.
pub(crate) fn small_file_line(path: &Path) -> Option<Vec<u8>> {
    whole_small_file(path).map(|bytes| trim_line_ends(&bytes).to_vec())
}

/// git's test for a git directory: a `HEAD` it takes, and `objects` and
/// `refs` it may enter in the common directory.
fn is_git_directory(dir: &Path) -> bool {
    if !is_head(&dir.join("HEAD")) {
        return false;
    }
    let named = dir.join("commondir");
    let common = match named.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => dir.to_path_buf(),
        _ => match whole_small_file(&named) {
            Some(text) if !trim_line_ends(&text).is_empty() => {
                dir.join(OsStr::from_bytes(trim_line_ends(&text)))
            }
            _ => return false,
        },
    };
    ["objects", "refs"].iter().all(|part| {
        let path = common.join(part);
        path.is_dir() && access(&path, AccessFlags::X_OK).is_ok()
    })
}

/// The most of a HEAD git reads: `validate_headref` reads 255 bytes, and
/// what lies past them is nothing to git.
const HEAD_READ: u64 = 255;

/// git's `validate_headref`, no more lenient: a link into `refs/`; or, in
/// the 255 bytes git reads, `ref:`, git's own spaces, then `refs/`; or a
/// commit id and nothing else.
fn is_head(head: &Path) -> bool {
    let Ok(meta) = head.symlink_metadata() else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return fs::read_link(head)
            .is_ok_and(|target| target.as_os_str().as_bytes().starts_with(b"refs/"));
    }
    let Some(mut text) = file_start(head, HEAD_READ) else {
        return false;
    };
    // git reads it as a C string: it ends at the first NUL.
    if let Some(end) = text.iter().position(|&byte| byte == 0) {
        text.truncate(end);
    }
    if let Some(name) = text.strip_prefix(b"ref:") {
        // git's `isspace`: space, tab, newline and carriage return — not a
        // form feed or a vertical tab, as C's has it.
        let start = name
            .iter()
            .position(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
            .unwrap_or(name.len());
        return name[start..].starts_with(b"refs/");
    }
    let id = trim_line_ends(&text);
    matches!(id.len(), 40 | 64) && id.iter().all(u8::is_ascii_hexdigit)
}

/// The most this reads of a file git reads whole (a `.git` file, a
/// `commondir`). Each is a line; one any longer is not taken.
const SMALL_FILE: u64 = 4096;

/// A small file git reads whole, all of it: `None` if it is longer than
/// `SMALL_FILE`, so that nothing past what this read could make git see it
/// otherwise, or if it holds a NUL, where git's reading of it and this one's
/// part.
fn whole_small_file(path: &Path) -> Option<Vec<u8>> {
    let bytes = file_start(path, SMALL_FILE + 1)?;
    (bytes.len() as u64 <= SMALL_FILE && !bytes.contains(&0)).then_some(bytes)
}

/// At most `limit` bytes from the start of a regular file. This runs before
/// any deadline does, so nothing planted in a file's place may hold it up:
/// it is opened without blocking, which a FIFO would otherwise do, and it
/// must be a regular file.
fn file_start(path: &Path, limit: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = File::options()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn trim_line_ends(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|byte| !matches!(byte, b'\n' | b'\r'))
        .map_or(0, |at| at + 1);
    &bytes[..end]
}

/// Whether `path` itself, not what it links to, is this user's, as git asks
/// before it uses a repository.
fn owned(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|meta| meta.uid() == nix::unistd::geteuid().as_raw())
}

/// When a process started, as the located `ps` tells it — the identity
/// check that stops reconcile from signalling a recycled pid. A `ps` the
/// policy refuses for these roots is not run, and nothing is known.
pub fn process_started(ps: &Tool, pid: i32, workspace: &[&Path]) -> Option<String> {
    let binary = ps.at(workspace).ok()?;
    let output = run_helper_with_env(
        &binary,
        &["-o", "lstart=", "-p", &pid.to_string()],
        None,
        Duration::from_secs(5),
        ps.path(workspace),
        &[],
    )
    .ok()?;
    let started = output.stdout.trim();
    (!started.is_empty()).then(|| started.to_string())
}

/// Every descendant of `root`, from one snapshot of the located `ps`. Codex
/// runs its tool commands in their own process groups, so signalling the
/// callee's group does not reach them (docs/SPIKE.md S3).
pub fn descendants(ps: &Tool, root: i32, workspace: &[&Path]) -> Vec<i32> {
    let Ok(binary) = ps.at(workspace) else {
        return Vec::new();
    };
    let Ok(output) = run_helper_with_env(
        &binary,
        &["-axo", "pid=,ppid="],
        None,
        Duration::from_secs(5),
        ps.path(workspace),
        &[],
    ) else {
        return Vec::new();
    };
    let table: Vec<(i32, i32)> = output
        .stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().map(str::parse::<i32>);
            Some((fields.next()?.ok()?, fields.next()?.ok()?))
        })
        .collect();
    let mut found = vec![root];
    let mut at = 0;
    while at < found.len() {
        let parent = found[at];
        found.extend(
            table
                .iter()
                .filter(|(_, ppid)| *ppid == parent)
                .map(|(pid, _)| *pid),
        );
        at += 1;
    }
    found.remove(0);
    found
}

pub fn signal_group(pgid: i32, signal: Signal) {
    let _ = killpg(Pid::from_raw(pgid), signal);
}

pub fn signal_processes(pids: &[i32], signal: Signal) {
    for pid in pids {
        let _ = kill(Pid::from_raw(*pid), signal);
    }
}

/// What a callee is started with.
pub struct Callee<'a> {
    pub harness: HarnessId,
    pub binary: &'a Path,
    pub argv: &'a [String],
    pub cwd: &'a Path,
    pub brief: &'a Path,
    pub billing: Billing,
    pub run_id: &'a str,
    pub depth: u32,
    pub caller: Option<HarnessId>,
    /// Its PATH (`own_path`: the pinned git's directories, the harness's,
    /// the system's, then the recorded PATH) — never the caller's.
    pub path: Option<OsString>,
    /// Its `TMPDIR`: the run's own private directory, never the caller's.
    pub tmpdir: &'a Path,
    /// Its settings folder (`harness.<id>.home`), when a person set one.
    pub home: Option<&'a Path>,
}

/// The variable that names a harness's settings folder, set to `home` when
/// a person configured one (`harness.<id>.home`), and absent otherwise — so
/// the harness uses its default under the passwd home. Never the caller's:
/// it would be the callee's whole configuration, its hooks and its MCP
/// servers among it. The callee and its `--version` get the same.
pub fn home_vars(harness: HarnessId, home: Option<&Path>) -> Vec<(OsString, OsString)> {
    let name = match harness {
        HarnessId::Claude => "CLAUDE_CONFIG_DIR",
        HarnessId::Codex => "CODEX_HOME",
    };
    home.map(|home| (OsString::from(name), home.as_os_str().to_os_string()))
        .into_iter()
        .collect()
}

/// The callee's environment: cleared, then rebuilt. From the caller, only
/// what cannot steer anything (`env::callee_passthrough`); the rest is
/// cahoots' own — PATH built from pins (`own_path`), HOME and SHELL from
/// passwd, a TMPDIR of the run's own, the settings folder config.toml names —
/// and, as every process here gets, `NO_LAZY_FETCH`: the harness runs git of
/// its own outside its tool sandbox. The caller's harness markers, tokens
/// and proxies never reach it — and a vendor API key only when billing says
/// so.
pub fn callee_environment(callee: &Callee<'_>) -> Res<Vec<(OsString, OsString)>> {
    let mut vars = env::callee_passthrough();
    if let Some(path) = &callee.path {
        vars.push(("PATH".into(), path.clone()));
    }
    vars.push(("HOME".into(), crate::dirs::passwd_home()?.into_os_string()));
    vars.push(("TMPDIR".into(), callee.tmpdir.as_os_str().to_os_string()));
    if let Some(shell) = crate::dirs::passwd_shell() {
        vars.push(("SHELL".into(), shell.into_os_string()));
    }
    vars.extend(home_vars(callee.harness, callee.home));
    vars.push(("CAHOOTS_DEPTH".into(), callee.depth.to_string().into()));
    vars.push(("CAHOOTS_RUN_ID".into(), callee.run_id.into()));
    if let Some(caller) = callee.caller {
        vars.push(("CAHOOTS_CALLER".into(), caller.as_str().into()));
    }
    if callee.billing == Billing::Api
        && let Some(key) = env::api_key(callee.harness)
    {
        vars.push(key);
    }
    Ok(vars)
}

/// Starts the harness: brief on stdin, output piped, its own process group so
/// the supervisor can signal it without signalling itself.
pub fn spawn_callee(callee: &Callee<'_>) -> Res<Child> {
    let brief = File::open(callee.brief)
        .map_err(|error| Fail::internal(format!("cannot open the brief: {error}")))?;
    let mut command = Command::new(callee.binary);
    scrubbed(&mut command, callee_environment(callee)?);
    command
        .args(callee.argv)
        .current_dir(callee.cwd)
        .stdin(Stdio::from(brief))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| {
            Fail::new(
                Exit::TargetUnavailable,
                format!("cannot start {}: {error}", callee.binary.display()),
            )
        })
}

/// Starts this same binary as the detached supervisor of `run_id`. It calls
/// `setsid` itself first thing, which takes it out of the caller's session —
/// and so out of reach of a harness that kills a timed-out tool call's
/// process group (docs/SPIKE.md S2). Its environment is inherited: it is
/// cahoots, not a callee.
pub fn spawn_supervisor(run_id: &str, log: &Path) -> Res<()> {
    let exe = std::env::current_exe()
        .map_err(|error| Fail::internal(format!("cannot find my own binary: {error}")))?;
    let log = File::options()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|error| Fail::internal(format!("cannot open the supervisor log: {error}")))?;
    let log_too = log
        .try_clone()
        .map_err(|error| Fail::internal(format!("cannot open the supervisor log: {error}")))?;
    Command::new(exe)
        .args(["__supervise", run_id])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_too))
        .spawn()
        .map(drop)
        .map_err(|error| Fail::internal(format!("cannot start the supervisor: {error}")))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn executable(dir: &Path, name: &str, mode: u32) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    /// A git directory by hand: a HEAD git takes, `objects` and `refs`.
    pub(crate) fn fake_git_dir(dir: &Path) {
        fs::create_dir_all(dir.join("objects")).unwrap();
        fs::create_dir_all(dir.join("refs")).unwrap();
        fs::write(dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    }

    #[test]
    fn an_owned_git_dir_is_a_git_directory_of_this_users_given_absolute() {
        let root = tempfile::tempdir().unwrap();
        let canonical = fs::canonicalize(root.path()).unwrap();
        let gitdir = canonical.join("repo.git");
        fake_git_dir(&gitdir);
        assert_eq!(owned_git_dir(&gitdir), Some(gitdir.clone()));
        // Through a link, it is what the link resolves to.
        std::os::unix::fs::symlink(&gitdir, canonical.join("alias")).unwrap();
        assert_eq!(owned_git_dir(&canonical.join("alias")), Some(gitdir));
        fs::create_dir(canonical.join("plain")).unwrap();
        for refused in [
            Path::new("repo.git"),
            &canonical.join("plain"),
            &canonical.join("missing"),
            Path::new("/"),
        ] {
            assert_eq!(owned_git_dir(refused), None, "{}", refused.display());
        }
    }

    #[test]
    fn a_small_file_line_is_read_whole_or_not_at_all() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("commondir");
        fs::write(&file, "../..\n").unwrap();
        assert_eq!(small_file_line(&file), Some(b"../..".to_vec()));
        fs::write(&file, vec![b'x'; SMALL_FILE as usize + 1]).unwrap();
        assert_eq!(small_file_line(&file), None);
        let fifo = root.path().join("fifo");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).unwrap();
        assert_eq!(small_file_line(&fifo), None, "a FIFO is not waited on");
    }

    #[test]
    fn a_capped_helper_is_stopped_past_its_cap() {
        // `yes` never stops by itself: only the cap ends it before the deadline.
        let yes = Path::new("/usr/bin/yes");
        let started = Instant::now();
        let (output, overflowed) =
            run_helper_capped(yes, &["x"], None, Duration::from_secs(20), None, &[], 1000).unwrap();
        assert!(overflowed);
        assert_eq!(output.bytes.len(), 1000);
        assert!(started.elapsed() < Duration::from_secs(10));

        let printf = Path::new("/usr/bin/printf");
        let (output, overflowed) = run_helper_capped(
            printf,
            &["abc"],
            None,
            Duration::from_secs(20),
            None,
            &[],
            3,
        )
        .unwrap();
        assert!(!overflowed, "exactly the cap is not past it");
        assert_eq!((output.status, output.bytes), (Some(0), b"abc".to_vec()));
    }

    #[test]
    fn a_binary_inside_the_workspace_is_refused() {
        let workspace = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(workspace.path()).unwrap();
        let binary = executable(&root, "codex", 0o755);
        let fail = resolve_binary(&binary, &[&root]).unwrap_err();
        assert_eq!(fail.exit, Exit::Policy);
        assert!(resolve_binary(&binary, &[]).is_ok());
    }

    #[test]
    fn a_binary_others_can_write_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let binary = executable(dir.path(), "codex", 0o775);
        assert_eq!(resolve_binary(&binary, &[]).unwrap_err().exit, Exit::Policy);
    }

    #[test]
    fn a_missing_binary_is_target_unavailable() {
        let fail = resolve_binary(Path::new("/nonexistent/codex"), &[]).unwrap_err();
        assert_eq!(fail.exit, Exit::TargetUnavailable);
    }

    #[test]
    fn a_symlink_is_judged_by_where_it_points() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(workspace.path()).unwrap();
        let real = executable(&root, "evil", 0o755);
        let link = outside.path().join("codex");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            resolve_binary(&link, &[&root]).unwrap_err().exit,
            Exit::Policy
        );
    }

    #[test]
    fn a_policy_refusal_of_a_system_tool_is_policy() {
        let workspace = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(workspace.path()).unwrap();
        let planted = executable(&root, "git", 0o755);
        let fail = as_system_tool("git", resolve_binary(&planted, &[&root])).unwrap_err();
        assert_eq!(fail.exit, Exit::Policy);
        assert!(
            fail.message.contains("refusing to run `git`"),
            "{}",
            fail.message
        );

        let open = executable(&root, "daft", 0o777);
        let fail = as_system_tool("daft", resolve_binary(&open, &[])).unwrap_err();
        assert_eq!(fail.exit, Exit::Policy);

        // Missing is not a refusal: it is something to install.
        let missing = Path::new("/nonexistent/git");
        let fail = as_system_tool("git", resolve_binary(missing, &[])).unwrap_err();
        assert_eq!(fail.exit, Exit::Config);
        assert!(
            fail.message.contains("cahoots needs `git`"),
            "{}",
            fail.message
        );
    }

    #[test]
    fn own_path_puts_each_binary_s_dirs_first_then_the_system_s() {
        let workspace = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(workspace.path()).unwrap();
        // Not under a temp directory: a recorded directory there never counts.
        let outside = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let out = fs::canonicalize(outside.path()).unwrap();
        let (git_dir, harness_dir, real_dir) = (out.join("g"), out.join("h"), out.join("real"));
        for dir in [&git_dir, &harness_dir, &real_dir] {
            fs::create_dir(dir).unwrap();
        }
        let git = executable(&git_dir, "git", 0o755);
        // A harness pinned through a link: the link's directory, then where
        // it resolves.
        let real = executable(&real_dir, "claude", 0o755);
        let harness = harness_dir.join("claude");
        std::os::unix::fs::symlink(&real, &harness).unwrap();
        let path = own_path(&[&git, &harness, &git], None, &[&root]).unwrap();
        let expected = std::env::join_paths([
            git_dir.clone(),
            harness_dir.clone(),
            real_dir.clone(),
            "/usr/bin".into(),
            "/bin".into(),
            "/usr/sbin".into(),
            "/sbin".into(),
        ])
        .unwrap();
        assert_eq!(path, expected);

        // A binary inside the workspace adds no directory; a relative one
        // none either; nor does one in a temp directory, or in one others
        // can write: what else is there is no one's choice.
        let inside = executable(&root, "git", 0o755);
        let open_dir = out.join("shared");
        fs::create_dir(&open_dir).unwrap();
        let shared = executable(&open_dir, "codex", 0o755);
        fs::set_permissions(&open_dir, fs::Permissions::from_mode(0o1777)).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let in_temp = executable(&fs::canonicalize(temp.path()).unwrap(), "daft", 0o755);
        let path = own_path(
            &[&inside, Path::new("relative/git"), &shared, &in_temp],
            None,
            &[&root],
        )
        .unwrap();
        assert_eq!(path, OsString::from("/usr/bin:/bin:/usr/sbin:/sbin"));
        fs::set_permissions(&open_dir, fs::Permissions::from_mode(0o755)).unwrap();

        // The recorded PATH comes last, without what no longer qualifies or
        // lies inside the workspace, and never shadows what came first.
        let mine = out.join("mine");
        fs::create_dir(&mine).unwrap();
        let open = out.join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
        let recorded = std::env::join_paths([
            mine.clone(),
            out.join("gone"),
            open.clone(),
            root.clone(),
            git_dir.clone(),
            "/tmp".into(),
        ])
        .unwrap();
        let path = own_path(&[&git], Some(&recorded), &[&root]).unwrap();
        let expected = std::env::join_paths([
            git_dir,
            "/usr/bin".into(),
            "/bin".into(),
            "/usr/sbin".into(),
            "/sbin".into(),
            mine,
        ])
        .unwrap();
        assert_eq!(path, expected);
    }

    #[test]
    fn a_dot_git_ends_the_walk_only_where_git_takes_it() {
        let root = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(root.path()).unwrap();
        // A repository at the top, by hand: HEAD, objects, refs.
        let gitdir = root.join(".git");
        for part in ["objects", "refs"] {
            fs::create_dir_all(gitdir.join(part)).unwrap();
        }
        fs::write(gitdir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let below = root.join("a/b");
        fs::create_dir_all(&below).unwrap();
        assert_eq!(repository_tops(&below), vec![root.clone()]);

        // What git looks past is no end of the walk, however it is dressed.
        // Nor is what git takes but this walk is unsure of — an id with more
        // after it: walking on refuses more, which is the safe side.
        let fake = root.join("a/.git");
        for (what, head, objects) in [
            ("an empty directory", None, false),
            (
                "a HEAD and no objects",
                Some("ref: refs/heads/main\n"),
                false,
            ),
            (
                "a HEAD git cannot read",
                Some("ref:\u{a0}refs/heads/main\n"),
                true,
            ),
            (
                "an id with more after it",
                Some(&*format!("{} x\n", "0".repeat(40))),
                true,
            ),
            // git reads 255 bytes of a HEAD: a ref that begins past them is
            // no ref to git, however much more this could read.
            (
                "a ref that begins past the 255 bytes git reads",
                Some(&*format!("ref:{}refs/heads/main\n", " ".repeat(251))),
                true,
            ),
            // git's own spaces are space, tab, newline and carriage return.
            (
                "a ref after a form feed",
                Some("ref:\u{c}refs/heads/main\n"),
                true,
            ),
        ] {
            let _ = fs::remove_dir_all(&fake);
            fs::create_dir_all(fake.join("refs")).unwrap();
            if objects {
                fs::create_dir_all(fake.join("objects")).unwrap();
            }
            if let Some(head) = head {
                fs::write(fake.join("HEAD"), head).unwrap();
            }
            assert_eq!(
                repository_tops(&below),
                vec![root.join("a"), root.clone()],
                "{what}"
            );
        }
        // A FIFO where HEAD should be: neither waited on nor taken.
        let _ = fs::remove_dir_all(&fake);
        fs::create_dir_all(fake.join("objects")).unwrap();
        fs::create_dir_all(fake.join("refs")).unwrap();
        nix::unistd::mkfifo(&fake.join("HEAD"), nix::sys::stat::Mode::S_IRWXU).unwrap();
        assert_eq!(
            repository_tops(&below),
            vec![root.join("a"), root.clone()],
            "a FIFO for a HEAD"
        );
        let _ = fs::remove_dir_all(&fake);
        // git reads a `.git` file whole: one whose end lies past what this
        // reads may end in something git does not take.
        let past = format!("gitdir: {}\n{}junk\n", gitdir.display(), "\n".repeat(5000));
        for named in [
            "gitdir: nowhere\n",
            "gitdir: \n",
            "not a gitdir line\n",
            &past,
        ] {
            fs::write(&fake, named).unwrap();
            assert_eq!(
                repository_tops(&below),
                vec![root.join("a"), root.clone()],
                "{named:?}"
            );
        }

        // What git takes ends it: a gitfile naming a git directory, and a
        // directory that is one.
        fs::write(&fake, format!("gitdir: {}\n", gitdir.display())).unwrap();
        assert_eq!(repository_tops(&below), vec![root.join("a")]);
        fs::remove_file(&fake).unwrap();
        fs::create_dir_all(fake.join("objects")).unwrap();
        fs::create_dir_all(fake.join("refs")).unwrap();
        fs::write(fake.join("HEAD"), format!("{}\n", "a".repeat(40))).unwrap();
        assert_eq!(repository_tops(&below), vec![root.join("a")]);
    }

    /// Every `GIT_NO_LAZY_FETCH=` line in what `env` printed.
    fn lazy_fetch_lines(output: &Output) -> Vec<String> {
        output
            .stdout
            .lines()
            .filter(|line| line.starts_with("GIT_NO_LAZY_FETCH="))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn every_helper_environment_forbids_lazy_fetch() {
        let env = Path::new("/usr/bin/env");
        let once = vec!["GIT_NO_LAZY_FETCH=1".to_string()];
        let deadline = Duration::from_secs(10);
        let plain = run_helper_with_path(env, &[] as &[&str], None, deadline, None).unwrap();
        assert_eq!(lazy_fetch_lines(&plain), once, "run_helper_with_path");
        let empty = run_helper_with_env(env, &[] as &[&str], None, deadline, None, &[]).unwrap();
        assert_eq!(
            lazy_fetch_lines(&empty),
            once,
            "an explicit, empty environment"
        );
        // What a caller passes comes first: it can neither relax nor drop it.
        let relaxed = [
            ("GIT_NO_LAZY_FETCH".into(), "0".into()),
            ("GIT_NO_LAZY_FETCH".into(), "".into()),
        ];
        let output =
            run_helper_with_env(env, &[] as &[&str], None, deadline, None, &relaxed).unwrap();
        assert_eq!(lazy_fetch_lines(&output), once, "a caller's own value");
        let (capped, overflowed) =
            run_helper_capped(env, &[] as &[&str], None, deadline, None, &relaxed, 1 << 20)
                .unwrap();
        assert!(!overflowed);
        assert_eq!(lazy_fetch_lines(&capped), once, "run_helper_capped");
        // The grouped helper: the tool that cuts a worktree, and every git
        // it starts.
        let Grouped::Exited(grouped) =
            run_helper_grouped(env, &[] as &[&str], None, deadline, None, &relaxed).unwrap()
        else {
            panic!("env's output was held");
        };
        assert_eq!(lazy_fetch_lines(&grouped), once, "run_helper_grouped");
    }

    #[test]
    fn the_git_floor_is_where_every_lazy_fetch_honours_the_variable() {
        for (version, honours) in [
            // Knows the variable, but its checkout and diff prefetch do not.
            ("git version 2.45.0", false),
            ("git version 2.45.1", true),
            ("git version 2.46.0", true),
            ("git version 2.56.0\n", true),
            ("git version 3.0.0", true),
            ("git version 2.39.3", false),
            ("git version 2.39.4", true),
            ("git version 2.39.5 (Apple Git-154)", true),
            ("git version 2.40.1", false),
            ("git version 2.40.2", true),
            ("git version 2.41.1", true),
            ("git version 2.42.1", false),
            ("git version 2.43.3", false),
            ("git version 2.43.4", true),
            ("git version 2.44.0", false),
            ("git version 2.44.1", true),
            ("git version 2.45.0.windows.1", false),
            ("git version 2.38.9", false),
            ("git version 1.99.99", false),
            ("git version 2.46", false),
            ("2.46.0", false),
            ("", false),
            ("git version two", false),
        ] {
            assert_eq!(honours_no_lazy_fetch(version), honours, "{version:?}");
        }
    }

    /// Waits until `pid` is gone: killed, and reaped by whoever inherited it.
    fn gone(pid: i32) -> bool {
        let started = Instant::now();
        while kill(Pid::from_raw(pid), None).is_ok() {
            if started.elapsed() > Duration::from_secs(10) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        true
    }

    #[test]
    fn a_grouped_helper_kills_its_group_once_it_exits_and_at_its_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        // Read by `sh`, not run itself: a file just written may not be run
        // while a fork elsewhere in the suite still holds it open.
        let sh = Path::new("/bin/sh");
        let script = |name: &str, text: String| {
            let path = root.join(name);
            fs::write(&path, text).unwrap();
            path
        };
        let pid = |name: &str| -> i32 {
            fs::read_to_string(root.join(name))
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        };
        let path = || Some(OsString::from("/usr/bin:/bin"));

        // A process left in the group, holding stdout: it goes with the
        // group, and what was printed is read at once.
        let left = script(
            "left.sh",
            format!(
                "sleep 30 &\necho $! > '{}'\necho done\n",
                root.join("left.pid").display()
            ),
        );
        let started = Instant::now();
        let Grouped::Exited(output) =
            run_helper_grouped(sh, &[&left], None, Duration::from_secs(20), path(), &[]).unwrap()
        else {
            panic!("the output was held");
        };
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!((output.status, output.stdout.as_str()), (Some(0), "done\n"));
        assert!(gone(pid("left.pid")), "the process left in the group lives");

        // One that left the group, still holding the pipe, is said and not
        // waited on past the drain: `tests/providers.rs` shows that end to
        // end, with a process that leaves the group the portable way.

        // Past the deadline, the whole group goes: the helper and what it started.
        let late = script(
            "late.sh",
            format!(
                "sleep 30 &\necho $! > '{}'\nsleep 30\n",
                root.join("late.pid").display()
            ),
        );
        let fail = run_helper_grouped(sh, &[&late], None, Duration::from_secs(1), path(), &[])
            .err()
            .expect("past its deadline");
        assert!(
            fail.message.ends_with("did not finish within 1s"),
            "{}",
            fail.message
        );
        assert!(gone(pid("late.pid")), "what the helper started lives");
    }

    #[test]
    fn a_pinned_system_tool_is_that_path_or_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let daft = executable(&root, "daft", 0o755);
        assert_eq!(pinned_system_tool("daft", &daft, &[]).unwrap(), daft);
        let fail = pinned_system_tool("daft", &root.join("nowhere"), &[]).unwrap_err();
        assert_eq!(fail.exit, Exit::Config);
        assert!(
            fail.message.starts_with("cahoots needs `daft`: "),
            "{}",
            fail.message
        );
        let fail = pinned_system_tool("daft", &daft, &[&root]).unwrap_err();
        assert_eq!(fail.exit, Exit::Policy);
        assert!(
            fail.message.starts_with("refusing to run `daft`: "),
            "{}",
            fail.message
        );
    }

    #[test]
    fn helpers_run_with_a_deadline() {
        let sleep = resolve_binary(Path::new("/bin/sleep"), &[]).unwrap();
        let path = || own_path(&[&sleep], None, &[]);
        assert!(
            run_helper_with_path(&sleep, &["5"], None, Duration::from_millis(100), path()).is_err()
        );
        let output =
            run_helper_with_path(&sleep, &["0"], None, Duration::from_secs(5), path()).unwrap();
        assert_eq!(output.status, Some(0));
    }
}
