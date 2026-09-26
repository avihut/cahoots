//! `cahoots settings`, end to end at a terminal of its own, against a world
//! of throwaway directories: one setting at a time from the command line,
//! and what reaches config.toml.

mod common;

use std::fs;

use common::World;
use serde_json::json;

#[test]
fn set_writes_one_key_among_the_others_and_reset_takes_it_out() {
    let world = World::new();
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();

    let set = world
        .at_terminal(&["settings", "set", "harness.codex.cap", "60"])
        .finish();
    assert_eq!(set.code, 0, "{}", set.json);
    assert_eq!(
        set.json["data"]["changed"],
        json!([{"key": "harness.codex.cap", "value": 60, "origin": "config"}])
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        before.replace(
            "harness.codex.enabled = true\n",
            "harness.codex.enabled = true\nharness.codex.cap = 60\n"
        ),
        "a dotted key among the dotted keys, and nothing else touched"
    );

    let reset = world
        .at_terminal(&["settings", "reset", "harness.codex.cap"])
        .finish();
    assert_eq!(reset.code, 0, "{}", reset.json);
    assert_eq!(
        reset.json["data"]["changed"],
        json!([{"key": "harness.codex.cap", "value": 75, "origin": "default"}])
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
}

#[test]
fn a_value_the_setting_cannot_take_is_refused_and_nothing_is_written() {
    let world = World::new();
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    for (args, why) in [
        (vec!["harness.codex.cap", "101"], "1–100"),
        (vec!["harness.codex.abort_at", "70"], "76–100"),
        (vec!["meter.use", "codexbar"], "agent-usage, ccusage, none"),
        (vec!["review.sample_rate", "20"], "0.0 to 1.0"),
        (vec!["harness.gemini.cap", "60"], "no setting is called"),
        (
            vec!["roles.advise.calibrate", "on"],
            "roles.advise.candidates",
        ),
    ] {
        let mut argv = vec!["settings", "set"];
        argv.extend(&args);
        let after = world.at_terminal(&argv).finish();
        assert_eq!(after.code, 2, "{args:?}: {}", after.json);
        let message = after.json["message"].as_str().unwrap();
        assert!(message.contains(why), "{args:?}: {message}");
        assert_eq!(fs::read_to_string(&file).unwrap(), before, "{args:?}");
    }
}

#[test]
fn a_share_a_list_and_a_new_table_are_written_as_config_toml_writes_them() {
    let world = World::new();
    let file = world.config.join("config.toml");
    for (key, value) in [
        ("review.sample_rate", "0.5"),
        (
            "roles.advise.candidates",
            "claude:opus:high,codex:gpt-6-astra:high",
        ),
        ("roles.advise.calibrate", "on"),
    ] {
        let after = world.at_terminal(&["settings", "set", key, value]).finish();
        assert_eq!(after.code, 0, "{key}: {}", after.json);
    }
    // New tables go after the others; what ended the file still ends it.
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.trim_end().ends_with(
            "\n[review]\nsample_rate = 0.5\n\n[roles.advise]\ncandidates = [\n  \
             { harness = \"claude\", model = \"opus\", effort = \"high\" },\n  \
             { harness = \"codex\", model = \"gpt-6-astra\", effort = \"high\" },\n]\n\
             calibrate = true"
        ),
        "{text}"
    );
    let registry = world.at_terminal(&["registry"]).finish();
    assert_eq!(
        registry.json["data"]["roles"]["advise"]["candidates"][0]["harness"],
        "claude"
    );
    // A role's list goes back to the default whole, its calibrate with it.
    let reset = world
        .at_terminal(&["settings", "reset", "roles.advise.candidates"])
        .finish();
    assert_eq!(reset.code, 0, "{}", reset.json);
    let text = fs::read_to_string(&file).unwrap();
    assert!(!text.contains("roles"), "{text}");
}

#[test]
fn with_nowhere_to_draw_the_page_settings_names_its_flags() {
    let world = World::new();
    let after = world
        .at_terminal_with(&["settings"], &[("TERM", "dumb")])
        .finish();
    assert_eq!(after.code, 2, "{}", after.json);
    let message = after.json["message"].as_str().unwrap();
    assert!(message.contains("cahoots settings set"), "{message}");
}
