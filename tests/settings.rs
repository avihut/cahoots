//! `cahoots settings`, end to end at a terminal of its own, against a world
//! of throwaway directories: the page, one setting at a time from the command
//! line, and what reaches config.toml.

mod common;

use std::fs;
use std::time::Duration;

use common::{Finished, World};
use nix::sys::termios::LocalFlags;
use serde_json::json;

const DOWN: &[u8] = b"\x1b[B";
const UP: &[u8] = b"\x1b[A";
const LEFT: &[u8] = b"\x1b[D";
const RIGHT: &[u8] = b"\x1b[C";
const ENTER: &[u8] = b"\r";
const TAB: &[u8] = b"\t";
const ESC: &[u8] = b"\x1b";

/// The terminal is as it was before the page: line by line and echoed, the
/// cursor shown, lines wrapping, and the shell's own screen.
fn given_back(after: &Finished) {
    assert!(
        after
            .mode
            .local_flags
            .contains(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG),
        "{:?}",
        after.mode.local_flags
    );
    for (off, on) in [
        ("\x1b[?25l", "\x1b[?25h"),
        ("\x1b[?7l", "\x1b[?7h"),
        ("\x1b[?1049h", "\x1b[?1049l"),
    ] {
        let (off, on) = (after.screen.rfind(off), after.screen.rfind(on));
        assert!(off.is_some() && on > off, "{:?}", after.screen);
    }
}

#[test]
fn the_page_changes_a_setting_in_place_and_gives_the_terminal_back() {
    let world = World::new();
    world.configure("# Kept by hand.\nharness.codex.cap = 60");
    let file = world.config.join("config.toml");
    let terminal = world.at_terminal(&["settings"]);
    terminal.wait_for("❯ Enabled");
    for shown in [" Claude Code", " Codex", " Usage meter", "• 60%"] {
        terminal.wait_for(shown);
    }
    terminal.press(DOWN);
    terminal.wait_for("❯ Usage cap");
    terminal.press(ENTER);
    terminal.wait_for("◀  75%  ▶");
    terminal.press(LEFT);
    terminal.wait_for("◀  70%  ▶");
    terminal.press(LEFT);
    terminal.wait_for("◀  65%  ▶");
    terminal.press(ENTER);
    terminal.wait_for("Saved to config.toml: [harness.claude] cap = 65");
    terminal.press(ESC);
    let after = terminal.finish();
    assert_eq!(after.code, 0, "{}", after.json);
    assert_eq!(
        after.json["data"]["changed"],
        json!([{"key": "harness.claude.cap", "value": 65, "origin": "config"}])
    );
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("harness.claude.cap = 65\n")
            && text.contains("# Kept by hand.\nharness.codex.cap = 60\n"),
        "{text}"
    );
    given_back(&after);
}

#[test]
fn closing_the_page_without_a_change_changes_nothing() {
    let world = World::new();
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    let terminal = world.at_terminal(&["settings"]);
    terminal.wait_for("❯ Enabled");
    // Into a box and out of it, then out of the page.
    terminal.press(DOWN);
    terminal.press(ENTER);
    terminal.wait_for("◀  75%  ▶");
    terminal.press(RIGHT);
    terminal.wait_for("◀  80%  ▶");
    terminal.press(ESC);
    terminal.wait_for("↑↓ move · tab section");
    terminal.press(ESC);
    let after = terminal.finish();
    assert_eq!(after.code, 0, "{}", after.json);
    assert_eq!(after.json["data"]["changed"], json!([]));
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
    given_back(&after);
}

#[test]
fn enter_turns_a_target_on_and_off_again() {
    let world = World::bare();
    world.configure("");
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    let terminal = world.at_terminal(&["settings"]);
    terminal.wait_for("❯ Enabled");
    terminal.press(TAB);
    terminal.wait_for("Runs go to Codex only while this is on.");
    terminal.press(ENTER);
    terminal.wait_for("Saved to config.toml: [harness.codex] enabled = true");
    assert!(
        fs::read_to_string(&file)
            .unwrap()
            .contains("harness.codex.enabled = true\n")
    );
    terminal.press(ENTER);
    terminal.wait_for("Took [harness.codex] enabled out of config.toml: back to off");
    terminal.press(ESC);
    let after = terminal.finish();
    assert_eq!(
        after.json["data"]["changed"],
        json!([{"key": "harness.codex.enabled", "value": false, "origin": "default"}])
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
}

#[test]
fn choosing_a_meter_writes_it_as_the_persons_choice() {
    let world = World::new();
    let terminal = world.at_terminal(&["settings"]);
    terminal.wait_for("❯ Enabled");
    terminal.press(TAB);
    terminal.press(TAB);
    terminal.wait_for("❯ Meter");
    terminal.press(ENTER);
    terminal.wait_for("● none ✓ (only cahoots' own runs-per-hour limit)");
    terminal.press(UP);
    terminal.wait_for("● ccusage (token counts");
    terminal.press(ENTER);
    terminal.wait_for("Saved to config.toml: [meter] use = \"ccusage\"");
    terminal.press(ESC);
    let after = terminal.finish();
    assert_eq!(after.code, 0, "{}", after.json);
    let text = fs::read_to_string(world.config.join("config.toml")).unwrap();
    assert!(text.contains("\n[meter]\nuse = \"ccusage\"\n"), "{text}");
}

#[test]
fn a_change_the_config_refuses_is_said_in_its_box_and_nothing_is_written() {
    let world = World::new();
    world.configure("harness.claude.abort_at = 85");
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    let terminal = world.at_terminal(&["settings"]);
    terminal.wait_for("❯ Enabled");
    terminal.press(DOWN);
    terminal.press(ENTER);
    terminal.wait_for("◀  75%  ▶");
    for shown in ["◀  80%  ▶", "◀  85%  ▶", "◀  90%  ▶"] {
        terminal.press(RIGHT);
        terminal.wait_for(shown);
    }
    terminal.press(ENTER);
    terminal.wait_for("must be above the cap (90)");
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
    terminal.press(ESC);
    terminal.wait_for("↑↓ move · tab section");
    terminal.press(ESC);
    let after = terminal.finish();
    assert_eq!(after.json["data"]["changed"], json!([]));
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
}

#[test]
fn the_page_is_drawn_again_at_a_new_size() {
    let world = World::new();
    let terminal = world.at_terminal(&["settings"]);
    terminal.wait_for(&format!("\x1b[2;1H\x1b[2K{}\x1b[3;1H", "─".repeat(100)));
    terminal.resize(30, 60);
    terminal.wait_for(&format!("\x1b[2;1H\x1b[2K{}\x1b[3;1H", "─".repeat(60)));
    terminal.wait_for("\x1b[30;1H");
    terminal.press(ESC);
    let after = terminal.finish();
    assert_eq!(after.code, 0, "{}", after.json);
    given_back(&after);
}

#[test]
fn a_terminal_that_answers_its_size_late_still_gets_the_page_at_its_size() {
    let world = World::new();
    // Far past the 300 ms the page waits: it is drawn at 80×24 first, and at
    // the terminal's own 100 columns once the answer comes.
    let terminal = world.at_slow_terminal(&["settings"], Duration::from_millis(600));
    terminal.wait_for(&format!("\x1b[2;1H\x1b[2K{}\x1b[3;1H", "─".repeat(100)));
    terminal.press(ESC);
    let after = terminal.finish();
    assert_eq!(after.code, 0, "{}", after.json);
    given_back(&after);
}

#[test]
fn a_config_that_does_not_parse_is_said_before_any_page() {
    let world = World::new();
    fs::write(
        world.config.join("config.toml"),
        "schema = 1\n[harness.codex\n",
    )
    .unwrap();
    let after = world.at_terminal(&["settings"]).finish();
    assert_eq!(after.code, 34, "{}", after.json);
    assert!(!after.screen.contains("\x1b[?1049h"), "{:?}", after.screen);
}

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
