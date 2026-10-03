//! Exploration, end to end: a configured share of new runs tries the next
//! listed candidate first, and every candidate still passes the usual checks.
//! Fake harnesses and throwaway directories only. Routing is forced with the
//! endpoint shares (0 and 1); a fractional share is checked against the draw
//! computed independently from each run's own recorded id — never by retrying
//! until a wanted draw appears.

mod common;

use std::fs;

use common::{Answer, World, wait_until};
use serde_json::{Value, json};

/// A caller's entries around two usable targets: with `--caller claude` the
/// list is `[codex m1, codex m2]`, with `--caller codex` it is `[claude c1,
/// claude c2]`.
const LIST: &[(&str, &str, &str)] = &[
    ("claude", "c1", "high"),
    ("codex", "m1", "high"),
    ("claude", "c2", "high"),
    ("codex", "m2", "high"),
];

fn entries(list: &[(&str, &str, &str)]) -> String {
    let each: Vec<String> = list
        .iter()
        .map(|(harness, model, effort)| {
            format!("  {{ harness = {harness:?}, model = {model:?}, effort = {effort:?} }},\n")
        })
        .collect();
    format!("[\n{}]", each.concat())
}

/// `[roles.<role>]` with these candidates.
fn role(role: &str, list: &[(&str, &str, &str)]) -> String {
    format!("[roles.{role}]\ncandidates = {}\n", entries(list))
}

/// `[explore.share]` with these roles' shares.
fn shares(shares: &[(&str, &str)]) -> String {
    let each: Vec<String> = shares
        .iter()
        .map(|(role, share)| format!("{role} = {share}\n"))
        .collect();
    format!("[explore.share]\n{}", each.concat())
}

fn kind(name: &str, role: &str, list: &[(&str, &str, &str)], share: Option<&str>) -> String {
    let own = share
        .map(|share| format!("[kinds.{name}.explore]\nshare = {share}\n"))
        .unwrap_or_default();
    format!(
        "[kinds.{name}]\ndescription = \"Review Rust.\"\nrole = {role:?}\ncandidates = {}\n{own}",
        entries(list)
    )
}

fn world_with(config: &str) -> World {
    let world = World::new();
    world.configure(config);
    world
}

fn history(world: &World) -> Vec<Value> {
    fs::read_to_string(world.state.join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn finished(world: &World, id: &str) -> Value {
    let mut found = None;
    wait_until("the finished history event", || {
        found = history(world)
            .into_iter()
            .find(|event| event["kind"] == "finished" && event["run"] == id);
        found.is_some()
    });
    found.unwrap()
}

/// A completed run that took `model`, labelled `exploration` in the record,
/// the finished event and (not blind) the envelope.
fn ran(world: &World, answer: &Answer, model: &str, exploration: bool) {
    assert_eq!(answer.code, 0, "{}", answer.json);
    let id = answer.run_id();
    let record = world.record(&id);
    assert_eq!(record["target"]["model"], model, "{}", answer.json);
    assert_eq!(record["exploration"], exploration, "{record}");
    assert_eq!(answer.data()["target"]["model"], model);
    assert_eq!(answer.data()["exploration"], exploration, "{}", answer.json);
    assert_eq!(
        finished(world, &id)["exploration"],
        exploration,
        "the finished event"
    );
}

fn run_kind(world: &World, name: &str, extra: &[&str]) -> Answer {
    let brief = world.brief("hello");
    let mut args = vec!["run", "--kind", name, "--brief", brief.to_str().unwrap()];
    args.extend(extra);
    world.ask(&args)
}

fn run_role(world: &World, role: &str, extra: &[&str]) -> Answer {
    let brief = world.brief("hello");
    let mut args = vec!["run", "--role", role, "--brief", brief.to_str().unwrap()];
    args.extend(extra);
    world.ask(&args)
}

fn pick_role(world: &World, role: &str, extra: &[&str]) -> Answer {
    let mut args = vec!["pick", "--role", role];
    args.extend(extra);
    world.ask(&args)
}

fn no_run_was_made(world: &World) {
    assert!(!world.state.join("runs").exists(), "a refusal left a run");
    assert!(history(world).is_empty(), "a refusal left a history event");
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

/// A run that holds its target's slot until it is cancelled.
fn hold(world: &World, extra: &[&str]) -> String {
    let answer = world.run("FAKE: sleep=120", &[&["--wait", "0"], extra].concat());
    assert_eq!(answer.code, 51, "{}", answer.json);
    let id = answer.run_id();
    wait_until("it is running", || world.record(&id)["state"] == "running");
    id
}

#[test]
fn zero_share_preserves_the_first_candidate() {
    for config in [
        String::new(),
        shares(&[("advise", "0.0")]),
        shares(&[("advise", "0")]),
        shares(&[("review", "1.0")]),
    ] {
        let world = world_with(&format!("{}{config}", role("advise", LIST)));
        let answer = world.run("hello", &[]);
        ran(&world, &answer, "m1", false);
    }
}

#[test]
fn full_share_tries_the_next_candidate_after_caller_filtering() {
    let world = world_with(&format!(
        "{}{}",
        role("advise", LIST),
        shares(&[("advise", "1.0")])
    ));
    let answer = world.run("hello", &[]);
    ran(&world, &answer, "m2", true);
    // The caller's own entries were taken out first: nothing ran on claude.
    assert_eq!(world.record(&answer.run_id())["target"]["harness"], "codex");
    // The other direction: the second of the remaining entries, as well.
    let other = world.run("hello", &["--caller", "codex"]);
    ran(&world, &other, "c2", true);
    assert_eq!(world.record(&other.run_id())["target"]["harness"], "claude");
}

#[test]
fn different_efforts_of_one_model_can_earn_exploration_evidence() {
    let list = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m1", "medium"),
    ];
    let world = world_with(&format!(
        "{}{}",
        role("advise", &list),
        shares(&[("advise", "1.0")])
    ));
    let answer = world.run("hello", &[]);
    ran(&world, &answer, "m1", true);
    let record = world.record(&answer.run_id());
    assert_eq!(record["target"]["effort"], "medium");
    assert_eq!(answer.data()["target"]["effort"], "medium");
}

#[test]
fn kind_share_overrides_its_role_and_reset_restores_inheritance() {
    let list = [("codex", "m1", "high"), ("codex", "m2", "high")];
    let file = |world: &World| fs::read_to_string(world.config.join("config.toml")).unwrap();
    // Role share 1, no override: the kind inherits it.
    let world = world_with(&format!(
        "{}{}",
        shares(&[("review", "1.0")]),
        kind("rust-review", "review", &list, None)
    ));
    ran(&world, &run_kind(&world, "rust-review", &[]), "m2", true);
    // An explicit zero overrides a nonzero role share.
    let world = world_with(&format!(
        "{}{}",
        shares(&[("review", "1.0")]),
        kind("rust-review", "review", &list, Some("0.0"))
    ));
    ran(&world, &run_kind(&world, "rust-review", &[]), "m1", false);
    // The same kind, reset: it inherits again. Its other fields stay.
    let before = file(&world);
    let reset = world
        .at_terminal(&["settings", "reset", "kinds.rust-review.explore.share"])
        .finish();
    assert_eq!(reset.code, 0, "{}", reset.json);
    let after = file(&world);
    assert_ne!(before, after);
    assert!(!after.contains("explore]"), "{after}");
    for kept in [
        "description = \"Review Rust.\"",
        "role = \"review\"",
        "model = \"m2\"",
    ] {
        assert!(after.contains(kept), "{after}");
    }
    ran(&world, &run_kind(&world, "rust-review", &[]), "m2", true);
    // One over a role at zero.
    let world = world_with(&kind("rust-review", "review", &list, Some("1.0")));
    ran(&world, &run_kind(&world, "rust-review", &[]), "m2", true);
    // Never the role's list: its own list exhausted is its own refusal, even
    // with a share of 1 on a role that has a good candidate left.
    let world = world_with(&format!(
        "{}{}",
        shares(&[("review", "1.0")]),
        kind("rust-review", "review", &list, Some("1.0"))
    ));
    let none = run_kind(&world, "rust-review", &["--caller", "codex"]);
    assert_eq!(none.code, 30, "{}", none.json);
    assert!(
        none.message()
            .starts_with("no enabled candidate for task kind \"rust-review\""),
        "{}",
        none.message()
    );
    no_run_was_made(&world);
}

#[test]
fn to_suppresses_exploration_even_with_several_candidates() {
    let list = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m2", "high"),
    ];
    let world = world_with(&format!(
        "{}{}",
        role("advise", &list),
        shares(&[("advise", "1.0")])
    ));
    // Share 1 and `--to`: the ordinary order within that harness.
    let answer = world.run("hello", &["--to", "codex"]);
    ran(&world, &answer, "m1", false);
    // Without it, the same share explores.
    ran(&world, &world.run("hello", &[]), "m2", true);
    // The existing refusals around `--to` are what they were.
    let itself = world.run("hello", &["--to", "claude"]);
    assert_eq!(itself.code, 33, "{}", itself.json);
    world.enable(&["claude"]);
    let off = world.run("hello", &["--to", "codex"]);
    assert_eq!(off.code, 31, "{}", off.json);
    // An enabled harness the kind does not list: no candidate.
    world.enable(&["claude", "codex"]);
    let only_codex = [("codex", "m1", "high"), ("codex", "m2", "high")];
    world.configure(&format!(
        "{}{}",
        shares(&[("review", "1.0")]),
        kind("rust-review", "review", &only_codex, Some("1.0"))
    ));
    let absent = run_kind(&world, "rust-review", &["--to", "claude"]);
    assert_eq!(absent.code, 30, "{}", absent.json);
    let ordinary = run_kind(&world, "rust-review", &["--to", "codex"]);
    ran(&world, &ordinary, "m1", false);
}

#[test]
fn one_remaining_candidate_does_not_explore() {
    let list = [("claude", "c1", "high"), ("codex", "m1", "high")];
    let world = world_with(&format!(
        "{}{}",
        role("advise", &list),
        shares(&[("advise", "1.0")])
    ));
    ran(&world, &world.run("hello", &[]), "m1", false);
    // Filtered to none: refused before anything is created.
    let world = world_with(&format!(
        "{}{}",
        role(
            "advise",
            &[("codex", "m1", "high"), ("codex", "m2", "high")]
        ),
        shares(&[("advise", "1.0")])
    ));
    let none = world.run("hello", &["--caller", "codex"]);
    assert_eq!(none.code, 30, "{}", none.json);
    assert!(none.json.get("data").is_none());
    no_run_was_made(&world);
}

#[test]
fn an_identical_next_entry_does_not_explore() {
    let repeated = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m2", "high"),
    ];
    let world = world_with(&format!(
        "{}{}",
        role("advise", &repeated),
        shares(&[("advise", "1.0")])
    ));
    // The first two remaining entries are the same candidate: no swap, and no
    // search past it for a different one.
    ran(&world, &world.run("hello", &[]), "m1", false);
    let preview = pick_role(&world, "advise", &["--caller", "claude"]);
    assert_eq!(preview.data()["exploration_share"], 0.0);
    // A distinct effort in the same place enables it.
    let distinct = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m1", "low"),
        ("codex", "m2", "high"),
    ];
    world.configure(&format!(
        "{}{}",
        role("advise", &distinct),
        shares(&[("advise", "1.0")])
    ));
    let answer = world.run("hello", &[]);
    ran(&world, &answer, "m1", true);
    assert_eq!(world.record(&answer.run_id())["target"]["effort"], "low");
}

#[test]
fn the_draw_matches_the_recorded_id() {
    let world = world_with(&format!(
        "{}{}",
        role(
            "advise",
            &[
                ("claude", "c1", "high"),
                ("codex", "m1", "high"),
                ("codex", "m2", "high")
            ]
        ),
        shares(&[("advise", "0.5")])
    ));
    for _ in 0..6 {
        let answer = world.run("hello", &[]);
        assert_eq!(answer.code, 0, "{}", answer.json);
        let id = answer.run_id();
        // The id the run is recorded under is the one the draw was made from.
        let drawn = cahoots::explore::drawn(&id, 0.5);
        ran(&world, &answer, if drawn { "m2" } else { "m1" }, drawn);
        assert_eq!(world.record(&id)["id"], id.as_str());
    }
}

#[test]
fn a_refused_exploration_falls_back_without_a_false_label() {
    // The promoted candidate is on a harness that is busy; the first is not.
    let world = world_with(&format!(
        "{}{}",
        role(
            "advise",
            &[("claude", "c1", "high"), ("codex", "m1", "high")]
        ),
        shares(&[("advise", "1.0")])
    ));
    let held = hold(&world, &["--to", "codex", "--caller", "claude"]);
    // No caller, so both are candidates; share 1 tries codex first.
    let answer = run_role(&world, "advise", &[]);
    ran(&world, &answer, "c1", false);
    assert_eq!(
        world.record(&answer.run_id())["target"]["harness"],
        "claude"
    );
    assert_eq!(world.ask(&["cancel", &held]).code, 42);

    // Both first entries refused, the third admitted: the third, unlabelled.
    let list = [
        ("codex", "m1", "high"),
        ("codex", "m2", "high"),
        ("claude", "c1", "high"),
    ];
    world.configure(&format!(
        "{}{}",
        role("advise", &list),
        shares(&[("advise", "1.0")])
    ));
    let held = hold(&world, &["--to", "codex", "--caller", "claude"]);
    let answer = run_role(&world, "advise", &[]);
    ran(&world, &answer, "c1", false);
    // Every candidate refused: 30, each reason, in the list's own order.
    let only = [("codex", "m1", "high"), ("codex", "m2", "high")];
    world.configure(&format!(
        "{}{}",
        role("advise", &only),
        shares(&[("advise", "1.0")])
    ));
    let none = run_role(&world, "advise", &[]);
    assert_eq!(none.code, 30, "{}", none.json);
    let message = none.message();
    let (first, second) = (
        message.find("codex (m1, high): ").expect(message),
        message.find("codex (m2, high): ").expect(message),
    );
    assert!(first < second, "{message}");
    assert!(message.contains("is already running as many jobs as it may"));
    assert_eq!(world.ask(&["cancel", &held]).code, 42);
}

#[test]
fn exploration_keeps_the_gate_and_ledger_refusals() {
    let list = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m2", "high"),
    ];
    let config = format!(
        "harness.codex.cap = 80\n{}{}",
        role("advise", &list),
        shares(&[("advise", "1.0")])
    );
    // Admitted: the promoted candidate is held to the role-adjusted cap, and
    // the reading is kept.
    let world = World::new();
    world.meter(json!({"guarded": {"code": 0, "percent": 35}}), &config);
    let answer = world.run("hello", &[]);
    ran(&world, &answer, "m2", true);
    assert_eq!(
        world.record(&answer.run_id())["admission"]["reading"]["percent"],
        35.0
    );
    let calls = world.meter_calls();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].starts_with("headroom --provider codex --cap 77 --forecast red --json"),
        "{calls:?}"
    );
    // A sole filtered candidate: each existing refusal, with its own code.
    let sole = format!(
        "{}{}",
        role(
            "advise",
            &[("claude", "c1", "high"), ("codex", "m1", "high")]
        ),
        shares(&[("advise", "1.0")])
    );
    for (code, retry) in [
        (24, "after_reset"),
        (25, "after_reset"),
        (26, "other_target"),
        (13, "other_target"),
    ] {
        let world = World::new();
        world.meter(json!({"guarded": {"code": code, "percent": 91}}), &sole);
        let refused = world.run("hello", &[]);
        assert_eq!(refused.code, code, "{}", refused.json);
        assert_eq!(refused.json["retry"], retry);
        no_run_was_made(&world);
    }
    let world = World::new();
    world.meter(
        json!({"guarded": {"code": 21}, "unguarded": {"code": 0, "percent": 1}}),
        &format!(
            "{}{}",
            role(
                "advise",
                &[("codex", "m1", "high"), ("claude", "c1", "high")]
            ),
            shares(&[("advise", "1.0")])
        ),
    );
    let stale = world.run("hello", &["--caller", "codex"]);
    assert_eq!(stale.code, 21, "{}", stale.json);
    no_run_was_made(&world);
    // Two same-harness candidates, both refused: 30, in the list's order.
    let world = World::new();
    world.meter(json!({"guarded": {"code": 24, "percent": 91}}), &config);
    let both = world.run("hello", &[]);
    assert_eq!(both.code, 30, "{}", both.json);
    let message = both.message();
    assert!(
        message.find("codex (m1, high)").expect(message)
            < message.find("codex (m2, high)").expect(message),
        "{message}"
    );
    no_run_was_made(&world);
    // The ledger: the first run is admitted; the second hits its own limit.
    let world = World::new();
    world.meter(
        json!({"guarded": {"code": 0, "percent": 1}}),
        &format!("[meter.ledger]\nmax_runs_per_hour = 1\n{}", {
            let (list, share) = (role("advise", &list), shares(&[("advise", "1.0")]));
            format!("{list}{share}")
        }),
    );
    ran(&world, &world.run("hello", &[]), "m2", true);
    let again = world.run("hello", &[]);
    assert_eq!(again.code, 30, "{}", again.json);
    assert_eq!(fs::read_dir(world.state.join("runs")).unwrap().count(), 1);
    let world = World::new();
    world.meter(
        json!({"guarded": {"code": 0, "percent": 1}}),
        &format!("[meter.ledger]\nmax_runs_per_hour = 0\n{sole}"),
    );
    let sole_refused = world.run("hello", &[]);
    assert_eq!(sole_refused.code, 24, "{}", sole_refused.json);
    no_run_was_made(&world);
}

#[test]
fn exploration_keeps_slots_depth_and_the_active_run_ceiling() {
    let list = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m2", "high"),
    ];
    let config = format!("{}{}", role("advise", &list), shares(&[("advise", "1.0")]));
    let world = world_with(&config);
    // Capacity available: admitted.
    ran(&world, &world.run("hello", &[]), "m2", true);
    // Held slots: every candidate on that harness is busy — aggregate 30.
    let held = hold(&world, &["--to", "codex"]);
    let busy = world.run("hello", &[]);
    assert_eq!(busy.code, 30, "{}", busy.json);
    assert!(
        busy.message().contains("codex (m1, high)") && busy.message().contains("codex (m2, high)")
    );
    assert_eq!(world.ask(&["cancel", &held]).code, 42);
    // A sole candidate keeps its own 32.
    let world = world_with(&format!(
        "{}{}",
        role(
            "advise",
            &[("claude", "c1", "high"), ("codex", "m1", "high")]
        ),
        shares(&[("advise", "1.0")])
    ));
    let held = hold(&world, &["--to", "codex"]);
    let sole = world.run("hello", &[]);
    assert_eq!(sole.code, 32, "{}", sole.json);
    assert_eq!(world.ask(&["cancel", &held]).code, 42);
    // Depth: a delegated run does not delegate, share or no share.
    let world = world_with(&config);
    let brief = world.brief("hello");
    let deep = common::answer(world.cahoots().env("CAHOOTS_DEPTH", "1").args([
        "run",
        "--role",
        "advise",
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]));
    assert_eq!(deep.code, 33, "{}", deep.json);
    no_run_was_made(&world);
    // The global ceiling.
    let world = world_with(&format!("limits.max_active_runs = 1\n{config}"));
    let held = hold(&world, &["--to", "codex"]);
    let full = world.run("hello", &[]);
    assert_eq!(full.code, 32, "{}", full.json);
    assert_eq!(full.json["class"], "busy");
    assert_eq!(world.ask(&["cancel", &held]).code, 42);
}

#[test]
fn exploration_uses_the_roles_existing_fence() {
    let list = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m2", "medium"),
        ("claude", "c2", "low"),
    ];
    let config = format!(
        "{}{}{}",
        shares(&[("advise", "1.0"), ("implement", "1.0")]),
        role("advise", &list),
        role("implement", &list)
    );
    let world = world_with(&config);
    for (caller, harness, model, effort, fence) in [
        (
            "claude",
            "codex",
            "m2",
            "medium",
            ["--sandbox", "read-only"],
        ),
        (
            "codex",
            "claude",
            "c2",
            "low",
            ["--tools", "Read,Grep,Glob"],
        ),
    ] {
        let answer = world.run("FAKE: dump", &["--caller", caller]);
        assert_eq!(answer.code, 0, "{}", answer.json);
        assert_eq!(world.record(&answer.run_id())["exploration"], true);
        let argv = argv(&answer);
        assert!(argv.windows(2).any(|w| w == fence), "{argv:?}");
        assert!(
            argv.windows(2).any(|w| w == ["-m", model])
                || argv.windows(2).any(|w| w == ["--model", model]),
            "{argv:?}"
        );
        assert!(argv.iter().any(|a| a.contains(effort)), "{argv:?}");
        assert!(
            !argv.iter().skip(1).any(|a| a.contains("explor")),
            "an exploration flag reached the harness: {argv:?}"
        );
        assert_eq!(answer.data()["target"]["harness"], harness);
    }
    // A writer: only in a worktree of its own, with its fence.
    for (caller, fence) in [
        ("claude", ["--sandbox", "workspace-write"]),
        ("codex", ["--tools", "Read,Grep,Glob,Edit,Write"]),
    ] {
        let answer = run_role(&world, "implement", &["--caller", caller, "--fork"]);
        assert_eq!(answer.code, 0, "{}", answer.json);
        assert_eq!(answer.data()["placement"], "fork");
        assert_eq!(world.record(&answer.run_id())["exploration"], true);
        let dump = world.brief("FAKE: dump");
        let dumped = world.ask(&[
            "run",
            "--role",
            "implement",
            "--caller",
            caller,
            "--fork",
            "--brief",
            dump.to_str().unwrap(),
        ]);
        let argv = argv(&dumped);
        assert!(argv.windows(2).any(|w| w == fence), "{argv:?}");
    }
    for flag in [None, Some("--in-place")] {
        let extra: Vec<&str> = ["--caller", "claude"].into_iter().chain(flag).collect();
        let refused = run_role(&world, "implement", &extra);
        assert_eq!(refused.code, 33, "{:?}: {}", flag, refused.json);
    }
}

/// Every run view for `id`: `status <id>`, `result`, `wait --timeout 0`, and
/// the run's entry in `status`'s list. `code` is what `result` and `wait`
/// answer with.
fn views(world: &World, id: &str, code: i32) -> Vec<Value> {
    let mut seen = Vec::new();
    for (args, expected) in [
        (vec!["status", id], 0),
        (vec!["result", id], code),
        (vec!["wait", id, "--timeout", "0"], code),
    ] {
        let answer = world.ask(&args);
        assert_eq!(answer.code, expected, "{args:?}: {}", answer.json);
        seen.push(answer.data().clone());
    }
    let list = world.ask(&["status"]);
    let entry = list.data()["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["run"] == id)
        .unwrap_or_else(|| panic!("{id} is not in the list: {}", list.json))
        .clone();
    seen.push(entry);
    seen
}

#[test]
fn the_label_is_fixed_across_run_views_and_configuration_changes() {
    let list = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m2", "high"),
    ];
    let on = format!("{}{}", role("advise", &list), shares(&[("advise", "1.0")]));
    let world = world_with(&on);
    let started = world.run("FAKE: sleep=120", &["--wait", "0"]);
    assert_eq!(started.code, 51, "{}", started.json);
    let id = started.run_id();
    assert_eq!(started.data()["exploration"], true);
    assert_eq!(world.record(&id)["exploration"], true);
    for open in views(&world, &id, 51) {
        assert_eq!(open["exploration"], true, "{open}");
    }
    // A change of config relabels nothing: not the record, not a view.
    world.configure(&format!(
        "{}{}",
        role("advise", &list),
        shares(&[("advise", "0.0")])
    ));
    for open in views(&world, &id, 51) {
        assert_eq!(open["exploration"], true, "{open}");
    }
    let cancelled = world.ask(&["cancel", &id]);
    assert_eq!(cancelled.code, 42, "{}", cancelled.json);
    assert_eq!(cancelled.data()["exploration"], true);
    for done in views(&world, &id, 42) {
        assert_eq!(done["exploration"], true, "{done}");
    }
    assert_eq!(world.record(&id)["exploration"], true);
    assert_eq!(finished(&world, &id)["exploration"], true);
    assert_eq!(finished(&world, &id)["state"], "cancelled");
    // A callee that fails keeps the label it was selected with.
    world.configure(&on);
    let failed = world.run("FAKE: fail", &[]);
    assert_eq!(failed.code, 40, "{}", failed.json);
    assert_eq!(failed.data()["exploration"], true);
    assert_eq!(finished(&world, &failed.run_id())["exploration"], true);
    // And one that was not selected stays false when the share is raised.
    world.configure(&format!(
        "{}{}",
        role("advise", &list),
        shares(&[("advise", "0.0")])
    ));
    let ordinary = world.run("hello", &[]);
    ran(&world, &ordinary, "m1", false);
    world.configure(&on);
    for view in views(&world, &ordinary.run_id(), 0) {
        assert_eq!(view["exploration"], false, "{view}");
    }
    assert_eq!(world.record(&ordinary.run_id())["exploration"], false);
}

#[test]
fn resume_keeps_the_session_without_a_new_draw() {
    let list = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m2", "high"),
    ];
    let world = world_with(&format!(
        "{}{}",
        role("advise", &list),
        shares(&[("advise", "1.0")])
    ));
    let first = world.run("hello", &[]);
    ran(&world, &first, "m2", true);
    let session = world.record(&first.run_id())["progress"]["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let brief = world.brief("more");
    let resume = |extra: &[&str]| {
        let mut args = vec!["resume", first.run_id().leak() as &str];
        args.extend(["--brief", brief.to_str().unwrap()]);
        args.extend(extra);
        world.ask(&args)
    };
    let again = resume(&["--caller", "claude"]);
    assert_eq!(again.code, 0, "{}", again.json);
    let id = again.run_id();
    assert_ne!(id, first.run_id());
    // The same target and session; a new, false label; its ancestry named.
    let record = world.record(&id);
    assert_eq!(record["target"], world.record(&first.run_id())["target"]);
    assert_eq!(record["progress"]["session_id"], session.as_str());
    assert_eq!(record["exploration"], false);
    assert_eq!(record["resumed_from"], first.run_id().as_str());
    assert_eq!(again.data()["exploration"], false);
    assert_eq!(finished(&world, &id)["exploration"], false);
    assert_eq!(world.record(&first.run_id())["exploration"], true);
    // Every existing resume refusal still applies.
    assert_eq!(resume(&["--caller", "codex"]).code, 33);
    world.enable(&["claude"]);
    assert_eq!(resume(&["--caller", "claude"]).code, 31);
    world.enable(&["claude", "codex"]);
    let held = hold(&world, &["--to", "codex"]);
    let busy = world.ask(&[
        "resume",
        &held,
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(busy.code, 51, "{}", busy.json);
    assert_eq!(world.ask(&["cancel", &held]).code, 42);
}

#[test]
fn pick_previews_ordinary_routing_without_drawing() {
    let list = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m2", "high"),
    ];
    let world = world_with(&format!(
        "{}{}",
        role("advise", &list),
        shares(&[("advise", "1.0")])
    ));
    let preview = pick_role(&world, "advise", &["--caller", "claude"]);
    assert_eq!(preview.code, 0, "{}", preview.json);
    // The ordinary first candidate, and the share a run may use.
    assert_eq!(preview.data()["target"]["model"], "m1");
    assert_eq!(preview.data()["exploration_share"], 1.0);
    let data = preview.data().as_object().unwrap();
    for absent in ["run", "exploration", "id"] {
        assert!(!data.contains_key(absent), "{absent}: {data:?}");
    }
    no_run_was_made(&world);
    // Suppression is reflected: `--to`, one candidate left.
    let to = pick_role(&world, "advise", &["--caller", "claude", "--to", "codex"]);
    assert_eq!(to.data()["exploration_share"], 0.0);
    assert_eq!(to.data()["target"]["model"], "m1");
    let one = pick_role(&world, "advise", &["--caller", "codex"]);
    assert_eq!(one.data()["target"]["model"], "c1");
    assert_eq!(
        one.data()["exploration_share"],
        0.0,
        "one candidate is left"
    );
    // A role at zero previews zero.
    let none = pick_role(&world, "review", &["--caller", "claude"]);
    assert_eq!(none.data()["exploration_share"], 0.0);
    // A kind inherits its role's share, or has its own.
    let kind_list = [("codex", "m1", "high"), ("codex", "m2", "high")];
    for (own, expected) in [(None, 0.3), (Some("0.7"), 0.7), (Some("0.0"), 0.0)] {
        let world = world_with(&format!(
            "{}{}",
            shares(&[("review", "0.3")]),
            kind("rust-review", "review", &kind_list, own)
        ));
        let preview = world.ask(&["pick", "--kind", "rust-review"]);
        assert_eq!(preview.code, 0, "{}", preview.json);
        assert_eq!(preview.data()["exploration_share"], expected);
        assert_eq!(preview.data()["target"]["model"], "m1");
        no_run_was_made(&world);
    }
    // An identical first pair previews zero.
    let same = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m1", "high"),
    ];
    let world = world_with(&format!(
        "{}{}",
        role("advise", &same),
        shares(&[("advise", "1.0")])
    ));
    let preview = pick_role(&world, "advise", &["--caller", "claude"]);
    assert_eq!(preview.data()["exploration_share"], 0.0);
}

#[test]
fn agent_caller_exploration_unsticks_role_calibration() {
    let list = |calibrate: bool| {
        format!(
            "[roles.advise]\ncalibrate = {calibrate}\ncandidates = {}\n",
            entries(&[
                ("claude", "c1", "high"),
                ("codex", "m1", "high"),
                ("codex", "m2", "high"),
            ])
        )
    };
    let config = |share: &str, routing: bool, calibrate: bool| {
        format!(
            "meter.ledger.max_runs_per_hour = 100\n[review]\nenabled = true\napply_routing = {routing}\n{}{}",
            list(calibrate),
            shares(&[("advise", share)])
        )
    };
    let world = world_with(&config("0.0", true, true));
    let outcome = |answer: &Answer, label: &str| {
        let id = answer.run_id();
        finished(&world, &id);
        assert_eq!(world.ask(&["outcome", &id, label]).code, 0);
    };
    // The explicit caller leaves only codex; with no share the second codex
    // candidate never runs, so it can never earn evidence.
    for _ in 0..8 {
        let answer = world.run("hello", &[]);
        assert_eq!(world.record(&answer.run_id())["target"]["model"], "m1");
        outcome(&answer, "discarded");
    }
    assert_eq!(
        pick_role(&world, "advise", &["--caller", "claude"]).data()["target"]["model"],
        "m1"
    );
    // With exploration, it does — eight real runs — and does better.
    world.configure(&config("1.0", true, true));
    for _ in 0..8 {
        let answer = world.run("hello", &[]);
        ran(&world, &answer, "m2", true);
        outcome(&answer, "accepted");
    }
    // Exploration off again: the learned swap, once authorized, now changes
    // what the agent caller is picked.
    world.configure(&config("0.0", true, true));
    let picked = pick_role(&world, "advise", &["--caller", "claude"]);
    assert_eq!(picked.data()["target"]["model"], "m2", "{}", picked.json);
    // Shadow mode, and a list a person wrote without `calibrate`, do not apply it.
    world.configure(&config("0.0", false, true));
    assert_eq!(
        pick_role(&world, "advise", &["--caller", "claude"]).data()["target"]["model"],
        "m1"
    );
    world.configure(&config("0.0", true, false));
    assert_eq!(
        pick_role(&world, "advise", &["--caller", "claude"]).data()["target"]["model"],
        "m1"
    );
}

fn hidden(data: &Value, harness: &str) {
    assert_eq!(data["blind"], true, "{data}");
    assert_eq!(data["target"], json!({"harness": harness}), "{data}");
    assert!(data.get("model_reported").is_none(), "{data}");
    // Membership, not `null`: neither false nor null stands in for the key.
    assert!(
        !data.as_object().unwrap().contains_key("exploration"),
        "{data}"
    );
}

fn revealed(world: &World, data: &Value, id: &str, label: bool) {
    assert_eq!(data["blind"], false, "{data}");
    assert_eq!(data["exploration"], label, "{data}");
    assert_eq!(data["target"], world.record(id)["target"], "{data}");
}

#[test]
fn a_blind_run_withholds_its_exploration_label_until_its_outcome() {
    let list = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m2", "high"),
    ];
    let config = |share: &str| {
        format!(
            "review.blind = true\nreview.enabled = true\nreview.sample_rate = 1.0\n{}{}",
            role("advise", &list),
            shares(&[("advise", share)])
        )
    };
    let world = world_with(&config("1.0"));
    let seen_hidden = |id: &str, code: i32| {
        for view in views(&world, id, code) {
            hidden(&view, "codex");
        }
    };
    // A forced exploration, and an ordinary run: private labels differ.
    let explored = world.run("hello", &[]);
    assert_eq!(explored.code, 0, "{}", explored.json);
    hidden(explored.data(), "codex");
    let explored_id = explored.run_id();
    finished(&world, &explored_id);
    world.configure(&config("0.0"));
    let ordinary = world.run("hello", &[]);
    hidden(ordinary.data(), "codex");
    let ordinary_id = ordinary.run_id();
    finished(&world, &ordinary_id);
    for (id, label) in [(&explored_id, true), (&ordinary_id, false)] {
        assert_eq!(world.record(id)["exploration"], label);
        assert_eq!(finished(&world, id)["exploration"], label);
        assert_eq!(
            world.record(id)["target"]["model"],
            if label { "m2" } else { "m1" }
        );
        seen_hidden(id, 0);
    }
    // An unfinished run: withheld before it ends, and through its cancellation.
    world.configure(&config("1.0"));
    let started = world.run("FAKE: sleep=120", &["--wait", "0"]);
    assert_eq!(started.code, 51, "{}", started.json);
    hidden(started.data(), "codex");
    let open_id = started.run_id();
    seen_hidden(&open_id, 51);
    assert_eq!(world.record(&open_id)["exploration"], true);
    let cancelled = world.ask(&["cancel", &open_id]);
    assert_eq!(cancelled.code, 42, "{}", cancelled.json);
    hidden(cancelled.data(), "codex");
    finished(&world, &open_id);
    // A change of config reveals nothing.
    world.configure(&config("0.0"));
    for id in [&explored_id, &ordinary_id] {
        seen_hidden(id, 0);
    }
    seen_hidden(&open_id, 42);
    // `review next` has no such field to leak, and a review is not an outcome.
    let next = world.ask(&["review", "next", "--caller", "claude"]);
    assert_eq!(next.code, 0, "{}", next.json);
    assert!(
        !next.json.to_string().contains("exploration"),
        "{}",
        next.json
    );
    let item = next.data()["next"]["run"]
        .as_str()
        .expect("something to review")
        .to_string();
    let submitted = world.ask(&[
        "review",
        "submit",
        &item,
        "--caller",
        "claude",
        "--finding",
        "brief_too_broad",
    ]);
    assert_eq!(submitted.code, 0, "{}", submitted.json);
    for id in [&explored_id, &ordinary_id] {
        seen_hidden(id, 0);
    }
    // Each outcome reveals that run's saved boolean, with its full target.
    for (id, code, label) in [
        (&explored_id, 0, true),
        (&ordinary_id, 0, false),
        (&open_id, 42, true),
    ] {
        let recorded = world.ask(&["outcome", id, "accepted"]);
        assert_eq!(recorded.code, 0, "{}", recorded.json);
        for view in views(&world, id, code) {
            revealed(&world, &view, id, label);
        }
    }
    // Still private after the fact: nothing a view says changed the record.
    assert_eq!(world.record(&explored_id)["exploration"], true);
    assert_eq!(world.record(&ordinary_id)["exploration"], false);
}

#[test]
fn a_blind_fork_writer_still_shows_its_base_commit_and_patch() {
    let list = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m2", "high"),
    ];
    let world = world_with(&format!(
        "review.blind = true\n{}{}",
        role("implement", &list),
        shares(&[("implement", "1.0")])
    ));
    let brief = world.brief("FAKE: write=x.txt");
    let answer = world.ask(&[
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--fork",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    hidden(answer.data(), "codex");
    let head = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&world.work)
            .env_remove("GIT_DIR")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(answer.data()["base_commit"], head.trim());
    assert_eq!(answer.data()["patch"]["files"][0]["path"], "x.txt");
    assert_eq!(world.record(&answer.run_id())["exploration"], true);
}

#[test]
fn a_blind_resume_needs_its_own_outcome_to_reveal_the_false_label() {
    let list = [
        ("claude", "c1", "high"),
        ("codex", "m1", "high"),
        ("codex", "m2", "high"),
    ];
    let world = world_with(&format!(
        "review.blind = true\n{}{}",
        role("advise", &list),
        shares(&[("advise", "1.0")])
    ));
    let first = world.run("hello", &[]);
    assert_eq!(first.code, 0, "{}", first.json);
    let parent = first.run_id();
    finished(&world, &parent);
    assert_eq!(world.ask(&["outcome", &parent, "accepted"]).code, 0);
    for view in views(&world, &parent, 0) {
        revealed(&world, &view, &parent, true);
    }
    // The resume snapshots today's blind policy, and its label is false.
    let brief = world.brief("more");
    let again = world.ask(&[
        "resume",
        &parent,
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(again.code, 0, "{}", again.json);
    let id = again.run_id();
    assert_eq!(world.record(&id)["exploration"], false);
    assert_eq!(world.record(&id)["blind"], true);
    hidden(again.data(), "codex");
    finished(&world, &id);
    // The parent's outcome does not reveal it.
    for view in views(&world, &id, 0) {
        hidden(&view, "codex");
    }
    assert_eq!(world.ask(&["outcome", &id, "discarded"]).code, 0);
    for view in views(&world, &id, 0) {
        revealed(&world, &view, &id, false);
    }
}

#[test]
fn old_records_load_as_nonexploration() {
    let world = World::new();
    let id = world.run("hello", &[]).run_id();
    let mut record = world.record(&id);
    assert_eq!(
        record["exploration"], false,
        "false is written, not left out"
    );
    record.as_object_mut().unwrap().remove("exploration");
    let old: cahoots::run::record::RunRecord = serde_json::from_value(record.clone()).unwrap();
    assert!(!old.exploration);
    fs::write(
        world.run_file(&id, "run.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let later = world.ask(&["result", &id]);
    assert_eq!(later.code, 0, "{}", later.json);
    assert_eq!(later.data()["exploration"], false);
}
