//! How the human verbs end when a person reads them: their words, as
//! `tui::Ending`s, made from what the logic returned. The settings' words
//! (the page, `set`, `reset`, `enable` and `registry`) live in `settings.rs`
//! with the rest of them. Nothing here decides anything or prints anything:
//! `cli::emit` draws what these say, on stdout, when stdout is a terminal.

use std::path::Path;

use serde_json::Value;

use super::settings::tilde;
use crate::install::files::{Outcome, Report};
use crate::install::rules::rules_file;
use crate::model::HarnessId;
use crate::tui::{Block, Ending, Last, Paste};

/// What `install` did, for its words.
pub struct Installed<'a> {
    /// `None` when install's question opened the rail.
    pub title: Option<String>,
    pub dry_run: bool,
    pub files: &'a [Report],
    /// What was decided about the usage meter, in the logic's sentence.
    pub meter: String,
    /// What was written to config.toml, as the settings page says it.
    pub saved: Vec<String>,
    pub still_to_do: Option<&'a str>,
    /// The rules each harness is still missing.
    pub rules: &'a [(HarnessId, Vec<String>)],
    pub home: &'a Path,
}

/// `install`: the files, the meter, what is left to do, and the rules to
/// paste under the file each belongs in.
pub fn installed(done: Installed<'_>) -> Ending {
    let mut blocks = files(done.files, done.dry_run, done.home);
    blocks.push(Block::info(done.meter, done.saved));
    if let Some(to_do) = done.still_to_do {
        blocks.push(Block::warning(home_as_tilde(to_do, done.home), Vec::new()));
    }
    let paste: Vec<Paste> = done
        .rules
        .iter()
        .filter(|(_, rules)| !rules.is_empty())
        .map(|(harness, rules)| Paste {
            heading: format!("Add to {}:", rules_file(*harness)),
            lines: pasteable(*harness, rules),
        })
        .collect();
    let last = match (done.dry_run, paste.is_empty()) {
        (true, true) => "A dry run: nothing was written.",
        (true, false) => "A dry run: nothing was written. The rules below are still missing.",
        (false, false) => {
            "Add the rules below yourself: cahoots never edits a harness's permissions. Then \
             `cahoots enable <harness>` for each target you want, and `cahoots doctor` to check."
        }
        (false, true) => {
            "Next: `cahoots enable <harness>` for each target you want, and `cahoots doctor` to \
             check."
        }
    };
    Ending {
        title: done.title,
        blocks,
        last: Last::Said(last.to_string()),
        paste,
    }
}

/// `uninstall`: what went, what was left alone, and the rules that are the
/// person's to take out.
pub fn uninstalled(title: String, dry_run: bool, reports: &[Report], home: &Path) -> Ending {
    let removed = reports
        .iter()
        .any(|report| report.outcome == Outcome::Removed);
    let last = if reports.is_empty() {
        "Nothing to remove: install has written nothing here."
    } else if dry_run {
        "A dry run: nothing was removed."
    } else if removed {
        "Take out the permission rules you added: cahoots never edits them."
    } else {
        "Nothing was removed."
    };
    Ending {
        title: Some(title),
        blocks: files(reports, dry_run, home),
        last: Last::Said(last.to_string()),
        paste: Vec::new(),
    }
}

/// `learn list`, from its envelope's data: whether review is on, what is
/// on record, and each note in effect with what reviewers wrote. Here, and
/// only here, a reviewer's own words reach anyone, and `tui` shows any
/// control character in them escaped.
pub fn learned(title: String, data: &Value, home: &Path) -> Ending {
    let review = if data["review_enabled"].as_bool() == Some(true) {
        let share = data["sample_rate"].as_f64().unwrap_or(0.0) * 100.0;
        format!("Review is on, for {}% of finished runs", share.round())
    } else {
        "Review is off: `cahoots settings` turns it on".to_string()
    };
    let record = format!(
        "On record: {} and {}, in {}",
        count(data["runs_on_record"].as_u64().unwrap_or(0), "run", "runs"),
        count(
            data["reviews_on_record"].as_u64().unwrap_or(0),
            "review",
            "reviews"
        ),
        home_as_tilde(data["where"].as_str().unwrap_or_default(), home)
    );
    let mut blocks = vec![Block::info(review, vec![record])];
    let mut notes = 0;
    for (scope, list) in data["notes_in_effect"].as_object().into_iter().flatten() {
        let mut lines = Vec::new();
        for note in list.as_array().into_iter().flatten() {
            notes += 1;
            let text = note["text"].as_str().unwrap_or_default();
            lines.push(match note["support"].as_u64().unwrap_or(0) {
                1 => format!("{text} 1 review says so."),
                support => format!("{text} {support} reviews agree."),
            });
            let wrote: Vec<String> = note["what_reviewers_wrote"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|detail| format!("\"{detail}\""))
                .collect();
            if !wrote.is_empty() {
                lines.push(format!("What reviewers wrote: {}", wrote.join(", ")));
            }
        }
        blocks.push(Block::done(scope.clone(), lines));
    }
    let last = match notes {
        0 => "No notes in effect".to_string(),
        1 => "1 note in effect".to_string(),
        n => format!("{n} notes in effect"),
    };
    Ending {
        title: Some(title),
        blocks,
        last: Last::Said(last),
        paste: Vec::new(),
    }
}

/// `learn reset`.
pub fn forgot(title: String) -> Ending {
    Ending::just(
        Some(title),
        Last::Said("Reviews recorded before now no longer count toward any note.".to_string()),
    )
}

/// The files `install` or `uninstall` reported, a block for each thing that
/// happened to them: what was written or removed first, then how many were
/// up to date already, then what was left alone and why.
fn files(reports: &[Report], dry_run: bool, home: &Path) -> Vec<Block> {
    let mut groups: Vec<(&Outcome, Vec<String>)> = Vec::new();
    let mut up_to_date = 0;
    for report in reports {
        if report.outcome == Outcome::UpToDate {
            up_to_date += 1;
            continue;
        }
        let path = tilde(&report.path, home);
        match groups
            .iter_mut()
            .find(|(outcome, _)| **outcome == report.outcome)
        {
            Some((_, paths)) => paths.push(path),
            None => groups.push((&report.outcome, vec![path])),
        }
    }
    let would = |done: &str, would: &str| {
        if dry_run {
            would.to_string()
        } else {
            done.to_string()
        }
    };
    let mut skipped = Vec::new();
    let mut blocks = Vec::new();
    for (outcome, paths) in groups {
        match outcome {
            Outcome::Installed => {
                blocks.push(Block::done(would("Installed", "Would install"), paths))
            }
            Outcome::Updated { from } => blocks.push(Block::done(
                would(
                    &format!("Updated from {from}"),
                    &format!("Would update from {from}"),
                ),
                paths,
            )),
            Outcome::Refreshed => blocks.push(Block::done(
                would(
                    "Refreshed: they had changed since this version wrote them",
                    "Would refresh: they have changed since this version wrote them",
                ),
                paths,
            )),
            Outcome::Removed => blocks.push(Block::done(would("Removed", "Would remove"), paths)),
            Outcome::Skipped { why } => skipped.push(Block::info(format!("Skipped: {why}"), paths)),
            Outcome::UpToDate => {}
        }
    }
    if up_to_date > 0 {
        blocks.push(Block::done(
            format!("Up to date: {}", count(up_to_date, "file", "files")),
            Vec::new(),
        ));
    }
    blocks.extend(skipped);
    blocks
}

/// A harness's rules as they go into its file: Claude Code's are entries
/// of a JSON list, with a comma between them; Codex's are lines of their own.
fn pasteable(harness: HarnessId, rules: &[String]) -> Vec<String> {
    match harness {
        HarnessId::Claude => rules
            .iter()
            .enumerate()
            .map(|(n, rule)| {
                if n + 1 < rules.len() {
                    format!("{rule},")
                } else {
                    rule.clone()
                }
            })
            .collect(),
        HarnessId::Codex => rules.to_vec(),
    }
}

/// `text` with every path under `home` written from `~`.
fn home_as_tilde(text: &str, home: &Path) -> String {
    text.replace(&format!("{}/", home.display()), "~/")
}

fn count(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::tui::{Colors, Rail};

    const HOME: &str = "/home/someone";

    fn report(path: &str, outcome: Outcome) -> Report {
        Report {
            path: PathBuf::from(HOME).join(path),
            outcome,
        }
    }

    /// The ending as a person reads it, colors off.
    fn shown(ending: &Ending) -> String {
        let mut screen = Vec::new();
        Rail::new(&mut screen, Colors::OFF).end(ending);
        String::from_utf8(screen).unwrap()
    }

    #[test]
    fn install_says_what_it_wrote_the_meter_what_is_left_and_the_rules_to_paste() {
        let files = [
            report("shared/skills/cahoots/SKILL.md", Outcome::Installed),
            report(
                "first/agents/cahoots-delegate.md",
                Outcome::Updated {
                    from: "0.1.0".into(),
                },
            ),
            report("first/skills/cahoots/SKILL.md", Outcome::UpToDate),
            report("first/skills/cahoots-review/SKILL.md", Outcome::UpToDate),
            report(
                "second/skills/cahoots/SKILL.md",
                Outcome::Skipped {
                    why: "~/second does not exist — that harness is not set up here".into(),
                },
            ),
            report("shared/skills/cahoots-review/SKILL.md", Outcome::Installed),
        ];
        let rules = [
            (
                HarnessId::Claude,
                vec![
                    "\"Bash(cahoots pick:*)\"".to_string(),
                    "\"Bash(cahoots run:*)\"".to_string(),
                ],
            ),
            (HarnessId::Codex, Vec::new()),
        ];
        let to_do =
            format!("ccusage counts tokens: set a limit in {HOME}/.config/cahoots/config.toml.");
        let ending = installed(Installed {
            title: Some("cahoots install".into()),
            dry_run: false,
            files: &files,
            meter: "Usage meter: ccusage (you named it).".into(),
            saved: vec!["Saved to config.toml: [meter] use = \"ccusage\"".into()],
            still_to_do: Some(&to_do),
            rules: &rules,
            home: Path::new(HOME),
        });
        let expected = format!(
            "┌  cahoots install\n\
             │\n\
             ◇  Installed\n\
             │  ~/shared/skills/cahoots/SKILL.md\n\
             │  ~/shared/skills/cahoots-review/SKILL.md\n\
             │\n\
             ◇  Updated from 0.1.0\n\
             │  ~/first/agents/cahoots-delegate.md\n\
             │\n\
             ◇  Up to date: 2 files\n\
             │\n\
             ●  Skipped: ~/second does not exist — that harness is not set up here\n\
             │  ~/second/skills/cahoots/SKILL.md\n\
             │\n\
             ●  Usage meter: ccusage (you named it).\n\
             │  Saved to config.toml: [meter] use = \"ccusage\"\n\
             │\n\
             ▲  ccusage counts tokens: set a limit in ~/.config/cahoots/config.toml.\n\
             │\n\
             └  Add the rules below yourself: cahoots never edits a harness's permissions.\n   \
             Then `cahoots enable <harness>` for each target you want, and\n   \
             `cahoots doctor` to check.\n\
             \n\
             Add to {}:\n\
             \"Bash(cahoots pick:*)\",\n\
             \"Bash(cahoots run:*)\"\n",
            rules_file(HarnessId::Claude)
        );
        assert_eq!(
            shown(&ending),
            expected,
            "a harness with no rules missing gets no block to paste"
        );
    }

    #[test]
    fn codex_rules_are_lines_of_their_own_and_a_dry_run_says_so() {
        let rules = [(
            HarnessId::Codex,
            vec![
                "prefix_rule(pattern=[\"cahoots\", \"pick\"], decision=\"allow\")".to_string(),
                "prefix_rule(pattern=[\"cahoots\", \"run\"], decision=\"allow\")".to_string(),
            ],
        )];
        let files = [report("second/skills/cahoots/SKILL.md", Outcome::Installed)];
        let ending = installed(Installed {
            title: None,
            dry_run: true,
            files: &files,
            meter: "Usage meter: more than one was found, and a real install asks which to use."
                .into(),
            saved: Vec::new(),
            still_to_do: None,
            rules: &rules,
            home: Path::new(HOME),
        });
        let text = shown(&ending);
        assert!(text.starts_with("◇  Would install\n"), "{text}");
        let expected = format!(
            "└  A dry run: nothing was written. The rules below are still missing.\n\n\
             Add to {}:\n\
             prefix_rule(pattern=[\"cahoots\", \"pick\"], decision=\"allow\")\n\
             prefix_rule(pattern=[\"cahoots\", \"run\"], decision=\"allow\")\n",
            rules_file(HarnessId::Codex)
        );
        assert!(text.contains(&expected), "{text}");
        let nothing_left = installed(Installed {
            title: None,
            dry_run: false,
            files: &[],
            meter: "Usage meter: ccusage.".into(),
            saved: Vec::new(),
            still_to_do: None,
            rules: &[(HarnessId::Codex, Vec::new())],
            home: Path::new(HOME),
        });
        assert!(nothing_left.paste.is_empty());
        assert_eq!(
            nothing_left.last,
            Last::Said(
                "Next: `cahoots enable <harness>` for each target you want, and \
                 `cahoots doctor` to check."
                    .into()
            )
        );
    }

    #[test]
    fn uninstall_says_what_went_what_stayed_and_what_is_the_persons_to_undo() {
        let files = [
            report("first/skills/cahoots/SKILL.md", Outcome::Removed),
            report(
                "second/agents/cahoots-delegate.toml",
                Outcome::Skipped {
                    why: "no longer carries the cahoots_version stamp — someone made it theirs; \
                          left alone"
                        .into(),
                },
            ),
        ];
        let text = shown(&uninstalled(
            "cahoots uninstall".into(),
            false,
            &files,
            Path::new(HOME),
        ));
        assert_eq!(
            text,
            "┌  cahoots uninstall\n\
             │\n\
             ◇  Removed\n\
             │  ~/first/skills/cahoots/SKILL.md\n\
             │\n\
             ●  Skipped: no longer carries the cahoots_version stamp — someone made it\n\
             │  theirs; left alone\n\
             │  ~/second/agents/cahoots-delegate.toml\n\
             │\n\
             └  Take out the permission rules you added: cahoots never edits them.\n"
        );
        let dry = uninstalled("t".into(), true, &files[..1], Path::new(HOME));
        assert_eq!(
            dry.blocks[0],
            Block::done(
                "Would remove",
                vec!["~/first/skills/cahoots/SKILL.md".into()]
            )
        );
        assert_eq!(
            dry.last,
            Last::Said("A dry run: nothing was removed.".into())
        );
        assert_eq!(
            uninstalled("t".into(), false, &[], Path::new(HOME)).last,
            Last::Said("Nothing to remove: install has written nothing here.".into())
        );
        assert_eq!(
            uninstalled("t".into(), false, &files[1..], Path::new(HOME)).last,
            Last::Said("Nothing was removed.".into())
        );
    }

    #[test]
    fn learn_list_shows_each_note_with_what_reviewers_wrote() {
        let data = serde_json::json!({
            "review_enabled": true,
            "sample_rate": 0.2,
            "runs_on_record": 14,
            "reviews_on_record": 3,
            "where": format!("{HOME}/.local/state/cahoots/history.jsonl"),
            "notes_in_effect": {
                "review · codex": [{
                    "kind": "model_underpowered",
                    "text": "This model has been out of its depth for this role.",
                    "support": 2,
                    "last_seen": 0,
                    "what_reviewers_wrote": ["missed the race in the retry loop", "too shallow"],
                }],
            },
        });
        assert_eq!(
            shown(&learned(
                "cahoots learn list".into(),
                &data,
                Path::new(HOME)
            )),
            "┌  cahoots learn list\n\
             │\n\
             ●  Review is on, for 20% of finished runs\n\
             │  On record: 14 runs and 3 reviews, in ~/.local/state/cahoots/history.jsonl\n\
             │\n\
             ◇  review · codex\n\
             │  This model has been out of its depth for this role. 2 reviews agree.\n\
             │  What reviewers wrote: \"missed the race in the retry loop\", \"too shallow\"\n\
             │\n\
             └  1 note in effect\n"
        );
        let quiet = serde_json::json!({
            "review_enabled": false, "sample_rate": 0.2, "runs_on_record": 1,
            "reviews_on_record": 0, "where": "/elsewhere/history.jsonl", "notes_in_effect": {},
        });
        let ending = learned("cahoots learn list".into(), &quiet, Path::new(HOME));
        assert_eq!(
            ending.blocks,
            [Block::info(
                "Review is off: `cahoots settings` turns it on",
                vec!["On record: 1 run and 0 reviews, in /elsewhere/history.jsonl".into()]
            )]
        );
        assert_eq!(ending.last, Last::Said("No notes in effect".into()));
    }

    #[test]
    fn learn_reset_says_what_no_longer_counts() {
        assert_eq!(
            shown(&forgot("cahoots learn reset".into())),
            "┌  cahoots learn reset\n│\n\
             └  Reviews recorded before now no longer count toward any note.\n"
        );
    }
}
