//! How much of a fork writer's diff survived: what share of the blocks in
//! its patch are still part of the repository's change since the commit the
//! run started from — the net change `git diff <base> <HEAD>`, so a block a
//! later commit rewrote falls out of it again. An implicit grade, at no cost.
//!
//! Measured when `outcome` is recorded, and once more by `report` after
//! `WINDOW_DAYS`, so "kept, then rewritten" shows as a second, lower number.
//! Each measurement is a line in the history: a commit id and two counts,
//! never content.
//!
//! Logic only (hard rule 11): it returns data, and the words are
//! `cli::endings`'. Data only (hard rule 6): nothing in routing reads it.
//!
//! The repository is the one the run started in, pinned at launch
//! (`RepoPin`): every `git` here is given that git directory, re-checked, and
//! never follows the tree's `.git`. It reads commits and nothing else — no
//! worktree, no index — so no clean filter, fsmonitor or submodule runs.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::dirs::Dirs;
use crate::exit::{Exit, Res};
use crate::history::{self, Event, Story};
use crate::patch::{self, Commit, PatchSummary};
use crate::placement;
use crate::run::record::State;
use crate::spawn;
use crate::tools::Tool;

/// How long after a run finishes its survival is settled.
pub const WINDOW_DAYS: u64 = 14;
/// The fewest lines a writer's block must have to count: a `+}` matches
/// everywhere.
pub const MIN_BLOCK_LINES: u32 = 2;
/// How long one measurement may take, all of its steps together.
pub const DEADLINE: Duration = Duration::from_secs(8);
/// The most a net diff may weigh. Past it, the measurement is `too_large`.
pub const NET_CAP: usize = 32 * 1024 * 1024;
/// How long `report` spends measuring in one call.
pub const REPORT_BUDGET: Duration = Duration::from_secs(30);

const WINDOW_SECS: u64 = WINDOW_DAYS * 24 * 3600;

/// Why a measurement says nothing. A closed vocabulary, never free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// The pinned repository's common directory is gone.
    RepositoryGone,
    /// The pinned git directory is there, but no longer the repository's.
    PinChanged,
    /// The base commit is not in the repository.
    BaseGone,
    /// The net diff took longer than `DEADLINE` or was past `NET_CAP`.
    TooLarge,
    /// The binary policy refused `git`.
    GitRefused,
    /// Anything else: no HEAD, a timeout, git failing.
    GitFailed,
}

/// One measurement of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measure {
    /// The HEAD measured against.
    pub tip: Option<Commit>,
    pub kept: u32,
    pub counted: u32,
    pub unknown: Option<Why>,
}

impl Measure {
    fn unknown(why: Why) -> Measure {
        Measure {
            tip: None,
            kept: 0,
            counted: 0,
            unknown: Some(why),
        }
    }

    /// `kept / counted`, or `None` when unknown or when nothing counted.
    pub fn share(&self) -> Option<f64> {
        (self.unknown.is_none() && self.counted > 0)
            .then(|| f64::from(self.kept) / f64::from(self.counted))
    }

    /// This measurement as a history line about `run`, taken at `t`.
    pub fn event(&self, run: &str, t: u64) -> Event {
        Event::Survival {
            t,
            run: run.to_string(),
            tip: self.tip.clone(),
            kept: self.kept,
            counted: self.counted,
            unknown: self.unknown,
        }
    }
}

/// The repository a run started in, pinned at launch before any writer
/// existed: the git directory of the run's tree, and the repository's common
/// directory. Both absolute, with no NUL — so a hand-edited record or
/// history line cannot put anything else into a git argv — and canonical
/// when recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPin")]
pub struct RepoPin {
    pub tree: PathBuf,
    pub common: PathBuf,
}

#[derive(Deserialize)]
struct RawPin {
    tree: PathBuf,
    common: PathBuf,
}

impl TryFrom<RawPin> for RepoPin {
    type Error = String;
    fn try_from(raw: RawPin) -> Result<RepoPin, String> {
        for path in [&raw.tree, &raw.common] {
            if !path.is_absolute() || path.as_os_str().as_bytes().contains(&0) {
                return Err(format!("{path:?} is not an absolute path"));
            }
        }
        Ok(RepoPin {
            tree: raw.tree,
            common: raw.common,
        })
    }
}

/// The pin for a run that works from `dir`, read now — at launch, in the
/// client, before the writer exists, never later. `None` if the tree's git
/// directory and the common directory are not both owned git directories at
/// git's layout; such a run is not measured.
pub fn pin(dirs: &Dirs, git: &Tool, dir: &Path, roots: &[&Path]) -> Option<RepoPin> {
    let tree = placement::git_dir_of(dirs, git, dir, roots)?;
    let (_, common) = spawn::git_roots(git, dir, roots).ok()??;
    let tree = spawn::owned_git_dir(&tree)?;
    let common = spawn::owned_git_dir(&common)?;
    at_layout(&tree, &common).then_some(RepoPin { tree, common })
}

/// Whether `tree` is the git directory git keeps for a tree of the
/// repository whose common directory is `common`: that directory itself, or
/// `<common>/worktrees/<name>` whose `commondir` names it. Both canonical.
fn at_layout(tree: &Path, common: &Path) -> bool {
    if tree == common {
        return true;
    }
    placement::is_linked_gitdir(tree, common)
        && spawn::small_file_line(&tree.join("commondir"))
            .filter(|line| !line.is_empty())
            .and_then(|line| std::fs::canonicalize(tree.join(OsStr::from_bytes(&line))).ok())
            .is_some_and(|named| named == common)
}

/// The git directory to read against, from the pin, checked again now: the
/// tree's own if it is still there and still the repository's; the common
/// directory's if the tree's is gone (its worktree was removed). One that is
/// there and is not the repository's any more is never read.
pub fn trusted(pin: &RepoPin) -> Result<PathBuf, Why> {
    let common = spawn::owned_git_dir(&pin.common).ok_or(Why::RepositoryGone)?;
    if common != pin.common {
        return Err(Why::PinChanged);
    }
    match pin.tree.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(common),
        Err(_) => return Err(Why::PinChanged),
        Ok(_) => {}
    }
    spawn::owned_git_dir(&pin.tree)
        .filter(|tree| *tree == pin.tree && at_layout(tree, &common))
        .ok_or(Why::PinChanged)
}

/// Whether a run is measured at all: a fork writer that finished `done`
/// with a patch, a base commit and a pin.
pub fn measurable(story: &Story) -> bool {
    story.state == State::Done
        && story.patch.is_some()
        && story.base_commit.is_some()
        && story.base_repo.is_some()
}

/// The runs a later run resumed: a resumed fork's patch holds the first
/// run's blocks too, so the latest run of a chain stands for all of it.
pub fn superseded(stories: &[Story]) -> BTreeSet<String> {
    stories
        .iter()
        .filter(|story| story.patch.is_some())
        .filter_map(|story| story.resumed_from.clone())
        .collect()
}

/// `(kept, counted)`: the writer's blocks of `MIN_BLOCK_LINES` or more, and
/// those of them that are in `net` — the same block, in the same file.
pub fn count(summary: &PatchSummary, net: &[patch::Block]) -> (u32, u32) {
    let present: BTreeSet<(&str, &str)> = net
        .iter()
        .map(|block| (block.path.as_str(), block.hash.as_str()))
        .collect();
    let (mut kept, mut counted) = (0, 0);
    for file in &summary.files {
        for (hash, lines) in &file.hunks {
            if *lines < MIN_BLOCK_LINES {
                continue;
            }
            counted += 1;
            if present.contains(&(file.path.as_str(), hash.as_str())) {
                kept += 1;
            }
        }
    }
    (kept, counted)
}

/// Where a run stands: its latest measurement before the window passed, its
/// latest after, and whether it is due one now.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Standing {
    pub early: Option<Measure>,
    pub settled: Option<Measure>,
    pub due: bool,
}

/// A run's standing at `now`, from its measurements. Pure. Superseded runs
/// are the caller's to leave out.
pub fn classify(story: &Story, now: u64) -> Standing {
    let settles = story.t.saturating_add(WINDOW_SECS);
    let mut standing = Standing::default();
    for (t, measure) in &story.measures {
        match *t < settles {
            true => standing.early = Some(measure.clone()),
            false => standing.settled = Some(measure.clone()),
        }
    }
    standing.due = measurable(story) && now >= settles && standing.settled.is_none();
    standing
}

/// What a row of `report` says about survival: each run in one bucket, by
/// its latest measurement.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Tally {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settled: Option<Settled>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub early: Option<Early>,
    pub unknown: u32,
    pub unmeasured: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Settled {
    pub runs: u32,
    /// The mean of the runs' shares, those that counted nothing left out.
    pub share: Option<f64>,
    /// Runs whose settled share is below their early one.
    pub fell: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Early {
    pub runs: u32,
    pub share: Option<f64>,
}

/// The buckets over `standings`. Pure.
pub fn tally(standings: &[Standing]) -> Tally {
    let mut tally = Tally::default();
    let (mut settled, mut early) = (Vec::new(), Vec::new());
    let mut fell = 0;
    for standing in standings {
        match (&standing.settled, &standing.early) {
            (Some(last), _) | (None, Some(last)) if last.unknown.is_some() => tally.unknown += 1,
            (Some(last), before) => {
                let (now, then) = (last.share(), before.as_ref().and_then(Measure::share));
                if let (Some(now), Some(then)) = (now, then)
                    && now < then
                {
                    fell += 1;
                }
                settled.push(now);
            }
            (None, Some(last)) => early.push(last.share()),
            (None, None) => tally.unmeasured += 1,
        }
    }
    let mean = |shares: &[Option<f64>]| {
        let known: Vec<f64> = shares.iter().flatten().copied().collect();
        (!known.is_empty())
            .then(|| (known.iter().sum::<f64>() / known.len() as f64 * 100.0).round() / 100.0)
    };
    if !settled.is_empty() {
        tally.settled = Some(Settled {
            runs: settled.len() as u32,
            share: mean(&settled),
            fell,
        });
    }
    if !early.is_empty() {
        tally.early = Some(Early {
            runs: early.len() as u32,
            share: mean(&early),
        });
    }
    tally
}

/// The standings `report` folds into its rows: every measurable run that no
/// later run superseded, by id.
pub fn standings<'a>(
    stories: &'a [Story],
    superseded: &BTreeSet<String>,
    now: u64,
) -> BTreeMap<&'a str, Standing> {
    stories
        .iter()
        .filter(|story| measurable(story) && !superseded.contains(&story.run))
        .map(|story| (story.run.as_str(), classify(story, now)))
        .collect()
}

/// Measures `story` now. Never fails: a step that cannot be taken is an
/// unknown measurement, with its reason. `roots` is the asking process's
/// workspace: no `git` from it, or from the repository, is run.
pub fn measure(dirs: &Dirs, git: &Tool, story: &Story, roots: &[&Path]) -> Measure {
    match measuring(dirs, git, story, roots) {
        Ok(measure) => measure,
        Err(why) => Measure::unknown(why),
    }
}

fn measuring(dirs: &Dirs, git: &Tool, story: &Story, roots: &[&Path]) -> Result<Measure, Why> {
    let started = Instant::now();
    let (Some(pin), Some(base), Some(summary)) = (
        story.base_repo.as_ref(),
        story.base_commit.as_ref(),
        story.patch.as_ref(),
    ) else {
        return Err(Why::GitFailed);
    };
    let gitdir = trusted(pin)?;

    let mut roots = roots.to_vec();
    roots.extend([gitdir.as_path(), pin.common.as_path()]);
    if pin.common.file_name() == Some(OsStr::new(".git"))
        && let Some(top) = pin.common.parent()
    {
        roots.push(top);
    }
    if story.dir.exists() {
        roots.push(&story.dir);
    }
    let path = git.path(&roots);
    let git = git.at(&roots).map_err(|fail| match fail.exit {
        Exit::Policy => Why::GitRefused,
        _ => Why::GitFailed,
    })?;
    let mut prefix = placement::quiet_git_args(dirs).map_err(|_| Why::GitFailed)?;
    let mut git_dir = OsString::from("--git-dir=");
    git_dir.push(&gitdir);
    prefix.push(git_dir);

    // One step: its argv after the prefix, with the time that is left.
    let step = |rest: &[&str], cap: usize| -> Res<(spawn::Output, bool)> {
        let left = DEADLINE
            .checked_sub(started.elapsed())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| crate::exit::Fail::internal("out of time"))?;
        let mut args = prefix.clone();
        args.extend(rest.iter().map(OsString::from));
        spawn::run_helper_capped(&git, &args, Some(&gitdir), left, path.clone(), &[], cap)
    };
    let resolve = |name: &str| -> Result<Option<Commit>, Why> {
        let (output, _) = step(
            &["rev-parse", "--verify", "--quiet", "--end-of-options", name],
            4096,
        )
        .map_err(|_| Why::GitFailed)?;
        Ok((output.status == Some(0))
            .then(|| Commit::parse(output.stdout.trim()))
            .flatten())
    };

    let tip = resolve("HEAD^{commit}")?.ok_or(Why::GitFailed)?;
    resolve(&format!("{base}^{{commit}}"))?.ok_or(Why::BaseGone)?;

    let mut diff: Vec<&str> = vec!["--no-pager"];
    diff.extend(patch::DIFF.iter().filter(|arg| **arg != "--binary"));
    diff.extend(["--end-of-options", base.as_str(), tip.as_str(), "--"]);
    let (output, overflowed) = step(&diff, NET_CAP).map_err(|_| match started.elapsed() {
        spent if spent >= DEADLINE => Why::TooLarge,
        _ => Why::GitFailed,
    })?;
    if overflowed {
        return Err(Why::TooLarge);
    }
    if output.status != Some(0) {
        return Err(Why::GitFailed);
    }
    let (kept, counted) = count(summary, &patch::blocks(&output.bytes));
    Ok(Measure {
        tip: Some(tip),
        kept,
        counted,
        unknown: None,
    })
}

/// Measures `story` and appends the measurement to the history. The
/// measurement is returned whatever the append did, and so is the append's
/// failure: a caller that has already said its piece may ignore it.
pub fn record(dirs: &Dirs, git: &Tool, story: &Story, roots: &[&Path]) -> (Event, Res<()>) {
    let event = measure(dirs, git, story, roots).event(&story.run, crate::run::record::now());
    let appended = history::append(dirs, &event);
    (event, appended)
}

/// What `report` measured in one call, and how many due runs it did not
/// reach.
#[derive(Debug, Default)]
pub struct CatchUp {
    pub events: Vec<Event>,
    pub pending: usize,
}

/// The runs among `stories` that are due a settled measurement at `now`.
pub fn due<'a>(stories: &'a [Story], superseded: &BTreeSet<String>, now: u64) -> Vec<&'a Story> {
    let mut due: Vec<&Story> = stories
        .iter()
        .filter(|story| !superseded.contains(&story.run) && classify(story, now).due)
        .collect();
    due.sort_by_key(|story| story.t);
    due
}

/// Measures every due run once, oldest first, within `REPORT_BUDGET`, and
/// appends each measurement, best effort. What it measured comes back either
/// way, for `report` to fold in; what it did not reach stays pending.
pub fn catch_up(
    dirs: &Dirs,
    git: &Tool,
    stories: &[Story],
    superseded: &BTreeSet<String>,
    now: u64,
    roots: &[&Path],
) -> CatchUp {
    let started = Instant::now();
    let mut events = Vec::new();
    let due = due(stories, superseded, now);
    let pending = within(
        &due,
        REPORT_BUDGET,
        DEADLINE,
        || started.elapsed(),
        |story| events.push(record(dirs, git, story, roots).0),
    );
    CatchUp { events, pending }
}

/// Takes `due` in its order while a whole `deadline` still fits in what is
/// left of `budget` (`spent` says how much is gone), and returns how many it
/// did not take. A measurement is never started that the budget would cut
/// short: one cut short would be recorded unknown, and never tried again.
fn within<T>(
    due: &[T],
    budget: Duration,
    deadline: Duration,
    spent: impl Fn() -> Duration,
    mut take: impl FnMut(&T),
) -> usize {
    for (at, item) in due.iter().enumerate() {
        if spent().saturating_add(deadline) > budget {
            return due.len() - at;
        }
        take(item);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::{Block, FileSummary};

    fn summary(files: &[(&str, &[(&str, u32)])]) -> PatchSummary {
        PatchSummary {
            files: files
                .iter()
                .map(|(path, hunks)| FileSummary {
                    path: path.to_string(),
                    added: 0,
                    removed: 0,
                    hunks: hunks
                        .iter()
                        .map(|(hash, lines)| (hash.to_string(), *lines))
                        .collect(),
                })
                .collect(),
            ..PatchSummary::default()
        }
    }

    fn block(path: &str, hash: &str) -> Block {
        Block {
            path: path.into(),
            hash: hash.into(),
            lines: 2,
        }
    }

    #[test]
    fn a_block_counts_where_it_is_in_the_same_file() {
        let writer = summary(&[
            ("a", &[("h1", 2), ("h2", 3), ("tiny", 1)]),
            ("b", &[("h3", 2)]),
        ]);
        let net = [block("a", "h1"), block("a", "h3"), block("a", "tiny")];
        // h1 is kept; h2 is gone; h3 is in another file; a 1-line block is
        // not counted, kept or not.
        assert_eq!(count(&writer, &net), (1, 3));
        assert_eq!(count(&writer, &[]), (0, 3));
        assert_eq!(count(&summary(&[("a", &[("tiny", 1)])]), &net), (0, 0));
        // A truncated summary is counted over what it lists.
        let mut cut = writer.clone();
        cut.truncated = true;
        assert_eq!(count(&cut, &net), (1, 3));
    }

    fn known(kept: u32, counted: u32) -> Measure {
        Measure {
            tip: Commit::parse(&"a".repeat(40)),
            kept,
            counted,
            unknown: None,
        }
    }

    #[test]
    fn a_share_is_none_when_unknown_or_nothing_counted() {
        assert_eq!(known(3, 4).share(), Some(0.75));
        assert_eq!(known(0, 0).share(), None);
        assert_eq!(Measure::unknown(Why::BaseGone).share(), None);
    }

    #[test]
    fn why_is_a_closed_snake_case_vocabulary() {
        let names: Vec<String> = [
            Why::RepositoryGone,
            Why::PinChanged,
            Why::BaseGone,
            Why::TooLarge,
            Why::GitRefused,
            Why::GitFailed,
        ]
        .iter()
        .map(|why| {
            serde_json::to_value(why)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
        assert_eq!(
            names,
            [
                "repository_gone",
                "pin_changed",
                "base_gone",
                "too_large",
                "git_refused",
                "git_failed"
            ]
        );
        assert!(serde_json::from_str::<Why>("\"no such thing\"").is_err());
    }

    #[test]
    fn the_survival_diff_is_the_patch_diff_without_binary() {
        assert_eq!(patch::DIFF.iter().filter(|a| **a == "--binary").count(), 1);
        let kept: Vec<&&str> = patch::DIFF.iter().filter(|a| **a != "--binary").collect();
        assert_eq!(kept.len(), patch::DIFF.len() - 1);
        assert!(kept.contains(&&"--no-ext-diff") && kept.contains(&&"--no-textconv"));
    }

    fn story(run: &str, t: u64) -> Story {
        let line = format!(
            r#"{{"kind":"finished","t":{t},"run":"{run}","role":"implement","caller":"claude","target":{{"harness":"codex","model":"m","effort":"high"}},"dir":"/w","state":"done","exit":0,"tokens_in":1,"tokens_out":1,"secs":1,"sampled":false,"base_commit":"{}","patch":{{"files":[],"added":0,"removed":0}},"base_repo":{{"tree":"/w/.git","common":"/w/.git"}}}}"#,
            "b".repeat(40)
        );
        history::stories(&[serde_json::from_str(&line).unwrap()]).remove(0)
    }

    #[test]
    fn a_run_is_early_until_its_window_passes_and_the_latest_wins() {
        let mut run = story("r", 1000);
        let settles = 1000 + WINDOW_SECS;
        assert_eq!(classify(&run, settles - 1), Standing::default());
        assert!(classify(&run, settles).due);
        run.measures = vec![(1001, known(0, 1)), (1002, known(1, 1))];
        let standing = classify(&run, settles);
        assert_eq!(standing.early, Some(known(1, 1)));
        assert!(standing.settled.is_none() && standing.due);
        run.measures.push((settles, known(0, 1)));
        let standing = classify(&run, settles + 5);
        assert_eq!(standing.settled, Some(known(0, 1)));
        assert!(!standing.due);
        // An unknown result after the window is settled too: never retried.
        run.measures
            .push((settles + 1, Measure::unknown(Why::GitFailed)));
        assert!(!classify(&run, settles + 5).due);
        // A run that cannot be measured is never due.
        run.measures.clear();
        run.base_repo = None;
        assert!(!classify(&run, settles).due);
    }

    #[test]
    fn a_resumed_run_supersedes_the_one_it_continues() {
        let first = story("a", 1);
        let mut second = story("b", 2);
        second.resumed_from = Some("a".into());
        let mut third = story("c", 3);
        third.resumed_from = Some("b".into());
        let all = [first.clone(), second.clone(), third];
        assert_eq!(
            superseded(&all),
            BTreeSet::from(["a".to_string(), "b".to_string()])
        );
        // A resumed run that never finished supersedes nothing; one with no
        // patch does not either.
        assert!(superseded(std::slice::from_ref(&first)).is_empty());
        second.patch = None;
        assert!(superseded(&[first, second]).is_empty());
    }

    #[test]
    fn the_tally_puts_each_run_in_one_bucket() {
        let standing = |early: Option<Measure>, settled: Option<Measure>| Standing {
            early,
            settled,
            due: false,
        };
        let tally = tally(&[
            standing(Some(known(1, 1)), Some(known(0, 1))),
            standing(Some(known(1, 2)), Some(known(1, 2))),
            standing(None, Some(known(1, 1))),
            standing(Some(known(1, 4)), None),
            standing(Some(known(0, 0)), None),
            standing(Some(known(1, 1)), Some(Measure::unknown(Why::BaseGone))),
            standing(Some(Measure::unknown(Why::GitFailed)), None),
            standing(None, None),
        ]);
        assert_eq!(
            tally.settled,
            Some(Settled {
                runs: 3,
                share: Some(0.5),
                fell: 1
            })
        );
        assert_eq!(
            tally.early,
            Some(Early {
                runs: 2,
                share: Some(0.25)
            })
        );
        assert_eq!((tally.unknown, tally.unmeasured), (2, 1));

        // Equal is not a fall; nothing counted is no share at all.
        let flat = super::tally(&[standing(Some(known(1, 2)), Some(known(1, 2)))]);
        assert_eq!(flat.settled.unwrap().fell, 0);
        let empty = super::tally(&[standing(None, Some(known(0, 0)))]);
        assert_eq!(empty.settled.unwrap().share, None);
        let json = serde_json::to_value(super::tally(&[standing(None, None)])).unwrap();
        assert_eq!(json, serde_json::json!({"unknown": 0, "unmeasured": 1}));
    }

    #[test]
    fn shares_are_rounded_to_two_places() {
        let thirds = tally(&[Standing {
            early: None,
            settled: Some(known(1, 3)),
            due: false,
        }]);
        assert_eq!(thirds.settled.unwrap().share, Some(0.33));
    }

    #[test]
    fn report_starts_a_measurement_only_when_a_whole_deadline_fits() {
        use std::cell::Cell;
        let secs = Duration::from_secs;
        // Each measurement takes 7 s of a 30 s budget, with an 8 s deadline:
        // started at 0, 7, 14 and 21 (21 + 8 = 29); not at 28.
        let clock = Cell::new(Duration::ZERO);
        let mut taken = Vec::new();
        let due = ["oldest", "second", "third", "fourth", "fifth", "newest"];
        let pending = within(
            &due,
            secs(30),
            secs(8),
            || clock.get(),
            |run| {
                taken.push(*run);
                clock.set(clock.get() + secs(7));
            },
        );
        assert_eq!(taken, ["oldest", "second", "third", "fourth"]);
        assert_eq!(pending, 2);

        // Exactly a deadline left is enough; a moment less is not.
        let at = |spent: Duration| {
            let mut taken = 0;
            let pending = within(&[()], secs(30), secs(8), || spent, |_| taken += 1);
            (taken, pending)
        };
        assert_eq!(at(secs(22)), (1, 0));
        assert_eq!(at(secs(22) + Duration::from_millis(1)), (0, 1));
        assert_eq!(at(secs(40)), (0, 1));
        // Nothing due, nothing pending.
        assert_eq!(within::<()>(&[], secs(30), secs(8), || secs(0), |_| {}), 0);
    }

    #[test]
    fn the_due_runs_are_taken_oldest_first() {
        let runs = vec![story("c", 30), story("a", 10), story("b", 20)];
        let now = 30 + WINDOW_SECS;
        let order: Vec<&str> = due(&runs, &BTreeSet::new(), now)
            .iter()
            .map(|story| story.run.as_str())
            .collect();
        assert_eq!(order, ["a", "b", "c"]);
        // Not yet due, or superseded: not taken.
        let order: Vec<&str> = due(&runs, &BTreeSet::from(["a".to_string()]), now - 10)
            .iter()
            .map(|story| story.run.as_str())
            .collect();
        assert_eq!(order, ["b"]);
    }

    #[test]
    fn a_pin_is_absolute_paths_and_nothing_else() {
        let ok: RepoPin =
            serde_json::from_str(r#"{"tree":"/r/.git/worktrees/w","common":"/r/.git"}"#).unwrap();
        assert_eq!(ok.common, PathBuf::from("/r/.git"));
        for bad in [
            r#"{"tree":"r/.git","common":"/r/.git"}"#,
            r#"{"tree":"/r/.git","common":"--exec=x"}"#,
            r#"{"tree":"/r/.git\u0000x","common":"/r/.git"}"#,
            r#"{"tree":"/r/.git"}"#,
        ] {
            assert!(serde_json::from_str::<RepoPin>(bad).is_err(), "{bad}");
        }
    }

    /// A repository's common directory and one linked worktree's git
    /// directory, by hand, canonical.
    fn layout() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(root.path()).unwrap();
        let common = base.join("repo/.git");
        let tree = common.join("worktrees/wt");
        spawn::tests::fake_git_dir(&common);
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("HEAD"), "ref: refs/heads/wt\n").unwrap();
        std::fs::write(tree.join("commondir"), "../..\n").unwrap();
        (root, common, tree)
    }

    #[test]
    fn a_pin_is_trusted_only_at_gits_layout() {
        let (root, common, tree) = layout();
        let pin = |tree: &Path| RepoPin {
            tree: tree.to_path_buf(),
            common: common.clone(),
        };
        assert_eq!(trusted(&pin(&common)), Ok(common.clone()));
        assert_eq!(trusted(&pin(&tree)), Ok(tree.clone()));

        // Not there at all: the repository's own HEAD.
        let gone = common.join("worktrees/removed");
        assert_eq!(trusted(&pin(&gone)), Ok(common.clone()));

        // A commondir that names another repository.
        let other = root.path().join("other/.git");
        spawn::tests::fake_git_dir(&other);
        std::fs::write(tree.join("commondir"), format!("{}\n", other.display())).unwrap();
        assert_eq!(trusted(&pin(&tree)), Err(Why::PinChanged));
        std::fs::write(tree.join("commondir"), "../..\n").unwrap();

        // A plain directory, a git directory outside `<common>/worktrees`.
        let plain = common.join("worktrees/plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(trusted(&pin(&plain)), Err(Why::PinChanged));
        let outside = std::fs::canonicalize(root.path())
            .unwrap()
            .join("other/.git");
        assert_eq!(trusted(&pin(&outside)), Err(Why::PinChanged));
        // A standalone git directory where the linked one was.
        let standalone = common.join("worktrees/standalone");
        spawn::tests::fake_git_dir(&standalone);
        assert_eq!(trusted(&pin(&standalone)), Err(Why::PinChanged));

        // The common directory gone, or not a git directory any more.
        let missing = RepoPin {
            tree: tree.clone(),
            common: root.path().join("nowhere/.git"),
        };
        assert_eq!(trusted(&missing), Err(Why::RepositoryGone));
        std::fs::remove_dir_all(common.join("objects")).unwrap();
        assert_eq!(trusted(&pin(&tree)), Err(Why::RepositoryGone));
    }
}
