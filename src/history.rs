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
//! answer, no file content: ids, enums, counts, the working directory, the
//! commit a run started from, and for a writer the repo-relative paths it
//! touched and a hash and line count of each block it changed — derived from
//! content, never content. A fork writer's line also holds where its
//! repository's git directories are (`base_repo`), and each measurement of
//! how much of its diff survived is a line of its own: a commit id and two
//! counts.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::dirs::{Dirs, ensure_private_dir};
use crate::exit::{Fail, Res};
use crate::model::{Candidate, HarnessId, Role, TaskKindName};
use crate::patch::{Commit, PatchSummary};
use crate::run::record::{RunRecord, State, now};
use crate::survival::{Measure, RepoPin, Why};

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
        #[serde(default)]
        task_kind: Option<TaskKindName>,
        caller: Option<HarnessId>,
        target: Candidate,
        #[serde(default)]
        blind: bool,
        /// Exploration promoted this run's target, fixed when it was chosen.
        #[serde(default)]
        exploration: bool,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_commit: Option<Commit>,
        /// A fork writer's patch, summed up; it outlives the patch itself.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        patch: Option<PatchSummary>,
        /// The run this one continues.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resumed_from: Option<String>,
        /// A fork's repository, pinned at launch (`survival::RepoPin`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_repo: Option<Box<RepoPin>>,
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
    /// How much of a fork writer's diff survived, measured at `t`
    /// (`survival`): against which HEAD, and how many of its blocks were
    /// kept of those counted — or why that is unknown.
    Survival {
        t: u64,
        run: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tip: Option<Commit>,
        kept: u32,
        counted: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unknown: Option<Why>,
    },
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

/// FNV-1a 64. Not for security: the same on every machine and in every
/// version. The review sample and the patch summary's block hashes are both
/// this, and changing it re-selects the one and re-hashes the other.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A selection nobody gets to steer, from the run id alone.
pub fn is_sampled(run_id: &str, rate: f64) -> bool {
    ((fnv1a64(run_id.as_bytes()) % 10_000) as f64) < rate.clamp(0.0, 1.0) * 10_000.0
}

pub fn finished(record: &RunRecord, sample_rate: f64) -> Event {
    let ended = record.finished_at.unwrap_or_else(now);
    Event::Finished {
        t: ended,
        run: record.id.clone(),
        role: record.role,
        task_kind: record.kind.clone(),
        caller: record.caller,
        target: record.target.clone(),
        blind: record.blind,
        exploration: record.exploration,
        dir: record.base.clone().unwrap_or_else(|| record.cwd.clone()),
        state: record.state,
        exit: record.exit_code.unwrap_or(1),
        tokens_in: record.progress.tokens_input,
        tokens_out: record.progress.tokens_output,
        secs: ended.saturating_sub(record.started_at.unwrap_or(record.created_at)),
        sampled: is_sampled(&record.id, sample_rate),
        base_commit: record.base_commit.clone(),
        patch: record.patch.clone(),
        resumed_from: record.resumed_from.clone(),
        base_repo: record.base_repo.clone().map(Box::new),
    }
}

/// One run's story, folded from its events.
#[derive(Debug, Clone, Serialize)]
pub struct Story {
    pub run: String,
    pub t: u64,
    pub role: Role,
    pub kind: Option<TaskKindName>,
    pub caller: Option<HarnessId>,
    pub target: Candidate,
    pub blind: bool,
    pub exploration: bool,
    pub dir: PathBuf,
    pub state: State,
    pub exit: u8,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub secs: u64,
    pub sampled: bool,
    pub base_commit: Option<Commit>,
    pub patch: Option<PatchSummary>,
    pub resumed_from: Option<String>,
    /// Never shown: the pin stays out of what agents read.
    #[serde(skip)]
    pub base_repo: Option<RepoPin>,
    /// `None` means unknown — never inferred from anything else.
    pub outcome: Option<Outcome>,
    /// Every survival measurement, in the order of the file.
    #[serde(skip)]
    pub measures: Vec<(u64, Measure)>,
}

pub fn stories(events: &[Event]) -> Vec<Story> {
    let mut by_run: BTreeMap<&str, Story> = BTreeMap::new();
    for event in events {
        match event {
            Event::Finished {
                t,
                run,
                role,
                task_kind,
                caller,
                target,
                blind,
                exploration,
                dir,
                state,
                exit,
                tokens_in,
                tokens_out,
                secs,
                sampled,
                base_commit,
                patch,
                resumed_from,
                base_repo,
            } => {
                by_run.insert(
                    run,
                    Story {
                        run: run.clone(),
                        t: *t,
                        role: *role,
                        kind: task_kind.clone(),
                        caller: *caller,
                        target: target.clone(),
                        blind: *blind,
                        exploration: *exploration,
                        dir: dir.clone(),
                        state: *state,
                        exit: *exit,
                        tokens_in: *tokens_in,
                        tokens_out: *tokens_out,
                        secs: *secs,
                        sampled: *sampled,
                        base_commit: base_commit.clone(),
                        patch: patch.clone(),
                        resumed_from: resumed_from.clone(),
                        base_repo: base_repo.as_deref().cloned(),
                        outcome: None,
                        measures: Vec::new(),
                    },
                );
            }
            // The last word wins: a caller may change its mind.
            Event::Outcome { run, outcome, .. } => {
                if let Some(story) = by_run.get_mut(run.as_str()) {
                    story.outcome = Some(*outcome);
                }
            }
            // A measurement of a run that is not here is ignored, like an
            // orphan outcome.
            Event::Survival {
                t,
                run,
                tip,
                kept,
                counted,
                unknown,
            } => {
                if let Some(story) = by_run.get_mut(run.as_str()) {
                    story.measures.push((
                        *t,
                        Measure {
                            tip: tip.clone(),
                            kept: *kept,
                            counted: *counted,
                            unknown: *unknown,
                        },
                    ));
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
        // A line from before base commits and patches has neither.
        assert!(stories[0].base_commit.is_none());
        assert!(stories[0].patch.is_none());
    }

    #[test]
    fn the_new_fields_round_trip() {
        let mut event: Event = serde_json::from_str(OLD_FINISHED).unwrap();
        // Absent, they are not written at all: a reader's line grows by nothing.
        let raw = serde_json::to_string(&event).unwrap();
        assert!(
            !raw.contains("base_commit") && !raw.contains("patch"),
            "{raw}"
        );

        let sha = "0123456789abcdef0123456789abcdef01234567";
        let summary = crate::patch::summarize(b"diff --git a/f b/f\n@@ -0,0 +1 @@\n+x\n");
        if let Event::Finished {
            base_commit, patch, ..
        } = &mut event
        {
            *base_commit = Commit::parse(sha);
            *patch = Some(summary.clone());
        }
        let raw = serde_json::to_string(&event).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(json["base_commit"], sha);
        assert_eq!(json["patch"]["files"][0]["path"], "f");
        assert!(json["patch"].get("truncated").is_none(), "{raw}");
        let folded = stories(&[serde_json::from_str(&raw).unwrap()]);
        assert_eq!(folded[0].base_commit.as_ref().unwrap().as_str(), sha);
        assert_eq!(folded[0].patch.as_ref(), Some(&summary));

        // A base commit that is not one does not parse, and the line is skipped.
        let forged = raw.replace(sha, "--exec=x");
        assert!(serde_json::from_str::<Event>(&forged).is_err());
    }

    #[test]
    fn survival_lines_fold_into_their_run_and_orphans_are_ignored() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let lines = [
            format!(
                r#"{{"kind":"survival","t":4,"run":"orphan","tip":"{sha}","kept":1,"counted":1}}"#
            ),
            OLD_FINISHED.to_string(),
            format!(r#"{{"kind":"survival","t":11,"run":"a","tip":"{sha}","kept":3,"counted":4}}"#),
            r#"{"kind":"survival","t":12,"run":"a","kept":0,"counted":0,"unknown":"base_gone"}"#
                .to_string(),
            r#"{"kind":"survival","t":13,"run":"a","kept":0,"counted":0,"unknown":"made_up"}"#
                .to_string(),
        ];
        let events: Vec<Event> = lines
            .iter()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        assert_eq!(events.len(), 4, "an unknown reason is not one");
        let folded = stories(&events);
        assert_eq!(folded.len(), 1);
        let measures = &folded[0].measures;
        assert_eq!(measures.len(), 2);
        assert_eq!(measures[0].0, 11);
        assert_eq!(measures[0].1.tip.as_ref().unwrap().as_str(), sha);
        assert_eq!((measures[0].1.kept, measures[0].1.counted), (3, 4));
        assert_eq!(measures[1].1.unknown, Some(Why::BaseGone));
        // Written back, an unknown one has no tip at all.
        let raw = serde_json::to_string(&events[3]).unwrap();
        assert_eq!(
            raw,
            r#"{"kind":"survival","t":12,"run":"a","kept":0,"counted":0,"unknown":"base_gone"}"#
        );
        let raw = serde_json::to_string(&events[2]).unwrap();
        assert!(!raw.contains("unknown"), "{raw}");
    }

    #[test]
    fn a_finished_line_carries_the_pin_and_the_run_it_continues() {
        let old: Event = serde_json::from_str(OLD_FINISHED).unwrap();
        let folded = stories(std::slice::from_ref(&old));
        assert!(folded[0].resumed_from.is_none() && folded[0].base_repo.is_none());
        let raw = serde_json::to_string(&old).unwrap();
        assert!(
            !raw.contains("resumed_from") && !raw.contains("base_repo"),
            "{raw}"
        );

        let line = OLD_FINISHED.replace(
            r#""sampled":true"#,
            r#""sampled":true,"resumed_from":"z","base_repo":{"tree":"/r/.git/worktrees/w","common":"/r/.git"}"#,
        );
        let event: Event = serde_json::from_str(&line).unwrap();
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["resumed_from"], "z");
        assert_eq!(json["base_repo"]["tree"], "/r/.git/worktrees/w");
        assert_eq!(json["base_repo"]["common"], "/r/.git");
        let folded = stories(&[event]);
        assert_eq!(folded[0].resumed_from.as_deref(), Some("z"));
        assert_eq!(
            folded[0].base_repo.as_ref().unwrap().common,
            PathBuf::from("/r/.git")
        );
        // A story never shows its pin.
        let shown = serde_json::to_value(&folded[0]).unwrap();
        assert!(shown.get("base_repo").is_none(), "{shown}");

        // A pin that is not absolute paths does not parse.
        let forged = line.replace("/r/.git\"}", "--exec=x\"}");
        assert!(serde_json::from_str::<Event>(&forged).is_err());
    }

    const OLD_FINISHED: &str = r#"{"kind":"finished","t":10,"run":"a","role":"review","caller":"claude","target":{"harness":"codex","model":"m","effort":"high"},"dir":"/w","state":"done","exit":0,"tokens_in":9,"tokens_out":1,"secs":3,"sampled":true}"#;

    #[test]
    fn task_kind_does_not_replace_the_history_event_kind() {
        let mut event: Event = serde_json::from_str(OLD_FINISHED).unwrap();
        if let Event::Finished { task_kind, .. } = &mut event {
            *task_kind = Some(TaskKindName::try_from("rust-review".to_string()).unwrap());
        }
        let raw = serde_json::to_string(&event).unwrap();
        assert_eq!(raw.matches("\"kind\":").count(), 1);
        assert_eq!(raw.matches("\"task_kind\":").count(), 1);
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(json["kind"], "finished");
        assert_eq!(json["task_kind"], "rust-review");
        let folded = stories(&[
            serde_json::from_str(&raw).unwrap(),
            Event::Outcome {
                t: 12,
                run: "a".into(),
                outcome: Outcome::Accepted,
            },
        ]);
        assert_eq!(folded[0].kind.as_ref().unwrap().as_str(), "rust-review");
        assert_eq!(folded[0].outcome, Some(Outcome::Accepted));
    }

    #[test]
    fn old_finished_events_have_no_task_kind() {
        let event: Event = serde_json::from_str(OLD_FINISHED).unwrap();
        assert!(stories(std::slice::from_ref(&event))[0].kind.is_none());
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.get("task_kind").unwrap().is_null());
    }
    #[test]
    fn legacy_finished_events_default_to_open() {
        let event: Event = serde_json::from_str(OLD_FINISHED).unwrap();
        assert!(!stories(&[event])[0].blind);
    }

    #[test]
    fn finished_stories_preserve_blind_policy() {
        let mut event: Event = serde_json::from_str(OLD_FINISHED).unwrap();
        if let Event::Finished { blind, .. } = &mut event {
            *blind = true;
        }
        let folded = stories(&[
            event,
            Event::Outcome {
                t: 12,
                run: "a".into(),
                outcome: Outcome::Accepted,
            },
        ]);
        assert!(folded[0].blind);
        assert_eq!(folded[0].outcome, Some(Outcome::Accepted));
        assert_eq!(folded[0].target.model.as_str(), "m");
    }

    #[test]
    fn outcome_append_io_failure_is_internal() {
        let root = tempfile::tempdir().unwrap();
        let dirs = Dirs {
            home: root.path().join("home"),
            config: root.path().join("config"),
            state: root.path().join("state"),
            data: root.path().join("data"),
            overridden: true,
        };
        ensure_private_dir(&dirs.state).unwrap();
        std::fs::create_dir(path(&dirs)).unwrap();
        let error = append(
            &dirs,
            &Event::Outcome {
                t: 12,
                run: "a".into(),
                outcome: Outcome::Accepted,
            },
        )
        .unwrap_err();
        assert_eq!(error.exit, crate::exit::Exit::Internal);
        assert!(read(&dirs).is_empty());
        assert_eq!(std::fs::read_dir(path(&dirs)).unwrap().count(), 0);
    }

    #[test]
    fn old_records_and_finished_events_default_to_nonexploration() {
        let event: Event = serde_json::from_str(OLD_FINISHED).unwrap();
        let Event::Finished { exploration, .. } = &event else {
            panic!("not a finished event")
        };
        assert!(!exploration);
        assert!(!stories(std::slice::from_ref(&event))[0].exploration);
        // Written back, the boolean is explicit — false as well as true.
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["exploration"], false, "{json}");
        // #29's own defaults are still what they were.
        assert!(stories(&[event])[0].base_commit.is_none());
    }

    #[test]
    fn exploration_round_trips_into_finished_stories() {
        for label in [true, false] {
            let mut event: Event = serde_json::from_str(OLD_FINISHED).unwrap();
            let sha = "0123456789abcdef0123456789abcdef01234567";
            let summary = crate::patch::summarize(b"diff --git a/f b/f\n@@ -0,0 +1 @@\n+x\n");
            if let Event::Finished {
                exploration,
                blind,
                task_kind,
                base_commit,
                patch,
                ..
            } = &mut event
            {
                *exploration = label;
                *blind = true;
                *task_kind = Some(TaskKindName::try_from("rust-review".to_string()).unwrap());
                *base_commit = Commit::parse(sha);
                *patch = Some(summary.clone());
            }
            let raw = serde_json::to_string(&event).unwrap();
            let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(json["exploration"], label);
            let events = [
                serde_json::from_str::<Event>(&raw).unwrap(),
                Event::Outcome {
                    t: 12,
                    run: "a".into(),
                    outcome: Outcome::Discarded,
                },
            ];
            let folded = stories(&events);
            // Beside kind, blind, base commit and patch; an outcome never
            // changes the private value.
            assert_eq!(folded[0].exploration, label);
            assert!(folded[0].blind);
            assert_eq!(folded[0].kind.as_ref().unwrap().as_str(), "rust-review");
            assert_eq!(folded[0].base_commit.as_ref().unwrap().as_str(), sha);
            assert_eq!(folded[0].patch.as_ref(), Some(&summary));
            assert_eq!(folded[0].outcome, Some(Outcome::Discarded));
            assert_eq!(stories(&events[..1])[0].exploration, label);
        }
    }
}
