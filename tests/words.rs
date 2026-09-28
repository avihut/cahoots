//! The verbs a person reads, as a person runs them: the human verbs,
//! `doctor` and `report`, with stdin, stdout and stderr on one terminal, so
//! each ends in words on the rail, and nothing on the screen is JSON. Piped,
//! the same verbs print the envelope byte for byte, and `tests/cli.rs`,
//! `tests/meters.rs`, `tests/settings.rs` and the tests that ask `doctor` and
//! `report` hold that side. The settings page and install's question are run
//! this way beside their other tests.

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
fn a_bad_command_line_for_a_verb_a_person_reads_is_clap_s_own_words_and_nothing_more() {
    let world = World::new();
    for (args, says) in [
        (vec!["enable", "gemini"], "error: invalid value 'gemini'"),
        (
            vec!["report", "--days", "abc"],
            "error: invalid value 'abc'",
        ),
        (
            vec!["doctor", "--bogus"],
            "error: unexpected argument '--bogus'",
        ),
    ] {
        let after = world.as_a_person(&args).finish();
        assert_eq!(after.code, 2, "{args:?}: {}", after.text());
        let text = after.text();
        assert!(text.starts_with(says), "{args:?}: {text}");
        assert!(!text.contains('┌'), "{args:?}: {text}");
        no_json(&after);
    }
}

#[test]
fn a_command_line_that_names_no_verb_is_clap_s_own_words_at_a_terminal() {
    let world = World::new();
    let bare = world.as_a_person(&[]).finish();
    assert_eq!(bare.code, 2, "{}", bare.text());
    let text = bare.text();
    assert!(
        text.contains("Usage: cahoots <COMMAND>\n\nCommands:\n"),
        "the help: {text}"
    );
    no_json(&bare);
    for (args, says) in [
        (vec!["instal"], "tip: some similar subcommands exist"),
        (
            vec!["--bogus"],
            "error: unexpected argument '--bogus' found",
        ),
    ] {
        let after = world.as_a_person(&args).finish();
        assert_eq!(after.code, 2, "{args:?}: {}", after.text());
        assert!(after.text().contains(says), "{args:?}: {}", after.text());
        no_json(&after);
    }
}

#[test]
fn a_bad_command_line_for_an_agent_verb_keeps_the_envelope_at_a_terminal() {
    let world = World::new();
    for (args, says) in [
        (
            vec!["run", "--role", "deploy", "--brief", "b.md"],
            "invalid value 'deploy'",
        ),
        (vec!["review"], "`cahoots review` needs a command"),
    ] {
        let after = world.as_a_person(&args).finish();
        assert_eq!(after.code, 2, "{args:?}: {}", after.text());
        let text = after.text();
        let at = text
            .find('{')
            .unwrap_or_else(|| panic!("{args:?}: no envelope after clap's words: {text}"));
        let json: serde_json::Value = serde_json::from_str(text[at..].trim())
            .unwrap_or_else(|error| panic!("{args:?}: {error}\n{text}"));
        assert_eq!(json["class"], "usage_error", "{args:?}");
        let message = json["message"].as_str().unwrap();
        assert!(message.contains(says), "{args:?}: {message}");
    }
}

#[test]
fn agent_verbs_and_the_other_inspect_verbs_keep_the_envelope_even_at_a_terminal() {
    let world = World::new();
    for args in [vec!["status"], vec!["exit-codes"], vec!["skill"]] {
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

/// The line of `text` that starts with `start`.
fn line<'a>(text: &'a str, start: &str) -> &'a str {
    text.lines()
        .find(|line| line.starts_with(start))
        .unwrap_or_else(|| panic!("no line starts with {start:?} in:\n{text}"))
}

#[test]
fn doctor_marks_every_check_and_ends_with_the_rules_to_paste() {
    let world = World::new();
    let after = world.as_a_person(&["doctor"]).finish();
    assert_eq!(after.code, 0, "{}", after.text());
    let text = after.text();
    assert!(text.starts_with("┌  cahoots doctor\n│\n"), "{text}");
    assert!(
        line(&text, "◇  config ").ends_with("  parses, and every value is in range"),
        "{text}"
    );
    for harness in ["claude", "codex"] {
        let rules = line(&text, &format!("▲  {harness}: caller rules "));
        assert!(
            rules.ends_with("  to delegate without a prompt, add the rules below"),
            "{rules:?}"
        );
    }
    // Every text starts in one column, after the widest label.
    let column = |start: &str| {
        line(&text, start)
            .find("  ")
            .map(|at| at + line(&text, start)[at..].find(|c: char| c != ' ').unwrap())
    };
    assert_eq!(column("◇  config "), column("▲  claude: caller rules "));
    // A dev build warns that it honours its overrides, so the count has a warning.
    let last = line(&text, "└  ");
    assert!(
        last.contains(" checks: ") && last.contains(" warning"),
        "{last:?}"
    );
    for shown in [
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
}

#[test]
fn a_failed_check_ends_doctor_in_red_with_its_exit_code() {
    let world = World::new();
    fs::write(
        world.config.join("config.toml"),
        "schema = 1\n[harness.codex\n",
    )
    .unwrap();
    let after = world
        .as_a_person_with(&["doctor"], &[("NO_COLOR", "")])
        .finish();
    assert_eq!(after.code, 34, "{}", after.text());
    let text = after.text();
    assert!(line(&text, "■  config ").contains("config.toml"), "{text}");
    let last = "3 checks: 1 passed, 1 warning, 1 failed";
    assert!(text.ends_with(&format!("│\n└  {last}\n")), "{text}");
    assert!(
        after.screen.contains("\u{1b}[31m■\u{1b}[0m")
            && after.screen.contains(&format!("\u{1b}[31m{last}\u{1b}[0m")),
        "{:?}",
        after.screen
    );
    no_json(&after);
}

#[test]
fn report_says_how_each_role_and_target_did() {
    let world = World::new();
    world.run("one", &[]);
    world.run("FAKE: fail", &[]);
    let after = world.as_a_person(&["report"]).finish();
    assert_eq!(after.code, 0, "{}", after.text());
    let text = after.text();
    assert!(
        text.starts_with(
            "┌  cahoots report\n│\n◇  advise · codex · gpt-6-astra · high\n\
             │  2 runs: 1 done, 1 failed\n│  Outcomes: 1 unknown\n│  Median time "
        ),
        "{text}"
    );
    assert!(
        text.ends_with("│\n└  2 runs in the last 30 days\n"),
        "{text}"
    );
    no_json(&after);

    let empty = World::new()
        .as_a_person(&["report", "--days", "1"])
        .finish();
    assert_eq!(
        empty.text(),
        "┌  cahoots report\n│\n└  No runs in the last day\n"
    );
}

#[test]
fn report_suggest_shows_each_role_s_order_with_its_evidence_and_the_swap() {
    let world = World::new();
    world.configure("[review]\nenabled = true");
    world.history_where_the_second_choice_does_better(8);
    let after = world.as_a_person(&["report", "--suggest"]).finish();
    assert_eq!(after.code, 0, "{}", after.text());
    let text = after.text();
    for shown in [
        "●  Routing: shadow — shown, not used (review.apply_routing = true to use it)\n\
         │  The rule: a candidate moves up ONE place",
        "◇  advise\n\
         │  1. codex gpt-6-astra high: score 0.00 from 8 runs (8 discarded)\n\
         │  2. claude opus high: score 1.00 from 8 runs (8 accepted)\n\
         │  claude opus high moves up past codex gpt-6-astra high: shown, not used\n",
        "◇  review\n│  1. ",
        "│  No change: the evidence does not support a change\n",
        "└  16 runs in the last 30 days\n",
    ] {
        assert!(text.contains(shown), "{shown:?} in:\n{text}");
    }
    no_json(&after);
}

#[test]
fn doctor_and_report_keep_the_envelope_when_stdin_is_not_a_terminal() {
    // A terminal on stdout alone proves nothing: an agent may run a
    // command with one.
    let world = World::new();
    for args in [vec!["doctor"], vec!["report"]] {
        let after = world.with_stdin_elsewhere(&args).finish();
        assert_eq!(after.code, 0, "{args:?}: {}", after.text());
        let text = after.text();
        let json: serde_json::Value = serde_json::from_str(text.trim())
            .unwrap_or_else(|error| panic!("{args:?}: {error}\n{text}"));
        assert_eq!(json["v"], 1, "{args:?}");
    }
    let refused = world
        .with_stdin_elsewhere(&["report", "--days", "abc"])
        .finish();
    assert_eq!(refused.code, 2, "{}", refused.text());
    let text = refused.text();
    let at = text
        .find('{')
        .unwrap_or_else(|| panic!("no envelope after clap's words: {text}"));
    let json: serde_json::Value = serde_json::from_str(text[at..].trim()).unwrap();
    assert_eq!(json["class"], "usage_error");
}
