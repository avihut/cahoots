//! The caller's environment chooses nothing for the callee: its PATH, its
//! TMPDIR (which Codex makes writable), its SHELL and its settings folders
//! (CODEX_HOME, CLAUDE_CONFIG_DIR — the callee's whole configuration) all
//! come from cahoots, config.toml and the passwd database
//! (docs/THREAT-MODEL.md, The callee's environment).

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use common::{Answer, World, path_str};
use serde_json::Value;

/// `cahoots run --role <role> --caller <caller>` with `FAKE: dump` as the
/// brief, and `env` in cahoots' own environment; the dump, and the answer.
fn dumped(world: &World, role: &str, caller: &str, env: &[(&str, &str)]) -> (Value, Answer) {
    let brief = world.brief("FAKE: dump\nFAKE: write=x.txt");
    let mut command = world.cahoots();
    command.args([
        "run",
        "--role",
        role,
        "--caller",
        caller,
        "--brief",
        path_str(&brief),
    ]);
    if role == "implement" {
        command.arg("--fork");
    }
    command.envs(env.iter().copied());
    let answer = common::answer(&mut command);
    assert_eq!(answer.code, 0, "{}", answer.json);
    (serde_json::from_str(answer.text()).unwrap(), answer)
}

#[test]
fn a_callers_tmpdir_does_not_reach_the_callee() {
    let world = World::new();
    let home = path_str(&world.home).to_string();
    // A Codex writer in a fork, and a Claude Code reader.
    for (role, caller) in [("implement", "claude"), ("advise", "codex")] {
        let (dump, answer) = dumped(&world, role, caller, &[("TMPDIR", &home)]);
        let tmp = common::callee_tmpdir(&answer.run_id());
        assert_eq!(dump["env"]["TMPDIR"], path_str(&tmp), "{role}");
        // Private while it ran, and never in cahoots' own directories.
        assert_eq!(dump["tmpdir_mode"], 0o700, "{role}: {dump}");
        assert!(!tmp.starts_with(&world.state), "{role}");
    }
}

#[test]
fn the_run_tmpdir_goes_with_the_run() {
    let world = World::new();
    let (_, answer) = dumped(&world, "advise", "claude", &[]);
    assert!(!common::callee_tmpdir(&answer.run_id()).exists());
    // A cancelled run's too.
    let run = world.run("FAKE: sleep=30", &["--wait", "0"]);
    let id = run.run_id();
    let tmp = common::callee_tmpdir(&id);
    common::wait_until("the callee started", || tmp.is_dir());
    assert_eq!(world.ask(&["cancel", &id]).code, 42);
    assert!(!tmp.exists(), "the run's temp directory outlived it");
    // What is there is removed only if it is a directory of this user's:
    // never what a link in its place points at.
    let kept = world.root.join("kept");
    fs::create_dir_all(&kept).unwrap();
    fs::write(kept.join("file"), "x").unwrap();
    let id = format!("link-{}", std::process::id());
    let link = common::callee_tmpdir(&id);
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(&kept, &link).unwrap();
    cahoots::run::record::remove_callee_tmpdir(&id);
    assert!(kept.join("file").is_file(), "a link was followed");
    fs::remove_file(&link).unwrap();
}

#[test]
fn a_callers_codex_home_is_not_used() {
    let world = World::new();
    let (evil, evil2) = (world.root.join("evil"), world.root.join("evil2"));
    for dir in [&evil, &evil2] {
        fs::create_dir_all(dir).unwrap();
    }
    let env = [
        ("CODEX_HOME", path_str(&evil)),
        ("CLAUDE_CONFIG_DIR", path_str(&evil2)),
    ];
    let (codex, _) = dumped(&world, "advise", "claude", &env);
    assert!(codex["env"].get("CODEX_HOME").is_none(), "{codex}");
    let (claude, _) = dumped(&world, "advise", "codex", &env);
    assert!(claude["env"].get("CLAUDE_CONFIG_DIR").is_none(), "{claude}");

    // The one config.toml names is the one the callee gets, and the one its
    // `--version` is asked with.
    let (mine, mine2) = (world.root.join("h"), world.root.join("h2"));
    for dir in [&mine, &mine2] {
        fs::create_dir_all(dir).unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    world.configure(&format!(
        "harness.codex.home = {mine:?}\nharness.claude.home = {mine2:?}"
    ));
    let asked = world.bin.join("codex.version-env");
    fs::write(&asked, "").unwrap();
    let (codex, _) = dumped(&world, "advise", "claude", &env);
    assert_eq!(codex["env"]["CODEX_HOME"], path_str(&mine));
    let (claude, _) = dumped(&world, "advise", "codex", &env);
    assert_eq!(claude["env"]["CLAUDE_CONFIG_DIR"], path_str(&mine2));
    let calls = fs::read_to_string(&asked).unwrap();
    assert!(!calls.is_empty());
    for call in calls.lines() {
        let call: Value = serde_json::from_str(call).unwrap();
        assert_eq!(call["codex_home"], path_str(&mine), "{call}");
    }
}

#[test]
fn a_configured_home_in_the_workspace_or_open_to_others_is_refused() {
    let pick = |world: &World| world.ask(&["pick", "--role", "advise", "--caller", "claude"]);
    let world = World::new();
    let inside = world.work.join("h");
    fs::create_dir_all(&inside).unwrap();
    world.configure(&format!("harness.codex.home = {inside:?}"));
    let answer = pick(&world);
    assert_eq!(answer.code, 33, "{}", answer.json);
    assert!(
        answer
            .message()
            .contains("refusing to use harness.codex.home"),
        "{}",
        answer.json
    );

    let open = world.root.join("open");
    fs::create_dir_all(&open).unwrap();
    fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
    world.configure(&format!("harness.codex.home = {open:?}"));
    assert_eq!(pick(&world).code, 31);

    world.configure(&format!(
        "harness.codex.home = {:?}",
        world.root.join("missing")
    ));
    assert_eq!(pick(&world).code, 31);
}

#[test]
fn the_callee_shell_comes_from_passwd() {
    let world = World::new();
    let planted = world.root.join("planted/sh");
    let (dump, _) = dumped(&world, "advise", "claude", &[("SHELL", path_str(&planted))]);
    let user = nix::unistd::User::from_uid(nix::unistd::getuid())
        .unwrap()
        .unwrap();
    if user.shell.as_os_str().is_empty() {
        assert!(dump["env"].get("SHELL").is_none(), "{dump}");
    } else {
        assert_eq!(dump["env"]["SHELL"], path_str(&user.shell));
    }
}

#[test]
fn only_the_harmless_variables_pass() {
    let world = World::new();
    let (dump, _) = dumped(
        &world,
        "advise",
        "claude",
        &[("FOO", "1"), ("LC_ALL", "C"), ("LANG", "C"), ("TZ", "UTC")],
    );
    let env = dump["env"].as_object().unwrap();
    for kept in ["LC_ALL", "LANG", "TZ"] {
        assert!(env.contains_key(kept), "{kept}: {dump}");
    }
    assert!(!env.contains_key("FOO"), "{dump}");
    let allowed = [
        "USER",
        "LOGNAME",
        "LANG",
        "TERM",
        "TZ",
        "PATH",
        "HOME",
        "TMPDIR",
        "SHELL",
        "CAHOOTS_DEPTH",
        "CAHOOTS_RUN_ID",
        "CAHOOTS_CALLER",
        "GIT_NO_LAZY_FETCH",
    ];
    for key in env.keys() {
        assert!(
            allowed.contains(&key.as_str()) || key.starts_with("LC_"),
            "{key} reached the callee: {dump}"
        );
    }
}

#[test]
fn doctor_says_when_the_recorded_path_is_stale() {
    let world = World::new();
    let (mine, new) = (world.root.join("mine"), world.root.join("new"));
    fs::create_dir_all(&mine).unwrap();
    let doctor = |path: &str| {
        let answer = common::answer(world.cahoots().arg("doctor").env("PATH", path));
        assert_eq!(answer.code, 0, "{}", answer.json);
        answer.data()["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["check"] == "tools: path")
            .unwrap()
            .clone()
    };
    let unset = doctor(path_str(&mine));
    assert_eq!(unset["status"], "warn", "{unset}");
    assert!(
        unset["detail"].as_str().unwrap().contains("not recorded"),
        "{unset}"
    );

    world.configure(&format!("tools.path = {mine:?}"));
    let current = doctor(path_str(&mine));
    assert_eq!(current["status"], "ok", "{current}");
    assert_eq!(current["detail"], "recorded: 1 directory");

    fs::create_dir_all(&new).unwrap();
    let behind = doctor(&format!("{}:{}", mine.display(), new.display()));
    assert_eq!(behind["status"], "warn", "{behind}");
    let detail = behind["detail"].as_str().unwrap();
    assert!(
        detail.contains("also has") && detail.contains(path_str(&new)),
        "{behind}"
    );

    fs::remove_dir(&mine).unwrap();
    let stale = doctor(path_str(&new));
    assert_eq!(stale["status"], "warn", "{stale}");
    let detail = stale["detail"].as_str().unwrap();
    assert!(
        detail.contains("no longer qualify") && detail.contains(path_str(&mine)),
        "{stale}"
    );
}
