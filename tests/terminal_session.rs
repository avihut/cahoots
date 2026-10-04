//! A terminal session ends its command and the command's group, however the
//! test ends (#85): a `std::process::Child` that is dropped is left running,
//! and `AtTerminal` must not be.

mod common;

use std::fs;
use std::os::unix::process::CommandExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::{AtTerminal, Stdout, alive, wait_until};

/// A shell that starts a `sleep` of its own and writes its pid to `pidfile`,
/// then waits: a command with a descendant in its group.
fn with_a_descendant(pidfile: &Path) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "sleep 60 & echo $! > \"$1\"; wait", "sh"]);
    command.arg(pidfile);
    command
}

fn pid_in(pidfile: &Path) -> i64 {
    wait_until("the descendant wrote its pid", || {
        fs::read_to_string(pidfile).is_ok_and(|text| text.trim().parse::<i64>().is_ok())
    });
    fs::read_to_string(pidfile).unwrap().trim().parse().unwrap()
}

fn start(command: Command) -> AtTerminal {
    AtTerminal::start(command, Duration::from_millis(0), Stdout::Terminal)
}

fn gone(pid: i64) {
    wait_until(&format!("process {pid} is gone"), || !alive(pid));
}

#[test]
fn a_session_dropped_without_finish_takes_the_command_and_its_group() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let terminal = start(with_a_descendant(&pidfile));
    let descendant = pid_in(&pidfile);
    assert!(alive(descendant));
    drop(terminal);
    gone(descendant);
}

#[test]
fn a_session_that_panics_in_a_wait_takes_the_command_and_its_group() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let terminal = start(with_a_descendant(&pidfile));
        let _ = pid_in(&pidfile);
        // The test fails here, as a `wait_for` that gives up does.
        let _hold = &terminal;
        panic!("the test failed with the terminal open");
    }));
    assert!(outcome.is_err());
    gone(pid_in(&pidfile));
}

#[test]
fn a_finished_session_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "sleep 60 & echo $! > \"$1\"", "sh"]);
    command.arg(&pidfile);
    let finished = start(command).finish();
    assert_eq!(finished.code, 0);
    // The descendant outlived the command that started it, and the session's
    // end took it with the group.
    gone(pid_in(&pidfile));
}

/// The piped mode, which `World::at_terminal` uses: a descendant that holds
/// the command's stdout open must not keep `finish` waiting on it.
#[test]
fn a_finished_piped_session_does_not_wait_on_a_descendant_holding_its_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "echo '{}'; sleep 60 & echo $! > \"$1\"", "sh"]);
    command.arg(&pidfile);
    let started = Instant::now();
    let finished = AtTerminal::start(command, Duration::from_millis(0), Stdout::Piped).finish();
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "finish waited on the descendant: {:?}",
        started.elapsed()
    );
    assert_eq!(finished.code, 0);
    gone(pid_in(&pidfile));
}

/// Cleanup signals the session's own group, by an id it still holds when the
/// command has long been reaped, and no one else's.
#[test]
fn ending_a_session_spares_a_process_in_another_group() {
    let bystander = Command::new("sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = i64::from(bystander.id());
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "sleep 60 & echo $! > \"$1\"", "sh"]);
    command.arg(&pidfile);
    let terminal = start(command);
    let descendant = pid_in(&pidfile);
    // The command exits and is reaped (`finish`) while its descendant lives on.
    assert!(alive(descendant));
    let finished = terminal.finish();
    assert_eq!(finished.code, 0);
    gone(descendant);
    assert!(alive(pid), "a process in another group was signalled");
    let mut bystander = bystander;
    bystander.kill().unwrap();
    bystander.wait().unwrap();
}

/// The refusal case: what the helper is held against. Without the drop, a
/// started process outlives the handle that started it, so a test that
/// leaves it dropped is caught here rather than on a machine's process table.
#[test]
fn a_plain_child_that_is_dropped_is_left_running() {
    let child = Command::new("sleep").arg("60").spawn().unwrap();
    let pid = i64::from(child.id());
    drop(child);
    assert!(alive(pid));
    // Cleaned up by hand, and reaped: a zombie still shows in `ps`.
    let pid = nix::unistd::Pid::from_raw(pid as i32);
    nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL).unwrap();
    nix::sys::wait::waitpid(pid, None).unwrap();
}
