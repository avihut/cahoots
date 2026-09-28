//! The human verbs as a person runs them: stdin, stdout and stderr on one
//! terminal, so each ends in words on the rail, and nothing on the screen is
//! JSON. Piped, the same verbs print the envelope byte for byte, and
//! `tests/cli.rs`, `tests/meters.rs` and `tests/settings.rs` hold that side.
//! The settings page and install's question are run this way beside their
//! other tests.

mod common;

use std::fs;

use common::{Finished, World, fake_at};

/// No envelope reached the screen.
fn no_json(after: &Finished) {
    let text = after.text();
    assert!(
        !text.contains("\"v\"") && !text.contains("\"class\""),
        "JSON on a person's screen:\n{text}"
    );
}

/// Every agent home, and both usage meters on PATH, so install looks for
/// nothing outside the world.
fn ready(world: &World) {
    for dir in [".claude", ".codex", ".agents/skills"] {
        fs::create_dir_all(world.home.join(dir)).unwrap();
    }
    fake_at(&world.bin.join("ccusage"));
    fake_at(&world.bin.join("usage-cli"));
    fs::write(
        world.bin.join("usage-cli.plan"),
        r#"{"unguarded": {"code": 0, "percent": 12}}"#,
    )
    .unwrap();
}

#[test]
fn install_ends_with_the_rules_to_paste_under_the_file_each_belongs_in() {
    let world = World::bare();
    world.configure("");
    ready(&world);
    let after = world
        .as_a_person(&["install", "--meter", "ccusage"])
        .finish();
    assert_eq!(after.code, 0, "{}", after.text());
    let text = after.text();
    for shown in [
        "┌  cahoots install\n│\n◇  Installed\n│  ~/.agents/skills/cahoots/SKILL.md\n",
        "●  Usage meter: ccusage (you named it).\n\
         │  Saved to config.toml: [meter] use = \"ccusage\"\n│\n",
        "▲  ccusage counts tokens and cannot see a plan's limit",
        "└  Add the rules below yourself: cahoots never edits a harness's permissions.\n",
        "\n\nAdd to ~/.claude/settings.json (permissions.allow):\n\"Bash(cahoots pick:*)\",\n",
        "\"Bash(cahoots review:*)\"\n\nAdd to ~/.codex/rules/default.rules:\n\
         prefix_rule(pattern=[\"cahoots\", \"pick\"], decision=\"allow\")\n",
    ] {
        assert!(text.contains(shown), "{shown:?} in:\n{text}");
    }
    assert!(
        text.ends_with("prefix_rule(pattern=[\"cahoots\", \"review\"], decision=\"allow\")\n"),
        "the rules end it, flush left:\n{text}"
    );
    no_json(&after);
    assert!(world.home.join(".claude/skills/cahoots/SKILL.md").is_file());
    let config = fs::read_to_string(world.config.join("config.toml")).unwrap();
    assert!(config.contains("use = \"ccusage\""), "{config}");
}

#[test]
fn uninstall_says_what_went_and_what_is_left_to_the_person() {
    let world = World::bare();
    world.configure("");
    ready(&world);
    let installed = world.as_a_person(&["install", "--meter", "none"]).finish();
    assert_eq!(installed.code, 0, "{}", installed.text());
    let after = world.as_a_person(&["uninstall"]).finish();
    assert_eq!(after.code, 0, "{}", after.text());
    let text = after.text();
    assert!(
        text.starts_with("┌  cahoots uninstall\n│\n◇  Removed\n│  ~/"),
        "{text}"
    );
    assert!(
        text.ends_with(
            "│\n└  Take out the permission rules you added: cahoots never edits them.\n"
        ),
        "{text}"
    );
    no_json(&after);
    assert!(!world.home.join(".claude/skills/cahoots/SKILL.md").exists());
    let again = world.as_a_person(&["uninstall"]).finish();
    assert_eq!(
        again.text(),
        "┌  cahoots uninstall\n│\n└  Nothing to remove: install has written nothing here.\n"
    );
}

#[test]
fn a_change_to_config_toml_is_said_with_the_line_the_settings_page_shows() {
    let world = World::bare();
    world.configure("");
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    for (args, said) in [
        (
            vec!["enable", "codex"],
            "┌  cahoots enable\n│\n└  Saved to config.toml: [harness.codex] enabled = true\n",
        ),
        (
            vec!["enable", "codex", "--off"],
            "┌  cahoots enable\n│\n\
             └  Took [harness.codex] enabled out of config.toml: back to off\n",
        ),
        (
            vec!["settings", "set", "harness.codex.cap", "60"],
            "┌  cahoots settings\n│\n└  Saved to config.toml: [harness.codex] cap = 60\n",
        ),
        (
            vec!["settings", "reset", "harness.codex.cap"],
            "┌  cahoots settings\n│\n\
             └  Took [harness.codex] cap out of config.toml: back to 75%\n",
        ),
    ] {
        let after = world.as_a_person(&args).finish();
        assert_eq!(after.code, 0, "{args:?}: {}", after.text());
        assert_eq!(after.text(), said, "{args:?}");
    }
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        before,
        "each change was undone"
    );
}

#[test]
fn a_refusal_closes_the_rail_in_red_with_the_same_exit_code() {
    let world = World::new();
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    let args = ["settings", "set", "harness.codex.cap", "101"];
    let after = world.as_a_person(&args).finish();
    assert_eq!(after.code, 2, "{}", after.text());
    assert_eq!(
        after.text(),
        "┌  cahoots settings\n│\n└  harness.codex.cap = 101: 1–100\n"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), before);

    // In color the last word is red; a terminal that shows no escape codes
    // gets none at all.
    let colored = world.as_a_person_with(&args, &[("NO_COLOR", "")]).finish();
    assert!(
        colored
            .screen
            .contains("\u{1b}[31mharness.codex.cap = 101: 1–100\u{1b}[0m"),
        "{:?}",
        colored.screen
    );
    let dumb = world
        .as_a_person_with(&args, &[("NO_COLOR", ""), ("TERM", "dumb")])
        .finish();
    assert_eq!(dumb.code, 2);
    assert!(!dumb.screen.contains('\u{1b}'), "{:?}", dumb.screen);
}

#[test]
fn a_bad_command_line_for_a_human_verb_is_clap_s_own_words_and_nothing_more() {
    let world = World::new();
    let after = world.as_a_person(&["enable", "gemini"]).finish();
    assert_eq!(after.code, 2, "{}", after.text());
    let text = after.text();
    assert!(text.starts_with("error: invalid value 'gemini'"), "{text}");
    assert!(!text.contains('┌'), "{text}");
    no_json(&after);
}

#[test]
fn agent_and_inspect_verbs_keep_the_envelope_even_at_a_terminal() {
    let world = World::new();
    for args in [vec!["status"], vec!["exit-codes"]] {
        let after = world.as_a_person(&args).finish();
        assert_eq!(after.code, 0, "{args:?}: {}", after.text());
        let text = after.text();
        let json: serde_json::Value = serde_json::from_str(text.trim())
            .unwrap_or_else(|error| panic!("{args:?}: {error}\n{text}"));
        assert_eq!(json["v"], 1, "{args:?}");
    }
}

#[test]
fn registry_lists_every_setting_in_effect_under_its_section() {
    let world = World::new();
    world.configure("harness.codex.cap = 60");
    let after = world.as_a_person(&["registry"]).finish();
    assert_eq!(after.code, 0, "{}", after.text());
    let text = after.text();
    for section in [
        "┌  cahoots registry\n│\n◇  Claude Code\n",
        "│\n◇  Codex\n",
        "│\n◇  Usage meter\n",
        "│\n◇  Runs\n",
        "│\n◇  Review\n",
        "│\n◇  Roles\n",
    ] {
        assert!(text.contains(section), "{section:?} in:\n{text}");
    }
    let codex = &text[text.find("◇  Codex\n").unwrap()..];
    let cap = codex
        .lines()
        .find(|line| line.starts_with("│  Usage cap"))
        .unwrap();
    assert!(cap.ends_with("• 60%"), "set in config.toml: {cap:?}");
    let advise = text
        .lines()
        .find(|line| line.starts_with("│  Advise "))
        .unwrap();
    assert!(advise.contains(", then "), "{advise:?}");
    assert!(text.contains("└  • set in"), "{text}");
    no_json(&after);
}
