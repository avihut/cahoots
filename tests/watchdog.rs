//! The mid-run watchdog: a run that is already going is stopped when its
//! target crosses `abort_at` — on two FRESH readings in a row, and on nothing
//! less. Stopping work in flight is a higher bar than refusing to start it.

mod common;

use common::World;
use serde_json::json;

const KNOBS: &str = "harness.codex.cap = 80\nharness.codex.abort_at = 90\nlimits.watchdog_secs = 1";

#[test]
fn a_run_is_stopped_when_its_target_crosses_the_abort_threshold() {
    let world = World::new();
    world.meter(
        json!({"guarded": {"code": 0, "percent": 40}, "watch": {"code": 24, "percent": 93}}),
        &format!("{KNOBS}\nreview.blind = true"),
    );
    let answer = world.run("FAKE: sleep=120", &["--wait", "120"]);
    assert_eq!(answer.code, 43, "{}", answer.json);
    assert_eq!(answer.data()["state"], "budget");
    assert_eq!(answer.data()["blind"], true);
    assert_eq!(answer.data()["target"], json!({"harness": "codex"}));
    assert!(answer.data().get("model_reported").is_none());
    let id = answer.run_id();
    common::wait_until("budget run history", || {
        std::fs::read_to_string(world.state.join("history.jsonl"))
            .unwrap_or_default()
            .contains(&id)
    });
    assert_eq!(world.ask(&["outcome", &id, "discarded"]).code, 0);
    let revealed = world.ask(&["result", &id]);
    assert_eq!(revealed.code, 43);
    assert_eq!(revealed.data()["blind"], false);
    assert_eq!(revealed.data()["target"], world.record(&id)["target"]);
    assert!(revealed.data().get("model_reported").is_some());
    assert_eq!(answer.json["retry"], "after_reset");
    assert!(
        answer.message().contains("90%") && answer.message().contains("93% used"),
        "{}",
        answer.message()
    );
    // Stopped, not lost: the session was on disk, so it can be picked up later.
    assert_eq!(answer.data()["resumable"], true);

    let watches: Vec<String> = world
        .meter_calls()
        .into_iter()
        .filter(|c| !c.contains("--forecast"))
        .collect();
    assert!(watches.len() >= 2, "it takes two readings: {watches:?}");
    assert!(
        watches[0].contains("--cap 90") && watches[0].contains("--max-data-age"),
        "{watches:?}"
    );
}

#[test]
fn a_reading_that_is_not_fresh_never_stops_a_run() {
    for code in [21, 13, 26, 99] {
        let world = World::new();
        world.meter(
            json!({"guarded": {"code": 0, "percent": 40}, "watch": {"code": code}}),
            KNOBS,
        );
        let answer = world.run("FAKE: sleep=3\nFAKE: say=finished", &["--wait", "120"]);
        assert_eq!(answer.code, 0, "watch exit {code}: {}", answer.json);
        assert_eq!(answer.text(), "finished");
    }
}

#[test]
fn one_reading_over_is_not_enough() {
    let world = World::new();
    world.meter(
        json!({
            "guarded": {"code": 0, "percent": 40},
            "watch_sequence": [{"code": 24, "percent": 95}, {"code": 0, "percent": 70}],
        }),
        KNOBS,
    );
    // Over, under, over, under, … for five seconds: never twice in a row.
    let answer = world.run("FAKE: sleep=5\nFAKE: say=finished", &["--wait", "120"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let watches = world
        .meter_calls()
        .into_iter()
        .filter(|c| !c.contains("--forecast"))
        .count();
    assert!(
        watches >= 3,
        "the watchdog was not watching ({watches} readings)"
    );
}

#[test]
fn without_a_tracker_there_is_no_watchdog() {
    let world = World::new();
    world.configure("limits.watchdog_secs = 1");
    let answer = world.run("FAKE: sleep=3\nFAKE: say=finished", &["--wait", "120"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(world.meter_calls().is_empty());
}
