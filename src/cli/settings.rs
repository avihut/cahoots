//! `cahoots settings`: every setting on a page at the terminal, or one at a
//! time from the command line (`settings set`, `settings reset`). What each
//! setting is and how it is written is `crate::settings`; the page is
//! `crate::tui`. This is where they meet (hard rule 11): the words for each
//! setting, its row on the page, and what each answer changes in config.toml.

use std::path::Path;

use serde_json::json;

use super::SettingsAction;
use super::questions::NO_METER;
use crate::config::UserConfig;
use crate::dirs::Dirs;
use crate::exit::{Envelope, Exit, Fail, Res};
use crate::meter::{MeterFile, MeterId};
use crate::model::{HarnessId, Role};
use crate::settings::{self, Key, Kind, Origin, Section, Setting, Source, Unit, Value};
use crate::tui::{self, Answer, Choice, Edit, Event, Row};

pub fn settings(action: Option<SettingsAction>) -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    let file = dirs.config_file();
    let key = match action {
        None => return page(&dirs),
        Some(SettingsAction::Set { key, value }) => {
            let now = current(&dirs)?;
            let setting = settings::find(&now, &key)?;
            let value = setting.parse(&value)?;
            settings::set(&file, setting.key, &value)?;
            setting.key
        }
        Some(SettingsAction::Reset { key: name }) => {
            // The file must be good to change it at all.
            current(&dirs)?;
            let key = Key::parse(&name)
                .ok_or_else(|| Fail::new(Exit::Usage, format!("no setting is called {name:?}")))?;
            settings::reset(&file, key)?;
            key
        }
    };
    let after = current(&dirs)?;
    Ok(changed(&[key], &after))
}

/// Every setting as the files say now.
fn current(dirs: &Dirs) -> Res<Vec<Setting>> {
    let config = UserConfig::load(&dirs.config_file())?;
    let found = MeterFile::load(dirs)?;
    Ok(settings::current(
        &config,
        found.as_ref(),
        crate::env::path_var().as_deref(),
    ))
}

/// What was changed, as the envelope lists it: each setting's value now,
/// and where that value comes from.
fn changed(keys: &[Key], now: &[Setting]) -> Envelope {
    let changed: Vec<serde_json::Value> = keys
        .iter()
        .map(|key| {
            let setting = now.iter().find(|setting| setting.key == *key);
            json!({
                "key": key.name(),
                "value": setting.and_then(|s| s.value.as_ref()).map(Value::to_json),
                "origin": setting.map_or(Origin::Default, |s| s.origin).as_str(),
            })
        })
        .collect();
    Envelope::new(Exit::Ok, None).with_data(json!({ "changed": changed }))
}

/// There is no terminal to show the page at.
fn no_page() -> Fail {
    Fail::new(
        Exit::Usage,
        "the settings page is shown at a terminal (stdin and stderr, `TERM` not `dumb`) — \
         `cahoots settings set <key> <value>` changes one setting without it, and \
         `cahoots settings reset <key>` puts one back",
    )
}

/// The page: each answer is saved to config.toml as it is given, and the
/// settings are read again after each, so a change made by hand meanwhile
/// shows too. Closing it lists what changed.
fn page(dirs: &Dirs) -> Res<Envelope> {
    let file = dirs.config_file();
    // A file that does not parse is said so before any page is drawn.
    let mut now = current(dirs)?;
    if crate::env::dumb_terminal() {
        return Err(no_page());
    }
    let Some(mut terminal) = tui::Terminal::full_screen() else {
        return Err(no_page());
    };
    let mut keys: Vec<Key> = Vec::new();
    {
        let mut stderr = std::io::stderr();
        let mut screen = tui::Screen::new(&mut stderr, super::colors(), terminal.size());
        let mut page = tui::Page::new(tilde(&file, &dirs.home), rows(&now, &dirs.home));
        while let Event::Save(at, answer) = screen.next(&mut page, &mut terminal) {
            let Some(setting) = now.get(at) else {
                continue;
            };
            match carry_out(&file, setting, answer) {
                Ok(done) => {
                    if !keys.contains(&setting.key) {
                        keys.push(setting.key);
                    }
                    now = current(dirs)?;
                    page.saved(rows(&now, &dirs.home), done);
                }
                Err(fail) => page.refused(fail.message),
            }
        }
    }
    // The terminal is given back before anything more is said.
    drop(terminal);
    Ok(changed(&keys, &now))
}

/// Saves an answer, and says what that did to config.toml. An answer that
/// puts a setting back as it would be without one takes it out of the file.
fn carry_out(file: &Path, setting: &Setting, answer: Answer) -> Res<String> {
    let chosen = match (answer, &setting.kind) {
        (Answer::Flip, Kind::Toggle) => Some(Value::Bool(setting.value != Some(Value::Bool(true)))),
        (Answer::Chose(n), Kind::Choice(names)) => names.get(n).map(|n| Value::Name(n.to_string())),
        (Answer::Chose(n), Kind::Program(programs)) => {
            programs.get(n).map(|p| Value::Program(p.path.clone()))
        }
        (Answer::Number(n), Kind::Number { .. }) => n.map(Value::Number),
        (Answer::Number(n), Kind::Share) => n.map(|percent| Value::Share(percent as f64 / 100.0)),
        (Answer::Order(order), Kind::Order) => match &setting.value {
            Some(Value::Candidates(now)) => Some(Value::Candidates(
                order.iter().filter_map(|n| now.get(*n).cloned()).collect(),
            )),
            _ => None,
        },
        _ => return Err(Fail::internal("an answer that does not fit its setting")),
    };
    let (table, key) = split(setting.key);
    // Which meter is used is a choice even when it is the one found: once
    // chosen, a second meter found later does not bring the question.
    let chosen_meter = setting.key == Key::Meter;
    match chosen {
        Some(value) if chosen_meter || Some(&value) != setting.default.as_ref() => {
            settings::set(file, setting.key, &value)?;
            Ok(match &value {
                Value::Candidates(list) => format!(
                    "Saved to config.toml: [{table}] {key}, {} first",
                    candidate(&list[0])
                ),
                _ => format!(
                    "Saved to config.toml: [{table}] {key} = {}",
                    value.to_toml()
                ),
            })
        }
        _ => {
            settings::reset(file, setting.key)?;
            let back = setting
                .default
                .as_ref()
                .map_or("not set".to_string(), |d| shown(Some(d), &setting.kind));
            Ok(format!(
                "Took [{table}] {key} out of config.toml: back to {back}"
            ))
        }
    }
}

/// A key's table, and its own name: `harness.codex.cap` is `harness.codex`
/// and `cap`.
fn split(key: Key) -> (String, String) {
    let name = key.name();
    match name.rsplit_once('.') {
        Some((table, key)) => (table.to_string(), key.to_string()),
        None => (String::new(), name),
    }
}

fn rows(settings: &[Setting], home: &Path) -> Vec<Row> {
    settings
        .iter()
        .map(|setting| {
            let (label, help) = words(setting.key);
            Row {
                id: setting.key.name(),
                section: section(setting.key.section()).to_string(),
                label,
                value: match &setting.value {
                    Some(Value::Program(path)) => tilde(path, home),
                    value => shown(value.as_ref(), &setting.kind),
                },
                origin: match setting.origin {
                    Origin::Config => tui::Origin::Set,
                    Origin::Default => tui::Origin::Default,
                    Origin::Found => tui::Origin::Found,
                },
                help,
                note: note(setting),
                edit: edit(setting, home),
            }
        })
        .collect()
}

fn section(section: Section) -> &'static str {
    match section {
        Section::Harness(id) => name(id),
        Section::Meter => "Usage meter",
        Section::Runs => "Runs",
        Section::Review => "Review",
        Section::Roles => "Roles",
    }
}

fn name(id: HarnessId) -> &'static str {
    match id {
        HarnessId::Claude => "Claude Code",
        HarnessId::Codex => "Codex",
    }
}

/// A setting's label, and what it does.
fn words(key: Key) -> (String, String) {
    let (label, help): (&str, String) = match key {
        Key::Enabled(id) => (
            "Enabled",
            format!(
                "Runs go to {} only while this is on. A run sends your code to its vendor.",
                name(id)
            ),
        ),
        Key::Cap(id) => (
            "Usage cap",
            format!(
                "A run starts only while less than this much of {}'s plan is used.",
                name(id)
            ),
        ),
        Key::AbortAt(id) => (
            "Stop runs at",
            format!(
                "A run that is already going is stopped once this much of {}'s plan is used. \
                 Above the usage cap.",
                name(id)
            ),
        ),
        Key::MaxConcurrent(id) => (
            "Runs at once",
            format!("How many runs {} takes at the same time.", name(id)),
        ),
        Key::Billing(id) => (
            "Billing",
            format!("How {} is paid for when cahoots runs it.", name(id)),
        ),
        Key::Binary(id) => ("Program", format!("Which {} program runs.", name(id))),
        Key::Meter => (
            "Meter",
            "Where cahoots reads how much of each plan is used.".to_string(),
        ),
        Key::MeterBinary(MeterId::AgentUsage) => (
            "Agent Usage program",
            "Which usage-cli the agent-usage meter runs.".to_string(),
        ),
        Key::MaxDataAge => (
            "Data age",
            "How old Agent Usage's numbers may be before they count as stale.".to_string(),
        ),
        Key::MeterBinary(MeterId::Ccusage) => (
            "ccusage program",
            "Which ccusage the ccusage meter runs.".to_string(),
        ),
        Key::ClaudeBlockTokens => (
            "Claude Code block",
            "Tokens in one 5-hour block that count as Claude Code's whole plan. ccusage can't \
             see the plan's limit, so a percentage exists only against this."
                .to_string(),
        ),
        Key::CodexDayTokens => (
            "Codex day",
            "Tokens in one day that count as Codex's whole plan. Without it, Codex runs are held \
             only by the runs-per-hour limit."
                .to_string(),
        ),
        Key::RunsPerHour => (
            "Runs per hour",
            "cahoots' own ceiling on the runs started in an hour, with a meter or without one. \
             At 0, no run starts."
                .to_string(),
        ),
        Key::TokensPerDay => (
            "Tokens per day",
            "cahoots' own ceiling on the tokens its runs use in a day.".to_string(),
        ),
        Key::MaxActiveRuns => (
            "Runs at once",
            "How many runs may go at the same time, across every agent.".to_string(),
        ),
        Key::MaxDepth => (
            "Delegation depth",
            "How deep delegation goes. At 1, an agent that was delegated to can't delegate \
             again; at 0, nothing is delegated."
                .to_string(),
        ),
        Key::Timeout => (
            "Time limit",
            "A run is stopped once it has gone this long.".to_string(),
        ),
        Key::Wait => (
            "Wait for result",
            "How long `cahoots run` waits for the result before it hands back the run's id."
                .to_string(),
        ),
        Key::IntGrace => (
            "Interrupt grace",
            "How long a run being stopped gets after SIGINT, before SIGTERM.".to_string(),
        ),
        Key::TermGrace => (
            "Terminate grace",
            "How long a run being stopped gets after SIGTERM, before SIGKILL.".to_string(),
        ),
        Key::Watchdog => (
            "Watchdog",
            "How often a running job's agent is checked against where its runs stop.".to_string(),
        ),
        Key::AllowInPlace => (
            "Writers in place",
            "Let a writer work in your own working tree (`--in-place`) instead of a worktree of \
             its own."
                .to_string(),
        ),
        Key::Review => (
            "Review runs",
            "Offer a sample of finished runs for review by the agent that delegated them. It \
             spends that agent's plan."
                .to_string(),
        ),
        Key::SampleRate => (
            "Sample",
            "The share of finished runs offered for review.".to_string(),
        ),
        Key::ApplyRouting => (
            "Apply routing",
            "Let outcomes reorder a role's candidates, by one place at most. While it is off, \
             `cahoots report --suggest` only says what it would do."
                .to_string(),
        ),
        Key::Candidates(role) => (
            role_label(role),
            format!(
                "Who takes {} runs, first choice first. The next is tried when one is over its \
                 cap or busy.",
                role_work(role)
            ),
        ),
        Key::Calibrate(role) => {
            return (
                format!("{} learns", role_label(role)),
                "Let what reviews learn reorder this list of yours, by one place at most."
                    .to_string(),
            );
        }
    };
    (label.to_string(), help)
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::Advise => "Advise",
        Role::Review => "Review",
        Role::Explore => "Explore",
        Role::Implement => "Implement",
    }
}

fn role_work(role: Role) -> &'static str {
    match role {
        Role::Advise => "advice",
        Role::Review => "review",
        Role::Explore => "exploration",
        Role::Implement => "implementation",
    }
}

/// A value as the page shows it.
fn shown(value: Option<&Value>, kind: &Kind) -> String {
    match (value, kind) {
        (None, Kind::Program(_)) => "not found".to_string(),
        (None, _) => "not set".to_string(),
        (Some(Value::Bool(on)), _) => (if *on { "on" } else { "off" }).to_string(),
        (Some(Value::Number(n)), Kind::Number { unit, .. }) => tui::number(*n, tui_unit(*unit)),
        (Some(Value::Share(share)), _) => format!("{}%", percent(*share)),
        (Some(Value::Candidates(list)), _) => list
            .iter()
            .map(|c| format!("{} {}", c.harness, c.model.as_str()))
            .collect::<Vec<_>>()
            .join(", then "),
        (Some(value), _) => value.to_string(),
    }
}

fn percent(share: f64) -> i64 {
    (share * 100.0).round() as i64
}

fn candidate(candidate: &crate::model::Candidate) -> String {
    format!("{} {}", candidate.harness, candidate.model.as_str())
}

fn tui_unit(unit: Unit) -> tui::Unit {
    match unit {
        Unit::Percent => tui::Unit::Percent,
        Unit::Seconds => tui::Unit::Seconds,
        Unit::Tokens => tui::Unit::Tokens,
        Unit::Count => tui::Unit::Count,
    }
}

/// Where a value comes from, and what changing it does.
fn note(setting: &Setting) -> String {
    if setting.locked {
        return "Runs stop at 100% while the usage cap is 100%: lower the cap to set this."
            .to_string();
    }
    match (setting.origin, setting.key, &setting.value) {
        (Origin::Config, _, _) => match &setting.default {
            Some(default) => format!(
                "Set in config.toml. The default is {}.",
                shown(Some(default), &setting.kind)
            ),
            None => "Set in config.toml. Without it, there is no such limit.".to_string(),
        },
        (Origin::Found, Key::Meter, _) => {
            "The only meter `cahoots install` found. Choose it, and your choice is saved to \
             config.toml."
                .to_string()
        }
        (Origin::Found, _, _) => {
            "Where `cahoots install` found it. Choose another, and it is saved to config.toml."
                .to_string()
        }
        (Origin::Default, Key::Binary(_), None) => {
            "Not on your PATH: runs to it are refused until it is.".to_string()
        }
        (Origin::Default, Key::Binary(_), Some(_)) => {
            "The first on your PATH. Choose another, and it is saved to config.toml.".to_string()
        }
        (Origin::Default, Key::MeterBinary(_), None) => {
            "Not found: `cahoots install` looks for it.".to_string()
        }
        (Origin::Default, _, None) => {
            "Not set: there is no such limit. Set it, and it is saved to config.toml.".to_string()
        }
        (Origin::Default, _, Some(_)) => {
            "The default. Change it, and the change is saved to config.toml.".to_string()
        }
    }
}

/// How the page changes a setting.
fn edit(setting: &Setting, home: &Path) -> Edit {
    if setting.locked {
        return Edit::Fixed;
    }
    let number = |value: Option<&Value>| match value {
        Some(Value::Number(n)) => Some(*n),
        Some(Value::Share(share)) => Some(percent(*share)),
        _ => None,
    };
    match &setting.kind {
        Kind::Toggle => Edit::Toggle,
        Kind::Choice(names) => Edit::Choose {
            choices: names
                .iter()
                .map(|n| Choice::new(*n, choice_hint(setting.key, n)))
                .collect(),
            current: names
                .iter()
                .position(|n| setting.value == Some(Value::Name(n.to_string()))),
        },
        Kind::Program(programs) if programs.is_empty() => Edit::Fixed,
        Kind::Program(programs) => Edit::Choose {
            choices: programs
                .iter()
                .map(|program| {
                    Choice::new(
                        tilde(&program.path, home),
                        match program.from {
                            Source::Path => "on your PATH",
                            Source::Install => "where install found it",
                            Source::Config => "set in config.toml",
                        },
                    )
                })
                .collect(),
            current: programs
                .iter()
                .position(|p| setting.value == Some(Value::Program(p.path.clone()))),
        },
        Kind::Number { min, max, unit } => {
            let default = number(setting.default.as_ref());
            Edit::Step {
                value: number(setting.value.as_ref()),
                min: *min,
                max: *max,
                unit: tui_unit(*unit),
                default,
                start: match setting.key {
                    Key::ClaudeBlockTokens => 50_000_000,
                    Key::CodexDayTokens => 20_000_000,
                    Key::TokensPerDay => 10_000_000,
                    _ => default.unwrap_or(*min),
                },
            }
        }
        Kind::Share => {
            let default = number(setting.default.as_ref());
            Edit::Step {
                value: number(setting.value.as_ref()),
                min: 0,
                max: 100,
                unit: tui::Unit::Percent,
                default,
                start: default.unwrap_or(0),
            }
        }
        Kind::Order => Edit::Order {
            items: match &setting.value {
                Some(Value::Candidates(list)) => list
                    .iter()
                    .map(|c| format!("{}  {}  {}", c.harness, c.model.as_str(), c.effort))
                    .collect(),
                _ => Vec::new(),
            },
        },
    }
}

fn choice_hint(key: Key, choice: &str) -> &'static str {
    match (key, choice) {
        (Key::Billing(_), "subscription") => "your signed-in plan: API keys are kept from the run",
        (Key::Billing(_), _) => "per-token API billing, on purpose: the key is passed through",
        (Key::Meter, "agent-usage") => MeterId::AgentUsage.summary(),
        (Key::Meter, "ccusage") => MeterId::Ccusage.summary(),
        (Key::Meter, _) => NO_METER,
        _ => "",
    }
}

/// A path with the home directory as `~`.
fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now(text: &str) -> Vec<Setting> {
        settings::current(&UserConfig::parse(text).unwrap(), None, None)
    }

    #[test]
    fn every_setting_has_words_and_a_row() {
        let settings = now(
            "schema = 1\n[roles.advise]\ncandidates = [{ harness = \"claude\", model = \"opus\", effort = \"high\" }]",
        );
        let rows = rows(&settings, Path::new("/home/u"));
        assert_eq!(rows.len(), settings.len());
        for row in &rows {
            assert!(!row.label.is_empty() && row.help.ends_with('.'), "{row:?}");
            assert!(!row.note.is_empty(), "{row:?}");
        }
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert!(labels.contains(&"Advise learns"), "{labels:?}");
        let cap = rows
            .iter()
            .find(|row| row.id == "harness.codex.cap")
            .unwrap();
        assert_eq!((cap.section.as_str(), cap.value.as_str()), ("Codex", "75%"));
        assert_eq!(
            cap.note,
            "The default. Change it, and the change is saved to config.toml."
        );
    }

    #[test]
    fn a_value_set_says_so_and_names_its_default() {
        let settings = now(
            "schema = 1\n[meter.ccusage]\nclaude_block_tokens = 300_000_000\n[review]\nsample_rate = 0.5",
        );
        let rows = rows(&settings, Path::new("/home/u"));
        let block = rows
            .iter()
            .find(|row| row.id == "meter.ccusage.claude_block_tokens")
            .unwrap();
        assert_eq!(block.value, "300M tokens");
        assert_eq!(block.origin, tui::Origin::Set);
        assert_eq!(
            block.note,
            "Set in config.toml. Without it, there is no such limit."
        );
        let sample = rows
            .iter()
            .find(|row| row.id == "review.sample_rate")
            .unwrap();
        assert_eq!(
            (sample.value.as_str(), sample.note.as_str()),
            ("50%", "Set in config.toml. The default is 20%.")
        );
        assert!(matches!(
            sample.edit,
            Edit::Step {
                value: Some(50),
                default: Some(20),
                ..
            }
        ));
    }

    #[test]
    fn an_answer_that_puts_back_the_default_takes_the_key_out() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("config.toml");
        std::fs::write(&file, "schema = 1\n").unwrap();
        let read = || settings::current(&UserConfig::load(&file).unwrap(), None, None);
        let find =
            |settings: &[Setting], name: &str| settings::find(settings, name).unwrap().clone();

        let done = carry_out(
            &file,
            &find(&read(), "harness.codex.cap"),
            Answer::Number(Some(60)),
        )
        .unwrap();
        assert_eq!(done, "Saved to config.toml: [harness.codex] cap = 60");
        let done = carry_out(
            &file,
            &find(&read(), "harness.codex.cap"),
            Answer::Number(Some(75)),
        )
        .unwrap();
        assert_eq!(
            done,
            "Took [harness.codex] cap out of config.toml: back to 75%"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "schema = 1\n");

        let done = carry_out(&file, &find(&read(), "harness.codex.enabled"), Answer::Flip).unwrap();
        assert_eq!(done, "Saved to config.toml: [harness.codex] enabled = true");
        carry_out(&file, &find(&read(), "harness.codex.enabled"), Answer::Flip).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "schema = 1\n");

        let done = carry_out(
            &file,
            &find(&read(), "review.sample_rate"),
            Answer::Number(Some(35)),
        )
        .unwrap();
        assert_eq!(done, "Saved to config.toml: [review] sample_rate = 0.35");

        // Even the default meter is written: a choice is a choice.
        let done = carry_out(&file, &find(&read(), "meter.use"), Answer::Chose(2)).unwrap();
        assert_eq!(done, "Saved to config.toml: [meter] use = \"none\"");

        let done = carry_out(
            &file,
            &find(&read(), "roles.explore.candidates"),
            Answer::Order(vec![1, 0]),
        )
        .unwrap();
        assert_eq!(
            done,
            "Saved to config.toml: [roles.explore] candidates, claude sonnet first"
        );
    }
}
