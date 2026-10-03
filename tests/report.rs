//! `report`'s groups by kind, and the evidence under each row. Run history
//! is written as fixtures in a throwaway world, so the exact sample is known;
//! one test gets its history from real runs of the fake harness.
mod common;

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use common::{Answer, World};
use serde_json::{Value, json};

const DAY: u64 = 24 * 3600;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

const CUSTOM: &str = r#"{ harness = "codex", model = "custom", effort = "medium" }"#;

fn kind(name: &str, role: &str, candidates: &[&str]) -> String {
    format!(
        "[kinds.{name}]\ndescription = \"Review Rust.\"\nrole = {role:?}\ncandidates = [{}]\n",
        candidates.join(", ")
    )
}

/// A history file in the making: one `finished` line, and any outcome, per run.
#[derive(Default)]
struct History(String);

struct Ran<'a> {
    kind: Option<&'a str>,
    role: &'a str,
    target: (&'a str, &'a str, &'a str),
    state: &'a str,
    outcome: Option<&'a str>,
    t: u64,
}

impl<'a> Ran<'a> {
    fn new(kind: Option<&'a str>) -> Ran<'a> {
        Ran {
            kind,
            role: "review",
            target: ("codex", "custom", "medium"),
            state: "done",
            outcome: None,
            t: now(),
        }
    }
    fn state(mut self, state: &'a str) -> Self {
        self.state = state;
        self
    }
    fn outcome(mut self, outcome: &'a str) -> Self {
        self.outcome = Some(outcome);
        self
    }
    fn role(mut self, role: &'a str) -> Self {
        self.role = role;
        self
    }
    fn effort(mut self, effort: &'a str) -> Self {
        self.target.2 = effort;
        self
    }
    fn model(mut self, model: &'a str) -> Self {
        self.target.1 = model;
        self
    }
    fn target(mut self, harness: &'a str, model: &'a str, effort: &'a str) -> Self {
        self.target = (harness, model, effort);
        self
    }
    fn at(mut self, t: u64) -> Self {
        self.t = t;
        self
    }
}

impl History {
    fn add(&mut self, ran: Ran) -> &mut Self {
        let run = format!("0198c0de-0000-7000-8000-{:012}", self.0.lines().count());
        let (harness, model, effort) = ran.target;
        let task_kind = match ran.kind {
            Some(name) => format!("\"task_kind\":\"{name}\","),
            None => String::new(),
        };
        self.0.push_str(&format!(
            "{{\"kind\":\"finished\",\"t\":{t},\"run\":\"{run}\",\"role\":\"{role}\",{task_kind}\
             \"caller\":null,\"target\":{{\"harness\":\"{harness}\",\"model\":\"{model}\",\
             \"effort\":\"{effort}\"}},\"dir\":\"/w\",\"state\":\"{state}\",\"exit\":0,\
             \"tokens_in\":10,\"tokens_out\":5,\"secs\":4,\"sampled\":false}}\n",
            t = ran.t,
            role = ran.role,
            state = ran.state,
        ));
        if let Some(outcome) = ran.outcome {
            self.outcome(&run, ran.t, outcome);
        }
        self
    }
    fn outcome(&mut self, run: &str, t: u64, outcome: &str) {
        self.0.push_str(&format!(
            "{{\"kind\":\"outcome\",\"t\":{t},\"run\":\"{run}\",\"outcome\":\"{outcome}\"}}\n"
        ));
    }
    /// `n` runs in a row of the same shape.
    fn times(&mut self, n: usize, make: impl Fn() -> Ran<'static>) -> &mut Self {
        for _ in 0..n {
            self.add(make());
        }
        self
    }
    fn write(&self, world: &World) {
        fs::create_dir_all(&world.state).unwrap();
        fs::write(world.state.join("history.jsonl"), &self.0).unwrap();
    }
}

fn report(world: &World, extra: &[&str]) -> Answer {
    let mut args = vec!["report"];
    args.extend(extra);
    let answer = world.ask(&args);
    assert_eq!(answer.code, 0, "{}", answer.json);
    answer
}

const KEY: &str = "rust-review · review · codex · custom · medium";

fn close(value: &Value, expected: f64) {
    let got = value
        .as_f64()
        .unwrap_or_else(|| panic!("not a number: {value}"));
    assert!((got - expected).abs() < 1e-9, "{got} != {expected}");
}

#[test]
fn report_groups_kinds_without_changing_role_totals() {
    let world = World::new();
    world.configure(&format!(
        "[roles.review]\ncandidates = [{CUSTOM}]\n{}{}",
        kind("rust-review", "review", &[CUSTOM]),
        kind("docs-review", "review", &[CUSTOM]),
    ));
    let brief = world.brief("hello");
    let brief = brief.to_str().unwrap();
    for args in [
        vec!["--kind", "rust-review"],
        vec!["--kind", "rust-review"],
        vec!["--kind", "docs-review"],
        vec!["--role", "review"],
    ] {
        let mut run = vec!["run", "--caller", "claude", "--brief", brief];
        run.extend(args);
        let answer = world.ask(&run);
        assert_eq!(answer.code, 0, "{}", answer.json);
    }
    let data = report(&world, &[]).data().clone();
    assert_eq!(data["runs"], 4);
    assert_eq!(data["sample_floor"], 8);
    let roles = data["by_role_and_target"].as_object().unwrap();
    assert_eq!(roles.len(), 1);
    assert_eq!(roles["review · codex · custom · medium"]["runs"], 4);
    let kinds = data["by_kind_and_target"].as_object().unwrap();
    assert_eq!(
        kinds.keys().collect::<Vec<_>>(),
        [
            "docs-review · review · codex · custom · medium",
            "rust-review · review · codex · custom · medium"
        ]
    );
    assert_eq!(kinds[KEY]["runs"], 2);
    assert_eq!(
        kinds["docs-review · review · codex · custom · medium"]["runs"],
        1
    );
    // The unlabelled run is in the role totals only.
    assert!(
        !kinds
            .keys()
            .any(|key| key.starts_with(" ·") || key.starts_with("review ·"))
    );
}

#[test]
fn report_seeds_every_current_kind_candidate() {
    let world = World::new();
    world.enable(&["claude"]);
    world.configure(&format!(
        "{}{}",
        kind(
            "rust-review",
            "review",
            &[
                CUSTOM,
                r#"{ harness = "codex", model = "custom", effort = "high" }"#,
                r#"{ harness = "claude", model = "never", effort = "low" }"#,
            ]
        ),
        kind("explore-docs", "explore", &[CUSTOM]),
    ));
    let data = report(&world, &[]).data().clone();
    assert_eq!(data["runs"], 0);
    assert!(data["by_role_and_target"].as_object().unwrap().is_empty());
    let kinds = data["by_kind_and_target"].as_object().unwrap();
    assert_eq!(
        kinds.keys().collect::<Vec<_>>(),
        [
            "explore-docs · explore · codex · custom · medium",
            "rust-review · review · claude · never · low",
            "rust-review · review · codex · custom · high",
            "rust-review · review · codex · custom · medium",
        ]
    );
    for row in kinds.values() {
        for field in ["runs", "done", "failed", "tokens_in", "median_secs"] {
            assert_eq!(row[field], 0);
        }
        let evidence = &row["evidence"];
        assert_eq!(evidence["n"], 0);
        assert_eq!(evidence["enough_evidence"], false);
        assert!(evidence["score"].is_null());
        assert!(evidence["score_standard_error"].is_null());
        for name in ["accepted", "reworked", "discarded", "failed"] {
            assert!(evidence["rates"][name]["value"].is_null());
            assert!(evidence["rates"][name]["standard_error"].is_null());
        }
    }
    // No kinds and no history: nothing seeded, and the shape is exactly this.
    let bare = report(&World::new(), &[]);
    assert_eq!(
        bare.json,
        json!({"v":1,"code":0,"class":"ok","retry":"never","data":{
            "days":30,"runs":0,"sample_floor":8,
            "by_role_and_target":{},"by_kind_and_target":{}
        }})
    );
}

#[test]
fn report_keeps_removed_candidates_and_recorded_roles() {
    let world = World::new();
    let mut history = History::default();
    history
        .add(Ran::new(Some("rust-review")))
        .add(Ran::new(Some("rust-review")).role("advise"))
        .add(Ran::new(Some("gone-kind")).model("old"))
        .add(Ran::new(Some("rust-review")).at(now() - 40 * DAY));
    history.write(&world);
    // The kind now has another role and another candidate list.
    world.configure(&kind(
        "rust-review",
        "explore",
        &[r#"{ harness = "codex", model = "fresh", effort = "low" }"#],
    ));
    let data = report(&world, &[]).data().clone();
    let kinds = data["by_kind_and_target"].as_object().unwrap();
    assert_eq!(
        kinds.keys().collect::<Vec<_>>(),
        [
            "gone-kind · review · codex · old · medium",
            "rust-review · advise · codex · custom · medium",
            "rust-review · explore · codex · fresh · low",
            "rust-review · review · codex · custom · medium",
        ]
    );
    // Old role, old candidate: separate from the seeded row; the 40-day-old
    // run is outside the window; the seeded row has nothing.
    assert_eq!(
        kinds["rust-review · review · codex · custom · medium"]["runs"],
        1
    );
    assert_eq!(
        kinds["rust-review · explore · codex · fresh · low"]["runs"],
        0
    );
    assert_eq!(data["runs"], 3);
    // Wide enough, the old run is back.
    let wide = report(&world, &["--days", "60"]).data().clone();
    assert_eq!(
        wide["by_kind_and_target"]["rust-review · review · codex · custom · medium"]["runs"],
        2
    );
}

#[test]
fn report_rates_use_rated_or_failed_evidence() {
    let world = World::new();
    let mut history = History::default();
    for ran in [
        Ran::new(Some("rust-review")).outcome("accepted"),
        Ran::new(Some("rust-review")).outcome("reworked"),
        Ran::new(Some("rust-review")).outcome("discarded"),
        // A rated failure is its explicit outcome, once.
        Ran::new(Some("rust-review"))
            .state("failed")
            .outcome("accepted"),
        // Unrated: failures count against the candidate…
        Ran::new(Some("rust-review")).state("failed"),
        Ran::new(Some("rust-review")).state("crashed"),
        Ran::new(Some("rust-review")).state("timed_out"),
        // …and nothing else is a grade.
        Ran::new(Some("rust-review")),
        Ran::new(Some("rust-review")).state("cancelled"),
        Ran::new(Some("rust-review")).state("budget"),
    ] {
        history.add(ran);
    }
    history.write(&world);
    let data = report(&world, &[]).data().clone();
    let row = &data["by_kind_and_target"][KEY];
    // The ordinary counters keep their definitions…
    assert_eq!(row["runs"], 10);
    assert_eq!(row["done"], 4);
    assert_eq!(row["failed"], 3);
    assert_eq!(row["timed_out"], 1);
    assert_eq!(row["cancelled"], 1);
    assert_eq!(row["stopped_by_budget"], 1);
    assert_eq!(row["outcome_unknown"], 1);
    // …and the evidence has its own denominator.
    let evidence = &row["evidence"];
    assert_eq!(evidence["n"], 7);
    assert_eq!(evidence["accepted"], 2);
    assert_eq!(evidence["reworked"], 1);
    assert_eq!(evidence["discarded"], 1);
    assert_eq!(evidence["failed"], 3);
    assert_eq!(evidence["enough_evidence"], false);
    // The role totals count the same runs, kind or not.
    assert_eq!(
        data["by_role_and_target"]["review · codex · custom · medium"]["evidence"]["n"],
        7
    );
}

#[test]
fn report_errors_match_known_samples() {
    let world = World::new();
    let mut history = History::default();
    // Mixed: 3 accepted, 2 reworked, 1 discarded, 2 failed — n = 8.
    for ran in [
        Ran::new(Some("rust-review")).outcome("accepted"),
        Ran::new(Some("rust-review")).outcome("accepted"),
        Ran::new(Some("rust-review")).outcome("accepted"),
        Ran::new(Some("rust-review")).outcome("reworked"),
        Ran::new(Some("rust-review")).outcome("reworked"),
        Ran::new(Some("rust-review")).outcome("discarded"),
        Ran::new(Some("rust-review")).state("failed"),
        Ran::new(Some("rust-review")).state("timed_out"),
    ] {
        history.add(ran);
    }
    // One observation; all alike; two half-credits.
    history.add(Ran::new(Some("one")).outcome("accepted"));
    history.times(5, || Ran::new(Some("alike")).outcome("discarded"));
    history.times(2, || Ran::new(Some("halves")).outcome("reworked"));
    history.write(&world);
    let data = report(&world, &[]).data().clone();
    let rows = &data["by_kind_and_target"];
    let at = |name: &str| &rows[format!("{name} · review · codex · custom · medium")]["evidence"];

    let mixed = at("rust-review");
    assert_eq!(mixed["n"], 8);
    assert_eq!(mixed["enough_evidence"], true);
    let (n, a, r, d, f) = (8.0_f64, 3.0_f64, 2.0_f64, 1.0_f64, 2.0_f64);
    for (name, k) in [
        ("accepted", a),
        ("reworked", r),
        ("discarded", d),
        ("failed", f),
    ] {
        let p = k / n;
        close(&mixed["rates"][name]["value"], p);
        close(
            &mixed["rates"][name]["standard_error"],
            (p * (1.0 - p) / n).sqrt(),
        );
    }
    // Score: observations 1,1,1,½,½,0,0,0 — mean 0.5, sum of squares 3.5, so
    // the sample variance is (3.5 − 8 × 0.25) / 7 and the error √(var / 8).
    close(&mixed["score"], 0.5);
    close(
        &mixed["score_standard_error"],
        ((3.5 - 2.0) / 7.0 / 8.0_f64).sqrt(),
    );

    // One observation: the estimate is there, its error is zero, the score's is not.
    let one = at("one");
    close(&one["rates"]["accepted"]["value"], 1.0);
    close(&one["rates"]["accepted"]["standard_error"], 0.0);
    close(&one["score"], 1.0);
    assert!(one["score_standard_error"].is_null());
    // All alike: no spread.
    close(&at("alike")["rates"]["discarded"]["standard_error"], 0.0);
    close(&at("alike")["score_standard_error"], 0.0);
    // Two half-credits: no spread in the score, which the Bernoulli formula
    // would put at √(0.25 / 2) — and the share of reworked is certain.
    let halves = at("halves");
    close(&halves["score"], 0.5);
    close(&halves["score_standard_error"], 0.0);
}

#[test]
fn report_floor_changes_at_eight_observations() {
    let world = World::new();
    let mut history = History::default();
    history.times(7, || Ran::new(Some("seven")).outcome("accepted"));
    history.times(8, || Ran::new(Some("eight")).outcome("accepted"));
    // Another effort of the same model is another candidate, never pooled.
    history.add(Ran::new(Some("seven")).effort("high").outcome("accepted"));
    history.write(&world);
    world.configure(&kind("empty", "review", &[CUSTOM]));
    let data = report(&world, &[]).data().clone();
    let at = |name: &str| {
        &data["by_kind_and_target"][format!("{name} · review · codex · custom · medium")]["evidence"]
    };
    assert_eq!(at("seven")["n"], 7);
    assert_eq!(
        data["by_kind_and_target"]["seven · review · codex · custom · high"]["evidence"]["n"],
        1
    );
    assert_eq!(at("seven")["enough_evidence"], false);
    // Below the floor the estimates are still in the JSON.
    close(&at("seven")["rates"]["accepted"]["value"], 1.0);
    close(&at("seven")["score"], 1.0);
    assert_eq!(at("eight")["enough_evidence"], true);
    assert_eq!(at("empty")["enough_evidence"], false);
    assert!(at("empty")["score"].is_null());
}

#[test]
fn report_window_uses_finished_time() {
    let world = World::new();
    let mut history = History::default();
    let old = now() - 40 * DAY;
    history.add(Ran::new(Some("k")).at(old));
    // A late label on the old run does not bring it into the window.
    history.outcome("0198c0de-0000-7000-8000-000000000000", now(), "accepted");
    history.add(Ran::new(Some("k")));
    history.write(&world);
    let data = report(&world, &[]).data().clone();
    assert_eq!(data["runs"], 1);
    assert_eq!(
        data["by_kind_and_target"]["k · review · codex · custom · medium"]["evidence"]["n"],
        0
    );
    assert_eq!(report(&world, &["--days", "60"]).data()["runs"], 2);
    // The exact cutoff is pinned where time can be given: `report::summarize`'s tests.
}

#[test]
fn report_uses_folded_outcomes_and_surviving_history() {
    let world = World::new();
    let mut history = History::default();
    history.add(Ran::new(Some("k")).outcome("discarded"));
    // The last word wins.
    history.outcome("0198c0de-0000-7000-8000-000000000000", now(), "accepted");
    // An outcome for a run nobody recorded is not a run.
    history.outcome("0198c0de-0000-7000-8000-ffffffffffff", now(), "accepted");
    history
        .0
        .push_str(&format!("{{\"kind\":\"forget\",\"t\":{}}}\n", now()));
    history.write(&world);
    assert!(
        !world.state.join("runs").exists(),
        "no run content is kept at all"
    );
    let data = report(&world, &[]).data().clone();
    assert_eq!(data["runs"], 1);
    // A real run whose content then goes: the history is what report reads.
    let real = World::new();
    let ran = real.run("hello", &[]);
    assert_eq!(ran.code, 0, "{}", ran.json);
    fs::remove_dir_all(real.state.join("runs").join(ran.run_id())).unwrap();
    let after = report(&real, &[]).data().clone();
    assert_eq!(after["runs"], 1);
    assert_eq!(
        after["by_role_and_target"]["advise · codex · gpt-6-astra · high"]["runs"],
        1
    );
    let evidence = &data["by_kind_and_target"]["k · review · codex · custom · medium"]["evidence"];
    assert_eq!(
        (evidence["n"].clone(), evidence["accepted"].clone()),
        (json!(1), json!(1))
    );
    assert_eq!(evidence["discarded"], 0);
}

#[test]
fn report_accepts_legacy_and_capture_history() {
    let world = World::new();
    let now = now();
    let line = |run: &str, extra: &str| {
        format!(
            "{{\"kind\":\"finished\",\"t\":{now},\"run\":\"{run}\",\"role\":\"review\",{extra}\
             \"caller\":null,\"target\":{{\"harness\":\"codex\",\"model\":\"custom\",\"effort\":\"medium\"}},\
             \"dir\":\"/w\",\"state\":\"done\",\"exit\":0,\"tokens_in\":1,\"tokens_out\":1,\
             \"secs\":1,\"sampled\":false}}\n"
        )
    };
    let mut text = String::new();
    // Before kinds, before capture; blind; null capture; a populated one.
    text.push_str(&line("a", ""));
    text.push_str(&line("b", "\"blind\":true,"));
    text.push_str(&line(
        "c",
        "\"task_kind\":\"k\",\"base_commit\":null,\"patch\":null,",
    ));
    text.push_str("this is not json\n{\"kind\":\"from-the-future\"}\n");
    let populated = line("d", "\"task_kind\":\"k\",");
    text.push_str(&populated.replace(
        "\"sampled\":false}",
        "\"sampled\":false,\"base_commit\":\"0123456789abcdef0123456789abcdef01234567\",\
         \"patch\":{\"files\":[{\"path\":\"src/a.rs\",\"added\":1,\"removed\":0,\
         \"hunks\":[[\"00ff\",1]]}],\"added\":1,\"removed\":0}}",
    ));
    fs::create_dir_all(&world.state).unwrap();
    fs::write(world.state.join("history.jsonl"), text).unwrap();
    let data = report(&world, &[]).data().clone();
    // Four runs; the two junk lines are skipped; the capture fields are ignored.
    assert_eq!(data["runs"], 4, "{data}");
    assert_eq!(
        data["by_role_and_target"]["review · codex · custom · medium"]["runs"],
        4
    );
    // The event's own `kind` tag is not the task kind; and no unnamed kind.
    let kinds = data["by_kind_and_target"].as_object().unwrap();
    assert_eq!(
        kinds.keys().collect::<Vec<_>>(),
        [KEY.replace("rust-review", "k").as_str()]
    );
    assert_eq!(kinds["k · review · codex · custom · medium"]["runs"], 2);
}

#[test]
fn report_configuration_failure_is_exit_34() {
    let world = World::bare();
    // No config at all: the defaults.
    let missing = world.ask(&["report"]);
    assert_eq!(missing.code, 0, "{}", missing.json);
    fs::write(world.config.join("config.toml"), "schema = 1\n[kinds.x\n").unwrap();
    let broken = world.ask(&["report"]);
    assert_eq!(broken.code, 34, "{}", broken.json);
    assert_eq!(broken.json["v"], 1);
    assert_eq!(broken.json["class"], "config_error");
    assert_eq!(broken.json["retry"], "fix_config");
    assert!(
        broken.json["message"]
            .as_str()
            .unwrap()
            .contains("config.toml")
    );
    assert!(broken.json.get("data").is_none(), "{}", broken.json);
    // Arguments are still clap's.
    let usage = world.ask(&["report", "--days", "abc"]);
    assert_eq!(usage.code, 2, "{}", usage.json);
    assert_eq!(usage.json["class"], "usage_error");
}

#[test]
fn report_suggest_adds_uncertainty_without_changing_swaps() {
    let world = World::new();
    world.configure("[review]\nenabled = true\n");
    // Role-only advise runs ten days ago: inside suggestion's 90 days, outside
    // the report's one day. Kind runs of the same candidate must change nothing.
    let then = now() - 10 * DAY;
    let mut history = History::default();
    history.times(8, || {
        Ran::new(None)
            .role("advise")
            .target("codex", "gpt-6-astra", "high")
            .outcome("discarded")
            .at(then)
    });
    history.times(8, || {
        Ran::new(None)
            .role("advise")
            .target("claude", "opus", "high")
            .outcome("accepted")
            .at(then)
    });
    history.times(8, || {
        Ran::new(Some("k"))
            .role("advise")
            .target("codex", "gpt-6-astra", "high")
            .outcome("accepted")
            .at(then)
    });
    history.write(&world);
    let wide = report(&world, &["--suggest"]).data().clone();
    let narrow = report(&world, &["--suggest", "--days", "1"]).data().clone();
    // The window moves the totals and not the suggestions.
    assert_eq!(wide["runs"], 24);
    assert_eq!(narrow["runs"], 0);
    assert_eq!(wide["routing"], narrow["routing"]);
    let advise = &narrow["routing"]["roles"]["advise"];
    assert_eq!(advise["suggested_swap"]["move_up"]["model"], "opus");
    assert_eq!(advise["suggested_swap"]["in_effect"], false);
    let first = &advise["order"][0]["evidence"];
    assert_eq!(
        (first["n"].clone(), first["discarded"].clone()),
        (json!(8), json!(8))
    );
    assert_eq!(first["enough_evidence"], true);
    close(&first["score"], 0.0);
    close(&first["score_standard_error"], 0.0);
    close(&first["rates"]["discarded"]["value"], 1.0);
    let second = &advise["order"][1]["evidence"];
    close(&second["score"], 1.0);
    assert_eq!(second["accepted"], 8);
}
