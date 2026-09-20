//! `cahoots doctor` — a read-only look at everything a run depends on, so that
//! "why was I refused" has an answer before the run is attempted. It changes
//! nothing: not the config, not a harness's permission rules.

use std::time::Duration;

use serde::Serialize;
use serde_json::json;

use crate::dirs::Dirs;
use crate::env;
use crate::exit::{Envelope, Exit, Res};
use crate::harness;
use crate::install::rules;
use crate::model::HarnessId;
use crate::pick;
use crate::registry::Registry;
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

    match &registry.meters.agent_usage {
        None => checks.push(check(
            "meter: agent-usage",
            Status::Warn,
            "not configured — only the built-in ledger gates runs, and it cannot see what you use outside cahoots",
        )),
        Some(meter) => match spawn::resolve_binary("usage-cli", Some(&meter.binary), &[]) {
            Err(fail) => checks.push(check("meter: agent-usage", Status::Fail, fail.message)),
            Ok(binary) => {
                for id in HarnessId::ALL.into_iter().filter(|id| registry.harness(*id).enabled) {
                    let args = ["headroom", "--provider", id.as_str(), "--cap", "100", "--json"];
                    let status = spawn::run_helper(&binary, &args, None, Duration::from_secs(10))
                        .ok()
                        .and_then(|output| output.status);
                    checks.push(match status {
                        Some(0 | 24 | 25) => check(format!("meter: {id}"), Status::Ok, "the tracker answers"),
                        Some(26) => check(format!("meter: {id}"), Status::Fail, "the tracker has no numbers for it — every run will be refused"),
                        Some(19) => check(format!("meter: {id}"), Status::Fail, "this usage-cli has no `headroom` noun — update the tracker"),
                        _ => check(format!("meter: {id}"), Status::Fail, "the tracker gave no answer — is its daemon running?"),
                    });
                }
            }
        },
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

fn finish(checks: Vec<Check>) -> Envelope {
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let exit = if failed == 0 { Exit::Ok } else { Exit::Config };
    let message = (failed > 0).then(|| format!("{failed} check(s) failed"));
    Envelope::new(exit, message).with_data(json!({ "checks": checks }))
}
