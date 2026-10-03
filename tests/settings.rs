//! `cahoots settings`, end to end at a terminal of its own, against a world
//! of throwaway directories: the page, one setting at a time from the command
//! line, and what reaches config.toml.

mod common;

use std::fs;
use std::time::Duration;

use common::{Finished, World, fake_at};
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
fn closed_as_a_person_the_page_says_each_change_it_saved_or_that_nothing_changed() {
    let world = World::new();
    let terminal = world.as_a_person(&["settings"]);
    terminal.wait_for("❯ Enabled");
    terminal.press(DOWN);
    terminal.wait_for("❯ Usage cap");
    terminal.press(ENTER);
    terminal.wait_for("◀  75%  ▶");
    terminal.press(LEFT);
    terminal.wait_for("◀  70%  ▶");
    terminal.press(ENTER);
    terminal.wait_for("Saved to config.toml: [harness.claude] cap = 70");
    terminal.press(TAB);
    terminal.wait_for("Runs go to Codex only while this is on.");
    terminal.press(ENTER);
    terminal.wait_for("Took [harness.codex] enabled out of config.toml: back to off");
    terminal.press(ESC);
    let after = terminal.finish();
    assert_eq!(after.code, 0, "{}", after.text());
    assert!(after.json.is_null(), "no JSON on a person's stdout");
    assert!(
        after.text().ends_with(
            "┌  cahoots settings\n│\n\
             │  Saved to config.toml: [harness.claude] cap = 70\n\
             │  Took [harness.codex] enabled out of config.toml: back to off\n\
             │\n└  2 settings changed\n"
        ),
        "{}",
        after.text()
    );
    given_back(&after);

    let terminal = world.as_a_person(&["settings"]);
    terminal.wait_for("❯ Enabled");
    terminal.press(ESC);
    let after = terminal.finish();
    assert_eq!(after.code, 0);
    assert!(
        after
            .text()
            .ends_with("┌  cahoots settings\n│\n└  Nothing changed\n"),
        "{}",
        after.text()
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

const TASK_KINDS: &str = r#"# my kinds
[kinds.rust-review]
description = "Review Rust." # my description
role = "review"
candidates = [{ harness = "codex", model = "custom", effort = "high" }, { harness = "codex", model = "custom", effort = "medium" }]
[kinds.zed]
description = "Sibling."
role = "advise"
candidates = [{ harness = "claude", model = "other", effort = "low" }]
"#;

#[test]
fn kind_fields_are_set_without_changing_neighboring_config() {
    let world = World::new();
    world.configure(TASK_KINDS);
    let file = world.config.join("config.toml");
    for (leaf, value) in [
        ("description", "My changed words."),
        ("role", "explore"),
        ("candidates", "claude:new:low,codex:custom:medium"),
    ] {
        let key = format!("kinds.rust-review.{leaf}");
        let after = world
            .at_terminal(&["settings", "set", &key, value])
            .finish();
        assert_eq!(after.code, 0, "{}", after.json);
        assert_eq!(
            after.json["data"]["changed"],
            json!([{"key": key, "value": value, "origin": "config"}])
        );
    }
    let before = fs::read_to_string(&file).unwrap();
    assert!(before.contains("# my kinds") && before.contains("# my description"));
    assert!(before.contains(&TASK_KINDS[TASK_KINDS.find("[kinds.zed]").unwrap()..]));
    for (key, value) in [
        ("kinds.rust-review.description", "bad\ntext"),
        ("kinds.rust-review.role", "deploy"),
        ("kinds.rust-review.candidates", "codex:m:high,codex:m:high"),
        ("kinds.missing.role", "review"),
        ("kinds.rust-review.cap", "100"),
    ] {
        let after = world.at_terminal(&["settings", "set", key, value]).finish();
        assert_eq!(after.code, 2, "{}", after.json);
        assert_eq!(fs::read_to_string(&file).unwrap(), before);
    }
    for leaf in ["description", "role", "candidates"] {
        let missing = format!("kinds.missing.{leaf}");
        let after = world.at_terminal(&["settings", "reset", &missing]).finish();
        assert_eq!(after.code, 2, "{}", after.json);
        assert_eq!(
            after.json["message"],
            "unknown task kind \"missing\" — define description, role and candidates together in [kinds.missing] in config.toml first"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), before);
        let key = format!("kinds.rust-review.{leaf}");
        let after = world.at_terminal(&["settings", "reset", &key]).finish();
        assert_eq!(after.code, 2, "{}", after.json);
        assert_eq!(
            after.json["message"],
            format!(
                "{key} is required for this task kind — remove [kinds.rust-review] from config.toml to remove the kind"
            )
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), before);
    }
    let person = world
        .as_a_person(&[
            "settings",
            "set",
            "kinds.rust-review.description",
            "Person's words.",
        ])
        .finish();
    assert_eq!(person.code, 0);
    assert!(person.json.is_null());
    assert!(
        person.text().contains(
            "Saved to config.toml: [kinds.rust-review] description = \"Person's words.\""
        )
    );
}

#[test]
fn the_page_edits_a_kind_role_and_candidate_order() {
    for person in [false, true] {
        let world = World::new();
        world.configure(TASK_KINDS);
        let terminal = if person {
            world.as_a_person(&["settings"])
        } else {
            world.at_terminal(&["settings"])
        };
        terminal.wait_for("❯ Enabled");
        terminal.resize(40, 180);
        // Claude, Codex, meter, runs, worktrees, review, roles, then the
        // first kind.
        for _ in 0..7 {
            terminal.press(TAB);
        }
        terminal.wait_for("❯ Description");
        terminal.wait_for("Change it with settings set kinds.rust-review.description");
        terminal.press(ENTER);
        terminal.press(DOWN);
        terminal.wait_for("❯ Role");
        terminal.press(ENTER);
        terminal.wait_for("● review ✓ (find problems; read-only)");
        terminal.press(DOWN);
        terminal.press(ENTER);
        terminal.wait_for("Saved to config.toml: [kinds.rust-review] role = \"explore\"");
        terminal.press(DOWN);
        terminal.wait_for("❯ Candidates");
        terminal.press(ENTER);
        terminal.wait_for("codex  custom  high");
        terminal.wait_for("codex  custom  medium");
        terminal.press(b" ");
        terminal.press(DOWN);
        terminal.press(b" ");
        terminal.press(ENTER);
        terminal.wait_for(
            "Saved to config.toml: [kinds.rust-review] candidates, codex custom medium first",
        );
        terminal.press(ESC);
        let after = terminal.finish();
        assert_eq!(after.code, 0, "{}", after.text());
        given_back(&after);
        let config = cahoots::config::UserConfig::load(&world.config.join("config.toml")).unwrap();
        let key = cahoots::model::TaskKindName::try_from("rust-review".to_string()).unwrap();
        let entry = &config.kinds[&key];
        assert_eq!(entry.description, "Review Rust.");
        assert_eq!(entry.role, cahoots::model::Role::Explore);
        assert_eq!(entry.candidates[0].effort, cahoots::model::Effort::Medium);
        assert_eq!(entry.candidates[1].effort, cahoots::model::Effort::High);
        if person {
            assert!(after.json.is_null());
            assert!(after.text().contains("2 settings changed"));
        } else {
            assert_eq!(after.json["data"]["changed"].as_array().unwrap().len(), 2);
        }
    }
}

#[test]
fn blind_setting_toggles_sets_and_resets_without_touching_other_config() {
    use cahoots::settings::{Key, Origin, Value, current};
    let world = World::new();
    world.configure(&format!(
        "review.enabled = false\n# keep this comment\n{TASK_KINDS}"
    ));
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    let row = || {
        let config = cahoots::config::UserConfig::load(&file).unwrap();
        current(&config, None, None)
            .into_iter()
            .find(|s| s.key == Key::Blind)
            .unwrap()
    };
    let default = row();
    assert_eq!(default.value, Some(Value::Bool(false)));
    assert_eq!(default.origin, Origin::Default);
    assert!(!default.locked);
    let terminal = world.at_terminal(&["settings"]);
    terminal.wait_for("❯ Enabled");
    // Claude, Codex, meter, runs, worktrees, then review.
    for _ in 0..5 {
        terminal.press(TAB);
    }
    terminal.wait_for("❯ Review runs");
    terminal.press(DOWN);
    terminal.wait_for("❯ Blind runs");
    terminal.resize(30, 180);
    terminal.wait_for("Hide the model and effort until the run’s outcome is recorded, so the answer is judged before its author is known.");
    terminal.press(ENTER);
    terminal.wait_for("Saved to config.toml: [review] blind = true");
    terminal.press(ESC);
    let after = terminal.finish();
    assert_eq!(after.code, 0, "{}", after.json);
    assert_eq!(
        after.json["data"]["changed"],
        json!([{"key": "review.blind", "value": true, "origin": "config"}])
    );
    given_back(&after);
    assert_eq!(row().value, Some(Value::Bool(true)));
    assert_eq!(row().origin, Origin::Config);
    assert!(!row().locked);
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("# keep this comment"));
    assert!(text.contains(TASK_KINDS));
    let reset = world
        .at_terminal(&["settings", "reset", "review.blind"])
        .finish();
    assert_eq!(reset.code, 0, "{}", reset.json);
    assert_eq!(
        reset.json["data"]["changed"],
        json!([{"key": "review.blind", "value": false, "origin": "default"}])
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
    for value in ["true", "false"] {
        let set = world
            .at_terminal(&["settings", "set", "review.blind", value])
            .finish();
        assert_eq!(set.code, 0, "{}", set.json);
        assert_eq!(row().value, Some(Value::Bool(value == "true")));
        assert_eq!(row().origin, Origin::Config);
    }
    assert_eq!(
        world
            .at_terminal(&["settings", "reset", "review.blind"])
            .finish()
            .code,
        0
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
}

#[test]
fn blind_setting_rejects_nonbooleans_and_remains_human_only() {
    let world = World::new();
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    let invalid = world
        .at_terminal(&["settings", "set", "review.blind", "maybe"])
        .finish();
    assert_eq!(invalid.code, 2, "{}", invalid.json);
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
    // Policy through its pure refusal function, with isolated configuration.
    for args in [
        vec!["settings"],
        vec!["settings", "set", "review.blind", "true"],
        vec!["settings", "reset", "review.blind"],
    ] {
        let mut argv = vec!["cahoots"];
        argv.extend(args);
        let cli = <cahoots::cli::Cli as clap::Parser>::try_parse_from(argv).unwrap();
        assert_eq!(
            cahoots::cli::refusal(&cli.verb, false).unwrap().exit.code(),
            33
        );
    }
    for value in ["'true'", "1"] {
        world.configure(&format!("review.blind = {value}"));
        let config_before = fs::read_to_string(&file).unwrap();
        let error = world.at_terminal(&["settings"]).finish();
        assert_eq!(error.code, 34, "{}", error.json);
        assert_eq!(error.json["class"], "config_error");
        assert_eq!(error.json["retry"], "fix_config");
        assert_eq!(fs::read_to_string(&file).unwrap(), config_before);
        assert_eq!(world.run("answer", &[]).code, 34);
        assert!(!world.state.join("runs").exists());
    }
}

/// config.toml with its blank lines folded away: a table taken out of the
/// file leaves the blank line that was above it, above whatever follows.
fn squeezed(file: &std::path::Path) -> String {
    fs::read_to_string(file).unwrap().replace("\n\n", "\n")
}

#[test]
fn exploration_shares_set_and_reset_without_changing_neighboring_config() {
    let world = World::new();
    world.configure(&format!(
        "[review]\n# Kept by hand.\nblind = true # mine\n{TASK_KINDS}"
    ));
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    let run = |args: &[&str]| {
        let mut argv = vec!["settings"];
        argv.extend(args);
        let done = world.at_terminal(&argv).finish();
        assert_eq!(done.code, 0, "{args:?}: {}", done.json);
        done.json["data"]["changed"].clone()
    };
    assert_eq!(
        run(&["set", "explore.share.review", "0.25"]),
        json!([{"key": "explore.share.review", "value": 0.25, "origin": "config"}])
    );
    // The kind inherits it until it has a share of its own, zero included.
    assert_eq!(
        run(&["set", "kinds.rust-review.explore.share", "0"]),
        json!([{"key": "kinds.rust-review.explore.share", "value": 0.0, "origin": "config"}])
    );
    let text = fs::read_to_string(&file).unwrap();
    for kept in [
        "# Kept by hand.",
        "blind = true # mine",
        "# my kinds",
        "# my description",
        "[kinds.zed]",
    ] {
        assert!(text.contains(kept), "{kept}: {text}");
    }
    let config = cahoots::config::UserConfig::load(&file).unwrap();
    assert_eq!(config.explore.share[&cahoots::model::Role::Review], 0.25);
    let kind = config
        .kinds
        .values()
        .find(|k| k.description == "Review Rust.")
        .unwrap();
    assert_eq!((kind.explore.share, kind.candidates.len()), (Some(0.0), 2));
    // Resetting the kind restores inheritance — its role's share is the
    // default it shows — and keeps its description, role and candidates.
    assert_eq!(
        run(&["reset", "kinds.rust-review.explore.share"]),
        json!([{"key": "kinds.rust-review.explore.share", "value": 0.25, "origin": "default"}])
    );
    let config = cahoots::config::UserConfig::load(&file).unwrap();
    let kind = config
        .kinds
        .values()
        .find(|k| k.description == "Review Rust.")
        .unwrap();
    assert_eq!((kind.explore.share, kind.candidates.len()), (None, 2));
    // A role's reset puts zero back, and takes only its own share.
    assert_eq!(
        run(&["reset", "explore.share.review"]),
        json!([{"key": "explore.share.review", "value": 0.0, "origin": "default"}])
    );
    assert_eq!(squeezed(&file), before.replace("\n\n", "\n"));
}

#[test]
fn exploration_share_edits_refuse_invalid_values_and_unknown_kinds() {
    let world = World::new();
    world.configure(TASK_KINDS);
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    for (key, value) in [
        ("explore.share.advise", "-0.1"),
        ("explore.share.advise", "1.5"),
        ("explore.share.advise", "nan"),
        ("explore.share.advise", "inf"),
        ("explore.share.advise", "-inf"),
        ("explore.share.advise", "50%"),
        ("explore.share.advise", "half"),
        ("kinds.rust-review.explore.share", "2"),
        ("kinds.rust-review.explore.share", "nan"),
        ("explore.share.deploy", "0.5"),
        ("kinds.missing.explore.share", "0.5"),
    ] {
        let after = world.at_terminal(&["settings", "set", key, value]).finish();
        assert_eq!(after.code, 2, "{key} {value}: {}", after.json);
        assert_eq!(after.json["class"], "usage_error");
        assert_eq!(fs::read_to_string(&file).unwrap(), before, "{key} {value}");
    }
    let after = world
        .at_terminal(&["settings", "set", "kinds.missing.explore.share", "0.5"])
        .finish();
    assert!(
        after.json["message"]
            .as_str()
            .unwrap()
            .starts_with("unknown task kind \"missing\""),
        "{}",
        after.json
    );
    let after = world
        .at_terminal(&["settings", "reset", "kinds.missing.explore.share"])
        .finish();
    assert_eq!(after.code, 2, "{}", after.json);
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
    // A stored value out of range is a configuration error, whoever wrote it.
    world.configure("[explore.share]\nadvise = 1.5");
    let stored = world.ask(&["pick", "--role", "advise"]);
    assert_eq!(stored.code, 34, "{}", stored.json);
    assert_eq!(stored.json["class"], "config_error");
    assert_eq!(stored.json["retry"], "fix_config");
    assert!(
        stored
            .message()
            .ends_with("explore.share.advise = 1.5: must be 0–1"),
        "{}",
        stored.message()
    );
    let page = world.at_terminal(&["settings"]).finish();
    assert_eq!(page.code, 34, "{}", page.json);
    assert_eq!(world.run("answer", &[]).code, 34);
    assert!(!world.state.join("runs").exists());
    // Not at a terminal that can draw it, the page names its flags.
    let dumb = world
        .at_terminal_with(&["settings"], &[("TERM", "dumb")])
        .finish();
    assert_ne!(dumb.code, 0);
}

#[test]
fn the_page_edits_role_and_kind_exploration_shares() {
    let world = World::new();
    world.configure(TASK_KINDS);
    let file = world.config.join("config.toml");
    let terminal = world.at_terminal(&["settings"]);
    terminal.wait_for("❯ Enabled");
    terminal.resize(40, 180);
    // Claude, Codex, meter, runs, worktrees, review: then the roles.
    for _ in 0..6 {
        terminal.press(TAB);
    }
    terminal.wait_for("❯ Advise");
    terminal.press(DOWN);
    terminal.wait_for("❯ Exploration share: advise");
    terminal.wait_for(
        "The share of new runs that try the next listed candidate first. Every candidate passes the usual checks. Does not apply with --to or resume.",
    );
    terminal.press(ENTER);
    terminal.wait_for("◀  0%  ▶");
    terminal.press(RIGHT);
    terminal.press(RIGHT);
    terminal.wait_for("◀  10%  ▶");
    terminal.press(ENTER);
    terminal.wait_for("Saved to config.toml: [explore.share] advise = 0.1");
    // Reset puts the default back, which takes the key out.
    terminal.press(ENTER);
    terminal.wait_for("◀  10%  ▶");
    terminal.press(b"r");
    terminal.wait_for("◀  0%  ▶");
    terminal.press(ENTER);
    terminal.wait_for("Took [explore.share] advise out of config.toml: back to 0%");
    // Next to the kind: its share is inherited until it is set.
    terminal.press(TAB);
    terminal.wait_for("❯ Description");
    for _ in 0..3 {
        terminal.press(DOWN);
    }
    terminal.wait_for("❯ Exploration share");
    terminal.wait_for(
        "Inherited from this kind’s role. Set a share to override it; reset to inherit again.",
    );
    terminal.press(ENTER);
    terminal.press(RIGHT);
    terminal.wait_for("◀  5%  ▶");
    terminal.press(ENTER);
    terminal.wait_for("Saved to config.toml: [kinds.rust-review.explore] share = 0.05");
    terminal.press(ESC);
    let after = terminal.finish();
    assert_eq!(after.code, 0, "{}", after.text());
    given_back(&after);
    assert_eq!(
        after.json["data"]["changed"],
        json!([
            {"key": "explore.share.advise", "value": 0.0, "origin": "default"},
            {"key": "kinds.rust-review.explore.share", "value": 0.05, "origin": "config"},
        ])
    );
    let config = cahoots::config::UserConfig::load(&file).unwrap();
    assert!(config.explore.share.is_empty());
    let kind = config
        .kinds
        .values()
        .find(|k| k.description == "Review Rust.")
        .unwrap();
    assert_eq!((kind.explore.share, kind.candidates.len()), (Some(0.05), 2));
    // Human-only, through the pure refusal and never by running a human verb
    // against anything real.
    for args in [
        vec!["settings", "set", "explore.share.advise", "0.1"],
        vec!["settings", "reset", "kinds.rust-review.explore.share"],
    ] {
        let mut argv = vec!["cahoots"];
        argv.extend(args);
        let cli = <cahoots::cli::Cli as clap::Parser>::try_parse_from(argv).unwrap();
        assert_eq!(
            cahoots::cli::refusal(&cli.verb, false).unwrap().exit.code(),
            33
        );
    }
}

/// `settings set <key> <value>`, at a terminal as a human verb needs.
fn set(world: &World, key: &str, value: &str) -> Finished {
    world.at_terminal(&["settings", "set", key, value]).finish()
}

#[test]
fn fork_settings_are_set_and_reset_through_the_flags() {
    let world = World::new();
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    let daft = world.bin.join("daft");
    fake_at(&daft);

    for (key, value, shown) in [
        ("fork.provider", "daft", json!("daft")),
        ("fork.daft.hooks", "on", json!(true)),
        ("fork.daft.binary", daft.to_str().unwrap(), json!(daft)),
    ] {
        let after = set(&world, key, value);
        assert_eq!(after.code, 0, "{key}: {}", after.json);
        assert_eq!(
            after.json["data"]["changed"],
            json!([{"key": key, "value": shown, "origin": "config"}]),
            "{key}"
        );
    }
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.starts_with(before.trim_end()), "{text}");
    assert!(
        text.contains(&format!(
            "\n[fork]\nprovider = \"daft\"\n\n[fork.daft]\nhooks = true\nbinary = {daft:?}\n"
        )),
        "{text}"
    );
    let registry =
        cahoots::registry::Registry::effective(&cahoots::config::UserConfig::load(&file).unwrap());
    assert_eq!(
        registry.fork.provider,
        cahoots::placement::provider::ProviderId::Daft
    );
    assert_eq!(registry.fork.daft_binary.as_deref(), Some(daft.as_path()));
    assert!(registry.fork.daft_hooks);
    // Asked its version once, when it was chosen; nothing else of it ran.
    assert_eq!(world.daft_versions().len(), 1);
    assert!(world.daft_calls().is_empty());

    for (key, back) in [
        ("fork.provider", json!("git")),
        ("fork.daft.hooks", json!(false)),
        ("fork.daft.binary", json!(null)),
    ] {
        let reset = world.at_terminal(&["settings", "reset", key]).finish();
        assert_eq!(reset.code, 0, "{key}: {}", reset.json);
        assert_eq!(
            reset.json["data"]["changed"],
            json!([{"key": key, "value": back, "origin": "default"}]),
            "{key}"
        );
    }
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
}

#[test]
fn a_provider_that_is_not_one_is_refused() {
    let world = World::new();
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    let after = set(&world, "fork.provider", "worktrunk");
    assert_eq!(after.code, 2, "{}", after.json);
    assert_eq!(
        after.json["message"],
        "fork.provider = worktrunk: one of git, daft"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
}

#[test]
fn a_daft_program_that_is_not_there_or_not_daft_is_refused() {
    let world = World::new();
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    let old = world.root.join("old/daft");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fake_at(&old);
    fs::write(world.root.join("old/daft.version"), "daft 1.20.0\n").unwrap();
    let missing = world.root.join("nope/daft");
    let codex = world.bin.join("codex");
    for (value, says) in [
        (
            "daft".to_string(),
            "fork.daft.binary = daft: an absolute path".to_string(),
        ),
        (
            missing.display().to_string(),
            format!("fork.daft.binary = {}: ", missing.display()),
        ),
        (
            codex.display().to_string(),
            format!("{} does not identify itself as daft", codex.display()),
        ),
        (
            old.display().to_string(),
            "daft 1.20.0 is older than the oldest version cahoots supports (1.27.0)".to_string(),
        ),
    ] {
        let after = set(&world, "fork.daft.binary", &value);
        assert_eq!(after.code, 2, "{value}: {}", after.json);
        let message = after.json["message"].as_str().unwrap();
        assert!(message.contains(&says), "{value}: {message}");
        if value == missing.display().to_string() {
            assert!(message.contains("No such file"), "{message}");
        }
        assert_eq!(fs::read_to_string(&file).unwrap(), before, "{value}");
    }
}

#[test]
fn a_config_naming_an_unknown_provider_is_refused() {
    let world = World::new();
    world.fork("fork.provider = \"worktrunk\"");
    let run = world.run("hello", &[]);
    assert_eq!(run.code, 34, "{}", run.json);
    assert!(run.message().contains("worktrunk"), "{}", run.json);
    assert!(!world.state.join("runs").exists(), "a run was created");
    let doctor = world.ask(&["doctor"]);
    assert_eq!(doctor.code, 34, "{}", doctor.json);
    let config = doctor.data()["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "config")
        .unwrap()
        .clone();
    assert_eq!(config["status"], "fail", "{config}");
    assert!(
        config["detail"].as_str().unwrap().contains("worktrunk"),
        "{config}"
    );
}

#[test]
fn the_page_chooses_daft_in_the_worktrees_section() {
    let world = World::new();
    let terminal = world.at_terminal(&["settings"]);
    terminal.wait_for("❯ Enabled");
    terminal.resize(30, 180);
    // Claude, Codex, meter, runs, then worktrees.
    for _ in 0..4 {
        terminal.press(TAB);
    }
    terminal.wait_for("❯ Cut by");
    terminal.wait_for("What cuts a writer's worktree.");
    terminal.press(ENTER);
    terminal.wait_for("● git ✓ (a detached worktree in cahoots' state directory");
    terminal.press(DOWN);
    terminal.wait_for("● daft (daft's own layout, where the repository has a daft.yml");
    terminal.press(ENTER);
    terminal.wait_for("Saved to config.toml: [fork] provider = \"daft\"");
    terminal.press(ESC);
    let after = terminal.finish();
    assert_eq!(after.code, 0, "{}", after.json);
    assert_eq!(
        after.json["data"]["changed"],
        json!([{"key": "fork.provider", "value": "daft", "origin": "config"}])
    );
    given_back(&after);
    let text = fs::read_to_string(world.config.join("config.toml")).unwrap();
    assert!(text.contains("\n[fork]\nprovider = \"daft\"\n"), "{text}");
}
