//! `outcome`, the history and `report`: the long memory that review and
//! learning stand on. What a run WAS outlives what it SAID.

mod common;

use std::fs;

use common::World;

fn history(world: &World) -> Vec<serde_json::Value> {
    fs::read_to_string(world.state.join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn every_finished_run_leaves_one_line_whatever_became_of_it() {
    let world = World::new();
    let done = world.run("hello", &[]).run_id();
    let failed = world.run("FAKE: fail", &[]).run_id();
    let timed_out = world
        .run("FAKE: sleep=120", &["--timeout", "1", "--wait", "30"])
        .run_id();

    let lines = history(&world);
    let of = |run: &str| {
        lines
            .iter()
            .find(|l| l["run"] == run)
            .unwrap_or_else(|| panic!("no line for {run}"))
    };
    assert_eq!(
        (of(&done)["state"].as_str(), of(&done)["exit"].as_i64()),
        (Some("done"), Some(0))
    );
    assert_eq!(of(&failed)["exit"], 40);
    assert_eq!(of(&timed_out)["state"], "timed_out");
    assert_eq!(of(&done)["tokens_out"], 7);
    assert_eq!(of(&done)["dir"], world.work.to_str().unwrap());
    // Review is off by default: nothing is ever sampled.
    assert!(lines.iter().all(|l| l["sampled"] == false));
    // A refusal is not a run.
    world.configure("[meter.ledger]\nmax_runs_per_hour = 1");
    assert_eq!(world.run("hello", &[]).code, 24);
    assert_eq!(history(&world).len(), 3);
    // Nothing in the history is content.
    let text = fs::read_to_string(world.state.join("history.jsonl")).unwrap();
    assert!(!text.contains("hello") && !text.contains("pong"));
}

#[test]
fn sampling_follows_the_rate_of_the_moment_and_only_when_review_is_on() {
    let world = World::new();
    world.configure("[review]\nenabled = true\nsample_rate = 1.0");
    world.run("one", &[]);
    world.configure("[review]\nenabled = true\nsample_rate = 0.0");
    world.run("two", &[]);
    let sampled: Vec<bool> = history(&world)
        .iter()
        .map(|l| l["sampled"].as_bool().unwrap())
        .collect();
    assert_eq!(sampled, [true, false], "the bit is fixed when the run ends");
}

#[test]
fn an_outcome_is_recorded_for_a_finished_run_and_the_last_word_wins() {
    let world = World::new();
    let run = world.run("hello", &[]).run_id();
    assert_eq!(world.ask(&["outcome", &run, "discarded"]).code, 0);
    assert_eq!(world.ask(&["outcome", &run, "reworked"]).code, 0);

    let report = world.ask(&["report"]);
    let rows = report.data()["by_role_and_target"].as_object().unwrap();
    let row = &rows["advise · codex · gpt-6-astra · high"];
    assert_eq!(
        (
            row["runs"].as_i64(),
            row["reworked"].as_i64(),
            row["discarded"].as_i64()
        ),
        (Some(1), Some(1), Some(0))
    );

    // Closed vocabulary; real, finished runs only.
    assert_eq!(world.ask(&["outcome", &run, "meh"]).code, 2);
    assert_eq!(
        world
            .ask(&[
                "outcome",
                "0198c0de-0000-7000-8000-000000000000",
                "accepted"
            ])
            .code,
        50
    );
    let running = world.run("FAKE: sleep=120", &["--wait", "0"]).run_id();
    assert_eq!(world.ask(&["outcome", &running, "accepted"]).code, 51);
    assert_eq!(world.ask(&["cancel", &running]).code, 42);
}

#[test]
fn the_history_outlives_the_runs_content() {
    let world = World::new();
    let run = world.run("hello", &[]).run_id();
    fs::remove_dir_all(world.state.join("runs").join(&run)).unwrap(); // aged out
    assert_eq!(world.ask(&["result", &run]).code, 50, "the content is gone");
    assert_eq!(
        world.ask(&["outcome", &run, "accepted"]).code,
        0,
        "what it was is not"
    );
    assert_eq!(world.ask(&["report"]).data()["runs"], 1);
}

#[test]
fn report_says_unknown_rather_than_guessing() {
    let world = World::new();
    world.run("one", &[]);
    world.run("FAKE: fail", &[]);
    let report = world.ask(&["report", "--days", "1"]);
    let row = &report.data()["by_role_and_target"]["advise · codex · gpt-6-astra · high"];
    assert_eq!(row["runs"], 2);
    assert_eq!(
        row["outcome_unknown"], 1,
        "a finished run nobody rated is unknown, not accepted"
    );
    assert_eq!(row["failed"], 1);
    assert_eq!(row["accepted"], 0);
}
