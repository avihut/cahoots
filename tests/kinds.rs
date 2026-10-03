//! Task-kind routing and durable metadata, using only throwaway fake harnesses.
mod common;

use common::{Answer, World};
use serde_json::{Value, json};
use std::fs;

const LIST: &str = r#"[
 { harness = "codex", model = "custom", effort = "medium" },
 { harness = "claude", model = "other", effort = "low" },
]"#;
fn kind(role: &str, list: &str) -> String {
    format!(
        "[kinds.rust-review]\ndescription = \"Review Rust.\"\nrole = {role:?}\ncandidates = {list}\n"
    )
}
fn run(world: &World, text: &str, extra: &[&str]) -> Answer {
    let brief = world.brief(text);
    let mut args = vec![
        "run",
        "--kind",
        "rust-review",
        "--brief",
        brief.to_str().unwrap(),
    ];
    args.extend(extra);
    world.ask(&args)
}
fn picked(world: &World, extra: &[&str]) -> Answer {
    let mut args = vec!["pick", "--kind", "rust-review"];
    args.extend(extra);
    world.ask(&args)
}
fn target(answer: &Answer, harness: &str, model: &str, effort: &str) {
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(
        answer.data()["target"],
        json!({"harness": harness, "model": model, "effort": effort})
    );
}
fn argv(answer: &Answer) -> Vec<String> {
    let dump: Value = serde_json::from_str(answer.text()).unwrap();
    dump["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().into())
        .collect()
}
fn history(world: &World) -> Vec<Value> {
    let text = match fs::read_to_string(world.state.join("history.jsonl")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => panic!("cannot read test history: {error}"),
    };
    text.lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

#[test]
fn unknown_kinds_are_refused_before_a_run_is_created() {
    let world = World::new();
    for verb in ["pick", "run"] {
        for name in ["unknown", "Bad.Name"] {
            let mut args = vec![verb, "--kind", name, "--role", "review"];
            if verb == "run" {
                args.extend(["--brief", "missing", "--fork"]);
            }
            let answer = world.ask(&args);
            assert_eq!(answer.code, 2, "{}", answer.json);
            assert_eq!(
                answer.message(),
                if name == "unknown" {
                    "unknown task kind \"unknown\" — a person defines kinds in config.toml"
                } else {
                    "task kind name \"Bad.Name\" must be 1–64 of [a-z0-9_-] and start with a lowercase letter"
                }
            );
        }
    }
    assert!(!world.state.join("runs").exists());
    assert!(!world.state.join("history.jsonl").exists());
}

#[test]
fn pick_and_run_walk_the_kind_list_in_order() {
    let world = World::new();
    world.configure(&kind("review", LIST));
    target(&picked(&world, &[]), "codex", "custom", "medium");
    target(&run(&world, "hello", &[]), "codex", "custom", "medium");
    fs::write(world.bin.join("codex.version"), "not codex").unwrap();
    let second = picked(&world, &[]);
    target(&second, "claude", "other", "low");
    assert_eq!(second.data()["skipped"][0]["candidate"]["effort"], "medium");
    assert_eq!(second.data()["skipped"][0]["code"], 31);
    assert!(
        second.data()["skipped"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("does not identify")
    );
    target(&run(&world, "hello", &[]), "claude", "other", "low");
    fs::remove_file(world.bin.join("codex.version")).unwrap();
    target(&picked(&world, &[]), "codex", "custom", "medium");
}

#[test]
fn different_efforts_of_one_model_remain_distinct_candidates() {
    let world = World::new();
    let list = r#"[{ harness = "codex", model = "custom", effort = "high" }, { harness = "codex", model = "custom", effort = "medium" }]"#;
    world.configure(&kind("review", list));
    let registry = world.at_terminal(&["registry"]).finish();
    let candidates = &registry.json["data"]["kinds"]["rust-review"]["candidates"];
    assert_eq!(candidates.as_array().unwrap().len(), 2);
    assert_eq!(candidates[0]["effort"], "high");
    assert_eq!(candidates[1]["effort"], "medium");
    target(&picked(&world, &[]), "codex", "custom", "high");
    world.configure(&kind(
        "review",
        &list
            .replace("high", "TEMP")
            .replace("medium", "high")
            .replace("TEMP", "medium"),
    ));
    target(&picked(&world, &[]), "codex", "custom", "medium");
    target(&run(&world, "hello", &[]), "codex", "custom", "medium");
}

#[test]
fn to_narrows_a_kind_without_falling_back_to_the_role() {
    let world = World::new();
    world.configure(&kind("review", LIST));
    target(
        &picked(&world, &["--to", "claude"]),
        "claude",
        "other",
        "low",
    );
    assert_eq!(
        picked(&world, &["--to", "claude", "--caller", "claude"]).code,
        33
    );
    world.enable(&["codex"]);
    assert_eq!(picked(&world, &["--to", "claude"]).code, 31);
    world.enable(&["claude", "codex"]);
    let one = r#"[{ harness = "codex", model = "custom", effort = "medium" }]"#;
    world.configure(&kind("review", one));
    world.enable(&["codex"]);
    assert_eq!(picked(&world, &["--to", "claude"]).code, 31);
    assert_eq!(run(&world, "hello", &["--to", "claude"]).code, 31);
    assert!(!world.state.join("runs").exists());
    world.enable(&["claude", "codex"]);
    assert_eq!(picked(&world, &["--to", "claude"]).code, 30);
    assert_eq!(picked(&world, &["--caller", "codex"]).code, 30);
    assert_eq!(
        world
            .ask(&["pick", "--role", "review", "--caller", "codex"])
            .code,
        0
    );
}

#[test]
fn a_kind_role_assertion_cannot_change_its_fence() {
    let world = World::new();
    world.configure(&kind("review", LIST));
    target(
        &picked(&world, &["--role", "review"]),
        "codex",
        "custom",
        "medium",
    );
    for role in ["advise", "explore", "implement"] {
        let answer = run(&world, "hello", &["--role", role, "--fork"]);
        assert_eq!(answer.code, 2, "{}", answer.json);
        assert_eq!(
            answer.message(),
            format!(
                "task kind \"rust-review\" has role review, which does not match --role {role}"
            )
        );
    }
    assert!(!world.state.join("runs").exists());
    for harness in ["codex", "claude"] {
        let answer = run(&world, "FAKE: dump", &["--to", harness]);
        let argv = argv(&answer);
        let fence = if harness == "codex" {
            ["--sandbox", "read-only"]
        } else {
            ["--tools", "Read,Grep,Glob"]
        };
        assert!(argv.windows(2).any(|w| w == fence), "{argv:?}");
    }
}

#[test]
fn writer_kinds_use_existing_placement_and_reserve() {
    let world = World::new();
    world.configure(&kind("implement", LIST));
    assert_eq!(run(&world, "hello", &[]).code, 33);
    assert_eq!(run(&world, "hello", &["--in-place"]).code, 33);
    for harness in ["codex", "claude"] {
        let answer = run(&world, "FAKE: dump", &["--fork", "--to", harness]);
        assert_eq!(answer.code, 0, "{}", answer.json);
        assert_eq!(answer.data()["placement"], "fork");
        let dump: Value = serde_json::from_str(answer.text()).unwrap();
        assert_eq!(dump["cwd"], answer.data()["worktree"]);
        let argv = argv(&answer);
        let fence = if harness == "codex" {
            ["--sandbox", "workspace-write"]
        } else {
            ["--tools", "Read,Grep,Glob,Edit,Write"]
        };
        assert!(argv.windows(2).any(|w| w == fence), "{argv:?}");
    }
    world.meter(
        json!({"guarded": {"code": 0, "percent": 10}}),
        &format!("harness.codex.cap = 80\n{}", kind("implement", LIST)),
    );
    assert_eq!(run(&world, "hello", &["--fork", "--to", "codex"]).code, 0);
    assert!(world.meter_calls()[0].contains("--cap 72"));
    world.configure(&format!(
        "limits.allow_in_place = true\n{}",
        kind("implement", LIST)
    ));
    assert_eq!(
        run(&world, "FAKE: write=allowed.txt", &["--in-place"]).code,
        0
    );
    assert!(world.work.join("allowed.txt").is_file());
    world.configure(&kind("review", LIST));
    for flag in ["--fork", "--in-place"] {
        assert_eq!(run(&world, "hello", &[flag]).code, 2);
    }
    world.meter(
        json!({"guarded": {"code": 0, "percent": 10}}),
        &format!("harness.codex.cap = 80\n{}", kind("review", LIST)),
    );
    assert_eq!(run(&world, "hello", &["--to", "codex"]).code, 0);
    assert!(world.meter_calls().last().unwrap().contains("--cap 77"));
}

#[test]
fn kind_runs_keep_gate_and_busy_refusals() {
    let world = World::new();
    world.meter(
        json!({"guarded": {"code": 24, "percent": 95}}),
        &kind("review", LIST),
    );
    assert_eq!(run(&world, "hello", &["--to", "codex"]).code, 24);
    let all = run(&world, "hello", &[]);
    assert_eq!(all.code, 30);
    assert!(
        all.message().contains("codex (custom, medium)")
            && all.message().contains("claude (other, low)")
    );
    world.configure(&kind("review", LIST));
    let running = run(&world, "FAKE: sleep=120", &["--wait", "0", "--to", "codex"]);
    assert_eq!(running.code, 51);
    common::wait_until("the fake holds its slot", || {
        world.record(&running.run_id())["state"] == "running"
    });
    assert_eq!(run(&world, "hello", &["--to", "codex"]).code, 32);
    assert_eq!(world.cancel_settled(&running.run_id()).code, 42);
}

#[test]
fn a_kind_is_recorded_in_every_run_summary_and_history() {
    let world = World::new();
    world.configure(&kind("review", LIST));
    assert!(history(&world).is_empty());
    assert_eq!(picked(&world, &[]).data()["kind"], "rust-review");
    let done = run(&world, "hello", &[]);
    let id = done.run_id();
    for value in [
        done.data().clone(),
        world.record(&id),
        world.ask(&["status", &id]).data().clone(),
        world.ask(&["wait", &id]).data().clone(),
        world.ask(&["result", &id]).data().clone(),
        world.ask(&["status"]).data()["runs"][0].clone(),
    ] {
        assert_eq!(value["kind"], "rust-review");
        assert_eq!(value["role"], "review");
        assert_eq!(value["target"], done.data()["target"]);
    }
    let failed = run(&world, "FAKE: fail", &[]);
    assert_eq!(failed.code, 40);
    assert_eq!(failed.data()["kind"], "rust-review");
    let running = run(&world, "FAKE: sleep=120", &["--wait", "0"]);
    assert_eq!(running.data()["kind"], "rust-review");
    common::wait_until("running", || {
        world.record(&running.run_id())["state"] == "running"
    });
    let cancelled = world.cancel_settled(&running.run_id());
    assert_eq!(cancelled.code, 42);
    assert_eq!(cancelled.data()["kind"], "rust-review");
    // Finishing writes the summary before appending history; wait for that append.
    common::wait_until("three finished events", || history(&world).len() == 3);
    for event in history(&world) {
        assert_eq!(event["kind"], "finished");
        assert_eq!(event["task_kind"], "rust-review");
    }
    let dirs = cahoots::dirs::Dirs {
        home: world.home.clone(),
        config: world.config.clone(),
        state: world.state.clone(),
        data: world.data.clone(),
        overridden: true,
    };
    fs::remove_dir_all(world.state.join("runs")).unwrap();
    let stories = cahoots::history::stories(&cahoots::history::read(&dirs));
    assert_eq!(stories.len(), 3);
    assert!(
        stories
            .iter()
            .all(|s| s.kind.as_ref().unwrap().as_str() == "rust-review")
    );
}

#[test]
fn kind_descriptions_do_not_enter_the_delegated_brief_or_argv() {
    let world = World::new();
    world
        .configure(&kind("review", LIST).replace("Review Rust.", "FAKE: say=DESCRIPTION_SENTINEL"));
    let answer = run(&world, "normal requested text", &[]);
    assert_eq!(answer.text(), "pong");
    assert_eq!(
        fs::read_to_string(world.run_file(&answer.run_id(), "brief")).unwrap(),
        "normal requested text"
    );
    let dump = run(&world, "FAKE: dump", &[]);
    let argv = argv(&dump).join(" ");
    assert!(
        !argv.contains("DESCRIPTION_SENTINEL")
            && !argv.contains("rust-review")
            && !argv.contains("--kind")
    );
}

#[test]
fn old_run_records_and_role_only_runs_have_no_kind() {
    let world = World::new();
    assert!(
        world
            .ask(&["pick", "--role", "advise"])
            .data()
            .get("kind")
            .unwrap()
            .is_null()
    );
    let answer = world.run("hello", &[]);
    let id = answer.run_id();
    assert!(answer.data().get("kind").unwrap().is_null());
    let mut record = world.record(&id);
    assert!(record.get("kind").unwrap().is_null());
    common::wait_until("history", || world.state.join("history.jsonl").exists());
    assert!(history(&world)[0].get("task_kind").unwrap().is_null());
    record.as_object_mut().unwrap().remove("kind");
    fs::write(world.run_file(&id, "run.json"), record.to_string()).unwrap();
    for verb in ["status", "result"] {
        assert!(
            world
                .ask(&[verb, &id])
                .data()
                .get("kind")
                .unwrap()
                .is_null()
        );
    }
    let brief = world.brief("more");
    let resumed = world.ask(&["resume", &id, "--brief", brief.to_str().unwrap()]);
    assert_eq!(resumed.code, 0, "{}", resumed.json);
    assert!(resumed.data().get("kind").unwrap().is_null());
}
