//! How a verb a person reads ends: the human verbs, `doctor` and `report`,
//! in words, as `tui::Ending`s, made from what the logic returned. The
//! settings' words (the page, `set`, `reset`, `enable` and `registry`) live
//! in `settings.rs` with the rest of them. Nothing here decides anything or
//! prints anything: `cli::emit` draws what these say, on stdout, when a
//! person reads it (`cli::reader`).

use std::path::Path;

use serde_json::Value;

use super::settings::tilde;
use crate::doctor::{Check, EvidenceGap, Status};
use crate::install::files::{Outcome, Report};
use crate::install::rules::rules_file;
use crate::meter::tokens;
use crate::model::HarnessId;
use crate::tui::{Block, Checked, Ending, Last, Mark, Paste};

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
        .map(|(harness, rules)| to_paste(*harness, rules))
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

/// `doctor`: every check with its mark, and the rules a harness still
/// needs, to paste under the file each belongs in. The last word counts the
/// checks, in red when one failed.
pub fn checked(title: String, checks: &[Check], home: &Path) -> Ending {
    let rows = checks
        .iter()
        .map(|check| Checked {
            mark: match check.status {
                Status::Ok => Mark::Done,
                Status::Warn => Mark::Warning,
                Status::Fail => Mark::Failed,
            },
            label: check.check.clone(),
            // The label names the harness, and the heading over its rules
            // names the file.
            text: match &check.rules {
                Some(_) => "to delegate without a prompt, add the rules below".to_string(),
                None => home_as_tilde(&check.detail, home),
            },
        })
        .collect();
    let paste = checks
        .iter()
        .filter_map(|check| check.rules.as_ref())
        .map(|(harness, rules)| to_paste(*harness, rules))
        .collect();
    let with = |status| checks.iter().filter(|check| check.status == status).count() as u64;
    let (warned, failed) = (with(Status::Warn), with(Status::Fail));
    let mut counted = vec![format!("{} passed", with(Status::Ok))];
    if warned > 0 {
        counted.push(count(warned, "warning", "warnings"));
    }
    if failed > 0 {
        counted.push(format!("{failed} failed"));
    }
    let all = count(checks.len() as u64, "check", "checks");
    let last = match (warned, failed) {
        (0, 0) => Last::Said(format!("All {all} passed")),
        (_, 0) => Last::Said(format!("{all}: {}", counted.join(", "))),
        _ => Last::Refused(format!("{all}: {}", counted.join(", "))),
    };
    Ending {
        title: Some(title),
        blocks: vec![Block::Checks(rows)],
        last,
        paste,
    }
}

/// One warning for each candidate in a current list that nothing is known
/// about. The gaps are found by `doctor`; this only gives them their words —
/// and none of them advertises a command to run, which is not a thing yet.
pub fn evidence_warnings(gaps: &[EvidenceGap]) -> Vec<Check> {
    gaps.iter()
        .map(|gap| {
            let list = match &gap.kind {
                Some(kind) => format!("kind {kind}"),
                None => format!("role {}", gap.role),
            };
            Check {
                check: format!(
                    "{list}: {} {} {} evidence",
                    gap.candidate.harness,
                    gap.candidate.model.as_str(),
                    gap.candidate.effort
                ),
                status: Status::Warn,
                detail: "no rated or failed runs on record for this candidate in this list — \
                         not enough evidence"
                    .to_string(),
                rules: None,
            }
        })
        .collect()
}

/// `report`, from its envelope's data: for each role and target, how its
/// runs ended, what became of their results and what they cost; then, with
/// `--suggest`, what those outcomes say about each role's order.
pub fn reported(title: String, data: &Value) -> Ending {
    let floor = data["sample_floor"].as_u64().unwrap_or(0);
    let mut blocks = Vec::new();
    for (key, row) in data["by_role_and_target"].as_object().into_iter().flatten() {
        blocks.push(Block::done(key.clone(), row_lines(row, floor)));
    }
    for (key, row) in data["by_kind_and_target"].as_object().into_iter().flatten() {
        blocks.push(Block::done(format!("Kind · {key}"), row_lines(row, floor)));
    }
    if let Some(routing) = data.get("routing") {
        blocks.extend(routed(routing, floor));
    }
    let within = match data["days"].as_u64().unwrap_or(0) {
        1 => "the last day".to_string(),
        days => format!("the last {days} days"),
    };
    let last = match data["runs"].as_u64().unwrap_or(0) {
        0 => format!("No runs in {within}"),
        runs => format!("{} in {within}", count(runs, "run", "runs")),
    };
    Ending {
        title: Some(title),
        blocks,
        last: Last::Said(last),
        paste: Vec::new(),
    }
}

/// `report --suggest`: whether the suggestions are in effect and the rule
/// they follow, then each role's order with its evidence, and the one swap
/// it supports, or why none.
fn routed(routing: &Value, floor: u64) -> Vec<Block> {
    let mode = match routing["mode"].as_str().unwrap_or_default() {
        "applying" => "in effect",
        mode => mode,
    };
    let mut blocks = vec![Block::info(
        format!("Routing: {mode}"),
        vec![format!(
            "The rule: {}",
            routing["rule"].as_str().unwrap_or_default()
        )],
    )];
    for (role, said) in routing["roles"].as_object().into_iter().flatten() {
        let mut lines: Vec<String> = Vec::new();
        for (n, entry) in said["order"].as_array().into_iter().flatten().enumerate() {
            lines.push(format!(
                "{}. {}: {}",
                n + 1,
                candidate(&entry["candidate"]),
                evidence(&entry["evidence"], floor)
            ));
            if entry["evidence"]["enough_evidence"] == true {
                lines.push(rates_line(&entry["evidence"]));
            }
        }
        let swap = &said["suggested_swap"];
        if swap.is_object() {
            lines.push(format!(
                "{} moves up past {}: {}",
                candidate(&swap["move_up"]),
                candidate(&swap["past"]),
                if swap["in_effect"] == true {
                    "in effect"
                } else {
                    "shown, not used"
                }
            ));
        } else if let Some(why) = said["no_swap_because"].as_str() {
            lines.push(format!("No change: {why}"));
        }
        blocks.push(Block::done(role.clone(), lines));
    }
    blocks
}

/// A candidate as the registry lists it: `codex gpt-5 high`.
fn candidate(candidate: &Value) -> String {
    ["harness", "model", "effort"]
        .map(|field| candidate[field].as_str().unwrap_or_default())
        .join(" ")
}

/// What a candidate's rated and failed runs say — or that they are too few
/// to say anything. The data decides which: nothing is computed here.
fn evidence(evidence: &Value, floor: u64) -> String {
    let runs = count(evidence["n"].as_u64().unwrap_or(0), "run", "runs");
    if evidence["enough_evidence"] != true {
        return insufficient(evidence, floor);
    }
    let counted: Vec<String> = ["accepted", "reworked", "discarded", "failed"]
        .into_iter()
        .filter_map(|field| match evidence[field].as_u64().unwrap_or(0) {
            0 => None,
            n => Some(format!("{n} {field}")),
        })
        .collect();
    format!(
        "score {:.2} (standard error {:.2}) from {runs} ({})",
        evidence["score"].as_f64().unwrap_or(0.0),
        evidence["score_standard_error"].as_f64().unwrap_or(0.0),
        counted.join(", ")
    )
}

/// `report`'s lines for one row: how its runs ended, what became of their
/// results, what they cost, and what its evidence supports. A row with no
/// runs — a configured candidate nobody has used — says so and nothing more
/// than its evidence: it has no median to show.
fn row_lines(row: &Value, floor: u64) -> Vec<String> {
    let n = |field: &str| row[field].as_u64().unwrap_or(0);
    let said = |fields: &[(&str, &str)]| -> Vec<String> {
        fields
            .iter()
            .filter(|(field, _)| n(field) > 0)
            .map(|(field, words)| format!("{} {words}", n(field)))
            .collect()
    };
    let mut lines = Vec::new();
    if n("runs") == 0 {
        lines.push("0 runs".to_string());
    } else {
        let ends = [
            ("done", "done"),
            ("failed", "failed"),
            ("timed_out", "timed out"),
            ("cancelled", "cancelled"),
            ("stopped_by_budget", "stopped by budget"),
        ];
        let mut ended = said(&ends);
        let not_ended = n("runs").saturating_sub(ends.iter().map(|(field, _)| n(field)).sum());
        if not_ended > 0 {
            ended.push(format!("{not_ended} not ended"));
        }
        lines.push(format!(
            "{}: {}",
            count(n("runs"), "run", "runs"),
            ended.join(", ")
        ));
        let outcomes = said(&[
            ("accepted", "accepted"),
            ("reworked", "reworked"),
            ("discarded", "discarded"),
            ("outcome_unknown", "unknown"),
        ]);
        if !outcomes.is_empty() {
            lines.push(format!("Outcomes: {}", outcomes.join(", ")));
        }
        if let Some(survival) = row.get("survival") {
            lines.push(kept(survival));
        }
        let mut cost = format!("Median time {}", duration(n("median_secs")));
        if n("tokens_in") + n("tokens_out") > 0 {
            cost.push_str(&format!(
                " · {} tokens in, {} out",
                tokens(n("tokens_in")),
                tokens(n("tokens_out"))
            ));
        }
        lines.push(cost);
    }
    let evidence = &row["evidence"];
    if evidence.is_object() {
        if evidence["enough_evidence"] == true {
            lines.push(rates_line(evidence));
            lines.push(score(evidence));
        } else {
            lines.push(insufficient(evidence, floor));
        }
    }
    lines
}

/// How much of a row's writers' diffs survived: the settled share and how
/// many fell since their first measure, the share still settling, then
/// what is unknown and what is not measured yet. Zero parts are left out.
fn kept(survival: &Value) -> String {
    let percent = |share: &Value| share.as_f64().map(|share| (share * 100.0).round() as u64);
    let mut shares = Vec::new();
    for (bucket, label) in [("settled", "settled"), ("early", "still settling")] {
        let part = &survival[bucket];
        let runs = part["runs"].as_u64().unwrap_or(0);
        if runs == 0 {
            continue;
        }
        let mut said = match (bucket, percent(&part["share"])) {
            ("settled", Some(share)) => {
                format!("{share}% of {}", count(runs, "settled run", "settled runs"))
            }
            (_, Some(share)) => format!("{share}% of {runs} {label}"),
            ("settled", None) => format!(
                "nothing to count in {}",
                count(runs, "settled run", "settled runs")
            ),
            (_, None) => format!("nothing to count in {runs} {label}"),
        };
        let fell = part["fell"].as_u64().unwrap_or(0);
        if fell > 0 {
            said.push_str(&format!(" ({fell} fell)"));
        }
        shares.push(said);
    }
    let mut rest = Vec::new();
    for (field, words) in [("unknown", "unknown"), ("unmeasured", "not measured yet")] {
        let n = survival[field].as_u64().unwrap_or(0);
        if n > 0 {
            rest.push(format!("{n} {words}"));
        }
    }
    let parts: Vec<String> = [shares.join(", "), rest.join(", ")]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect();
    format!("Kept: {}", parts.join(" · "))
}

/// The sentence for evidence that is too thin to put numbers on.
fn insufficient(evidence: &Value, floor: u64) -> String {
    format!(
        "not enough evidence ({} rated or failed runs; need {floor})",
        evidence["n"].as_u64().unwrap_or(0)
    )
}

/// The line of shares, for a row and for a suggested candidate alike.
fn rates_line(evidence: &Value) -> String {
    format!(
        "Rates from {} rated or failed runs: {}",
        evidence["n"].as_u64().unwrap_or(0),
        rates(evidence)
    )
}

/// The four shares, each with one standard error in percentage points.
fn rates(evidence: &Value) -> String {
    ["accepted", "reworked", "discarded", "failed"]
        .into_iter()
        .map(|field| {
            let rate = &evidence["rates"][field];
            format!(
                "{field} {:.1}% (standard error {:.1} percentage points)",
                rate["value"].as_f64().unwrap_or(0.0) * 100.0,
                rate["standard_error"].as_f64().unwrap_or(0.0) * 100.0
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn score(evidence: &Value) -> String {
    format!(
        "Score {:.2} (standard error {:.2})",
        evidence["score"].as_f64().unwrap_or(0.0),
        evidence["score_standard_error"].as_f64().unwrap_or(0.0)
    )
}

/// Seconds as a person says them: `45s`, `4m 12s`, `1h 3m`.
fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
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

/// A harness's rules under the file they go in.
fn to_paste(harness: HarnessId, rules: &[String]) -> Paste {
    Paste {
        heading: format!("Add to {}:", rules_file(harness)),
        lines: pasteable(harness, rules),
    }
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

    fn check(name: &str, status: Status, detail: &str) -> Check {
        Check {
            check: name.into(),
            status,
            detail: detail.into(),
            rules: None,
        }
    }

    #[test]
    fn doctor_marks_each_check_and_puts_the_rules_a_harness_needs_under_its_file() {
        let rules = vec![
            "\"Bash(cahoots pick:*)\"".to_string(),
            "\"Bash(cahoots run:*)\"".to_string(),
        ];
        let checks = [
            check(
                "directories",
                Status::Ok,
                &format!("config {HOME}/config · state {HOME}/state"),
            ),
            Check {
                rules: Some((HarnessId::Claude, rules)),
                ..check(
                    "claude: caller rules",
                    Status::Warn,
                    "to let claude delegate without a prompt, a person adds to its file: …",
                )
            },
            check("meter", Status::Warn, "no usage meter"),
        ];
        // The widest label and two spaces: every text starts in one column.
        let column = |label: &str| " ".repeat(22 - label.chars().count());
        let expected = format!(
            "┌  cahoots doctor\n\
             │\n\
             ◇  directories{}config ~/config · state ~/state\n\
             ▲  claude: caller rules{}to delegate without a prompt, add the rules below\n\
             ▲  meter{}no usage meter\n\
             │\n\
             └  3 checks: 1 passed, 2 warnings\n\
             \n\
             Add to {}:\n\
             \"Bash(cahoots pick:*)\",\n\
             \"Bash(cahoots run:*)\"\n",
            column("directories"),
            column("claude: caller rules"),
            column("meter"),
            rules_file(HarnessId::Claude)
        );
        assert_eq!(
            shown(&checked("cahoots doctor".into(), &checks, Path::new(HOME))),
            expected
        );
    }

    #[test]
    fn an_evidence_gap_is_a_warning_in_the_labels_the_plan_set_and_no_command() {
        use crate::model::{Candidate, Effort, ModelName, Role, TaskKindName};
        let candidate = Candidate {
            harness: HarnessId::Codex,
            model: ModelName::try_from("custom".to_string()).unwrap(),
            effort: Effort::Medium,
        };
        let warnings = evidence_warnings(&[
            EvidenceGap {
                role: Role::Review,
                kind: None,
                candidate: candidate.clone(),
            },
            EvidenceGap {
                role: Role::Review,
                kind: Some(TaskKindName::try_from("rust-review".to_string()).unwrap()),
                candidate,
            },
        ]);
        let said: Vec<(&str, Status)> = warnings
            .iter()
            .map(|check| (check.check.as_str(), check.status))
            .collect();
        assert_eq!(
            said,
            [
                ("role review: codex custom medium evidence", Status::Warn),
                (
                    "kind rust-review: codex custom medium evidence",
                    Status::Warn
                ),
            ]
        );
        assert_eq!(
            warnings[1].detail,
            "no rated or failed runs on record for this candidate in this list — not enough evidence"
        );
    }

    #[test]
    fn doctor_s_last_word_counts_the_checks_and_is_red_when_one_failed() {
        let last = |statuses: &[Status]| {
            let checks: Vec<Check> = statuses
                .iter()
                .map(|status| check("a check", *status, "what it found"))
                .collect();
            checked("cahoots doctor".into(), &checks, Path::new(HOME)).last
        };
        assert_eq!(
            last(&[Status::Ok, Status::Ok]),
            Last::Said("All 2 checks passed".into())
        );
        assert_eq!(
            last(&[Status::Ok, Status::Warn]),
            Last::Said("2 checks: 1 passed, 1 warning".into())
        );
        assert_eq!(
            last(&[Status::Ok, Status::Warn, Status::Warn, Status::Fail]),
            Last::Refused("4 checks: 1 passed, 2 warnings, 1 failed".into())
        );
    }

    #[test]
    fn report_says_how_each_role_and_target_did_and_what_it_cost() {
        let data = serde_json::json!({
            "days": 1,
            "runs": 12,
            "by_role_and_target": {
                "review · codex · gpt-6-astra · high": {
                    "runs": 9, "done": 5, "failed": 1, "timed_out": 1, "cancelled": 1,
                    "stopped_by_budget": 1, "accepted": 2, "reworked": 1, "discarded": 1,
                    "outcome_unknown": 1, "tokens_in": 2_015_100, "tokens_out": 95_910,
                    "median_secs": 190
                },
                "advise · claude · opus · high": {
                    "runs": 3, "done": 2, "failed": 0, "timed_out": 0, "cancelled": 0,
                    "stopped_by_budget": 0, "accepted": 0, "reworked": 0, "discarded": 0,
                    "outcome_unknown": 0, "tokens_in": 0, "tokens_out": 0, "median_secs": 3700
                }
            }
        });
        assert_eq!(
            shown(&reported("cahoots report".into(), &data)),
            "┌  cahoots report\n\
             │\n\
             ◇  advise · claude · opus · high\n\
             │  3 runs: 2 done, 1 not ended\n\
             │  Median time 1h 1m\n\
             │\n\
             ◇  review · codex · gpt-6-astra · high\n\
             │  9 runs: 5 done, 1 failed, 1 timed out, 1 cancelled, 1 stopped by budget\n\
             │  Outcomes: 2 accepted, 1 reworked, 1 discarded, 1 unknown\n\
             │  Median time 3m 10s · 2.0M tokens in, 95k out\n\
             │\n\
             └  12 runs in the last day\n",
            "no outcomes and no tokens: no words for them"
        );
    }

    #[test]
    fn report_says_how_much_of_a_writers_diff_survived() {
        let row = |survival: serde_json::Value| {
            serde_json::json!({
                "days": 30,
                "runs": 10,
                "by_role_and_target": {
                    "implement · codex · m · high": {
                        "runs": 10, "done": 10, "accepted": 10, "median_secs": 60,
                        "survival": survival
                    }
                }
            })
        };
        let said = |survival| shown(&reported("cahoots report".into(), &row(survival)));
        let all = said(serde_json::json!({
            "settled": {"runs": 4, "share": 0.7, "fell": 1},
            "early": {"runs": 3, "share": 0.55},
            "unknown": 2,
            "unmeasured": 0
        }));
        assert!(
            all.contains(
                "│  Outcomes: 10 accepted\n\
                 │  Kept: 70% of 4 settled runs (1 fell), 55% of 3 still settling · 2 unknown\n\
                 │  Median time"
            ),
            "{all}"
        );
        let one = said(serde_json::json!({
            "settled": {"runs": 1, "share": null, "fell": 0},
            "unknown": 0,
            "unmeasured": 0
        }));
        assert!(
            one.contains("│  Kept: nothing to count in 1 settled run\n"),
            "{one}"
        );
        let unknown = said(serde_json::json!({"unknown": 1, "unmeasured": 2}));
        assert!(
            unknown.contains("│  Kept: 1 unknown, 2 not measured yet\n"),
            "{unknown}"
        );
    }

    #[test]
    fn report_suggest_shows_each_role_s_order_its_evidence_and_the_one_swap() {
        let codex =
            serde_json::json!({ "harness": "codex", "model": "gpt-6-astra", "effort": "high" });
        let claude = serde_json::json!({ "harness": "claude", "model": "opus", "effort": "high" });
        let none = serde_json::json!({
            "n": 0, "accepted": 0, "reworked": 0, "discarded": 0, "failed": 0, "score": null,
            "enough_evidence": false
        });
        let data = serde_json::json!({
            "days": 30,
            "runs": 0,
            "sample_floor": 8,
            "by_role_and_target": {},
            "routing": {
                "mode": "applying",
                "rule": "a candidate moves up ONE place past its neighbour",
                "roles": {
                    "advise": {
                        "order": [
                            { "candidate": codex, "evidence": {
                                "n": 4, "accepted": 1, "reworked": 0, "discarded": 0, "failed": 3,
                                "score": 0.25, "enough_evidence": false
                            } },
                            { "candidate": claude, "evidence": none }
                        ],
                        "suggested_swap": { "move_up": claude, "past": codex, "in_effect": true },
                        "no_swap_because": null
                    },
                    "review": {
                        "order": [ { "candidate": codex, "evidence": none } ],
                        "suggested_swap": null,
                        "no_swap_because": "the evidence does not support a change"
                    }
                }
            }
        });
        assert_eq!(
            shown(&reported("cahoots report".into(), &data)),
            "┌  cahoots report\n\
             │\n\
             ●  Routing: in effect\n\
             │  The rule: a candidate moves up ONE place past its neighbour\n\
             │\n\
             ◇  advise\n\
             │  1. codex gpt-6-astra high: not enough evidence (4 rated or failed runs; need\n\
             │  8)\n\
             │  2. claude opus high: not enough evidence (0 rated or failed runs; need 8)\n\
             │  claude opus high moves up past codex gpt-6-astra high: in effect\n\
             │\n\
             ◇  review\n\
             │  1. codex gpt-6-astra high: not enough evidence (0 rated or failed runs; need\n\
             │  8)\n\
             │  No change: the evidence does not support a change\n\
             │\n\
             └  No runs in the last 30 days\n"
        );
    }

    fn enough() -> Value {
        serde_json::json!({
            "n": 8, "accepted": 4, "reworked": 2, "discarded": 1, "failed": 1,
            "score": 0.625, "score_standard_error": 0.1, "enough_evidence": true,
            "rates": {
                "accepted": { "value": 0.5, "standard_error": 0.17677 },
                "reworked": { "value": 0.25, "standard_error": 0.15309 },
                "discarded": { "value": 0.125, "standard_error": 0.11692 },
                "failed": { "value": 0.125, "standard_error": 0.11692 }
            }
        })
    }

    #[test]
    fn a_row_with_enough_evidence_says_its_rates_and_score_with_their_errors() {
        let mut row = serde_json::json!({
            "runs": 8, "done": 8, "failed": 0, "timed_out": 0, "cancelled": 0,
            "stopped_by_budget": 0, "accepted": 4, "reworked": 2, "discarded": 1,
            "outcome_unknown": 1, "tokens_in": 0, "tokens_out": 0, "median_secs": 5
        });
        row["evidence"] = enough();
        assert_eq!(
            row_lines(&row, 8),
            [
                "8 runs: 8 done",
                "Outcomes: 4 accepted, 2 reworked, 1 discarded, 1 unknown",
                "Median time 5s",
                "Rates from 8 rated or failed runs: accepted 50.0% (standard error 17.7 \
                 percentage points), reworked 25.0% (standard error 15.3 percentage points), \
                 discarded 12.5% (standard error 11.7 percentage points), failed 12.5% \
                 (standard error 11.7 percentage points)",
                "Score 0.62 (standard error 0.10)",
            ]
        );
        // Below the floor: the sentence, and no number from the estimates.
        row["evidence"] = serde_json::json!({ "n": 7, "enough_evidence": false, "score": 0.5,
            "rates": { "accepted": { "value": 0.5, "standard_error": 0.2 } } });
        let lines = row_lines(&row, 8);
        assert_eq!(
            lines.last().unwrap(),
            "not enough evidence (7 rated or failed runs; need 8)"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.contains('%') || line.contains("Score"))
        );
    }

    #[test]
    fn a_row_nobody_has_run_says_so_and_shows_no_cost() {
        let row = serde_json::json!({
            "runs": 0, "median_secs": 0, "tokens_in": 0, "tokens_out": 0,
            "evidence": { "n": 0, "enough_evidence": false }
        });
        assert_eq!(
            row_lines(&row, 8),
            [
                "0 runs",
                "not enough evidence (0 rated or failed runs; need 8)"
            ]
        );
    }

    #[test]
    fn a_suggestion_with_enough_evidence_keeps_its_counts_and_adds_the_errors() {
        assert_eq!(
            evidence(&enough(), 8),
            "score 0.62 (standard error 0.10) from 8 runs (4 accepted, 2 reworked, \
             1 discarded, 1 failed)"
        );
        assert!(rates_line(&enough()).starts_with(
            "Rates from 8 rated or failed runs: accepted 50.0% (standard error 17.7 \
             percentage points), reworked 25.0%"
        ));
        assert_eq!(
            evidence(&serde_json::json!({ "n": 7, "enough_evidence": false }), 8),
            "not enough evidence (7 rated or failed runs; need 8)"
        );
    }

    #[test]
    fn a_duration_is_said_in_the_two_largest_units() {
        for (secs, said) in [
            (0, "0s"),
            (59, "59s"),
            (60, "1m 0s"),
            (3599, "59m 59s"),
            (3600, "1h 0m"),
            (3700, "1h 1m"),
        ] {
            assert_eq!(duration(secs), said);
        }
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
