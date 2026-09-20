//! Routing calibration: which candidate a role tries first, tuned to what has
//! actually worked on THIS machine.
//!
//! It moves on statistics, never on opinions — a reviewer's findings feed the
//! notes, and nothing else. The evidence is what callers did with results
//! (`outcome`) and whether runs failed; the adjustment is derived from the
//! history every time it is needed, as a pure function. So there is no
//! learned state to go stale, to tamper with, or to fall out of step with the
//! config: change the candidate list and the adjustment is simply recomputed
//! against the new one; let the evidence age out and it decays to the default
//! by itself.
//!
//! And it is bounded at compile time. [`LearnedAdjustments`] can say "in this
//! role, swap these two neighbours" and NOTHING else — there is no field in
//! it for a cap, a reserve, a model, an effort, a flag or a command (hard
//! rule 6). One swap per role, one position from the default, ever.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::history::{Outcome, Story};
use crate::model::{Candidate, Role};
use crate::run::record::State;

/// Evidence a candidate needs before it can be compared at all.
pub const MIN_SAMPLE: u32 = 8;
/// How much better the lower neighbour must have done to be tried first.
pub const MIN_GAP: f64 = 0.15;
/// Evidence older than this no longer counts: the adjustment decays.
pub const WINDOW_DAYS: u64 = 90;

/// Everything learning may change. It is this small on purpose.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LearnedAdjustments {
    /// Per role: the index `i` whose candidate trades places with `i + 1`.
    pub swaps: BTreeMap<Role, usize>,
}

impl LearnedAdjustments {
    /// Applies the one swap a role may have. Out of range — the list changed
    /// under it — is simply nothing to do.
    pub fn apply(&self, role: Role, candidates: &mut [Candidate]) -> bool {
        match self.swaps.get(&role) {
            Some(&at) if at + 1 < candidates.len() => {
                candidates.swap(at, at + 1);
                true
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Evidence {
    /// Runs that count: the caller said what became of the result, or the
    /// run failed by itself. Unknown outcomes are NOT evidence.
    pub n: u32,
    pub accepted: u32,
    pub reworked: u32,
    pub discarded: u32,
    pub failed: u32,
    /// accepted = 1, reworked = ½, discarded or failed = 0.
    pub score: Option<f64>,
}

pub fn evidence(stories: &[Story], role: Role, candidate: &Candidate, since: u64) -> Evidence {
    let mut e = Evidence::default();
    for story in stories {
        if story.role != role || story.target != *candidate || story.t < since {
            continue;
        }
        match (story.outcome, story.state) {
            (Some(Outcome::Accepted), _) => e.accepted += 1,
            (Some(Outcome::Reworked), _) => e.reworked += 1,
            (Some(Outcome::Discarded), _) => e.discarded += 1,
            // A run that broke by itself counts against the candidate. One
            // that was cancelled or stopped for budget was not its fault.
            (None, State::Failed | State::Crashed | State::TimedOut) => e.failed += 1,
            (None, _) => {}
        }
    }
    e.n = e.accepted + e.reworked + e.discarded + e.failed;
    if e.n > 0 {
        e.score = Some((f64::from(e.accepted) + 0.5 * f64::from(e.reworked)) / f64::from(e.n));
    }
    e
}

/// The one swap the evidence supports for a role, if any: the adjacent pair
/// where the LOWER candidate has done better by the widest margin — both of
/// them with enough evidence to say so.
pub fn suggest(
    stories: &[Story],
    role: Role,
    candidates: &[Candidate],
    since: u64,
) -> Option<usize> {
    let scored: Vec<Evidence> = candidates
        .iter()
        .map(|candidate| evidence(stories, role, candidate, since))
        .collect();
    (0..candidates.len().saturating_sub(1))
        .filter_map(|at| {
            let (upper, lower) = (&scored[at], &scored[at + 1]);
            if upper.n < MIN_SAMPLE || lower.n < MIN_SAMPLE {
                return None;
            }
            let gap = lower.score? - upper.score?;
            (gap >= MIN_GAP).then_some((at, gap))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(at, _)| at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Effort, HarnessId, ModelName};
    use std::path::PathBuf;

    fn candidate(harness: HarnessId, model: &str) -> Candidate {
        Candidate {
            harness,
            model: ModelName::try_from(model.to_string()).unwrap(),
            effort: Effort::High,
        }
    }

    fn stories(target: &Candidate, outcomes: &[(Option<Outcome>, State)]) -> Vec<Story> {
        outcomes
            .iter()
            .enumerate()
            .map(|(n, (outcome, state))| Story {
                run: format!("{}-{n}", target.model.as_str()),
                t: 1000,
                role: Role::Review,
                caller: None,
                target: target.clone(),
                dir: PathBuf::from("/w"),
                state: *state,
                exit: 0,
                tokens_in: 0,
                tokens_out: 0,
                secs: 1,
                sampled: false,
                outcome: *outcome,
            })
            .collect()
    }

    fn all(outcome: Outcome, n: usize) -> Vec<(Option<Outcome>, State)> {
        vec![(Some(outcome), State::Done); n]
    }

    #[test]
    fn the_lower_candidate_moves_up_only_on_enough_evidence_and_a_real_gap() {
        let (first, second) = (
            candidate(HarnessId::Codex, "a"),
            candidate(HarnessId::Claude, "b"),
        );
        let list = [first.clone(), second.clone()];
        let with = |a: Vec<_>, b: Vec<_>| {
            let mut s = stories(&first, &a);
            s.extend(stories(&second, &b));
            suggest(&s, Role::Review, &list, 0)
        };
        // Clearly better, with enough evidence on both sides.
        assert_eq!(
            with(all(Outcome::Discarded, 8), all(Outcome::Accepted, 8)),
            Some(0)
        );
        // Seven is not eight — on either side.
        assert_eq!(
            with(all(Outcome::Discarded, 8), all(Outcome::Accepted, 7)),
            None
        );
        assert_eq!(
            with(all(Outcome::Discarded, 7), all(Outcome::Accepted, 8)),
            None
        );
        // Better, but not by enough: 0.5 against 0.4.
        let mut upper = all(Outcome::Accepted, 4);
        upper.extend(all(Outcome::Discarded, 6));
        let mut lower = all(Outcome::Accepted, 5);
        lower.extend(all(Outcome::Discarded, 5));
        assert_eq!(with(upper, lower), None);
        // The default order is never "corrected" towards the one already first.
        assert_eq!(
            with(all(Outcome::Accepted, 8), all(Outcome::Discarded, 8)),
            None
        );
    }

    #[test]
    fn what_was_not_the_candidates_fault_is_not_evidence() {
        let c = candidate(HarnessId::Codex, "a");
        let s = stories(
            &c,
            &[
                (None, State::Done),      // nobody said: unknown, not accepted
                (None, State::Cancelled), // the caller stopped it
                (None, State::Budget),    // the watchdog stopped it
                (None, State::Failed),    // this one is on the candidate
                (Some(Outcome::Reworked), State::Done),
            ],
        );
        let e = evidence(&s, Role::Review, &c, 0);
        assert_eq!((e.n, e.failed, e.reworked), (2, 1, 1));
        assert_eq!(e.score, Some(0.25));
        // Old evidence ages out: that is the decay.
        assert_eq!(evidence(&s, Role::Review, &c, 2000).n, 0);
    }

    /// Hard rule 6, as a test: whatever the history says, applying what was
    /// learned only ever reorders — one adjacent swap — and changes nothing else.
    #[test]
    fn applying_what_was_learned_only_ever_reorders() {
        let original = vec![
            candidate(HarnessId::Codex, "a"),
            candidate(HarnessId::Claude, "b"),
            candidate(HarnessId::Codex, "c"),
        ];
        for at in 0..6 {
            let learned = LearnedAdjustments {
                swaps: BTreeMap::from([(Role::Review, at)]),
            };
            let mut list = original.clone();
            let applied = learned.apply(Role::Review, &mut list);
            assert_eq!(applied, at < 2, "swap {at}");
            let mut sorted = list.clone();
            sorted.sort_by(|x, y| x.model.cmp(&y.model));
            assert_eq!(
                sorted, original,
                "a candidate was added, dropped or altered"
            );
            let moved = list.iter().zip(&original).filter(|(x, y)| x != y).count();
            assert!(moved == 0 || moved == 2, "more than one swap");
            // Another role is untouched.
            let mut other = original.clone();
            assert!(!learned.apply(Role::Advise, &mut other));
        }
    }
}
