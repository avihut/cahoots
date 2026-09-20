//! Admission: may this target take one more run right now?
//!
//! `used + reserve(role) ≤ cap`, and every configured meter must agree.
//!
//! - **agent-usage** asks the Agent Usage tracker's `usage-cli headroom`
//!   (what share of the plan is used, how old that number is, where it is
//!   heading). Its exit codes are the gate's exit codes, unchanged.
//! - **ledger** is built in: cahoots' own run records. It works on day one
//!   without any tracker, and its runs-per-hour ceiling is what bounds an
//!   agent stuck in a retry loop.
//!
//! Anything the gate does not understand is a refusal. It fails closed.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dirs::Dirs;
use crate::exit::{Exit, Fail, Res};
use crate::model::{Candidate, HarnessId, Role};
use crate::registry::{DEFAULT_MAX_DATA_AGE_SECS, Registry};
use crate::run::record::{self, now};
use crate::spawn;

/// How much lower the cap is when a snapshot-on-use provider's number is
/// stale. A stale reading is a lower bound on usage, so the margin is what
/// pays for not knowing how much was used since.
pub const STALE_MARGIN: u8 = 15;

/// Why a run was let in — kept on the run record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Admission {
    /// The tracker's own answer, verbatim, when it was asked.
    pub reading: Option<Value>,
    pub notes: Vec<String>,
}

pub fn admit(dirs: &Dirs, registry: &Registry, target: &Candidate, role: Role) -> Res<Admission> {
    ledger(dirs, registry, target)?;
    agent_usage(registry, target.harness, role)
}

/// Providers whose numbers only refresh when the harness itself runs. For
/// these a strict freshness guard would refuse forever: nothing but a run
/// would ever make the data fresh again.
const fn refreshes_on_use(harness: HarnessId) -> bool {
    match harness {
        HarnessId::Codex => true,
        HarnessId::Claude => false,
    }
}

fn agent_usage(registry: &Registry, harness: HarnessId, role: Role) -> Res<Admission> {
    let Some(meter) = &registry.meters.agent_usage else {
        return Ok(Admission::default());
    };
    let binary = spawn::resolve_binary("usage-cli", Some(&meter.binary), &[])
        .map_err(|fail| Fail::new(Exit::NoDigest, format!("the usage meter: {}", fail.message)))?;
    let cap = registry
        .harness(harness)
        .cap
        .saturating_sub(role.reserve())
        .max(1);
    let max_age = meter.max_data_age_secs.unwrap_or(DEFAULT_MAX_DATA_AGE_SECS);

    let ask = |cap: u8, guard_age: bool| -> Res<(Exit, Option<Value>)> {
        let mut args = vec![
            "headroom".to_string(),
            "--provider".to_string(),
            harness.as_str().to_string(),
            "--cap".to_string(),
            cap.to_string(),
            "--forecast".to_string(),
            "red".to_string(),
            "--json".to_string(),
        ];
        if guard_age {
            args.extend([
                "--max-data-age".to_string(),
                format!("{}m", max_age.div_ceil(60)),
            ]);
        }
        let output =
            spawn::run_helper(&binary, &args, None, Duration::from_secs(10)).map_err(|fail| {
                Fail::new(Exit::NoDigest, format!("the usage meter: {}", fail.message))
            })?;
        let verdict = match output.status {
            Some(0) => Exit::Ok,
            Some(21) => Exit::Stale,
            Some(24) => Exit::OverCap,
            Some(25) => Exit::Forecast,
            Some(26) => Exit::NoData,
            // 13 (no digest), 19 (a usage-cli too old to know `headroom`),
            // a signal, anything new: not an answer, so not a yes.
            _ => Exit::NoDigest,
        };
        Ok((verdict, serde_json::from_str(output.stdout.trim()).ok()))
    };

    let (mut verdict, mut reading) = ask(cap, true)?;
    let mut notes = Vec::new();
    if verdict == Exit::Stale && refreshes_on_use(harness) {
        let lowered = cap.saturating_sub(STALE_MARGIN).max(1);
        (verdict, reading) = ask(lowered, false)?;
        if verdict == Exit::Ok {
            notes.push(format!(
                "{harness}'s usage data was stale, so it was held to {lowered}% instead of {cap}% — this run refreshes it"
            ));
        }
    }
    if verdict == Exit::Ok {
        return Ok(Admission { reading, notes });
    }
    let percent = reading.as_ref().and_then(|r| r["percent"].as_f64());
    let used = percent.map_or(String::new(), |p| format!(" ({p:.0}% used)"));
    let why = match verdict {
        Exit::Stale => format!(
            "{harness}'s usage data is older than {}m — is the tracker's daemon running?",
            max_age.div_ceil(60)
        ),
        Exit::OverCap => format!("{harness} is over its cap of {cap}%{used}"),
        Exit::Forecast => {
            format!("{harness} is on course to run out before its limit resets{used}")
        }
        Exit::NoData => format!("the tracker has no usage numbers for {harness}"),
        _ => format!(
            "the usage meter gave no answer for {harness} — cahoots needs a usage-cli that has the `headroom` noun, and a running tracker"
        ),
    };
    Err(Fail::new(verdict, why))
}

/// Reviewing spends the REVIEWER's own plan, so it is held to a stricter bar
/// than delegating: twenty points under that harness's cap, on fresh data.
/// With no tracker there is nothing to ask, and the daily review budget is
/// the only limit.
pub fn reviewer_may_spend(registry: &Registry, reviewer: HarnessId) -> Result<(), String> {
    let Some(meter) = &registry.meters.agent_usage else {
        return Ok(());
    };
    let refuse = || {
        Err(format!(
            "not now — reviewing spends {reviewer}'s own plan, and it is too close to its cap for that"
        ))
    };
    let Ok(binary) = spawn::resolve_binary("usage-cli", Some(&meter.binary), &[]) else {
        return refuse();
    };
    let cap = registry.harness(reviewer).cap.saturating_sub(20).max(1);
    let max_age = meter.max_data_age_secs.unwrap_or(DEFAULT_MAX_DATA_AGE_SECS);
    let args = [
        "headroom".to_string(),
        "--provider".to_string(),
        reviewer.as_str().to_string(),
        "--cap".to_string(),
        cap.to_string(),
        "--max-data-age".to_string(),
        format!("{}m", max_age.div_ceil(60)),
        "--forecast".to_string(),
        "red".to_string(),
        "--json".to_string(),
    ];
    match spawn::run_helper(&binary, &args, None, Duration::from_secs(10)) {
        Ok(output) if output.status == Some(0) => Ok(()),
        _ => refuse(),
    }
}

/// What the meter says about a run that is already going.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Watch {
    Under,
    Over {
        percent: Option<f64>,
    },
    /// Stale, missing, or not an answer. A watchdog that acted on this would
    /// kill runs because the tracker hiccuped; it does nothing instead.
    Unknown,
}

/// Asks the tracker whether `harness` has crossed its `abort_at`. `None` when
/// no tracker is configured — the built-in ledger cannot move during a run.
///
/// Unlike admission this never falls back to a stale reading: stopping work
/// in flight needs a FRESH number, so anything else is `Unknown`.
pub fn watch(registry: &Registry, harness: HarnessId) -> Option<Watch> {
    let meter = registry.meters.agent_usage.as_ref()?;
    let Ok(binary) = spawn::resolve_binary("usage-cli", Some(&meter.binary), &[]) else {
        return Some(Watch::Unknown);
    };
    let max_age = meter.max_data_age_secs.unwrap_or(DEFAULT_MAX_DATA_AGE_SECS);
    let args = [
        "headroom".to_string(),
        "--provider".to_string(),
        harness.as_str().to_string(),
        "--cap".to_string(),
        registry.harness(harness).abort_at.to_string(),
        "--max-data-age".to_string(),
        format!("{}m", max_age.div_ceil(60)),
        "--json".to_string(),
    ];
    let Ok(output) = spawn::run_helper(&binary, &args, None, Duration::from_secs(10)) else {
        return Some(Watch::Unknown);
    };
    Some(match output.status {
        Some(0) => Watch::Under,
        Some(24) => Watch::Over {
            percent: serde_json::from_str::<Value>(output.stdout.trim())
                .ok()
                .and_then(|reading| reading["percent"].as_f64()),
        },
        _ => Watch::Unknown,
    })
}

fn ledger(dirs: &Dirs, registry: &Registry, target: &Candidate) -> Res<()> {
    let runs: Vec<_> = record::all(dirs)
        .into_iter()
        .map(|(_, record)| record)
        .filter(|record| record.target.harness == target.harness)
        .collect();
    let since = |secs: u64| now().saturating_sub(secs);

    let last_hour = runs.iter().filter(|r| r.created_at >= since(3600)).count() as u32;
    let ceiling = registry.meters.ledger_max_runs_per_hour;
    if last_hour >= ceiling {
        return Err(Fail::new(
            Exit::OverCap,
            format!(
                "{} has taken {last_hour} runs in the last hour (ledger ceiling: {ceiling})",
                target.harness
            ),
        ));
    }
    if let Some(budget) = registry.meters.ledger_max_tokens_per_day {
        let spent: u64 = runs
            .iter()
            .filter(|r| r.created_at >= since(24 * 3600))
            .map(|r| r.progress.tokens_input + r.progress.tokens_output)
            .sum();
        if spent >= budget {
            return Err(Fail::new(
                Exit::OverCap,
                format!(
                    "{} has used {spent} tokens through cahoots in the last day (ledger budget: {budget})",
                    target.harness
                ),
            ));
        }
    }
    Ok(())
}
