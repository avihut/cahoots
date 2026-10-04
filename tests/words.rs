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
        "●  The programs cahoots runs, from your PATH\n│  git: ",
        "│  Your PATH: recorded, 1 folder\n│\n",
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
    // A PATH recorded already: `enable` records one only when none is.
    world.configure(&format!("tools.path = {:?}", world.bin));
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
    world.git_on_path();
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
fn doctor_without_a_git_fails_and_says_why_in_plain_words() {
    // No git pinned: one on PATH changes nothing.
    let world = World::new();
    world.pin_tool("git", None);
    let after = world.as_a_person(&["doctor"]).finish();
    assert_eq!(after.code, 34, "{}", after.text());
    let text = after.text();
    assert!(
        line(&text, "■  tools: git ").contains("cahoots needs `git`: none is pinned"),
        "{text}"
    );
    assert!(
        line(&text, "■  fork ").contains("without its git"),
        "{text}"
    );
    // Nor is a harness asked its version without it.
    assert!(
        line(&text, "■  claude: binary ").contains("until git is pinned"),
        "{text}"
    );
    // However the rail wraps it.
    let flat = text
        .split_whitespace()
        .filter(|word| *word != "│")
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        flat.contains(
            "run `cahoots install` (it pins the git on your PATH), or choose one with \
             `cahoots settings` (tools.git.binary)"
        ),
        "{text}"
    );
    let last = line(&text, "└  ");
    assert!(last.ends_with(", 4 failed"), "{last:?}");
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

/// The rail's lines joined into one, so a phrase the rail wrapped reads whole.
fn unwrapped(text: &str) -> String {
    text.replace("\n│  ", " ")
}

#[test]
fn report_suggest_shows_each_role_s_order_with_its_evidence_and_the_swap() {
    let world = World::new();
    world.configure("[review]\nenabled = true");
    world.history_where_the_second_choice_does_better(8);
    let after = world.as_a_person(&["report", "--suggest"]).finish();
    assert_eq!(after.code, 0, "{}", after.text());
    let text = unwrapped(&after.text());
    for shown in [
        "●  Routing: shadow — shown, not used (review.apply_routing = true to use it) \
         The rule: a candidate moves up ONE place",
        // Eight is the floor: the score comes with its error, and so do the shares.
        "1. codex gpt-6-astra high: score 0.00 (standard error 0.00) from 8 runs (8 discarded) \
         Rates from 8 rated or failed runs: accepted 0.0% (standard error 0.0 percentage points), \
         reworked 0.0% (standard error 0.0 percentage points), discarded 100.0% \
         (standard error 0.0 percentage points), failed 0.0% \
         (standard error 0.0 percentage points) 2. claude opus high: \
         score 1.00 (standard error 0.00) from 8 runs (8 accepted) \
         Rates from 8 rated or failed runs: accepted 100.0%",
        "claude opus high moves up past codex gpt-6-astra high: shown, not used",
        // Candidates nobody has rated yet are not judged.
        "1. codex gpt-5.6-sol high: not enough evidence (0 rated or failed runs; need 8)",
        "No change: the evidence does not support a change",
        "└  16 runs in the last 30 days",
    ] {
        assert!(text.contains(shown), "{shown:?} in:\n{text}");
    }
    no_json(&after);
}

/// A history with these runs of one kind (codex custom medium, review), in
/// order: each a state and an outcome, `""` for none.
fn history_of_kind(world: &World, kind: &str, runs: &[(&str, &str)]) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut lines = String::new();
    for (n, (state, outcome)) in runs.iter().enumerate() {
        let run = format!("0198c0de-0000-7000-8000-{n:012}");
        lines.push_str(&format!(
            "{{\"kind\":\"finished\",\"t\":{now},\"run\":\"{run}\",\"role\":\"review\",\
             \"task_kind\":\"{kind}\",\"caller\":null,\"target\":{{\"harness\":\"codex\",\
             \"model\":\"custom\",\"effort\":\"medium\"}},\"dir\":\"/w\",\"state\":\"{state}\",\
             \"exit\":0,\"tokens_in\":1,\"tokens_out\":1,\"secs\":1,\"sampled\":false}}\n"
        ));
        if !outcome.is_empty() {
            lines.push_str(&format!(
                "{{\"kind\":\"outcome\",\"t\":{now},\"run\":\"{run}\",\"outcome\":\"{outcome}\"}}\n"
            ));
        }
    }
    fs::create_dir_all(&world.state).unwrap();
    fs::write(world.state.join("history.jsonl"), lines).unwrap();
}

const KIND_CONFIG: &str = "[kinds.rust-review]\ndescription = \"Review Rust.\"\nrole = \"review\"\n\
     candidates = [{ harness = \"codex\", model = \"custom\", effort = \"medium\" }, \
     { harness = \"codex\", model = \"custom\", effort = \"high\" }]\n";

#[test]
fn report_words_show_errors_or_insufficient_evidence() {
    let world = World::new();
    world.configure(KIND_CONFIG);
    // Seven rated runs, then eight.
    let seven: Vec<(&str, &str)> = vec![("done", "accepted"); 7];
    history_of_kind(&world, "rust-review", &seven);
    let below = world.as_a_person(&["report"]).finish();
    assert_eq!(below.code, 0, "{}", below.text());
    let text = unwrapped(&below.text());
    assert!(
        text.contains("◇  Kind · rust-review · review · codex · custom · medium"),
        "{text}"
    );
    assert!(
        text.contains("not enough evidence (7 rated or failed runs; need 8)"),
        "{text}"
    );
    assert!(
        !text.contains('%') && !text.contains("Score") && !text.contains("standard error"),
        "no estimate below the floor:\n{text}"
    );
    // The kind's other candidate has no runs: a line saying so, with no cost,
    // and no "0 runs:" left dangling.
    assert!(
        text.contains(
            "◇  Kind · rust-review · review · codex · custom · high 0 runs \
             not enough evidence (0 rated or failed runs; need 8)"
        ),
        "{text}"
    );
    assert!(!text.contains("0 runs:"), "{text}");
    // The count at the end is of unique runs and ignores the seeded rows.
    assert!(text.ends_with("└  7 runs in the last 30 days\n"), "{text}");
    no_json(&below);

    let eight: Vec<(&str, &str)> =
        [vec![("done", "accepted"); 4], vec![("failed", ""); 4]].concat();
    history_of_kind(&world, "rust-review", &eight);
    let above = world.as_a_person(&["report"]).finish();
    let text = unwrapped(&above.text());
    assert!(
        text.contains(
            "Rates from 8 rated or failed runs: accepted 50.0% (standard error 17.7 \
             percentage points), reworked 0.0% (standard error 0.0 percentage points), \
             discarded 0.0% (standard error 0.0 percentage points), failed 50.0% \
             (standard error 17.7 percentage points) Score 0.50 (standard error 0.19)"
        ),
        "{text}"
    );

    // Configured rows with no runs at all: the final sentence is unchanged.
    let idle = World::new();
    idle.configure(KIND_CONFIG);
    let none = idle.as_a_person(&["report", "--days", "1"]).finish();
    let text = none.text();
    assert!(text.contains("◇  Kind · rust-review"), "{text}");
    assert!(text.ends_with("└  No runs in the last day\n"), "{text}");
}

#[test]
fn doctor_and_report_keep_the_envelope_when_stdin_is_not_a_terminal() {
    // A terminal on stdout alone proves nothing: an agent may run a
    // command with one.
    let world = World::new();
    world.git_on_path();
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

#[test]
fn registry_shows_kind_sections_and_efforts() {
    let world = World::new();
    world.configure(r#"[kinds.zed]
description = "Last task."
role = "advise"
candidates = [{ harness = "claude", model = "other", effort = "low" }]
[kinds.alpha]
description = "First task."
role = "review"
candidates = [{ harness = "codex", model = "custom", effort = "high" }, { harness = "codex", model = "custom", effort = "medium" }]
"#);
    let after = world.as_a_person(&["registry"]).finish();
    assert_eq!(after.code, 0);
    no_json(&after);
    let text = after.text();
    assert!(text.find("◇  Roles").unwrap() < text.find("◇  Kind · alpha").unwrap());
    assert!(text.find("◇  Kind · alpha").unwrap() < text.find("◇  Kind · zed").unwrap());
    for shown in [
        "Description",
        "Role",
        "Candidates",
        "First task.",
        "codex custom high, then codex custom medium",
        "claude other low",
    ] {
        assert!(text.contains(shown), "{text}");
    }
    let piped = world.at_terminal(&["registry"]).finish();
    assert_eq!(
        piped.json["data"]["kinds"]["alpha"]["description"],
        "First task."
    );
    assert_eq!(
        piped.json["data"]["kinds"]["alpha"]["candidates"][1]["effort"],
        "medium"
    );
    world.configure("");
    assert_eq!(
        world.at_terminal(&["registry"]).finish().json["data"]["kinds"],
        serde_json::json!({})
    );
    assert!(
        !world
            .as_a_person(&["registry"])
            .finish()
            .text()
            .contains("Kind ·")
    );
}

/// A tracked change to the solution, a tracked change to a test, and a new,
/// untracked test.
const EVALS_BRIEF: &str = "FAKE: append=src/lib.rs::pub fn b() {}\\n\n\
                           FAKE: append=tests/old_test.rs::#[test] fn more() {}\\n\n\
                           FAKE: append=tests/new_test.rs::#[test] fn new() {}\\n\n";

/// A person's screen with the rail's wrapped lines run together, so a
/// sentence reads whole whatever its length.
fn flowed(text: &str) -> String {
    text.replace("\n│  ", " ").replace("\n   ", " ")
}

/// `cahoots evals <args>` as a person runs it, with git on PATH.
fn evals(world: &World, args: &[&str]) -> Finished {
    let mut argv = vec!["evals"];
    argv.extend(args);
    world
        .as_a_person_with(&argv, &[("PATH", &world.path_with_git())])
        .finish()
}

#[test]
fn evals_add_says_what_the_task_holds() {
    let world = World::new();
    world.evals_fixture();
    let run = world.accepted_writer(EVALS_BRIEF, &[]);
    let id = run.run_id();
    let sha = run.data()["base_commit"].as_str().unwrap().to_string();
    let after = evals(&world, &["add", &id]);
    assert_eq!(after.code, 0, "{}", after.text());
    let text = after.text();
    let target = &run.data()["target"];
    let by = format!(
        "From a run by {} · {} · {}, accepted",
        target["harness"].as_str().unwrap(),
        target["model"].as_str().unwrap(),
        target["effort"].as_str().unwrap()
    );
    assert!(text.starts_with("┌  cahoots evals add\n│\n"), "{text}");
    for said in [
        format!("◇  Task {id}: a task of no kind at {} {by}", &sha[..12]),
        format!("Repository: {}", world.work.display()),
        "Hidden tests (2): tests/old_test.rs, tests/new_test.rs The rest of the change (1): src/lib.rs"
            .to_string(),
        format!(
            "└  Added to the suite, in {}/evals/tasks/{id}. It stays on this machine.",
            world.data.display()
        ),
    ] {
        assert!(flowed(&text).contains(&said), "{said:?} in:\n{text}");
    }
    assert!(!text.contains("▲"), "{text}");
    no_json(&after);
}

#[test]
fn evals_add_warns_when_there_are_no_hidden_tests() {
    let world = World::new();
    world.evals_fixture();
    let id = world
        .accepted_writer("FAKE: append=src/lib.rs::pub fn b() {}\\n\n", &[])
        .run_id();
    let after = evals(&world, &["add", &id]);
    assert_eq!(after.code, 0, "{}", after.text());
    let text = flowed(&after.text());
    assert!(text.contains("Hidden tests: none"), "{text}");
    assert!(
        text.contains(
            "▲  Its patch touches no test file, so the task has no hidden tests A test file is \
             one under a tests/, test/, spec/ or __tests__/ directory, or named like \
             foo_test.go, test_foo.py or foo.test.ts."
        ),
        "{text}"
    );
    no_json(&after);
}

#[test]
fn evals_list_flags_a_rotted_task() {
    let world = World::new();
    world.evals_fixture();
    world.git(&["switch", "-q", "-c", "scratch"]);
    fs::write(world.work.join("f.txt"), "scratch\n").unwrap();
    world.git(&["add", "f.txt"]);
    world.git(&["commit", "-q", "-m", "chore: scratch"]);
    let run = world.accepted_writer(EVALS_BRIEF, &[]);
    let id = run.run_id();
    assert_eq!(evals(&world, &["add", &id]).code, 0);
    world.lose_the_scratch_commit(&[&run]);
    let after = evals(&world, &["list"]);
    assert_eq!(after.code, 0, "{}", after.text());
    let text = after.text();
    assert!(text.starts_with("┌  cahoots evals list\n│\n"), "{text}");
    assert!(text.ends_with("│\n└  1 task, 1 rotted\n"), "{text}");
    let said = format!(
        "▲  {id} · no kind · {} {} 2 hidden tests Its base commit is gone from the repository, \
         so it cannot be replayed. `cahoots evals remove {id}` takes it out.",
        &run.data()["base_commit"].as_str().unwrap()[..12],
        world.work.display()
    );
    assert!(flowed(&text).contains(&said), "{said:?} in:\n{text}");
    no_json(&after);
}

#[test]
fn evals_list_with_no_tasks_says_how_to_add_one() {
    let world = World::new();
    let after = evals(&world, &["list"]);
    assert_eq!(after.code, 0, "{}", after.text());
    assert_eq!(
        flowed(&after.text()),
        "┌  cahoots evals list\n│\n└  No tasks yet: `cahoots evals add <run>` makes one from a \
         writer's run you accepted.\n"
    );
}

#[test]
fn evals_remove_says_so() {
    let world = World::new();
    world.evals_fixture();
    let id = world.accepted_writer(EVALS_BRIEF, &[]).run_id();
    assert_eq!(evals(&world, &["add", &id]).code, 0);
    let after = evals(&world, &["remove", &id]);
    assert_eq!(after.code, 0, "{}", after.text());
    assert_eq!(
        after.text(),
        format!("┌  cahoots evals remove\n│\n└  Task {id} removed from the suite.\n")
    );
}

#[test]
fn an_evals_refusal_closes_the_rail_in_red() {
    let world = World::new();
    let id = "0198c0de-0000-7000-8000-000000000000";
    let after = evals(&world, &["add", id]);
    assert_eq!(after.code, 50, "{}", after.text());
    assert_eq!(
        after.text(),
        format!("┌  cahoots evals add\n│\n└  no run {id}\n")
    );
}
