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
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::{AccessFlags, Pid, access};

use crate::config::Billing;
use crate::env;
use crate::exit::{Exit, Fail, Res};
use crate::model::HarnessId;

/// Finds `name` on PATH, or takes `configured`, and holds the result to the
/// binary policy: canonical and absolute, not inside the workspace, and not
/// writable by anyone but its owner. A harness binary an agent could have
/// just written is not a harness.
pub fn resolve_binary(name: &str, configured: Option<&Path>, workspace: &[&Path]) -> Res<PathBuf> {
    let unavailable = |why: String| Fail::new(Exit::TargetUnavailable, why);
    let found = match configured {
        Some(path) => path.to_path_buf(),
        None => find_on_path(name, env::path_var().as_deref())
            .ok_or_else(|| unavailable(format!("`{name}` is not on PATH")))?,
    };
    let canonical = fs::canonicalize(&found)
        .map_err(|error| unavailable(format!("{}: {error}", found.display())))?;
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
/// the current version stays a symlink. Relative entries are skipped.
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

/// What a finished helper command produced.
pub struct Output {
    pub status: Option<i32>,
    pub stdout: String,
}

/// Runs a short-lived helper (`--version`, `git`, `ps`) with a deadline and a
/// minimal environment, and returns its stdout.
pub fn run_helper<S: AsRef<OsStr>>(
    binary: &Path,
    args: &[S],
    cwd: Option<&Path>,
    deadline: Duration,
) -> Res<Output> {
    run_helper_with_env(binary, args, cwd, deadline, env::path_var(), &[])
}

/// [`run_helper`], with the PATH given instead of this process's — for a
/// usage meter, whose answer the calling agent must not be able to steer
/// through the interpreter or the tools it finds.
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
    let mut command = Command::new(binary);
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    if let Ok(home) = crate::dirs::passwd_home() {
        command.env("HOME", home);
    }
    command.envs(vars.iter().map(|(name, value)| (name, value)));
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .spawn()
        .map_err(|error| Fail::internal(format!("cannot start {}: {error}", binary.display())))?;
    // Drained on its own thread: a helper that fills the pipe would otherwise
    // block forever while this side waits for it to exit.
    let reader = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            text
        })
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
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
    let stdout = reader
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();
    Ok(Output {
        status: status.code(),
        stdout,
    })
}

/// A system tool cahoots itself needs (`git`, `daft`, `ps`), held to the same
/// binary policy as a harness. One that is missing is a setup problem (34);
/// one the policy refuses is a refusal (33), and is never run.
pub fn system_tool(name: &str, workspace: &[&Path]) -> Res<PathBuf> {
    as_system_tool(name, resolve_binary(name, None, workspace))
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

/// The PATH a system tool runs with: the caller's, without its relative
/// entries and without any directory inside `workspace`. cahoots resolves
/// the tool itself under the binary policy; this holds what the tool then
/// looks up for itself — the `git` that `daft` runs, git's own helpers and
/// filter programs — to the same rule.
pub fn helper_path(workspace: &[&Path]) -> Option<OsString> {
    helper_path_from(env::path_var().as_deref(), workspace)
}

fn helper_path_from(path: Option<&OsStr>, workspace: &[&Path]) -> Option<OsString> {
    let roots: Vec<PathBuf> = workspace
        .iter()
        .flat_map(|root| [root.to_path_buf(), canonical_of(root)])
        .collect();
    let kept: Vec<PathBuf> = std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .filter(|dir| {
            let resolved = canonical_of(dir);
            !roots
                .iter()
                .any(|root| dir.starts_with(root) || resolved.starts_with(root))
        })
        .collect();
    // Nothing left is no PATH at all, never an empty one: an empty entry
    // means the working directory to a shell's lookup.
    std::env::join_paths(kept)
        .ok()
        .filter(|joined| !joined.is_empty())
}

/// `git rev-parse` in `dir`: the toplevel and the common dir (which is what
/// two worktrees of one repository share), both canonical. `Ok(None)` outside
/// a repository, or with no `git` at all. A `git` inside `workspace`, `dir`
/// or the repository around it is refused, and is never run.
pub fn git_roots(dir: &Path, workspace: &[&Path]) -> Res<Option<(PathBuf, PathBuf)>> {
    let here = canonical_of(dir);
    // The repository's top as its `.git` shows it, before any git is run: a
    // `git` planted at the top of the repository, above `dir`, is refused
    // here, not after it has answered.
    let tops = repository_tops(&here);
    let mut roots = workspace.to_vec();
    roots.push(&here);
    roots.extend(tops.iter().map(PathBuf::as_path));
    let git = match system_tool("git", &roots) {
        Ok(git) => git,
        Err(fail) if fail.exit == Exit::Policy => return Err(fail),
        Err(_) => return Ok(None),
    };
    let path = helper_path(&roots);
    let ask = |what: &str| {
        let output = run_helper_with_env(
            &git,
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
    if git.starts_with(&toplevel) {
        return Err(Fail::policy(format!(
            "refusing to run `git`: {} is inside the workspace {} — refusing to run a binary the \
             workspace supplies",
            git.display(),
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

/// When a process started, as `ps` tells it — the identity check that stops
/// reconcile from signalling a recycled pid. A `ps` the policy refuses is not
/// run, and nothing is known.
pub fn process_started(pid: i32, workspace: &[&Path]) -> Option<String> {
    let ps = system_tool("ps", workspace).ok()?;
    let output = run_helper_with_env(
        &ps,
        &["-o", "lstart=", "-p", &pid.to_string()],
        None,
        Duration::from_secs(5),
        helper_path(workspace),
        &[],
    )
    .ok()?;
    let started = output.stdout.trim();
    (!started.is_empty()).then(|| started.to_string())
}

/// Every descendant of `root`, from one `ps` snapshot. Codex runs its tool
/// commands in their own process groups, so signalling the callee's group
/// does not reach them (docs/SPIKE.md S3).
pub fn descendants(root: i32, workspace: &[&Path]) -> Vec<i32> {
    let Ok(ps) = system_tool("ps", workspace) else {
        return Vec::new();
    };
    let Ok(output) = run_helper_with_env(
        &ps,
        &["-axo", "pid=,ppid="],
        None,
        Duration::from_secs(5),
        helper_path(workspace),
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
}

/// The callee's whole environment: cleared, then rebuilt from an allowlist,
/// with HOME from passwd. The caller's harness markers, tokens and proxies
/// never reach it — and a vendor API key only when billing says so.
pub fn callee_environment(callee: &Callee<'_>) -> Res<Vec<(OsString, OsString)>> {
    let mut vars = env::callee_passthrough(callee.harness);
    vars.push(("HOME".into(), crate::dirs::passwd_home()?.into_os_string()));
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
    Command::new(callee.binary)
        .args(callee.argv)
        .current_dir(callee.cwd)
        .env_clear()
        .envs(callee_environment(callee)?)
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
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn executable(dir: &Path, name: &str, mode: u32) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn a_binary_inside_the_workspace_is_refused() {
        let workspace = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(workspace.path()).unwrap();
        let binary = executable(&root, "codex", 0o755);
        let fail = resolve_binary("codex", Some(&binary), &[&root]).unwrap_err();
        assert_eq!(fail.exit, Exit::Policy);
        assert!(resolve_binary("codex", Some(&binary), &[]).is_ok());
    }

    #[test]
    fn a_binary_others_can_write_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let binary = executable(dir.path(), "codex", 0o775);
        assert_eq!(
            resolve_binary("codex", Some(&binary), &[])
                .unwrap_err()
                .exit,
            Exit::Policy
        );
    }

    #[test]
    fn a_missing_binary_is_target_unavailable() {
        let fail = resolve_binary("codex", Some(Path::new("/nonexistent/codex")), &[]).unwrap_err();
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
            resolve_binary("codex", Some(&link), &[&root])
                .unwrap_err()
                .exit,
            Exit::Policy
        );
    }

    #[test]
    fn a_policy_refusal_of_a_system_tool_is_policy() {
        let workspace = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(workspace.path()).unwrap();
        let planted = executable(&root, "git", 0o755);
        let fail =
            as_system_tool("git", resolve_binary("git", Some(&planted), &[&root])).unwrap_err();
        assert_eq!(fail.exit, Exit::Policy);
        assert!(
            fail.message.contains("refusing to run `git`"),
            "{}",
            fail.message
        );

        let open = executable(&root, "daft", 0o777);
        let fail = as_system_tool("daft", resolve_binary("daft", Some(&open), &[])).unwrap_err();
        assert_eq!(fail.exit, Exit::Policy);

        // Missing is not a refusal: it is something to install.
        let missing = Path::new("/nonexistent/git");
        let fail = as_system_tool("git", resolve_binary("git", Some(missing), &[])).unwrap_err();
        assert_eq!(fail.exit, Exit::Config);
        assert!(
            fail.message.contains("cahoots needs `git`"),
            "{}",
            fail.message
        );
    }

    #[test]
    fn helper_path_drops_workspace_and_relative_entries() {
        let workspace = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(workspace.path()).unwrap();
        let inside = root.join("bin");
        fs::create_dir(&inside).unwrap();
        let path = std::env::join_paths([
            Path::new("/usr/bin"),
            &inside,
            Path::new("relative/bin"),
            Path::new("/bin"),
            // Not there yet, and still inside: judged by its name.
            &root.join("later"),
        ])
        .unwrap();
        let kept = helper_path_from(Some(&path), &[&root]).unwrap();
        assert_eq!(kept, OsString::from("/usr/bin:/bin"));

        // The same directory by another name is still inside.
        let other = tempfile::tempdir().unwrap();
        let link = other.path().join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let path = std::env::join_paths([link.join("bin"), PathBuf::from("/bin")]).unwrap();
        assert_eq!(
            helper_path_from(Some(&path), &[&root]).unwrap(),
            OsString::from("/bin")
        );

        // Nothing left is no PATH, not an empty one.
        let path = std::env::join_paths([&inside]).unwrap();
        assert_eq!(helper_path_from(Some(&path), &[&root]), None);
        assert_eq!(helper_path_from(None, &[&root]), None);
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

    #[test]
    fn helpers_run_with_a_deadline() {
        let sleep = resolve_binary("sleep", None, &[]).unwrap();
        assert!(run_helper(&sleep, &["5"], None, Duration::from_millis(100)).is_err());
        let output = run_helper(&sleep, &["0"], None, Duration::from_secs(5)).unwrap();
        assert_eq!(output.status, Some(0));
    }
}
