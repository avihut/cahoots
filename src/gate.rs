//! Admission: may this target take one more run right now?
//!
//! Every configured meter must agree. The built-in **ledger** counts cahoots'
//! own runs, so the gate works on day one without any tracker — and its
//! runs-per-hour ceiling is what bounds an agent stuck in a retry loop.

use crate::dirs::Dirs;
use crate::exit::{Exit, Fail, Res};
use crate::model::{Candidate, Role};
use crate::registry::Registry;
use crate::run::record::{self, now};

pub fn admit(dirs: &Dirs, registry: &Registry, target: &Candidate, _role: Role) -> Res<()> {
    ledger(dirs, registry, target)
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
