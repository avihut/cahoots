//! Admission: may this target take one more run right now?
//!
//! `used + reserve(role) ≤ cap`, and both meters must agree:
//!
//! - **the usage meter** — a usage CLI the person already has (`meter.rs`):
//!   Agent Usage's `usage-cli headroom`, or ccusage. Its answer maps onto the
//!   tracker's `headroom` exit codes, which are the gate's.
//! - **the ledger** is built in: cahoots' own run records. It works on day
//!   one without any meter, and its runs-per-hour ceiling is what bounds an
//!   agent stuck in a retry loop.
//!
//! Anything the gate does not understand is a refusal. It fails closed.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dirs::Dirs;
use crate::exit::{Exit, Fail, Res};
use crate::meter::Ask;
use crate::model::{Candidate, HarnessId, Role};
use crate::registry::Registry;
use crate::run::record::{self, now};

/// How much lower the cap is when a reading that only refreshes on use is
/// stale. A stale reading is a lower bound on usage, so the margin is what
/// pays for not knowing how much was used since.
pub const STALE_MARGIN: u8 = 15;

/// Why a run was let in — kept on the run record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Admission {
    /// The meter's own answer, verbatim when it has one.
    pub reading: Option<Value>,
    pub notes: Vec<String>,
}

pub fn admit(dirs: &Dirs, registry: &Registry, target: &Candidate, role: Role) -> Res<Admission> {
    ledger(dirs, registry, target)?;
    usage(registry, target.harness, role)
}

fn usage(registry: &Registry, harness: HarnessId, role: Role) -> Res<Admission> {
    let Some(meter) = &registry.meters.usage else {
        return Ok(Admission::default());
    };
    if !meter.measures(harness) {
        return Ok(Admission::default());
    }
    let cap = registry
        .harness(harness)
        .cap
        .saturating_sub(role.reserve())
        .max(1);
    let ask = Ask {
        harness,
        cap,
        fresh: true,
        forecast: true,
    };
    let mut answer = meter.ask(&ask);
    let mut notes = Vec::new();
    if answer.verdict == Exit::Stale && meter.refreshes_on_use(harness) {
        let lowered = cap.saturating_sub(STALE_MARGIN).max(1);
        answer = meter.ask(&Ask {
            cap: lowered,
            fresh: false,
            ..ask
        });
        if answer.verdict == Exit::Ok {
            notes.push(format!(
                "{harness}'s usage data was stale, so it was held to {lowered}% instead of {cap}% — this run refreshes it"
            ));
        }
    }
    if answer.verdict == Exit::Ok {
        return Ok(Admission {
            reading: answer.reading,
            notes,
        });
    }
    Err(Fail::new(answer.verdict, meter.refusal(&ask, &answer)))
}

/// Reviewing spends the REVIEWER's own plan, so it is held to a stricter bar
/// than delegating: twenty points under that harness's cap, on fresh data.
/// With no meter there is nothing to ask, and the daily review budget is the
/// only limit.
pub fn reviewer_may_spend(registry: &Registry, reviewer: HarnessId) -> Result<(), String> {
    let Some(meter) = &registry.meters.usage else {
        return Ok(());
    };
    if !meter.measures(reviewer) {
        return Ok(());
    }
    let ask = Ask {
        harness: reviewer,
        cap: registry.harness(reviewer).cap.saturating_sub(20).max(1),
        fresh: true,
        forecast: true,
    };
    match meter.ask(&ask).verdict {
        Exit::Ok => Ok(()),
        _ => Err(format!(
            "not now — reviewing spends {reviewer}'s own plan, and it is too close to its cap for that"
        )),
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
    /// kill runs because the meter hiccuped; it does nothing instead.
    Unknown,
}

/// Whether a running job on `harness` can be watched at all: only a meter
/// with a percentage that moves during the run can say it crossed
/// `abort_at`. The built-in ledger cannot move during a run.
pub fn watches(registry: &Registry, harness: HarnessId) -> bool {
    registry
        .meters
        .usage
        .as_ref()
        .is_some_and(|meter| meter.watches(harness))
}

/// Asks the meter whether `harness` has crossed its `abort_at`. `None` when
/// nothing can watch it (`watches`).
///
/// Unlike admission this never falls back to a stale reading: stopping work
/// in flight needs a FRESH number, so anything else is `Unknown`.
pub fn watch(registry: &Registry, harness: HarnessId) -> Option<Watch> {
    let meter = registry.meters.usage.as_ref()?;
    if !meter.watches(harness) {
        return None;
    }
    let answer = meter.ask(&Ask {
        harness,
        cap: registry.harness(harness).abort_at,
        fresh: true,
        forecast: false,
    });
    Some(match answer.verdict {
        Exit::Ok => Watch::Under,
        Exit::OverCap => Watch::Over {
            percent: answer.percent,
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
