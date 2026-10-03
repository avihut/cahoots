//! The gate, against a fake `usage-cli` that speaks the `headroom` contract
//! (exit 0 ok · 21 stale · 24 over cap · 25 forecast · 26 no data · 13 no
//! digest). Anything else is not an answer — and not an answer is a refusal.

mod common;

use common::World;
use serde_json::json;

fn world(plan: serde_json::Value) -> World {
    let world = World::new();
    world.meter(plan, "harness.codex.cap = 80\nharness.claude.cap = 50");
    world
}

#[test]
fn under_the_cap_the_run_is_admitted_and_the_reading_is_kept() {
    let world = world(json!({"guarded": {"code": 0, "percent": 35}}));
    let answer = world.run("hello", &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let record = world.record(&answer.run_id());
    assert_eq!(record["admission"]["reading"]["percent"], 35.0);
    // cap 80, minus the 3-point reserve of a read-only role.
    let calls = world.meter_calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].starts_with("headroom --provider codex --cap 77 --forecast red --json"));
    assert!(calls[0].contains("--max-data-age 15m"));
}

#[test]
fn each_refusal_keeps_the_trackers_own_code() {
    for (code, retry) in [
        (24, "after_reset"),
        (25, "after_reset"),
        (26, "other_target"),
        (13, "other_target"),
    ] {
        let world = world(json!({"guarded": {"code": code, "percent": 91}}));
        let answer = world.run("hello", &[]);
        assert_eq!(answer.code, code, "{}", answer.json);
        assert_eq!(answer.json["retry"], retry);
        assert!(
            !world.state.join("runs").exists(),
            "a refused run left a record"
        );
    }
}

#[test]
fn what_the_gate_does_not_understand_is_a_refusal() {
    // 19: a usage-cli too old to know `headroom`. 1, 99: anything at all.
    for code in [19, 1, 99] {
        let world = world(json!({"guarded": {"code": code}}));
        assert_eq!(world.run("hello", &[]).code, 13);
    }
    // A meter that is configured but not there.
    let world = World::new();
    world.configure(
        "meter.use = \"agent-usage\"\n[meter.agent-usage]\nbinary = \"/nonexistent/usage-cli\"",
    );
    assert_eq!(world.run("hello", &[]).code, 13);
}

#[test]
fn stale_codex_data_is_re_asked_against_a_lower_cap() {
    // Codex's numbers only refresh when Codex runs: refusing on staleness
    // would refuse forever. A stale reading is a lower bound, so it is held
    // to cap − 15 — and the run itself makes the data fresh.
    let world = world(json!({"guarded": {"code": 21}, "unguarded": {"code": 0, "percent": 40}}));
    let answer = world.run("hello", &["--to", "codex"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(
        answer.data()["gate_notes"][0]
            .as_str()
            .unwrap()
            .contains("stale")
    );
    let calls = world.meter_calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].contains("--cap 62") && !calls[1].contains("--max-data-age"));

    let world =
        self::world(json!({"guarded": {"code": 21}, "unguarded": {"code": 24, "percent": 70}}));
    assert_eq!(world.run("hello", &["--to", "codex"]).code, 24);
}

/// A stale Claude reading, with what the tracker's `status` says beside it.
fn stale_claude(status: serde_json::Value) -> World {
    world(json!({
        "guarded": {"code": 21, "data_age": 2460},
        "unguarded": {"code": 0, "percent": 1},
        "status": status,
    }))
}

/// The refusal never says the tracker is down unless the tracker did: it
/// says how old the data is and what a person can look at.
fn refused_as_stale(world: &World) -> String {
    let answer = world.run("hello", &["--caller", "codex", "--to", "claude"]);
    assert_eq!(answer.code, 21, "{}", answer.json);
    let calls = world.meter_calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].contains("--cap 47"));
    assert!(calls[1].starts_with("status --provider claude"));
    let message = answer.json["message"].as_str().unwrap().to_string();
    assert!(
        message.contains("41m old, past the 15m allowed") && message.contains("`usage-cli status`"),
        "{message}"
    );
    assert!(!message.contains("daemon"), "{message}");
    message
}

#[test]
fn a_quiet_claude_the_tracker_still_polls_is_held_to_a_lower_cap() {
    // A quiet Claude is polled as seldom as hourly. Published ten minutes
    // ago with the next poll half an hour away, the tracker is polling: the
    // stale reading is a lower bound, and the run itself makes it poll.
    let world = stale_claude(
        json!({"code": 0, "provider": "claude", "generated": -600, "next_poll": 1800}),
    );
    let answer = world.run("hello", &["--caller", "codex", "--to", "claude"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let note = answer.data()["gate_notes"][0].as_str().unwrap();
    assert!(
        note.contains("still polling") && note.contains("32%"),
        "{note}"
    );
    let calls = world.meter_calls();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(calls[1].starts_with("status --provider claude"));
    assert!(calls[2].contains("--cap 32") && !calls[2].contains("--max-data-age"));
}

#[test]
fn a_tracker_that_stopped_polling_refuses_stale_claude_data() {
    // Published two hours ago, meaning to poll half an hour later.
    let world = stale_claude(
        json!({"code": 0, "provider": "claude", "generated": -7200, "next_poll": -5400}),
    );
    let message = refused_as_stale(&world);
    assert!(
        message.contains("last published 2h 0m ago, its next poll due 1h 30m ago"),
        "{message}"
    );
}

#[test]
fn stale_claude_data_with_no_word_from_the_tracker_is_refused() {
    for status in [
        json!(null),
        json!({"code": 0, "provider": "codex", "generated": -600, "next_poll": 1800}),
        json!({"code": 0, "provider": "claude", "generated": -600}),
    ] {
        let message = refused_as_stale(&stale_claude(status.clone()));
        assert!(
            message.contains("could not say whether it is still polling"),
            "{status} {message}"
        );
    }
}

#[test]
fn pick_falls_through_to_the_next_candidate_and_says_why() {
    let world = World::new();
    // Only codex is over: the fake answers by provider through two plans.
    world.meter(json!({"guarded": {"code": 24, "percent": 95}}), "");
    let nobody = world.ask(&["pick", "--role", "advise"]);
    assert_eq!(nobody.code, 30, "{}", nobody.json);
    assert!(nobody.message().contains("codex") && nobody.message().contains("claude"));

    // One candidate left (claude is the caller): its own refusal comes back.
    let one = world.ask(&["pick", "--role", "advise", "--caller", "claude"]);
    assert_eq!(one.code, 24);

    world.meter(json!({"guarded": {"code": 0, "percent": 10}}), "");
    let picked = world.ask(&["pick", "--role", "review", "--caller", "claude"]);
    assert_eq!(picked.code, 0, "{}", picked.json);
    assert_eq!(picked.data()["target"]["model"], "gpt-5.6-sol");
    assert_eq!(picked.data()["harness_version"], "0.155.1");
    assert!(
        !world.state.join("runs").exists(),
        "pick must not start anything"
    );
}

#[test]
fn doctor_reports_without_changing_anything() {
    let world = World::new();
    let report = world.ask(&["doctor"]);
    let checks = report.data()["checks"].as_array().unwrap();
    let find = |name: &str| {
        checks
            .iter()
            .find(|c| c["check"] == name)
            .unwrap_or_else(|| panic!("no {name} check"))
    };
    assert_eq!(find("config")["status"], "ok");
    assert_eq!(find("codex: binary")["status"], "ok");
    assert_eq!(find("codex: target")["status"], "ok");
    assert_eq!(find("meter")["status"], "warn", "no usage meter is on");
    assert_eq!(find("build")["status"], "warn", "a dev build must say so");

    std::fs::write(
        world.config.join("config.toml"),
        "schema = 1\nungated = true\n",
    )
    .unwrap();
    let broken = world.ask(&["doctor"]);
    assert_eq!(broken.code, 34);
}
