//! `cahoots doctor` — a read-only look at everything a run depends on, so that
//! "why was I refused" has an answer before the run is attempted. It changes
//! nothing: not the config, not a harness's permission rules.

use serde::Serialize;
use serde_json::json;

use crate::dirs::Dirs;
use crate::env;
use crate::exit::{Envelope, Exit, Res};
use crate::harness;
use crate::install::rules;
use crate::meter::{Answer, Ask, Ccusage, Chosen, UsageMeter, detect, tokens};
use crate::model::HarnessId;
use crate::pick;
use crate::registry::Registry;

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
}

fn check(name: impl Into<String>, status: Status, detail: impl Into<String>) -> Check {
    Check {
        check: name.into(),
        status,
        detail: detail.into(),
    }
}

pub fn doctor() -> Res<Envelope> {
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
            return Ok(finish(checks));
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
            check(
                format!("{id}: caller rules"),
                Status::Warn,
                format!(
                    "to let {id} delegate without a prompt, a person adds to {}: {}",
                    rules::rules_file(id),
                    lines.join("  ")
                ),
            )
        });
    }

    let stale = crate::install::files::stale(&dirs);
    checks.push(if stale.is_empty() {
        check(
            "installed files",
            Status::Ok,
            "nothing installed is out of date",
        )
    } else {
        let paths: Vec<String> = stale.iter().map(|p| p.display().to_string()).collect();
        check(
            "installed files",
            Status::Warn,
            format!("out of date — run `cahoots install`: {}", paths.join(", ")),
        )
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
    Ok(finish(checks))
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

fn finish(checks: Vec<Check>) -> Envelope {
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let exit = if failed == 0 { Exit::Ok } else { Exit::Config };
    let message = (failed > 0).then(|| format!("{failed} check(s) failed"));
    Envelope::new(exit, message).with_data(json!({ "checks": checks }))
}
