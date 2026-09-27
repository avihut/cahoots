//! The run model, end to end, against the fake harness: a detached
//! supervisor, the run directory as the source of truth, and every way a run
//! can end — each with its own exit code.

mod common;

use std::fs;

use common::{World, alive, wait_until};

#[test]
fn the_suite_never_touches_the_real_directories() {
    let world = World::new();
    let dirs = world.ask(&["__dirs"]);
    assert_eq!(
        dirs.data()["dev_overrides_honoured"],
        true,
        "the suite needs a dev build: run it with `mise run test`"
    );
    assert_eq!(dirs.data()["overridden"], true);
    assert_eq!(dirs.data()["state"], world.state.to_str().unwrap());
    assert_eq!(dirs.data()["config"], world.config.to_str().unwrap());
    assert_eq!(dirs.data()["home"], world.home.to_str().unwrap());
}

#[test]
fn a_dev_build_refuses_the_real_directories() {
    let world = World::new();
    // One override missing is enough: a dev build gets ALL of config, state
    // and home from somewhere throwaway, or it does not start.
    for missing in [
        "CAHOOTS_CONFIG_DIR",
        "CAHOOTS_STATE_DIR",
        "CAHOOTS_HOME_DIR",
    ] {
        let mut command = world.cahoots();
        command.args(["status"]).env_remove(missing);
        let answer = common::answer(&mut command);
        assert_eq!(answer.code, 34, "without {missing}: {}", answer.json);
        assert!(answer.message().contains("DEV build"));
    }
}

#[test]
fn a_run_returns_the_answer_marked_untrusted() {
    let world = World::new();
    let answer = world.run("FAKE: say=the gate is sound", &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["state"], "done");
    assert_eq!(answer.text(), "the gate is sound");
    assert_eq!(answer.data()["result"]["untrusted"], true);
    // advise → codex first; the caller (claude) was left out.
    assert_eq!(answer.data()["target"]["harness"], "codex");
    assert_eq!(answer.data()["tokens"]["output"], 7);

    let run = answer.run_id();
    let events = fs::read_to_string(world.run_file(&run, "events.jsonl")).unwrap();
    assert!(events.contains("thread.started"));
    assert_eq!(
        fs::read_to_string(world.run_file(&run, "final.md")).unwrap(),
        "the gate is sound"
    );
    // `result` and `wait` answer the same way for the same run.
    assert_eq!(world.ask(&["result", &run]).code, 0);
    assert_eq!(world.ask(&["wait", &run]).code, 0);
    assert_eq!(world.ask(&["status", &run]).data()["state"], "done");
    assert_eq!(
        world.ask(&["status"]).data()["runs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn the_caller_is_left_out_and_claude_gets_a_preset_session() {
    let world = World::new();
    let answer = world.run("FAKE: dump", &["--caller", "codex"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["target"]["harness"], "claude");
    let dump: serde_json::Value = serde_json::from_str(answer.text()).unwrap();
    let argv: Vec<&str> = dump["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let session = world.record(&answer.run_id())["progress"]["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let at = argv
        .iter()
        .position(|a| *a == "--session-id")
        .expect("a preset session id");
    assert_eq!(argv[at + 1], session);
    // The read-only fence, as the callee actually received it.
    assert!(argv.windows(2).any(|w| w == ["--tools", "Read,Grep,Glob"]));
    assert!(argv.contains(&"--strict-mcp-config"));
}

#[test]
fn nothing_runs_until_a_human_enables_a_target() {
    let world = World::bare();
    world.configure("");
    let answer = world.run("hello", &[]);
    assert_eq!(answer.code, 30, "{}", answer.json);
    assert_eq!(answer.json["retry"], "later");
    assert_eq!(world.run("hello", &["--to", "codex"]).code, 31);
    assert!(
        !world.state.join("runs").exists(),
        "a refusal must not leave a run behind"
    );

    world.enable(&["claude"]);
    // Only the caller itself is enabled: still nobody to ask.
    assert_eq!(world.run("hello", &[]).code, 30);
    assert_eq!(world.run("hello", &["--to", "claude"]).code, 33);
}

#[test]
fn enable_turns_a_target_on_in_config_toml_and_off_again() {
    let world = World::bare();
    world.configure("# Codex, but carefully.\nharness.codex.cap = 60");
    let on = world.at_terminal(&["enable", "codex"]).finish();
    assert_eq!(on.code, 0, "{}", on.json);
    assert_eq!(on.json["data"]["enabled"], serde_json::json!(["codex"]));
    let text = fs::read_to_string(world.config.join("config.toml")).unwrap();
    assert!(
        text.contains("harness.codex.enabled = true\n")
            && text.contains("# Codex, but carefully.\nharness.codex.cap = 60\n"),
        "{text}"
    );
    assert_eq!(world.run("hello", &["--to", "codex"]).code, 0);

    let off = world.at_terminal(&["enable", "codex", "--off"]).finish();
    assert_eq!(off.json["data"]["enabled"], serde_json::json!([]));
    assert!(
        !fs::read_to_string(world.config.join("config.toml"))
            .unwrap()
            .contains("enabled"),
        "off is the default, so nothing is written for it"
    );
    assert_eq!(world.run("hello", &["--to", "codex"]).code, 31);
}

#[test]
fn doctor_says_enabled_json_is_no_longer_read() {
    let world = World::new();
    fs::write(
        world.config.join("enabled.json"),
        r#"{"v":1,"enabled":["codex"]}"#,
    )
    .unwrap();
    let report = world.ask(&["doctor"]).json;
    let check = report["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "enabled.json")
        .unwrap_or_else(|| panic!("no enabled.json check in {report}"));
    assert_eq!(check["status"], "warn");
    assert!(
        check["detail"].as_str().unwrap().contains("cahoots enable"),
        "{check}"
    );
}

#[test]
fn the_callee_gets_a_scrubbed_environment() {
    let world = World::new();
    let brief = world.brief("FAKE: dump");
    let answer = common::answer(
        world
            .cahoots()
            .args([
                "run",
                "--role",
                "advise",
                "--caller",
                "claude",
                "--brief",
                brief.to_str().unwrap(),
            ])
            .env("CLAUDECODE", "1")
            .env("OPENAI_API_KEY", "not-a-real-key")
            .env("HTTPS_PROXY", "http://proxy.invalid")
            .env("LD_PRELOAD", "/tmp/evil.so")
            .env("HOME", "/tmp/not-my-home")
            .env("LC_ALL", "C"),
    );
    assert_eq!(answer.code, 0, "{}", answer.json);
    let dump: serde_json::Value = serde_json::from_str(answer.text()).unwrap();
    let env = dump["env"].as_object().unwrap();
    for gone in [
        "CLAUDECODE",
        "OPENAI_API_KEY",
        "HTTPS_PROXY",
        "LD_PRELOAD",
        "CAHOOTS_STATE_DIR",
    ] {
        assert!(!env.contains_key(gone), "{gone} reached the callee");
    }
    assert_ne!(
        env["HOME"], "/tmp/not-my-home",
        "HOME must come from passwd, not from the caller"
    );
    assert_eq!(env["LC_ALL"], "C");
    assert_eq!(env["CAHOOTS_DEPTH"], "1");
    assert_eq!(env["CAHOOTS_CALLER"], "claude");
    assert_eq!(dump["cwd"], world.work.to_str().unwrap());
}

#[test]
fn an_api_key_passes_only_when_billing_says_api() {
    let world = World::new();
    let text = format!(
        "schema = 1\n[harness.codex]\nenabled = true\nbinary = {:?}\nbilling = \"api\"\n",
        world.bin.join("codex")
    );
    fs::write(world.config.join("config.toml"), text).unwrap();
    let brief = world.brief("FAKE: dump");
    let answer = common::answer(
        world
            .cahoots()
            .args([
                "run",
                "--role",
                "advise",
                "--caller",
                "claude",
                "--brief",
                brief.to_str().unwrap(),
            ])
            .env("OPENAI_API_KEY", "not-a-real-key"),
    );
    assert_eq!(answer.code, 0, "{}", answer.json);
    let dump: serde_json::Value = serde_json::from_str(answer.text()).unwrap();
    assert_eq!(dump["env"]["OPENAI_API_KEY"], "not-a-real-key");
}

#[test]
fn a_long_run_outlives_the_call_that_started_it() {
    let world = World::new();
    let started = world.run("FAKE: sleep=2\nFAKE: say=worth the wait", &["--wait", "0"]);
    assert_eq!(started.code, 51, "{}", started.json);
    assert_eq!(started.json["retry"], "later");
    assert!(started.data()["result"].is_null());
    let run = started.run_id();

    let finished = world.ask(&["wait", &run, "--timeout", "30"]);
    assert_eq!(finished.code, 0, "{}", finished.json);
    assert_eq!(finished.text(), "worth the wait");
}

#[test]
fn cancel_stops_the_callee() {
    let world = World::new();
    let run = world.run("FAKE: sleep=120", &["--wait", "0"]).run_id();
    wait_until("the callee is running", || {
        world.record(&run)["callee_pid"].is_i64()
    });
    let pid = world.record(&run)["callee_pid"].as_i64().unwrap();
    assert!(alive(pid));

    let cancelled = world.ask(&["cancel", &run]);
    assert_eq!(cancelled.code, 42, "{}", cancelled.json);
    assert_eq!(cancelled.data()["state"], "cancelled");
    wait_until("the callee is gone", || !alive(pid));
    // Resumable: the session id was on disk before the run was stopped.
    assert_eq!(cancelled.data()["resumable"], true);
    assert_eq!(world.ask(&["result", &run]).code, 42);
}

#[test]
fn a_run_that_takes_too_long_is_stopped_and_sweeps_what_it_started() {
    let world = World::new();
    let answer = world.run(
        "FAKE: child\nFAKE: sleep=120",
        &["--timeout", "1", "--wait", "30"],
    );
    assert_eq!(answer.code, 41, "{}", answer.json);
    let run = answer.run_id();
    let log = fs::read_to_string(world.run_file(&run, "supervisor.log")).unwrap();
    let child: i64 = log
        .lines()
        .find_map(|line| line.strip_prefix("[callee] left child "))
        .expect("the fake reports its child")
        .trim()
        .parse()
        .unwrap();
    // The child was in a process group of its own — out of killpg's reach.
    wait_until("the orphan is swept", || !alive(child));
}

#[test]
fn a_failed_run_is_exit_40_with_the_callees_reason() {
    let world = World::new();
    let answer = world.run("FAKE: fail", &[]);
    assert_eq!(answer.code, 40, "{}", answer.json);
    assert_eq!(answer.message(), "the fake was told to fail");
    assert_eq!(answer.json["retry"], "other_target");
}

#[test]
fn a_delegated_run_may_not_delegate() {
    let world = World::new();
    let brief = world.brief("hello");
    let answer = common::answer(
        world
            .cahoots()
            .args([
                "run",
                "--role",
                "advise",
                "--caller",
                "codex",
                "--brief",
                brief.to_str().unwrap(),
            ])
            .env("CAHOOTS_DEPTH", "1"),
    );
    assert_eq!(answer.code, 33, "{}", answer.json);
}

#[test]
fn nested_harness_markers_need_an_explicit_caller() {
    let world = World::new();
    let brief = world.brief("hello");
    let mut command = world.cahoots();
    command
        .args([
            "run",
            "--role",
            "advise",
            "--brief",
            brief.to_str().unwrap(),
        ])
        .env("CLAUDECODE", "1")
        .env("CODEX_THREAD_ID", "t");
    assert_eq!(common::answer(&mut command).code, 2);
    // One marker is enough to know who to leave out.
    let mut command = world.cahoots();
    command
        .args([
            "run",
            "--role",
            "advise",
            "--brief",
            brief.to_str().unwrap(),
        ])
        .env("CODEX_THREAD_ID", "t");
    let answer = common::answer(&mut command);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["target"]["harness"], "claude");
}

#[test]
fn inside_codexs_sandbox_run_says_which_rule_is_missing() {
    let world = World::new();
    let brief = world.brief("hello");
    let mut command = world.cahoots();
    command
        .args([
            "run",
            "--role",
            "advise",
            "--caller",
            "codex",
            "--brief",
            brief.to_str().unwrap(),
        ])
        .env("CODEX_SANDBOX", "seatbelt");
    let answer = common::answer(&mut command);
    assert_eq!(answer.code, 33);
    assert!(
        answer
            .message()
            .contains(r#"prefix_rule(pattern=["cahoots", "run"]"#)
    );
}

#[test]
fn a_busy_target_is_skipped_and_an_explicit_one_is_busy() {
    let world = World::new();
    let first = world
        .run("FAKE: sleep=120", &["--wait", "0", "--to", "codex"])
        .run_id();
    wait_until("the first run holds its slot", || {
        world.record(&first)["state"] == "running"
    });

    assert_eq!(world.run("hello", &["--to", "codex"]).code, 32);
    // Without --to, advise falls through to... nobody: claude is the caller.
    assert_eq!(world.run("hello", &[]).code, 32);
    // A caller that is neither may still be served by the free harness.
    let other = world.run("hello", &["--caller", "codex", "--to", "claude"]);
    assert_eq!(other.code, 0, "{}", other.json);

    assert_eq!(world.ask(&["cancel", &first]).code, 42);
    assert_eq!(world.run("hello", &["--to", "codex"]).code, 0);
}

#[test]
fn a_dead_supervisor_is_noticed_and_its_callee_is_stopped() {
    let world = World::new();
    let run = world.run("FAKE: sleep=120", &["--wait", "0"]).run_id();
    wait_until("the callee is running", || {
        world.record(&run)["callee_started"].is_string()
    });
    let record = world.record(&run);
    let (supervisor, callee) = (
        record["supervisor_pid"].as_i64().unwrap(),
        record["callee_pid"].as_i64().unwrap(),
    );
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &supervisor.to_string()])
            .status()
            .unwrap()
            .success()
    );
    wait_until("the supervisor is gone", || !alive(supervisor));

    // Any later invocation reconciles.
    let status = world.ask(&["status", &run]);
    assert_eq!(status.data()["state"], "crashed");
    wait_until("the orphaned callee is stopped", || !alive(callee));
    assert_eq!(world.ask(&["result", &run]).code, 40);
}

#[test]
fn the_ledger_bounds_a_retry_loop() {
    let world = World::new();
    world.configure("[meter.ledger]\nmax_runs_per_hour = 2");
    assert_eq!(world.run("one", &[]).code, 0);
    assert_eq!(world.run("two", &[]).code, 0);
    let third = world.run("three", &[]);
    assert_eq!(third.code, 24, "{}", third.json);
    assert_eq!(third.json["retry"], "after_reset");
}

#[test]
fn a_brief_may_not_be_any_file_on_the_machine() {
    let world = World::new();
    let outside = world.bin.join("private.txt");
    fs::write(&outside, "a secret").unwrap();
    let answer = world.ask(&[
        "run",
        "--role",
        "advise",
        "--caller",
        "claude",
        "--brief",
        outside.to_str().unwrap(),
    ]);
    assert_eq!(answer.code, 33, "{}", answer.json);
    let answer = world.ask(&[
        "run", "--role", "advise", "--caller", "claude", "--brief", "brief.md", "--dir", "/",
    ]);
    assert_eq!(answer.code, 33, "{}", answer.json);
}

#[test]
fn a_binary_that_is_not_the_harness_is_refused() {
    let world = World::new();
    fs::write(world.bin.join("codex.version"), "Antigravity 1.107.0").unwrap();
    let answer = world.run("hello", &["--to", "codex"]);
    assert_eq!(answer.code, 31, "{}", answer.json);
    fs::write(world.bin.join("codex.version"), "codex-cli 0.100.0").unwrap();
    let answer = world.run("hello", &["--to", "codex"]);
    assert_eq!(answer.code, 31);
    assert!(answer.message().contains("older"));
}

#[test]
fn unknown_and_malformed_run_ids() {
    let world = World::new();
    assert_eq!(
        world
            .ask(&["result", "0198c0de-0000-7000-8000-000000000000"])
            .code,
        50
    );
    assert_eq!(world.ask(&["status", "../../etc"]).code, 2);
    assert_eq!(world.ask(&["cancel", "nope"]).code, 50);
}
