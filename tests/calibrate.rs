//! Routing calibration, end to end: the history decides, in bounds, and —
//! unless a person says otherwise — only on paper.

mod common;

use std::fs;

use common::World;

/// Writes a history in which, for `advise`, codex (the default first choice)
/// kept being thrown away and claude (second) kept being accepted.
fn history_where_the_second_choice_does_better(world: &World, each: usize) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut lines = String::new();
    for (harness, model, outcome) in [
        ("codex", "gpt-6-astra", "discarded"),
        ("claude", "opus", "accepted"),
    ] {
        for n in 0..each {
            let run = format!("0198c0de-0000-7000-8000-{harness:0>6}{n:06}");
            lines.push_str(&format!(
                "{{\"kind\":\"finished\",\"t\":{now},\"run\":\"{run}\",\"role\":\"advise\",\"caller\":null,\
                 \"target\":{{\"harness\":\"{harness}\",\"model\":\"{model}\",\"effort\":\"high\"}},\
                 \"dir\":\"/w\",\"state\":\"done\",\"exit\":0,\"tokens_in\":1,\"tokens_out\":1,\"secs\":1,\"sampled\":false}}\n\
                 {{\"kind\":\"outcome\",\"t\":{now},\"run\":\"{run}\",\"outcome\":\"{outcome}\"}}\n"
            ));
        }
    }
    fs::create_dir_all(&world.state).unwrap();
    fs::write(world.state.join("history.jsonl"), lines).unwrap();
}

fn first_choice(world: &World) -> String {
    // No caller: nobody is left out, so the first candidate is the role's first.
    let picked = world.ask(&["pick", "--role", "advise"]);
    assert_eq!(picked.code, 0, "{}", picked.json);
    picked.data()["target"]["harness"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn in_shadow_mode_the_suggestion_is_shown_and_nothing_changes() {
    let world = World::new();
    world.configure("[review]\nenabled = true");
    history_where_the_second_choice_does_better(&world, 8);

    let report = world.ask(&["report", "--suggest"]);
    assert_eq!(report.code, 0, "{}", report.json);
    let routing = &report.data()["routing"];
    assert!(routing["mode"].as_str().unwrap().starts_with("shadow"));
    let advise = &routing["roles"]["advise"];
    assert_eq!(advise["suggested_swap"]["move_up"]["harness"], "claude");
    assert_eq!(advise["suggested_swap"]["past"]["harness"], "codex");
    assert_eq!(advise["suggested_swap"]["in_effect"], false);
    assert_eq!(advise["order"][0]["evidence"]["n"], 8);
    assert_eq!(advise["order"][1]["evidence"]["score"], 1.0);
    // Roles with no evidence say so.
    assert!(routing["roles"]["review"]["suggested_swap"].is_null());

    assert_eq!(
        first_choice(&world),
        "codex",
        "shadow mode changed the routing"
    );
}

#[test]
fn when_a_person_turns_it_on_the_order_moves_by_one_place() {
    let world = World::new();
    world.configure("[review]\nenabled = true\napply_routing = true");
    history_where_the_second_choice_does_better(&world, 8);
    assert_eq!(first_choice(&world), "claude");
    let report = world.ask(&["report", "--suggest"]);
    assert_eq!(
        report.data()["routing"]["roles"]["advise"]["suggested_swap"]["in_effect"],
        true
    );
    // Another role's order is untouched.
    let review = world.ask(&["pick", "--role", "review"]);
    assert_eq!(review.data()["target"]["harness"], "codex");
}

#[test]
fn seven_runs_each_is_not_enough_and_review_off_means_off() {
    let world = World::new();
    world.configure("[review]\nenabled = true\napply_routing = true");
    history_where_the_second_choice_does_better(&world, 7);
    assert_eq!(first_choice(&world), "codex");

    let world = World::new();
    world.configure("[review]\napply_routing = true"); // enabled is still false
    history_where_the_second_choice_does_better(&world, 20);
    assert_eq!(
        first_choice(&world),
        "codex",
        "learning applied while review was off"
    );
}

#[test]
fn an_order_a_person_wrote_is_left_alone_unless_they_say_otherwise() {
    let list = "candidates = [\n  { harness = \"codex\", model = \"gpt-6-astra\", effort = \"high\" },\n  { harness = \"claude\", model = \"opus\", effort = \"high\" },\n]";
    let world = World::new();
    world.configure(&format!(
        "[review]\nenabled = true\napply_routing = true\n[roles.advise]\n{list}"
    ));
    history_where_the_second_choice_does_better(&world, 12);
    assert_eq!(
        first_choice(&world),
        "codex",
        "a person's own order was changed"
    );
    let report = world.ask(&["report", "--suggest"]);
    assert!(
        report.data()["routing"]["roles"]["advise"]["no_swap_because"]
            .as_str()
            .unwrap()
            .contains("written by a person")
    );

    world.configure(&format!(
        "[review]\nenabled = true\napply_routing = true\n[roles.advise]\ncalibrate = true\n{list}"
    ));
    assert_eq!(first_choice(&world), "claude");
}

#[test]
fn what_is_learned_never_touches_a_cap_a_model_or_an_effort() {
    let world = World::new();
    world.configure("harness.codex.cap = 61\n[review]\nenabled = true\napply_routing = true");
    history_where_the_second_choice_does_better(&world, 30);
    // `registry` is a person's verb; read the same thing through `pick` and `doctor`.
    let picked = world.ask(&["pick", "--role", "advise"]);
    assert_eq!(picked.data()["target"]["model"], "opus");
    assert_eq!(picked.data()["target"]["effort"], "high");
    let doctor = world.ask(&["doctor"]);
    let checks = doctor.data()["checks"].as_array().unwrap();
    let codex = checks
        .iter()
        .find(|c| c["check"] == "codex: target")
        .unwrap();
    assert!(
        codex["detail"].as_str().unwrap().contains("cap 61%"),
        "{codex}"
    );
}
