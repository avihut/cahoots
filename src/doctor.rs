//! `cahoots doctor` — a read-only look at everything a run depends on, so that
//! "why was I refused" has an answer before the run is attempted. It changes
//! nothing: not the config, not a harness's permission rules. It returns the
//! checks, with the evidence gaps beside them as data; the command layer gives
//! the gaps their words, `envelope` makes the checks the one JSON envelope, and
//! a person at a terminal reads the same checks in words (`cli::endings`).

use std::path::Path;

use serde::Serialize;
use serde_json::json;

use crate::dirs::Dirs;
use crate::env;
use crate::exit::{Envelope, Exit, Res};
use crate::harness;
use crate::harness::Version;
use crate::history::{self, Story};
use crate::install::files::{Stale, StaleWhy};
use crate::install::rules;
use crate::meter::{Answer, Ask, Ccusage, Chosen, UsageMeter, detect, tokens};
use crate::model::{Candidate, HarnessId, Role, TaskKindName};
use crate::paths::Workspace;
use crate::pick;
use crate::placement::provider::{self, ProviderId};
use crate::registry::{Origin, Registry};
use crate::spawn;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub check: String,
    pub status: Status,
    pub detail: String,
    /// The rules a harness still needs, one to a line, for a person to
    /// paste into its file. `detail` names them on one line, for the
    /// envelope, which never carries this.
    #[serde(skip)]
    pub rules: Option<(HarnessId, Vec<String>)>,
}

fn check(name: impl Into<String>, status: Status, detail: impl Into<String>) -> Check {
    Check {
        check: name.into(),
        status,
        detail: detail.into(),
        rules: None,
    }
}

/// A candidate in a list that is currently configured, with no rated or
/// failed run on record for it in that list. Data only: `cli::endings` says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceGap {
    pub role: Role,
    /// `Some` for a named kind's list, `None` for a role's.
    pub kind: Option<TaskKindName>,
    pub candidate: Candidate,
}

/// What `doctor` found: its checks, and the candidates nothing is known about.
#[derive(Debug)]
pub struct Diagnostics {
    pub checks: Vec<Check>,
    pub gaps: Vec<EvidenceGap>,
}

/// The candidates of every current kind list and every list a person wrote
/// that have no rated or failed run on record in THAT list: a kind's by kind
/// name, role and candidate; a role's by role and candidate, from runs with
/// no kind. History is read whole — no window, and `learn reset` does not
/// forget it — because this looks for an absence, not for a recent expiry.
/// Roles first, then kinds, each in its list's configured order.
pub fn evidence_gaps(registry: &Registry, stories: &[Story]) -> Vec<EvidenceGap> {
    let has_evidence = |matches: &dyn Fn(&Story) -> bool| {
        let mut evidence = crate::calibrate::Evidence::default();
        stories
            .iter()
            .filter(|story| matches(story))
            .for_each(|story| evidence.observe(story));
        evidence.finish();
        evidence.n > 0
    };
    let mut gaps = Vec::new();
    for (role, entry) in &registry.roles {
        if entry.origin != Origin::User {
            continue;
        }
        // Learning may have swapped two; the list a person wrote is the order.
        let mut candidates = entry.candidates.clone();
        if let Some(at) = entry.learned_swap.filter(|at| at + 1 < candidates.len()) {
            candidates.swap(at, at + 1);
        }
        for candidate in candidates {
            if !has_evidence(&|story| {
                story.kind.is_none() && story.role == *role && story.target == candidate
            }) {
                gaps.push(EvidenceGap {
                    role: *role,
                    kind: None,
                    candidate,
                });
            }
        }
    }
    for (name, entry) in &registry.kinds {
        for candidate in &entry.candidates {
            if !has_evidence(&|story| {
                story.kind.as_ref() == Some(name)
                    && story.role == entry.role
                    && story.target == *candidate
            }) {
                gaps.push(EvidenceGap {
                    role: entry.role,
                    kind: Some(name.clone()),
                    candidate: candidate.clone(),
                });
            }
        }
    }
    gaps
}

/// Every check, in order, and the evidence gaps beside them.
pub fn checks() -> Res<Diagnostics> {
    let mut checks = Vec::new();
    checks.push(check(
        "build",
        if env::dev_overrides_honoured() {
            Status::Warn
        } else {
            Status::Ok
        },
        if env::dev_overrides_honoured() {
            format!(
                "{} — a DEV build: CAHOOTS_*_DIR overrides are honoured; do not install this",
                crate::cli::VERSION
            )
        } else {
            crate::cli::VERSION.to_string()
        },
    ));

    let dirs = Dirs::resolve()?;
    checks.push(check(
        "directories",
        Status::Ok,
        format!(
            "config {} · state {}",
            dirs.config.display(),
            dirs.state.display()
        ),
    ));

    // Where builds before config.toml held it kept which targets were on. It
    // is not read at all now, so a target listed there is off until enabled
    // again.
    if dirs.config.join("enabled.json").exists() {
        checks.push(check(
            "enabled.json",
            Status::Warn,
            format!(
                "no longer read: a target is on when its table in {} says `enabled = true` — \
                 `cahoots enable <harness>` or `cahoots settings` writes that. Then delete {}",
                dirs.config_file().display(),
                dirs.config.join("enabled.json").display()
            ),
        ));
    }

    let registry = match Registry::load(&dirs) {
        Ok(registry) => {
            checks.push(check(
                "config",
                Status::Ok,
                "parses, and every value is in range",
            ));
            registry
        }
        Err(fail) => {
            checks.push(check("config", Status::Fail, fail.message));
            return Ok(Diagnostics {
                checks,
                gaps: Vec::new(),
            });
        }
    };

    for id in HarnessId::ALL {
        let entry = registry.harness(id);
        let tool = harness::harness(id);
        match pick::locate(&registry, id, &[]) {
            Ok((binary, version)) => {
                let newer = version >= tool.tested().1;
                checks.push(check(
                    format!("{id}: binary"),
                    if newer { Status::Warn } else { Status::Ok },
                    format!(
                        "{} {version}{}",
                        binary.display(),
                        if newer {
                            " — newer than the versions cahoots was tested against"
                        } else {
                            ""
                        }
                    ),
                ));
            }
            Err(fail) => checks.push(check(
                format!("{id}: binary"),
                if entry.enabled {
                    Status::Fail
                } else {
                    Status::Warn
                },
                fail.message,
            )),
        }
        checks.push(check(
            format!("{id}: target"),
            if entry.enabled { Status::Ok } else { Status::Warn },
            if entry.enabled {
                format!("enabled · cap {}% · {} at a time", entry.cap, entry.max_concurrent)
            } else {
                format!("not enabled — nothing is delegated to it until a person runs `cahoots enable {id}`")
            },
        ));
        let missing = rules::missing(&dirs.home, id);
        checks.push(if missing.is_empty() {
            check(
                format!("{id}: caller rules"),
                if rules::is_broad(&dirs.home, id) { Status::Warn } else { Status::Ok },
                if rules::is_broad(&dirs.home, id) {
                    "allowed — by a rule for ALL of cahoots, which also covers the verbs meant for people"
                } else {
                    "every agent verb is allowed"
                },
            )
        } else {
            let lines: Vec<String> = missing.iter().map(|verb| rules::rule(id, verb)).collect();
            let detail = format!(
                "to let {id} delegate without a prompt, a person adds to {}: {}",
                rules::rules_file(id),
                lines.join("  ")
            );
            Check {
                rules: Some((id, lines)),
                ..check(format!("{id}: caller rules"), Status::Warn, detail)
            }
        });
    }

    checks.push(fork_check(&registry));

    let stale = crate::install::files::stale(&dirs, &registry.kinds);
    checks.push(if stale.is_empty() {
        check(
            "installed files",
            Status::Ok,
            "nothing installed is out of date",
        )
    } else {
        check("installed files", Status::Warn, stale_detail(&stale))
    });

    match &registry.meters.usage {
        None => checks.push(check(
            "meter",
            Status::Warn,
            "no usage meter — only the built-in ledger gates runs, and it cannot see what you use \
             outside cahoots. `cahoots install` looks for Agent Usage and ccusage, and \
             `cahoots settings` chooses one",
        )),
        Some(meter) => meter_checks(&mut checks, &registry, meter),
    }
    checks.push(check(
        "meter: ledger",
        Status::Ok,
        format!(
            "at most {} runs per hour per target",
            registry.meters.ledger_max_runs_per_hour
        ),
    ));
    let gaps = evidence_gaps(&registry, &history::stories(&history::read(&dirs)));
    Ok(Diagnostics { checks, gaps })
}

/// What cuts a writer's worktree, and whether it can: the provider a person
/// chose, held to the same checks a cut makes, and — where git cuts in a
/// repository that has a `daft.yml` — that daft could. Read around the
/// working directory: the repository it is in, if any.
fn fork_check(registry: &Registry) -> Check {
    let fork = &registry.fork;
    let cwd = std::env::current_dir().ok();
    // A `git` planted in the workspace is refused here as everywhere, and
    // never asked its version: that refusal is the check's answer.
    let workspace = match cwd.as_deref().map(Workspace::around) {
        Some(Err(fail)) if fail.exit == Exit::Policy => {
            return check("fork", Status::Fail, fail.message);
        }
        Some(Ok(workspace)) => Some(workspace),
        _ => None,
    };
    let roots: Vec<&Path> = match &workspace {
        Some(workspace) => workspace.roots(),
        None => cwd.as_deref().into_iter().collect(),
    };
    let daft_yml = workspace
        .as_ref()
        .and_then(|workspace| workspace.toplevel.as_ref())
        .is_some_and(|top| top.join("daft.yml").is_file());
    let tool = provider::provider(fork.provider);
    let newer = |version: Version| {
        if version >= tool.tested().1 {
            " — newer than the versions cahoots was tested against"
        } else {
            ""
        }
    };
    match fork.provider {
        ProviderId::Git => {
            // Without a git, nothing is a repository, and `--fork` is refused
            // before any cut: there is nothing here a cut could fail on.
            if let Err(fail) = spawn::system_tool("git", &roots)
                && fail.exit != Exit::Policy
            {
                return check(
                    "fork",
                    Status::Warn,
                    "no git on PATH — cahoots cannot see a repository without one, so --fork is \
                     refused until git is there",
                );
            }
            match provider::locate(ProviderId::Git, fork, &roots) {
                Err(fail) => check("fork", Status::Fail, fail.message),
                Ok((_, version)) if daft_yml => check(
                    "fork",
                    Status::Warn,
                    format!(
                        "git {version} cuts each writer's worktree, and this repository has a \
                         daft.yml: choose daft with `cahoots settings` (fork.provider and \
                         fork.daft.binary) to have daft cut them here{}",
                        newer(version)
                    ),
                ),
                Ok((_, version)) => check(
                    "fork",
                    if newer(version).is_empty() {
                        Status::Ok
                    } else {
                        Status::Warn
                    },
                    format!(
                        "git {version} cuts each writer's worktree (fork.provider = \"git\"){}",
                        newer(version)
                    ),
                ),
            }
        }
        ProviderId::Daft if fork.daft_binary.is_none() => check(
            "fork",
            Status::Fail,
            "cahoots needs `daft`: none is chosen — fork.provider is \"daft\"; choose one with \
             fork.daft.binary (`cahoots settings`)",
        ),
        ProviderId::Daft => match provider::locate(ProviderId::Daft, fork, &roots) {
            Err(fail) => check("fork", Status::Fail, fail.message),
            Ok((binary, version)) => {
                let what = format!(
                    "daft {version} ({}) cuts each writer's worktree where the repository has a \
                     daft.yml, git elsewhere",
                    binary.display()
                );
                let (status, hooks) = if fork.daft_hooks {
                    (
                        Status::Warn,
                        "daft runs the repository's hooks in each new worktree, when daft trusts \
                         the repository (fork.daft.hooks = on)",
                    )
                } else {
                    (Status::Ok, "daft's hooks are off")
                };
                let status = if newer(version).is_empty() {
                    status
                } else {
                    Status::Warn
                };
                check("fork", status, format!("{what}; {hooks}{}", newer(version)))
            }
        },
    }
}

/// Doctor's words for what `install` would change, each path with why.
fn stale_detail(stale: &[Stale]) -> String {
    let paths: Vec<String> = stale
        .iter()
        .map(|file| {
            let why = match file.why {
                StaleWhy::Changed => "changed",
                StaleWhy::NotInstalled => "not installed yet",
                StaleWhy::NoLongerWanted => "no longer wanted",
            };
            format!("{} ({why})", file.path.display())
        })
        .collect();
    format!("out of date — run `cahoots install`: {}", paths.join(", "))
}

/// The usage meter: is it there and usable, and what does it say about each
/// enabled target?
fn meter_checks(checks: &mut Vec<Check>, registry: &Registry, meter: &UsageMeter) {
    let name = format!("meter: {}", meter.id());
    let by = match registry.meters.usage_chosen_by {
        Some(Chosen::Config) => "chosen in config.toml",
        _ => "the only one `cahoots install` found",
    };
    let exe = match meter.resolve() {
        Ok(exe) => exe,
        Err(fail) => {
            checks.push(check(
                name,
                Status::Fail,
                format!("{} ({by})", fail.message),
            ));
            return;
        }
    };
    let found = detect::probe(meter.id(), &exe.pinned);
    let what = match &found.version {
        Some(version) => format!("{} {version} — {by}", exe.pinned.display()),
        None => format!("{} — {by}", exe.pinned.display()),
    };
    checks.push(match (&found.unusable, &found.note) {
        (Some(why), _) => check(name, Status::Fail, format!("{what}: {why}")),
        (None, Some(note)) => check(name, Status::Warn, format!("{what}: {note}")),
        (None, None) => check(name, Status::Ok, what),
    });
    if !found.usable() {
        return;
    }
    for id in HarnessId::ALL
        .into_iter()
        .filter(|id| registry.harness(*id).enabled)
    {
        let answer = meter.ask(&Ask {
            harness: id,
            cap: 100,
            fresh: false,
            forecast: false,
        });
        let (status, detail) = match meter {
            UsageMeter::AgentUsage(_) => tracker_health(&answer),
            UsageMeter::Ccusage(ccusage) => ccusage_health(ccusage, id, &answer),
        };
        checks.push(check(format!("meter: {id}"), status, detail));
    }
}

fn tracker_health(answer: &Answer) -> (Status, String) {
    match answer.verdict {
        Exit::Ok | Exit::OverCap | Exit::Forecast => (
            Status::Ok,
            match answer.percent {
                Some(percent) => format!("the tracker answers — {percent:.0}% used"),
                None => "the tracker answers".to_string(),
            },
        ),
        Exit::NoData => (
            Status::Fail,
            "the tracker has no numbers for it — every run will be refused".to_string(),
        ),
        _ => (
            Status::Fail,
            answer.why.clone().unwrap_or_else(|| {
                "the tracker gave no answer — is its daemon running?".to_string()
            }),
        ),
    }
}

fn ccusage_health(meter: &Ccusage, harness: HarnessId, answer: &Answer) -> (Status, String) {
    if answer.verdict == Exit::NoDigest {
        return (
            Status::Fail,
            answer
                .why
                .clone()
                .unwrap_or_else(|| "ccusage gave no answer".to_string()),
        );
    }
    // Claude Code logged that it is at its limit: every run waits for the reset.
    if let Some(why) = &answer.why {
        return (Status::Warn, why.clone());
    }
    let used = answer
        .reading
        .as_ref()
        .and_then(|reading| reading["tokens"].as_u64())
        .unwrap_or(0);
    let (window, knob) = match harness {
        HarnessId::Claude => ("in this 5-hour block", "claude_block_tokens"),
        HarnessId::Codex => ("today", "codex_day_tokens"),
    };
    match (meter.limit(harness), answer.percent) {
        (Some(limit), Some(percent)) => (
            Status::Ok,
            format!(
                "{percent:.0}% of the {} tokens you set — {} used {window}",
                tokens(limit),
                tokens(used)
            ),
        ),
        _ if harness == HarnessId::Claude => (
            Status::Warn,
            format!(
                "{} tokens used {window}, and no limit declared: only Claude Code's own limit \
                 notice stops a run — set {knob} under [meter.ccusage] (`cahoots settings`) to \
                 cap it",
                tokens(used)
            ),
        ),
        _ => (
            Status::Warn,
            format!(
                "not measured: {} tokens used {window}, and no limit declared — set {knob} under \
                 [meter.ccusage] (`cahoots settings`) to cap it",
                tokens(used)
            ),
        ),
    }
}

/// The checks as the one JSON envelope: exit 34 when one failed.
pub fn envelope(checks: &[Check]) -> Envelope {
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let exit = if failed == 0 { Exit::Ok } else { Exit::Config };
    let message = (failed > 0).then(|| format!("{failed} check(s) failed"));
    Envelope::new(exit, message).with_data(json!({ "checks": checks }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rules_to_paste_never_reach_the_envelope() {
        let rules = Check {
            rules: Some((HarnessId::Claude, vec!["a rule".to_string()])),
            ..check(
                "claude: caller rules",
                Status::Warn,
                "a person adds: a rule",
            )
        };
        let envelope = envelope(&[rules]);
        assert_eq!(envelope.code, 0);
        assert_eq!(
            envelope.data,
            Some(json!({ "checks": [{
                "check": "claude: caller rules",
                "status": "warn",
                "detail": "a person adds: a rule",
            }] }))
        );
    }

    #[test]
    fn one_failed_check_is_exit_34() {
        let envelope = envelope(&[
            check("build", Status::Ok, "1.2.3"),
            check("config", Status::Fail, "unclosed table"),
        ]);
        assert_eq!(envelope.code, 34);
        assert_eq!(envelope.message.as_deref(), Some("1 check(s) failed"));
    }

    use crate::config::UserConfig;
    use crate::history::Outcome;
    use crate::model::{Effort, ModelName};
    use crate::run::record::State;

    fn candidate(model: &str, effort: Effort) -> Candidate {
        Candidate {
            harness: HarnessId::Codex,
            model: ModelName::try_from(model.to_string()).unwrap(),
            effort,
        }
    }

    fn story(
        kind: Option<&str>,
        role: Role,
        target: &Candidate,
        state: State,
        outcome: Option<Outcome>,
    ) -> Story {
        Story {
            run: "r".to_string(),
            t: 1,
            role,
            kind: kind.map(|name| TaskKindName::try_from(name.to_string()).unwrap()),
            caller: None,
            target: target.clone(),
            blind: false,
            exploration: false,
            dir: "/w".into(),
            state,
            exit: 0,
            tokens_in: 0,
            tokens_out: 0,
            secs: 1,
            sampled: false,
            base_commit: None,
            patch: None,
            resumed_from: None,
            base_repo: None,
            outcome,
            measures: Vec::new(),
        }
    }

    const CONFIG: &str = r#"
schema = 1
[roles.review]
candidates = [
  { harness = "codex", model = "a", effort = "high" },
  { harness = "codex", model = "b", effort = "high" },
]
[kinds.rust-review]
description = "Review Rust."
role = "review"
candidates = [
  { harness = "codex", model = "a", effort = "medium" },
  { harness = "codex", model = "a", effort = "high" },
]
"#;

    fn gaps(stories: &[Story]) -> Vec<(Option<String>, Role, String, Effort)> {
        let registry = Registry::effective(&UserConfig::parse(CONFIG).unwrap());
        evidence_gaps(&registry, stories)
            .into_iter()
            .map(|gap| {
                (
                    gap.kind.map(|kind| kind.to_string()),
                    gap.role,
                    gap.candidate.model.as_str().to_string(),
                    gap.candidate.effort,
                )
            })
            .collect()
    }

    #[test]
    fn a_current_list_is_warned_about_once_per_candidate_with_no_evidence_in_that_list() {
        let kind = Some("rust-review".to_string());
        let all = |kind: &Option<String>, role, model: &str, effort| {
            (kind.clone(), role, model.to_string(), effort)
        };
        // Nothing on record: the person's role list (and not the shipped
        // ones), then the kind, each in its configured order.
        assert_eq!(
            gaps(&[]),
            [
                all(&None, Role::Review, "a", Effort::High),
                all(&None, Role::Review, "b", Effort::High),
                all(&kind, Role::Review, "a", Effort::Medium),
                all(&kind, Role::Review, "a", Effort::High),
            ]
        );
        let (a_high, a_medium) = (candidate("a", Effort::High), candidate("a", Effort::Medium));
        let rated = Some(Outcome::Discarded);
        // One rated run in the list clears it — and only that list and that
        // effort: a kind's run never clears a role's, nor the other way, nor
        // another role's, and unknown, cancelled and budget are not evidence.
        let cleared = gaps(&[
            story(None, Role::Review, &a_high, State::Done, rated),
            story(
                kind.as_deref(),
                Role::Review,
                &a_medium,
                State::Failed,
                None,
            ),
            story(kind.as_deref(), Role::Advise, &a_high, State::Done, rated),
            story(kind.as_deref(), Role::Review, &a_high, State::Done, None),
            story(
                kind.as_deref(),
                Role::Review,
                &a_high,
                State::Cancelled,
                None,
            ),
            story(kind.as_deref(), Role::Review, &a_high, State::Budget, None),
        ]);
        assert_eq!(
            cleared,
            [
                all(&None, Role::Review, "b", Effort::High),
                all(&kind, Role::Review, "a", Effort::High),
            ]
        );
    }

    #[test]
    fn shipped_role_lists_are_left_alone() {
        let registry = Registry::effective(&UserConfig::parse("schema = 1").unwrap());
        assert!(evidence_gaps(&registry, &[]).is_empty());
    }

    #[test]
    fn the_order_a_person_wrote_survives_what_learning_swapped() {
        let mut registry = Registry::effective(&UserConfig::parse(CONFIG).unwrap());
        let entry = registry.roles.get_mut(&Role::Review).unwrap();
        entry.candidates.swap(0, 1);
        entry.learned_swap = Some(0);
        let models: Vec<String> = evidence_gaps(&registry, &[])
            .into_iter()
            .filter(|gap| gap.kind.is_none())
            .map(|gap| gap.candidate.model.as_str().to_string())
            .collect();
        assert_eq!(models, ["a", "b"]);
    }

    #[test]
    fn a_stale_file_says_why() {
        let stale = [
            (StaleWhy::Changed, "/h/a.md"),
            (StaleWhy::NotInstalled, "/h/b.toml"),
            (StaleWhy::NoLongerWanted, "/h/c.md"),
        ]
        .map(|(why, path)| Stale {
            path: path.into(),
            why,
        });
        assert_eq!(
            stale_detail(&stale),
            "out of date — run `cahoots install`: /h/a.md (changed), /h/b.toml (not installed \
             yet), /h/c.md (no longer wanted)"
        );
    }
}
