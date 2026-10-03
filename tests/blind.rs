//! Blind run contracts, using fake harnesses and disposable state only.
mod common;

use common::{Answer, World, wait_until};
use serde_json::{Value, json};
use std::fs;

const BLIND: &str = "review.blind = true";
const REVIEW: &str = "review.blind = true\nreview.enabled = true\nreview.sample_rate = 1.0";

fn hidden(data: &Value, harness: &str) {
    assert_eq!(data["blind"], true, "{data}");
    assert_eq!(data["target"], json!({"harness": harness}), "{data}");
    assert!(data.get("model_reported").is_none(), "{data}");
}
fn open(data: &Value, record: &Value) {
    assert_eq!(data["blind"], false, "{data}");
    assert_eq!(data["target"], record["target"]);
    assert_eq!(
        data.get("model_reported"),
        record["progress"].get("model_reported")
    );
}
fn history(world: &World) -> Vec<Value> {
    fs::read_to_string(world.state.join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect()
}
fn finished(world: &World, id: &str) {
    wait_until("Finished history event", || {
        history(world)
            .iter()
            .any(|e| e["kind"] == "finished" && e["run"] == id)
    });
}
fn views(world: &World, id: &str, code: i32, blind: bool) {
    let record = world.record(id);
    for (args, expected) in [
        (vec!["status", id], 0),
        (vec!["result", id], code),
        (vec!["wait", id, "--timeout", "0"], code),
    ] {
        let answer = world.ask(&args);
        assert_eq!(answer.code, expected, "{}", answer.json);
        if blind {
            hidden(answer.data(), record["target"]["harness"].as_str().unwrap());
        } else {
            open(answer.data(), &record);
        }
    }
}
fn reveal(world: &World, id: &str, label: &str) {
    finished(world, id);
    let outcome = world.ask(&["outcome", id, label]);
    assert_eq!(outcome.code, 0, "{}", outcome.json);
    assert_eq!(outcome.data(), &json!({"run": id, "outcome": label}));
}
fn completed(world: &World, text: &str) -> Answer {
    let answer = world.run(text, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    finished(world, &answer.run_id());
    answer
}

#[test]
fn blind_defaults_off_and_is_independent_of_review() {
    for config in [
        "",
        "review.blind = false",
        "review.blind = true\nreview.enabled = false",
    ] {
        let world = World::new();
        world.configure(config);
        let answer = completed(&world, "hello");
        if config.contains("true") {
            hidden(answer.data(), "codex");
            assert!(answer.data().get("pending_reviews").is_none());
            assert_eq!(
                world.ask(&["review", "next", "--caller", "claude"]).data(),
                &json!({"enabled": false, "pending": 0, "next": null})
            );
            assert_eq!(
                world
                    .ask(&["review", "submit", &answer.run_id(), "--caller", "claude"])
                    .code,
                33
            );
        } else {
            open(answer.data(), &world.record(&answer.run_id()));
        }
    }
}

#[test]
fn blind_hides_declared_and_reported_identity_in_both_directions() {
    for (caller, target) in [("claude", "codex"), ("codex", "claude")] {
        let world = World::new();
        world.configure(BLIND);
        let answer = world.run("FAKE: say=useful answer", &["--caller", caller]);
        assert_eq!(answer.code, 0, "{}", answer.json);
        let id = answer.run_id();
        finished(&world, &id);
        hidden(answer.data(), target);
        views(&world, &id, 0, true);
        let private = world.record(&id);
        assert_eq!(private["blind"], true);
        assert!(private["target"]["model"].is_string());
        assert!(private["target"]["effort"].is_string());
        if target == "claude" {
            assert_eq!(private["progress"]["model_reported"], "claude-fake");
        }
        assert_eq!(answer.text(), "useful answer");
        assert_eq!(answer.data()["result"]["untrusted"], true);
        assert_eq!(answer.data()["result"]["truncated"], false);
        assert_eq!(
            fs::read_to_string(world.run_file(&id, "final.md")).unwrap(),
            answer.text()
        );
        reveal(&world, &id, "accepted");
        views(&world, &id, 0, false);
    }
}

#[test]
fn all_run_views_use_the_same_blind_projection() {
    let world = World::new();
    world.configure(BLIND);
    let answer = world.run("FAKE: sleep=120", &["--wait", "0"]);
    let id = answer.run_id();
    assert_eq!(answer.code, 51);
    hidden(answer.data(), "codex");
    views(&world, &id, 51, true);
    let list = world.ask(&["status"]);
    assert_eq!(list.code, 0);
    hidden(&list.data()["runs"][0], "codex");
    let cancelled = world.ask(&["cancel", &id]);
    assert_eq!(cancelled.code, 42, "{}", cancelled.json);
    hidden(cancelled.data(), "codex");
    assert!(cancelled.data().get("result").is_none());
    views(&world, &id, 42, true);
    reveal(&world, &id, "discarded");
    let again = world.ask(&["cancel", &id]);
    assert_eq!(again.code, 42);
    open(again.data(), &world.record(&id));
}

#[test]
fn each_outcome_reveals_only_its_own_run() {
    for label in ["accepted", "reworked", "discarded"] {
        let world = World::new();
        world.configure(BLIND);
        let id = completed(&world, "first").run_id();
        let other = completed(&world, "second").run_id();
        views(&world, &id, 0, true);
        reveal(&world, &id, label);
        views(&world, &id, 0, false);
        views(&world, &other, 0, true);
        let list = world.ask(&["status"]);
        let entries = list.data()["runs"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        let rated = entries.iter().find(|entry| entry["run"] == id).unwrap();
        let unrated = entries.iter().find(|entry| entry["run"] == other).unwrap();
        open(rated, &world.record(&id));
        hidden(unrated, "codex");
        reveal(&world, &id, "reworked");
        views(&world, &id, 0, false);
        let report = world.ask(&["report"]);
        let row = &report.data()["by_role_and_target"]["advise · codex · gpt-6-astra · high"];
        assert_eq!(row["reworked"], 1);
        assert_eq!(row["runs"], 2);
        assert_eq!(world.record(&id)["blind"], true);
    }
}

#[test]
fn launch_policy_survives_configuration_changes() {
    let world = World::new();
    world.configure(BLIND);
    let id = world.run("FAKE: sleep=3", &["--wait", "0"]).run_id();
    wait_until("blind fake is running", || {
        world.record(&id)["state"] == "running"
    });
    world.configure("review.blind = false");
    assert_eq!(world.ask(&["wait", &id, "--timeout", "30"]).code, 0);
    views(&world, &id, 0, true);
    let open_id = world.run("FAKE: sleep=3", &["--wait", "0"]).run_id();
    wait_until("open fake is running", || {
        world.record(&open_id)["state"] == "running"
    });
    world.configure(BLIND);
    assert_eq!(world.ask(&["wait", &open_id, "--timeout", "30"]).code, 0);
    views(&world, &open_id, 0, false);
    let latest = completed(&world, "third");
    hidden(latest.data(), "codex");
    finished(&world, &id);
    finished(&world, &open_id);
    for (run, blind) in [(&id, true), (&open_id, false)] {
        assert_eq!(world.record(run)["blind"], blind);
        assert_eq!(
            history(&world).iter().find(|e| e["run"] == *run).unwrap()["blind"],
            blind
        );
    }
    // A run's blindness is its record's, whatever config.toml says now. A
    // config.toml that does not load is the person's to fix: every verb that
    // starts a tool reads the pinned tools from it, so it is refused (34).
    world.configure("review.blind = 'wrong'");
    for verb in ["status", "result"] {
        assert_eq!(world.ask(&[verb, &id]).code, 34, "{verb}");
    }
    assert_eq!(world.ask(&["wait", &id, "--timeout", "0"]).code, 34);
    assert_eq!(world.run("new", &[]).code, 34);
    world.configure("");
    for verb in ["status", "result"] {
        let answer = world.ask(&[verb, &id]);
        assert_eq!(answer.code, 0, "{}", answer.json);
        hidden(answer.data(), "codex");
    }
}

#[test]
fn resume_snapshots_current_policy_and_has_its_own_outcome() {
    for parent_blind in [false, true] {
        let world = World::new();
        world.configure(if parent_blind { BLIND } else { "" });
        let parent = completed(&world, "parent").run_id();
        world.configure(if parent_blind { "" } else { BLIND });
        let brief = world.brief("follow up");
        let child = world.ask(&[
            "resume",
            &parent,
            "--caller",
            "claude",
            "--brief",
            brief.to_str().unwrap(),
        ]);
        assert_eq!(child.code, 0, "{}", child.json);
        let id = child.run_id();
        finished(&world, &id);
        if parent_blind {
            open(child.data(), &world.record(&id));
        } else {
            hidden(child.data(), "codex");
        }
        views(&world, &id, 0, !parent_blind);
        let record = world.record(&id);
        let old = world.record(&parent);
        assert_eq!(record["target"], old["target"]);
        assert_eq!(record["resumed_from"], parent);
        assert_eq!(
            record["progress"]["session_id"],
            old["progress"]["session_id"]
        );
        reveal(&world, &parent, "accepted");
        views(&world, &id, 0, !parent_blind);
        reveal(&world, &id, "reworked");
        views(&world, &parent, 0, false);
        views(&world, &id, 0, false);
    }
    let world = World::new();
    world.configure(BLIND);
    let parent = completed(&world, "parent").run_id();
    let brief = world.brief("child");
    let child = world
        .ask(&[
            "resume",
            &parent,
            "--caller",
            "claude",
            "--brief",
            brief.to_str().unwrap(),
        ])
        .run_id();
    reveal(&world, &child, "accepted");
    views(&world, &parent, 0, true);
}

#[test]
fn blind_failures_keep_the_original_exit_contract() {
    for (text, args, code, class, retry) in [
        ("FAKE: fail", vec![], 40, "run_failed", "other_target"),
        (
            "FAKE: sleep=120",
            vec!["--timeout", "1", "--wait", "30"],
            41,
            "run_timed_out",
            "later",
        ),
    ] {
        let world = World::new();
        world.configure(BLIND);
        let answer = world.run(text, &args);
        let id = answer.run_id();
        assert_eq!(answer.code, code, "{}", answer.json);
        assert_eq!(answer.json["class"], class);
        assert_eq!(answer.json["retry"], retry);
        hidden(answer.data(), "codex");
        views(&world, &id, code, true);
        reveal(&world, &id, "discarded");
        views(&world, &id, code, false);
        assert_eq!(world.ask(&["result", &id]).message(), answer.message());
    }
}

#[test]
fn invalid_outcomes_do_not_reveal() {
    let world = World::new();
    world.configure(BLIND);
    let id = completed(&world, "done").run_id();
    let live = world.run("FAKE: sleep=120", &["--wait", "0"]).run_id();
    for (run, label, code, class, retry) in [
        (id.as_str(), "invalid", 2, "usage_error", "never"),
        ("../bad", "accepted", 2, "usage_error", "never"),
        ("absent", "accepted", 50, "no_such_run", "never"),
        (live.as_str(), "accepted", 51, "not_finished", "later"),
    ] {
        let answer = world.ask(&["outcome", run, label]);
        assert_eq!(answer.code, code, "{}", answer.json);
        assert_eq!(answer.json["class"], class);
        assert_eq!(answer.json["retry"], retry);
        assert!(answer.json.get("data").is_none());
        assert!(!answer.message().is_empty());
    }
    assert!(!history(&world).iter().any(|e| e["kind"] == "outcome"));
    views(&world, &id, 0, true);
    views(&world, &live, 51, true);
    assert_eq!(world.ask(&["cancel", &live]).code, 42);
}

#[test]
fn legacy_records_and_history_remain_open() {
    let world = World::new();
    world.configure("review.enabled = true\nreview.sample_rate = 1.0");
    let id = completed(&world, "legacy").run_id();
    let mut record = world.record(&id);
    record.as_object_mut().unwrap().remove("blind");
    fs::write(world.run_file(&id, "run.json"), record.to_string()).unwrap();
    let mut events = history(&world);
    for event in &mut events {
        event.as_object_mut().unwrap().remove("blind");
    }
    fs::write(
        world.state.join("history.jsonl"),
        events.iter().map(|e| format!("{e}\n")).collect::<String>(),
    )
    .unwrap();
    world.configure(REVIEW);
    views(&world, &id, 0, false);
    let next = world.ask(&["review", "next", "--caller", "claude"]);
    assert_eq!(next.data()["next"]["blind"], false);
    assert_eq!(next.data()["next"]["target"], record["target"]);
    hidden(completed(&world, "new").data(), "codex");
}

#[test]
fn missing_or_damaged_history_never_proves_an_outcome() {
    for damage in ["missing", "directory", "junk", "invalid", "orphan"] {
        let world = World::new();
        world.configure(BLIND);
        let id = completed(&world, "answer").run_id();
        let path = world.state.join("history.jsonl");
        let finished_line = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).unwrap();
        match damage {
            "missing" => {},
            "directory" => fs::create_dir(&path).unwrap(),
            "junk" => fs::write(&path, "broken\n").unwrap(),
            "invalid" => fs::write(&path, format!("{finished_line}{{\"kind\":\"outcome\",\"t\":1,\"run\":\"{id}\",\"outcome\":\"invalid\"}}\n")).unwrap(),
            _ => fs::write(&path, format!("{{\"kind\":\"outcome\",\"t\":1,\"run\":\"{id}\",\"outcome\":\"accepted\"}}\n{finished_line}")).unwrap(),
        }
        views(&world, &id, 0, true);
        if path.is_dir() {
            fs::remove_dir(&path).unwrap();
        }
        fs::write(&path, finished_line).unwrap();
        reveal(&world, &id, "accepted");
        views(&world, &id, 0, false);
    }
}

#[test]
fn blind_policy_and_original_labels_survive_content_retention() {
    let world = World::new();
    world.configure("review.blind = true\n[kinds.advice]\ndescription = 'Advice.'\nrole = 'advise'\ncandidates = [{harness = 'codex', model = 'gpt-6-astra', effort = 'high'}]");
    let answer = world.run("retained story", &["--kind", "advice"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let id = answer.run_id();
    finished(&world, &id);
    let record = world.record(&id);
    reveal(&world, &id, "accepted");
    reveal(&world, &id, "reworked");
    fs::remove_dir_all(world.run_file(&id, "run.json").parent().unwrap()).unwrap();
    reveal(&world, &id, "discarded");
    for verb in ["status", "result"] {
        assert_eq!(world.ask(&[verb, &id]).code, 50);
    }
    assert!(!world.run_file(&id, "run.json").exists());
    let events = history(&world);
    assert_eq!(events[0]["blind"], true);
    assert_eq!(events[0]["task_kind"], "advice");
    assert_eq!(events[0]["target"], record["target"]);
    assert_eq!(
        events
            .iter()
            .filter(|e| e["kind"] == "outcome")
            .map(|e| e["outcome"].clone())
            .collect::<Vec<_>>(),
        vec![json!("accepted"), json!("reworked"), json!("discarded")]
    );
    let report = world.ask(&["report"]);
    let row = &report.data()["by_role_and_target"]["advise · codex · gpt-6-astra · high"];
    assert_eq!(row["runs"], 1);
    assert_eq!(row["discarded"], 1);
    assert_eq!(row["accepted"], 0);
}

#[test]
fn blind_kind_runs_keep_the_kind_and_its_fence() {
    let world = World::new();
    world.configure("review.blind = true\n[kinds.rust-review]\ndescription = 'Review Rust.'\nrole = 'review'\ncandidates = [{harness = 'codex', model = 'custom', effort = 'medium'}, {harness = 'codex', model = 'custom', effort = 'high'}]");
    let brief = world.brief("review");
    let answer = world.ask(&[
        "run",
        "--kind",
        "rust-review",
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    hidden(answer.data(), "codex");
    let id = answer.run_id();
    finished(&world, &id);
    assert_eq!(answer.data()["kind"], "rust-review");
    assert_eq!(answer.data()["role"], "review");
    let record = world.record(&id);
    assert_eq!(
        record["target"],
        json!({"harness": "codex", "model": "custom", "effort": "medium"})
    );
    let events = history(&world);
    assert_eq!(events[0]["task_kind"], "rust-review");
    assert_eq!(events[0]["target"], record["target"]);
    reveal(&world, &id, "accepted");
    views(&world, &id, 0, false);
    let count = fs::read_dir(world.state.join("runs")).unwrap().count();
    let refused = world.ask(&["run", "--kind", "unknown", "--brief", "missing"]);
    assert_eq!(refused.code, 2);
    assert_eq!(
        refused.message(),
        "unknown task kind \"unknown\" — a person defines kinds in config.toml"
    );
    assert_eq!(
        fs::read_dir(world.state.join("runs")).unwrap().count(),
        count
    );
}

#[test]
fn blind_review_next_uses_the_same_reveal_rule() {
    for outcome_first in [true, false] {
        let world = World::new();
        world.configure(REVIEW);
        let answer = completed(&world, "FAKE: say=review evidence");
        let id = answer.run_id();
        let next = world.ask(&["review", "next", "--caller", "claude"]);
        assert_eq!(next.code, 0);
        hidden(&next.data()["next"], "codex");
        assert_eq!(next.data()["next"]["answer"]["text"], "review evidence");
        assert_eq!(next.data()["next"]["answer"]["untrusted"], true);
        assert_eq!(
            next.data()["next"]["brief"]["text"],
            "FAKE: say=review evidence"
        );
        assert_eq!(next.data()["next"]["brief"]["untrusted"], true);
        assert!(next.data()["rubric"].as_array().unwrap().len() >= 10);
        let wrong = world.ask(&["review", "submit", &id, "--caller", "codex"]);
        assert_eq!(wrong.code, 33);
        assert!(world.ask(&["review", "next", "--caller", "codex"]).data()["next"].is_null());
        if outcome_first {
            reveal(&world, &id, "accepted");
            let next_open = world.ask(&["review", "next", "--caller", "claude"]);
            assert_eq!(next_open.data()["next"]["blind"], false);
            assert_eq!(
                next_open.data()["next"]["target"],
                world.record(&id)["target"]
            );
            assert!(next_open.data()["next"].get("model_reported").is_none());
            assert_eq!(next_open.data()["rubric"], next.data()["rubric"]);
        }
        assert_eq!(
            world
                .ask(&["review", "submit", &id, "--caller", "claude"])
                .code,
            0
        );
        views(&world, &id, 0, !outcome_first);
        assert!(world.ask(&["review", "next", "--caller", "claude"]).data()["next"].is_null());
        assert_eq!(world.at_terminal(&["learn", "reset"]).finish().code, 0);
        views(&world, &id, 0, !outcome_first);
    }
}

#[test]
fn blind_scope_preserves_open_views_and_untrusted_text() {
    let world = World::new();
    world.configure(BLIND);
    let picked = world.ask(&["pick", "--role", "advise", "--caller", "claude"]);
    assert_eq!(picked.code, 0);
    assert!(picked.data()["target"]["model"].is_string());
    assert!(picked.data().get("blind").is_none());
    let answer = completed(&world, "FAKE: say=gpt-6-astra high says hello");
    hidden(answer.data(), "codex");
    assert_eq!(answer.text(), "gpt-6-astra high says hello");
    let report = world.ask(&["report"]);
    let row = &report.data()["by_role_and_target"]["advise · codex · gpt-6-astra · high"];
    assert_eq!(row["outcome_unknown"], 1);
    assert_eq!(row["runs"], 1);
}
