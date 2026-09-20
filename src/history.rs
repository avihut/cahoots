//! `<state>/history.jsonl` — the long memory. A run's CONTENT (its brief, its
//! answer) is kept for days; what it was and how it went is kept here, one
//! small line per event, for as long as the statistics need it.
//!
//! Append-only, several writers (a supervisor finishing, a caller recording
//! an outcome, a reviewer): each event is one short line written with
//! `O_APPEND`, which the kernel keeps whole. A run's story is the fold of its
//! events, so the file is its own rebuildable index — there is no database to
//! fall out of step with it.
//!
//! Everything in it stays on this machine (hard rule 7). It holds no brief, no
//! answer, no file content: ids, enums, counts, and the working directory.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::dirs::{Dirs, ensure_private_dir};
use crate::exit::{Fail, Res};
use crate::model::{Candidate, HarnessId, Role};
use crate::run::record::{RunRecord, State, now};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// The caller used the result as it came.
    Accepted,
    /// The caller used it, after fixing it.
    Reworked,
    /// The caller threw it away.
    Discarded,
}

impl std::str::FromStr for Outcome {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        serde_json::from_value(serde_json::Value::String(s.to_string()))
            .map_err(|_| format!("an outcome is accepted, reworked or discarded — not {s:?}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Finished {
        t: u64,
        run: String,
        role: Role,
        caller: Option<HarnessId>,
        target: Candidate,
        dir: PathBuf,
        state: State,
        exit: u8,
        tokens_in: u64,
        tokens_out: u64,
        secs: u64,
        /// Fixed HERE, when the run ends, from the run's id and the sample
        /// rate of that moment — so changing the rate later never re-selects
        /// history, and nobody chooses which runs get reviewed.
        sampled: bool,
    },
    Outcome {
        t: u64,
        run: String,
        outcome: Outcome,
    },
    /// What the delegating harness found when it reviewed a run. A closed
    /// vocabulary plus a filtered line of detail — see `review.rs`.
    Review {
        t: u64,
        run: String,
        reviewer: HarnessId,
        findings: Vec<crate::review::Finding>,
    },
    /// A person's `learn reset`: reviews before this moment no longer count.
    Forget { t: u64 },
}

fn path(dirs: &Dirs) -> PathBuf {
    dirs.state.join("history.jsonl")
}

pub fn append(dirs: &Dirs, event: &Event) -> Res<()> {
    ensure_private_dir(&dirs.state)?;
    let mut line = serde_json::to_string(event)
        .map_err(|error| Fail::internal(format!("cannot encode a history event: {error}")))?;
    line.push('\n');
    File::options()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path(dirs))
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .map_err(|error| Fail::internal(format!("cannot write the history: {error}")))
}

/// Every event, oldest first. A line that does not parse — a newer version's
/// event, a torn write — is skipped, not fatal.
pub fn read(dirs: &Dirs) -> Vec<Event> {
    std::fs::read_to_string(path(dirs))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// FNV-1a. Not for security: for a selection nobody gets to steer, that is the
/// same on every machine and in every version, from the run id alone.
pub fn is_sampled(run_id: &str, rate: f64) -> bool {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in run_id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    ((hash % 10_000) as f64) < rate.clamp(0.0, 1.0) * 10_000.0
}

pub fn finished(record: &RunRecord, sample_rate: f64) -> Event {
    let ended = record.finished_at.unwrap_or_else(now);
    Event::Finished {
        t: ended,
        run: record.id.clone(),
        role: record.role,
        caller: record.caller,
        target: record.target.clone(),
        dir: record.base.clone().unwrap_or_else(|| record.cwd.clone()),
        state: record.state,
        exit: record.exit_code.unwrap_or(1),
        tokens_in: record.progress.tokens_input,
        tokens_out: record.progress.tokens_output,
        secs: ended.saturating_sub(record.started_at.unwrap_or(record.created_at)),
        sampled: is_sampled(&record.id, sample_rate),
    }
}

/// One run's story, folded from its events.
#[derive(Debug, Clone, Serialize)]
pub struct Story {
    pub run: String,
    pub t: u64,
    pub role: Role,
    pub caller: Option<HarnessId>,
    pub target: Candidate,
    pub dir: PathBuf,
    pub state: State,
    pub exit: u8,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub secs: u64,
    pub sampled: bool,
    /// `None` means unknown — never inferred from anything else.
    pub outcome: Option<Outcome>,
}

pub fn stories(events: &[Event]) -> Vec<Story> {
    let mut by_run: BTreeMap<&str, Story> = BTreeMap::new();
    for event in events {
        match event {
            Event::Finished {
                t,
                run,
                role,
                caller,
                target,
                dir,
                state,
                exit,
                tokens_in,
                tokens_out,
                secs,
                sampled,
            } => {
                by_run.insert(
                    run,
                    Story {
                        run: run.clone(),
                        t: *t,
                        role: *role,
                        caller: *caller,
                        target: target.clone(),
                        dir: dir.clone(),
                        state: *state,
                        exit: *exit,
                        tokens_in: *tokens_in,
                        tokens_out: *tokens_out,
                        secs: *secs,
                        sampled: *sampled,
                        outcome: None,
                    },
                );
            }
            // The last word wins: a caller may change its mind.
            Event::Outcome { run, outcome, .. } => {
                if let Some(story) = by_run.get_mut(run.as_str()) {
                    story.outcome = Some(*outcome);
                }
            }
            Event::Review { .. } | Event::Forget { .. } => {}
        }
    }
    let mut stories: Vec<Story> = by_run.into_values().collect();
    stories.sort_by(|a, b| a.run.cmp(&b.run));
    stories
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_is_a_property_of_the_id_and_tracks_the_rate() {
        let ids: Vec<String> = (0..4000)
            .map(|n| format!("0198c0de-0000-7000-8000-{n:012}"))
            .collect();
        let picked = |rate| ids.iter().filter(|id| is_sampled(id, rate)).count();
        assert_eq!(picked(0.0), 0);
        assert_eq!(picked(1.0), ids.len());
        let fifth = picked(0.2);
        assert!((600..1000).contains(&fifth), "0.2 of 4000 sampled {fifth}");
        // The same id, the same answer — and raising the rate only ever ADDS runs.
        for id in &ids {
            assert_eq!(is_sampled(id, 0.2), is_sampled(id, 0.2));
            assert!(!is_sampled(id, 0.2) || is_sampled(id, 0.5));
        }
    }

    #[test]
    fn an_outcome_is_a_closed_vocabulary() {
        assert_eq!("reworked".parse::<Outcome>().unwrap(), Outcome::Reworked);
        for bad in ["", "ok", "Accepted", "accepted; rm -rf"] {
            assert!(bad.parse::<Outcome>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_story_is_the_fold_of_its_events_and_junk_lines_are_skipped() {
        let text = concat!(
            r#"{"kind":"outcome","t":5,"run":"orphan","outcome":"accepted"}"#,
            "\n",
            r#"{"kind":"finished","t":10,"run":"a","role":"review","caller":"claude","target":{"harness":"codex","model":"m","effort":"high"},"dir":"/w","state":"done","exit":0,"tokens_in":9,"tokens_out":1,"secs":3,"sampled":true}"#,
            "\n",
            "not json at all\n",
            r#"{"kind":"from_the_future","t":11}"#,
            "\n",
            r#"{"kind":"outcome","t":12,"run":"a","outcome":"discarded"}"#,
            "\n",
            r#"{"kind":"outcome","t":13,"run":"a","outcome":"reworked"}"#,
            "\n",
        );
        let events: Vec<Event> = text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        assert_eq!(events.len(), 4);
        let stories = stories(&events);
        assert_eq!(stories.len(), 1, "an outcome without a run is not a run");
        assert_eq!(
            stories[0].outcome,
            Some(Outcome::Reworked),
            "the last word wins"
        );
    }
}
