//! The ONLY module that starts processes (hard rule 3; `scripts/guard.sh`
//! holds it). Everything here takes an argv array — there is no shell, no
//! string to split, nothing to quote. It owns the callee's scrubbed
//! environment, binary resolution, process groups and signals.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;

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
    if let Some(root) = workspace.iter().find(|root| canonical.starts_with(root)) {
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

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
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
    run_helper_with_path(binary, args, cwd, deadline, env::path_var())
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

/// A system tool cahoots itself needs (`git`, `ps`), held to the same binary
/// policy as a harness.
pub fn system_tool(name: &str, workspace: &[&Path]) -> Res<PathBuf> {
    resolve_binary(name, None, workspace).map_err(|fail| {
        Fail::new(
            Exit::Config,
            format!("cahoots needs `{name}`: {}", fail.message),
        )
    })
}

/// `git rev-parse` in `dir`: the toplevel and the common dir (which is what
/// two worktrees of one repository share). `None` outside a repository.
pub fn git_roots(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let git = system_tool("git", &[]).ok()?;
    let ask = |what: &str| {
        let output = run_helper(
            &git,
            &["rev-parse", "--path-format=absolute", what],
            Some(dir),
            Duration::from_secs(10),
        )
        .ok()?;
        (output.status == Some(0)).then(|| PathBuf::from(output.stdout.trim()))
    };
    Some((ask("--show-toplevel")?, ask("--git-common-dir")?))
}

/// When a process started, as `ps` tells it — the identity check that stops
/// reconcile from signalling a recycled pid.
pub fn process_started(pid: i32) -> Option<String> {
    let ps = system_tool("ps", &[]).ok()?;
    let output = run_helper(
        &ps,
        &["-o", "lstart=", "-p", &pid.to_string()],
        None,
        Duration::from_secs(5),
    )
    .ok()?;
    let started = output.stdout.trim();
    (!started.is_empty()).then(|| started.to_string())
}

/// Every descendant of `root`, from one `ps` snapshot. Codex runs its tool
/// commands in their own process groups, so signalling the callee's group
/// does not reach them (docs/SPIKE.md S3).
pub fn descendants(root: i32) -> Vec<i32> {
    let Ok(ps) = system_tool("ps", &[]) else {
        return Vec::new();
    };
    let Ok(output) = run_helper(&ps, &["-axo", "pid=,ppid="], None, Duration::from_secs(5)) else {
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
    fn helpers_run_with_a_deadline() {
        let sleep = resolve_binary("sleep", None, &[]).unwrap();
        assert!(run_helper(&sleep, &["5"], None, Duration::from_millis(100)).is_err());
        let output = run_helper(&sleep, &["0"], None, Duration::from_secs(5)).unwrap();
        assert_eq!(output.status, Some(0));
    }
}
