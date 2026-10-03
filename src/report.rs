//! `cahoots report` — what delegation has actually been doing: per role and
//! target, and per named task kind and candidate, how many runs, how they
//! ended, what became of their results, and what they cost. Read from the
//! folded history, so it reaches past the few days a run's content is kept.
//! It is also what the learning statistics are built on, which is why
//! "unknown" is a column of its own and never folded into anything.
//!
//! Every row carries its evidence: the runs that were rated or failed by
//! themselves (`calibrate::Evidence`, the one definition), the share of each
//! kind of result with its standard error, and the score with its own. They
//! describe; they never decide — nothing here reaches routing, a gate or a cap.
//!
//! It also settles survival (`survival`): a fork writer whose window has
//! passed is measured once more, here, and the measurement filed in the
//! history; each row says how much of its writers' diffs survived.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::json;

use crate::calibrate::Evidence;
use crate::config::UserConfig;
use crate::dirs::Dirs;
use crate::exit::{Envelope, Exit, Res};
use crate::history::{self, Outcome, Story};
use crate::model::{Candidate, Effort, HarnessId, ModelName, Role, TaskKindName};
use crate::paths::Workspace;
use crate::registry::{KindEntry, Registry};
use crate::run::record::{State, now};
use crate::survival::{self, Standing};

/// How many rated or failed runs a row needs before its estimates are put in
/// words. A presentation floor only: it moves no route, and the JSON keeps
/// every estimate that can be computed.
pub const SAMPLE_FLOOR: u32 = 8;

/// One share, as a fraction, with its standard error.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct Rate {
    pub value: Option<f64>,
    pub standard_error: Option<f64>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct Rates {
    pub accepted: Rate,
    pub reworked: Rate,
    pub discarded: Rate,
    pub failed: Rate,
}

/// What a row's rated and failed runs say, and how far to trust it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct EvidenceView {
    pub n: u32,
    pub accepted: u32,
    pub reworked: u32,
    pub discarded: u32,
    pub failed: u32,
    pub score: Option<f64>,
    pub score_standard_error: Option<f64>,
    pub enough_evidence: bool,
    pub rates: Rates,
}

/// The standard error of a share `k` of `n`: `sqrt(p (1 - p) / n)`. A zero at
/// `n = 1` is an estimate, not certainty. Nothing without an observation.
pub fn rate_error(k: u32, n: u32) -> Option<f64> {
    if n == 0 {
        return None;
    }
    let p = f64::from(k) / f64::from(n);
    Some((p * (1.0 - p) / f64::from(n)).sqrt())
}

/// The standard error of the mean score (accepted 1, reworked ½, the rest 0),
/// from the sample variance of those observations — not the Bernoulli one,
/// which a half-credit observation breaks. Needs two observations.
pub fn score_error(accepted: u32, reworked: u32, n: u32) -> Option<f64> {
    if n < 2 {
        return None;
    }
    let n = f64::from(n);
    let score = (f64::from(accepted) + 0.5 * f64::from(reworked)) / n;
    let squares = f64::from(accepted) + 0.25 * f64::from(reworked);
    // Roundoff alone can take the difference below zero.
    Some(((squares - n * score * score).max(0.0) / (n * (n - 1.0))).sqrt())
}

pub fn view(evidence: &Evidence) -> EvidenceView {
    let n = evidence.n;
    let rate = |k: u32| Rate {
        value: (n > 0).then(|| f64::from(k) / f64::from(n)),
        standard_error: rate_error(k, n),
    };
    EvidenceView {
        n,
        accepted: evidence.accepted,
        reworked: evidence.reworked,
        discarded: evidence.discarded,
        failed: evidence.failed,
        score: evidence.score,
        score_standard_error: score_error(evidence.accepted, evidence.reworked, n),
        enough_evidence: n >= SAMPLE_FLOOR,
        rates: Rates {
            accepted: rate(evidence.accepted),
            reworked: rate(evidence.reworked),
            discarded: rate(evidence.discarded),
            failed: rate(evidence.failed),
        },
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Row {
    pub runs: u32,
    pub done: u32,
    pub failed: u32,
    pub timed_out: u32,
    pub cancelled: u32,
    pub stopped_by_budget: u32,
    pub accepted: u32,
    pub reworked: u32,
    pub discarded: u32,
    /// Finished fine, and nobody said what became of the result.
    pub outcome_unknown: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub median_secs: u64,
    pub evidence: EvidenceView,
    /// How much of the row's fork writers' diffs survived; absent when it
    /// has no writer that can be measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub survival: Option<survival::Tally>,
}

/// A row being added to: its counters, the durations its median comes from,
/// and the evidence. One definition for every grouping.
#[derive(Default)]
struct Tally {
    row: Row,
    secs: Vec<u64>,
    evidence: Evidence,
    /// The survival standings of the row's measured runs.
    standings: Vec<Standing>,
}

impl Tally {
    fn add(&mut self, story: &Story, standing: Option<&Standing>) {
        let row = &mut self.row;
        row.runs += 1;
        match story.state {
            State::Done => row.done += 1,
            State::Failed | State::Crashed => row.failed += 1,
            State::TimedOut => row.timed_out += 1,
            State::Cancelled => row.cancelled += 1,
            State::Budget => row.stopped_by_budget += 1,
            State::Starting | State::Running => {}
        }
        match story.outcome {
            Some(Outcome::Accepted) => row.accepted += 1,
            Some(Outcome::Reworked) => row.reworked += 1,
            Some(Outcome::Discarded) => row.discarded += 1,
            None if story.state == State::Done => row.outcome_unknown += 1,
            None => {}
        }
        row.tokens_in += story.tokens_in;
        row.tokens_out += story.tokens_out;
        self.secs.push(story.secs);
        self.evidence.observe(story);
        self.standings.extend(standing.cloned());
    }

    fn finish(mut self) -> Row {
        self.secs.sort_unstable();
        self.row.median_secs = self.secs.get(self.secs.len() / 2).copied().unwrap_or(0);
        self.evidence.finish();
        self.row.evidence = view(&self.evidence);
        self.row.survival = (!self.standings.is_empty()).then(|| survival::tally(&self.standings));
        self.row
    }
}

/// The role rows, from the stories and the survival standings of the runs
/// that are measured (`survival::standings`). Pure: it runs no git.
pub fn rows(
    stories: &[Story],
    standings: &BTreeMap<&str, Standing>,
) -> BTreeMap<String, (Candidate, Row)> {
    role_rows(stories.iter(), standings)
}

fn role_rows<'a>(
    stories: impl Iterator<Item = &'a Story>,
    standings: &BTreeMap<&str, Standing>,
) -> BTreeMap<String, (Candidate, Row)> {
    let mut grouped: BTreeMap<String, (Candidate, Tally)> = BTreeMap::new();
    for story in stories {
        let key = format!(
            "{} · {} · {} · {}",
            story.role,
            story.target.harness,
            story.target.model.as_str(),
            story.target.effort
        );
        grouped
            .entry(key)
            .or_insert_with(|| (story.target.clone(), Tally::default()))
            .1
            .add(story, standings.get(story.run.as_str()));
    }
    grouped
        .into_iter()
        .map(|(key, (target, tally))| (key, (target, tally.finish())))
        .collect()
}

/// A named kind's candidate under the role the kind had when the run was
/// made: a kind redefined with another role must not pool evidence from
/// different fences, and two efforts are two candidates.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct KindGroup {
    kind: TaskKindName,
    role: Role,
    harness: HarnessId,
    model: ModelName,
    effort: Effort,
}

impl KindGroup {
    fn new(kind: &TaskKindName, role: Role, target: &Candidate) -> KindGroup {
        KindGroup {
            kind: kind.clone(),
            role,
            harness: target.harness,
            model: target.model.clone(),
            effort: target.effort,
        }
    }

    fn key(&self) -> String {
        format!(
            "{} · {} · {} · {} · {}",
            self.kind,
            self.role,
            self.harness,
            self.model.as_str(),
            self.effort
        )
    }
}

/// Everything the report counts, for the runs that finished at or after `since`.
pub struct Summary {
    /// Unique runs in the window — the one count, whatever the groupings.
    pub runs: usize,
    pub by_role_and_target: BTreeMap<String, Row>,
    pub by_kind_and_target: BTreeMap<String, Row>,
}

/// The role totals and the kind groups. Every candidate of every kind in
/// `kinds` has a row, runs or none; a kind or candidate no longer configured
/// keeps the row its runs in the window earned. Runs with no kind are in the
/// role totals only. A row's survival is from `standings`.
pub fn summarize(
    stories: &[Story],
    kinds: &BTreeMap<TaskKindName, KindEntry>,
    standings: &BTreeMap<&str, Standing>,
    since: u64,
) -> Summary {
    let window: Vec<&Story> = stories.iter().filter(|story| story.t >= since).collect();
    let mut groups: BTreeMap<KindGroup, Tally> = BTreeMap::new();
    for (name, entry) in kinds {
        for candidate in &entry.candidates {
            groups
                .entry(KindGroup::new(name, entry.role, candidate))
                .or_default();
        }
    }
    for story in &window {
        if let Some(kind) = &story.kind {
            groups
                .entry(KindGroup::new(kind, story.role, &story.target))
                .or_default()
                .add(story, standings.get(story.run.as_str()));
        }
    }
    Summary {
        runs: window.len(),
        by_role_and_target: role_rows(window.iter().copied(), standings)
            .into_iter()
            .map(|(key, (_, row))| (key, row))
            .collect(),
        by_kind_and_target: groups
            .into_iter()
            .map(|(group, tally)| (group.key(), tally.finish()))
            .collect(),
    }
}

/// `report --suggest`: what the outcome statistics say about each role's
/// candidate order — role-only evidence per candidate (kind runs still count
/// in ordinary totals), the one swap it supports (if
/// any), and whether that swap is in effect or only being SHOWN (shadow mode,
/// the default). Computed against the order WITHOUT learning, so it reads as
/// "default → suggestion".
fn routing(dirs: &Dirs, unlearned: &Registry, stories: &[Story]) -> Res<serde_json::Value> {
    use crate::calibrate::{MIN_GAP, MIN_SAMPLE, WINDOW_DAYS, evidence};

    let learned = unlearned.learned(dirs);
    let since = now().saturating_sub(WINDOW_DAYS * 24 * 3600);
    let applying = unlearned.review.enabled && unlearned.review.apply_routing;

    let roles: serde_json::Map<String, serde_json::Value> = unlearned
        .roles
        .iter()
        .map(|(role, entry)| {
            let candidates: Vec<serde_json::Value> = entry
                .candidates
                .iter()
                .map(|c| json!({ "candidate": c, "evidence": view(&evidence(stories, *role, c, since)) }))
                .collect();
            let swap = learned.swaps.get(role).map(|at| {
                json!({
                    "move_up": entry.candidates[at + 1],
                    "past": entry.candidates[*at],
                    "in_effect": applying,
                })
            });
            let why_not = match (&swap, entry.calibrate) {
                (Some(_), _) => None,
                (None, false) => Some("this order was written by a person and is left alone (roles.<role>.calibrate = true to allow it)"),
                (None, true) => Some("the evidence does not support a change"),
            };
            (
                role.to_string(),
                json!({ "order": candidates, "suggested_swap": swap, "no_swap_because": why_not }),
            )
        })
        .collect();
    Ok(json!({
        "mode": if applying { "applying" } else { "shadow — shown, not used (review.apply_routing = true to use it)" },
        "rule": format!(
            "a candidate moves up ONE place past its neighbour when both have at least {MIN_SAMPLE} rated or failed runs in {WINDOW_DAYS} days and it scored at least {MIN_GAP} better (accepted = 1, reworked = ½, discarded or failed = 0)"
        ),
        "roles": roles,
    }))
}

pub fn report(days: u64, suggest: bool) -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    let config = UserConfig::load(&dirs.config_file())?;
    let registry = Registry::effective(&config);
    let now = now();
    let since = now.saturating_sub(days * 24 * 3600);
    let mut events = history::read(&dirs);
    // Over the whole history: a resumed run outside the window still
    // stands for the run it continues.
    let superseded = survival::superseded(&history::stories(&events));
    let shown: Vec<Story> = history::stories(&events)
        .into_iter()
        .filter(|story| story.t >= since)
        .collect();
    let caught = settle(&dirs, &shown, &superseded, now);
    let measured = caught.events.len();
    events.extend(caught.events);
    let stories = history::stories(&events);
    let standings = survival::standings(&stories, &superseded, now);
    let summary = summarize(&stories, &registry.kinds, &standings, since);
    let mut data = json!({
        "days": days,
        "runs": summary.runs,
        "sample_floor": SAMPLE_FLOOR,
        "by_role_and_target": summary.by_role_and_target,
        "by_kind_and_target": summary.by_kind_and_target,
        "survival": {
            "window_days": survival::WINDOW_DAYS,
            "measured": measured,
            "pending": caught.pending,
        },
    });
    if suggest {
        data["routing"] = routing(&dirs, &registry, &stories)?;
    }
    Ok(Envelope::new(Exit::Ok, None).with_data(data))
}

/// Measures the shown runs whose window has passed. Never a refusal: inside
/// the Codex sandbox, or where the asking workspace cannot be read, nothing
/// is measured and every due run stays pending.
fn settle(
    dirs: &Dirs,
    stories: &[Story],
    superseded: &std::collections::BTreeSet<String>,
    now: u64,
) -> survival::CatchUp {
    let untouched = || survival::CatchUp {
        events: Vec::new(),
        pending: survival::due(stories, superseded, now).len(),
    };
    if crate::env::in_codex_sandbox() {
        return untouched();
    }
    let Ok(workspace) = std::env::current_dir().and_then(|cwd| {
        Workspace::around(&cwd).map_err(|fail| std::io::Error::other(fail.message))
    }) else {
        return untouched();
    };
    survival::catch_up(dirs, stories, superseded, now, &workspace.roots())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn candidate(model: &str, effort: Effort) -> Candidate {
        Candidate {
            harness: HarnessId::Codex,
            model: ModelName::try_from(model.to_string()).unwrap(),
            effort,
        }
    }

    fn kind(name: &str) -> TaskKindName {
        TaskKindName::try_from(name.to_string()).unwrap()
    }

    fn story(
        run: &str,
        t: u64,
        kind_name: Option<&str>,
        role: Role,
        target: &Candidate,
        state: State,
        outcome: Option<Outcome>,
    ) -> Story {
        Story {
            run: run.to_string(),
            t,
            role,
            kind: kind_name.map(kind),
            caller: None,
            target: target.clone(),
            blind: false,
            exploration: false,
            dir: PathBuf::from("/w"),
            state,
            exit: 0,
            tokens_in: 1,
            tokens_out: 1,
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

    fn entry(role: Role, candidates: Vec<Candidate>) -> KindEntry {
        KindEntry {
            description: "d".to_string(),
            role,
            candidates,
            explore: crate::registry::KindExplore { share: None },
        }
    }

    fn close(left: Option<f64>, right: f64) {
        let left = left.expect("a number");
        assert!((left - right).abs() < 1e-12, "{left} != {right}");
    }

    #[test]
    fn a_share_s_error_is_defined_from_one_observation_and_never_non_finite() {
        assert_eq!(rate_error(0, 0), None);
        // One observation: the formula applies, and its zero is an estimate.
        assert_eq!(rate_error(1, 1), Some(0.0));
        assert_eq!(rate_error(0, 1), Some(0.0));
        close(rate_error(1, 4), (0.25f64 * 0.75 / 4.0).sqrt());
        close(rate_error(5, 10), 0.5 / 10f64.sqrt());
    }

    #[test]
    fn the_score_s_error_is_the_sample_variance_one_not_the_bernoulli_one() {
        assert_eq!(score_error(0, 0, 0), None);
        assert_eq!(score_error(1, 0, 1), None);
        // All alike: no spread.
        assert_eq!(score_error(5, 0, 5), Some(0.0));
        assert_eq!(score_error(0, 0, 5), Some(0.0));
        // Two half-credits: the score is 0.5 with no spread at all, where
        // the Bernoulli formula would say sqrt(0.25 / 2).
        assert_eq!(score_error(0, 2, 2), Some(0.0));
        // One accepted, one discarded: observations 1 and 0, variance ½.
        close(score_error(1, 0, 2), 0.5);
        // 2 accepted, 1 reworked, 1 discarded: 1, 1, ½, 0 — mean 0.625,
        // squares 2.25, variance (2.25 - 4 * 0.625²) / 3.
        close(
            score_error(2, 1, 4),
            ((2.25f64 - 4.0 * 0.625 * 0.625) / 12.0).sqrt(),
        );
    }

    #[test]
    fn the_floor_is_eight_and_estimates_are_kept_below_it() {
        let mut evidence = Evidence {
            accepted: 7,
            ..Evidence::default()
        };
        evidence.finish();
        let seven = view(&evidence);
        assert!(!seven.enough_evidence);
        assert_eq!(seven.rates.accepted.value, Some(1.0));
        evidence.accepted = 8;
        evidence.finish();
        assert!(view(&evidence).enough_evidence);
        assert_eq!(view(&Evidence::default()), EvidenceView::default());
    }

    #[test]
    fn a_story_counts_once_in_evidence_whatever_it_ended_as() {
        let target = candidate("m", Effort::High);
        let stories = [
            story(
                "a",
                10,
                Some("k"),
                Role::Review,
                &target,
                State::Failed,
                Some(Outcome::Accepted),
            ),
            story(
                "b",
                10,
                Some("k"),
                Role::Review,
                &target,
                State::Crashed,
                None,
            ),
            story(
                "c",
                10,
                Some("k"),
                Role::Review,
                &target,
                State::TimedOut,
                None,
            ),
            story("d", 10, Some("k"), Role::Review, &target, State::Done, None),
            story(
                "e",
                10,
                Some("k"),
                Role::Review,
                &target,
                State::Cancelled,
                None,
            ),
            story(
                "f",
                10,
                Some("k"),
                Role::Review,
                &target,
                State::Budget,
                None,
            ),
            story(
                "g",
                10,
                Some("k"),
                Role::Review,
                &target,
                State::Running,
                None,
            ),
        ];
        let summary = summarize(&stories, &BTreeMap::new(), &BTreeMap::new(), 0);
        let row = &summary.by_kind_and_target["k · review · codex · m · high"];
        // Counters keep their own definitions…
        assert_eq!(
            (row.runs, row.done, row.failed, row.timed_out, row.cancelled),
            (7, 1, 2, 1, 1)
        );
        assert_eq!((row.accepted, row.outcome_unknown), (1, 1));
        // …and the evidence is the rated runs plus the unrated failures.
        let e = &row.evidence;
        assert_eq!((e.n, e.accepted, e.failed), (3, 1, 2));
        assert_eq!(summary.runs, 7);
    }

    #[test]
    fn the_window_is_the_finish_time_and_includes_the_cutoff() {
        let target = candidate("m", Effort::High);
        let stories = [
            story(
                "a",
                99,
                Some("k"),
                Role::Review,
                &target,
                State::Done,
                Some(Outcome::Accepted),
            ),
            story(
                "b",
                100,
                Some("k"),
                Role::Review,
                &target,
                State::Done,
                None,
            ),
        ];
        let summary = summarize(&stories, &BTreeMap::new(), &BTreeMap::new(), 100);
        assert_eq!(summary.runs, 1);
        assert_eq!(summary.by_kind_and_target.values().next().unwrap().runs, 1);
        assert_eq!(
            summarize(&stories, &BTreeMap::new(), &BTreeMap::new(), 101).runs,
            0
        );
        assert!(
            summarize(&stories, &BTreeMap::new(), &BTreeMap::new(), 101)
                .by_kind_and_target
                .is_empty()
        );
    }

    #[test]
    fn every_configured_candidate_has_a_row_and_history_fills_or_adds_to_them() {
        let (medium, high) = (candidate("m", Effort::Medium), candidate("m", Effort::High));
        let kinds = BTreeMap::from([(
            kind("k"),
            entry(Role::Review, vec![medium.clone(), high.clone()]),
        )]);
        let old_role = story("a", 10, Some("k"), Role::Advise, &medium, State::Done, None);
        let removed = story(
            "b",
            10,
            Some("gone"),
            Role::Review,
            &high,
            State::Done,
            None,
        );
        let plain = story("c", 10, None, Role::Review, &high, State::Done, None);
        let summary = summarize(&[old_role, removed, plain], &kinds, &BTreeMap::new(), 0);
        let keys: Vec<&str> = summary
            .by_kind_and_target
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "gone · review · codex · m · high",
                "k · advise · codex · m · medium",
                "k · review · codex · m · high",
                "k · review · codex · m · medium",
            ]
        );
        let seeded = &summary.by_kind_and_target["k · review · codex · m · high"];
        assert_eq!((seeded.runs, seeded.median_secs), (0, 0));
        assert_eq!(seeded.evidence, EvidenceView::default());
        assert_eq!(
            summary.by_kind_and_target["k · advise · codex · m · medium"].runs,
            1
        );
        // An unlabelled run is in the role totals only, and counts once.
        assert_eq!(summary.runs, 3);
        assert_eq!(summary.by_role_and_target.len(), 2);
        assert_eq!(
            summary.by_role_and_target["review · codex · m · high"].runs,
            2
        );
    }

    #[test]
    fn a_row_shows_survival_only_for_the_runs_measured() {
        let target = candidate("m", Effort::High);
        let stories = vec![
            story(
                "w",
                5,
                Some("rust-fix"),
                Role::Implement,
                &target,
                State::Done,
                None,
            ),
            story("r", 5, None, Role::Advise, &target, State::Done, None),
        ];
        let plain = rows(&stories, &BTreeMap::new());
        for (_, row) in plain.values() {
            let json = serde_json::to_value(row).unwrap();
            assert!(json.get("survival").is_none(), "{json}");
        }
        let standings = BTreeMap::from([("w", Standing::default())]);
        let with = rows(&stories, &standings);
        let (_, writer) = &with["implement · codex · m · high"];
        assert_eq!(
            serde_json::to_value(writer).unwrap()["survival"],
            json!({"unknown": 0, "unmeasured": 1})
        );
        let (_, reader) = &with["advise · codex · m · high"];
        assert!(reader.survival.is_none());
        // A kind's row inherits it.
        let summary = summarize(&stories, &BTreeMap::new(), &standings, 0);
        let kind_row = &summary.by_kind_and_target["rust-fix · implement · codex · m · high"];
        assert_eq!(kind_row.survival.as_ref().unwrap().unmeasured, 1);
    }
}
