//! `cahoots report` — what delegation has actually been doing: per role and
//! target, how many runs, how they ended, what became of their results, and
//! what they cost. Read from the history, so it reaches past the few days a
//! run's content is kept. It is also what the learning statistics are built
//! on, which is why "unknown" is a column of its own and never folded into
//! anything.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::json;

use crate::dirs::Dirs;
use crate::exit::{Envelope, Exit, Res};
use crate::history::{self, Outcome, Story};
use crate::model::Candidate;
use crate::run::record::{State, now};

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
}

pub fn rows(stories: &[Story]) -> BTreeMap<String, (Candidate, Row)> {
    let mut grouped: BTreeMap<String, (Candidate, Row, Vec<u64>)> = BTreeMap::new();
    for story in stories {
        let key = format!(
            "{} · {} · {} · {}",
            story.role,
            story.target.harness,
            story.target.model.as_str(),
            story.target.effort
        );
        let (_, row, secs) = grouped
            .entry(key)
            .or_insert_with(|| (story.target.clone(), Row::default(), Vec::new()));
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
        secs.push(story.secs);
    }
    grouped
        .into_iter()
        .map(|(key, (target, mut row, mut secs))| {
            secs.sort_unstable();
            row.median_secs = secs.get(secs.len() / 2).copied().unwrap_or(0);
            (key, (target, row))
        })
        .collect()
}

pub fn report(days: u64) -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    let since = now().saturating_sub(days * 24 * 3600);
    let stories: Vec<Story> = history::stories(&history::read(&dirs))
        .into_iter()
        .filter(|story| story.t >= since)
        .collect();
    let rows: BTreeMap<String, Row> = rows(&stories)
        .into_iter()
        .map(|(key, (_, row))| (key, row))
        .collect();
    Ok(Envelope::new(Exit::Ok, None).with_data(json!({
        "days": days,
        "runs": stories.len(),
        "by_role_and_target": rows,
    })))
}
