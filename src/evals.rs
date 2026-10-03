//! The private eval suite: tasks made from a person's own accepted writer
//! runs (`evals add`), listed with whether each can still be replayed
//! (`evals list`), and taken out (`evals remove`).
//!
//! ```text
//! <data>/evals/tasks/<task>/   0700; <task> is the source run's id
//!   task.json      the task record                                    0600
//!   brief          the run's brief, byte for byte                     0600
//!   tests.diff     the hidden tests: the patch's sections for test files 0600
//!   solution.diff  every other section of the patch                   0600
//! ```
//!
//! A task is copied out of the run directory, which ages out, into the data
//! directory, which nothing ages out. Nothing here executes anything but
//! `git rev-parse`, and nothing sends a task anywhere. This is logic: it
//! returns data and refusals, and never prints.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::dirs::{Dirs, ensure_private_dir};
use crate::exit::{Exit, Fail, Res};
use crate::history::{self, Outcome};
use crate::model::{Candidate, Role, TaskKindName};
use crate::patch::{self, Commit};
use crate::placement::{self, Placement};
use crate::run::client;
use crate::run::record::{RunDir, RunRecord, now, validate_id, write_private};
use crate::spawn;
use crate::tools::{Tool, Tools};

const GIT_DEADLINE: Duration = Duration::from_secs(10);

/// A task: a brief, the commit it starts from, the hidden tests and the
/// reference solution — and where they came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub v: u32,
    /// The task's id: its run's.
    pub task: String,
    pub run: String,
    pub added_at: u64,
    pub role: Role,
    pub kind: Option<TaskKindName>,
    pub target: Candidate,
    /// The directory the writer's fork was cut from.
    pub repo: PathBuf,
    /// The repository itself: what outlives a linked worktree.
    pub git_common_dir: PathBuf,
    /// Held to the shape of an object id when read back, so an edited task
    /// file cannot put anything else into a git argv.
    pub base_commit: Commit,
    /// The files in `tests.diff` and in `solution.diff`, as the patch names
    /// them, in its order.
    pub hidden_tests: Vec<String>,
    pub solution: Vec<String>,
}

/// Whether a task can still be replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rot {
    None,
    CommitGone,
    RepositoryGone,
    /// It could not be checked: `rot_error` says why. Never taken for gone.
    Unknown,
}

/// A task as `evals list` shows it.
#[derive(Debug, Clone, Serialize)]
pub struct Listed {
    #[serde(flatten)]
    pub task: Task,
    pub path: PathBuf,
    pub rot: Rot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rot_error: Option<String>,
}

/// A task as `evals add` made it.
#[derive(Debug, Clone, Serialize)]
pub struct Added {
    #[serde(flatten)]
    pub task: Task,
    pub path: PathBuf,
}

fn refused(message: String) -> Fail {
    Fail::new(Exit::Usage, message)
}

/// `evals add <run>`: an accepted writer's run becomes a task. Each check in
/// turn, and the first that fails decides; nothing is written until all
/// have passed, and then all of it or nothing.
pub fn add(dirs: &Dirs, tools: &Tools, run: &str, invoker: &[&Path]) -> Res<Added> {
    validate_id(run)?;
    client::reconcile(dirs, tools, invoker);
    let stories = history::stories(&history::read(dirs));
    let story = stories.iter().find(|story| story.run == run);
    let dir = match RunDir::open(dirs, run) {
        Ok(dir) => dir,
        Err(fail) if fail.exit == Exit::NoSuchRun && story.is_some() => {
            return Err(Fail::new(
                Exit::NoSuchRun,
                format!(
                    "run {run} has aged out: a run keeps its brief and patch for 7 days, so it \
                     can no longer become a task"
                ),
            ));
        }
        Err(fail) => return Err(fail),
    };
    let record = dir.load()?;
    if !record.state.is_terminal() {
        return Err(Fail::new(
            Exit::NotFinished,
            format!("run {run} has not finished — there is nothing to make a task of yet"),
        ));
    }
    if record.role != Role::Implement {
        return Err(refused(format!(
            "run {run} was a {} run; only a writer's run becomes a task",
            record.role.as_str()
        )));
    }
    if let Some(from) = &record.resumed_from {
        return Err(refused(format!(
            "run {run} continues run {from}, so its brief is only the follow-up; only a run \
             that answered one brief becomes a task"
        )));
    }
    match story.and_then(|story| story.outcome) {
        Some(Outcome::Accepted) => {}
        None => {
            return Err(refused(format!(
                "run {run} has no outcome on record; only a run whose result was accepted \
                 becomes a task (`cahoots outcome {run} accepted` records one)"
            )));
        }
        Some(other) => {
            let said = match other {
                Outcome::Reworked => "reworked",
                _ => "discarded",
            };
            return Err(refused(format!(
                "run {run} was {said}; only a run whose result was accepted as it came becomes \
                 a task"
            )));
        }
    }
    if record.placement == Placement::InPlace {
        return Err(refused(format!(
            "run {run} worked in place, so no patch was kept to make a task of"
        )));
    }
    let no_patch = || {
        refused(format!(
            "no patch was kept for run {run} (its supervisor.log says why)"
        ))
    };
    let (Placement::Fork, Some(_), Some(base_commit), Some(gitdir), Some(repo)) = (
        record.placement,
        &record.patch,
        &record.base_commit,
        &record.gitdir,
        &record.base,
    ) else {
        return Err(no_patch());
    };
    // cahoots' own run directory, never the worktree; bytes, never text.
    let patch = fs::read(dir.patch_path()).map_err(|_| no_patch())?;
    if patch.is_empty() {
        return Err(refused(format!(
            "run {run} changed nothing, so there is nothing to grade against"
        )));
    }
    let tasks = dirs.evals_tasks();
    let path = tasks.join(run);
    let in_the_suite = || refused(format!("run {run} is already in the suite"));
    if path.symlink_metadata().is_ok() {
        return Err(in_the_suite());
    }
    let sections = patch::sections(&patch);
    if let Some((test, other)) = straddling(&sections) {
        return Err(refused(format!(
            "run {run}'s change has a test and a file of the rest that collide ({test} and \
             {other}: one inside the other, or the same path but for case), so its hidden tests \
             would not apply on their own"
        )));
    }

    let mut roots = invoker.to_vec();
    roots.extend(record.tool_roots());
    roots.push(gitdir);
    let gone = || {
        refused(format!(
            "the repository run {run} worked on is gone ({}), so its base commit cannot be checked",
            repo.display()
        ))
    };
    let git_common_dir = common_dir(dirs, &tools.git, &record, &roots)?.ok_or_else(gone)?;
    roots.push(&git_common_dir);
    match check(dirs, &tools.git, &git_common_dir, base_commit, &roots)? {
        (Rot::None, _) => {}
        (Rot::CommitGone, _) => {
            return Err(refused(format!(
                "run {run}'s base commit {} is no longer in {}, so it cannot be replayed",
                &base_commit.as_str()[..12],
                repo.display()
            )));
        }
        (Rot::RepositoryGone, _) => return Err(gone()),
        (Rot::Unknown, why) => {
            return Err(Fail::internal(format!(
                "cannot check run {run}'s base commit: {}",
                why.unwrap_or_default()
            )));
        }
    }

    let (mut tests, mut solution) = (Vec::new(), Vec::new());
    let (mut hidden_tests, mut rest) = (Vec::new(), Vec::new());
    for section in sections {
        if is_test_path(&section.path) {
            tests.extend_from_slice(section.bytes);
            hidden_tests.push(section.path);
        } else {
            solution.extend_from_slice(section.bytes);
            rest.push(section.path);
        }
    }
    let task = Task {
        v: 1,
        task: run.to_string(),
        run: run.to_string(),
        added_at: now(),
        role: record.role,
        kind: record.kind.clone(),
        target: record.target.clone(),
        repo: repo.clone(),
        git_common_dir: git_common_dir.clone(),
        base_commit: base_commit.clone(),
        hidden_tests,
        solution: rest,
    };
    let json = serde_json::to_vec_pretty(&task)
        .map_err(|error| Fail::internal(format!("cannot encode a task: {error}")))?;
    let brief = fs::read(dir.brief_path())
        .map_err(|error| Fail::internal(format!("cannot read run {run}'s brief: {error}")))?;

    // Built beside where it goes, under a name `list` skips, then renamed
    // into place: a task is all there or not there at all.
    ensure_private_dir(&tasks)?;
    let tmp = tasks.join(format!(".{run}.{}.tmp", std::process::id()));
    // One left by a writer that died is not reused.
    let _ = fs::remove_dir_all(&tmp);
    let build = || -> Res<()> {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&tmp)
            .map_err(|error| Fail::internal(format!("cannot create {}: {error}", tmp.display())))?;
        write_private(&tmp.join("task.json"), &json)?;
        write_private(&tmp.join("brief"), &brief)?;
        write_private(&tmp.join("tests.diff"), &tests)?;
        write_private(&tmp.join("solution.diff"), &solution)?;
        if path.symlink_metadata().is_ok() {
            return Err(in_the_suite());
        }
        fs::rename(&tmp, &path).map_err(|error| {
            match error.raw_os_error().map(nix::errno::Errno::from_raw) {
                Some(nix::errno::Errno::EEXIST | nix::errno::Errno::ENOTEMPTY) => in_the_suite(),
                _ => Fail::internal(format!("cannot write {}: {error}", path.display())),
            }
        })
    };
    if let Err(fail) = build() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(fail);
    }
    Ok(Added { task, path })
}

/// `evals list`: every task, oldest first, each with its rot check. What is
/// not a task — a name `list` skips, a link, a task file that does not load —
/// is left out, and one task that cannot be checked fails nothing. `located`
/// is the located git and the asking process's workspace, or why either
/// could not be found: then no git is run, and every task is `Unknown` with
/// that reason.
pub fn list(dirs: &Dirs, located: Result<(&Tool, &[&Path]), &Fail>) -> Vec<Listed> {
    let tasks = dirs.evals_tasks();
    let Ok(entries) = fs::read_dir(&tasks) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.') && validate_id(name).is_ok())
        .collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|name| {
            let path = tasks.join(&name);
            let task = load(&path, &name)?;
            let (rot, rot_error) = match located {
                Ok((git, invoker)) => rot(dirs, git, &task, invoker),
                Err(fail) => (Rot::Unknown, Some(fail.message.clone())),
            };
            Some(Listed {
                task,
                path,
                rot,
                rot_error,
            })
        })
        .collect()
}

/// The task in `path`, if it is one: a real directory holding a real
/// `task.json` that loads, and names this task.
fn load(path: &Path, name: &str) -> Option<Task> {
    if !path.symlink_metadata().ok()?.is_dir() {
        return None;
    }
    let file = path.join("task.json");
    if !file.symlink_metadata().ok()?.is_file() {
        return None;
    }
    let task: Task = serde_json::from_slice(&fs::read(file).ok()?).ok()?;
    (task.task == name).then_some(task)
}

/// `evals remove <task>`: the task's directory goes, whatever is in it —
/// a task whose file no longer loads too, which is how a person clears one
/// `list` no longer shows. A link where a task would be is not followed.
pub fn remove(dirs: &Dirs, task: &str) -> Res<String> {
    validate_id(task)?;
    let tasks = dirs.evals_tasks();
    let path = tasks.join(task);
    if !path.symlink_metadata().is_ok_and(|meta| meta.is_dir()) {
        return Err(Fail::new(
            Exit::NoSuchRun,
            format!("no task {task} in the suite"),
        ));
    }
    // Out of the suite at once, then gone.
    let removing = tasks.join(format!(".{task}.{}.removing", std::process::id()));
    fs::rename(&path, &removing)
        .and_then(|()| fs::remove_dir_all(&removing))
        .map_err(|error| Fail::internal(format!("cannot remove {}: {error}", path.display())))?;
    Ok(task.to_string())
}

/// The common git directory of the repository a run's fork belongs to: read
/// against the git directory pinned when the fork was cut, or, once that is
/// gone, from what the fork was cut from. `None` when neither can say. A
/// `git` the binary policy refuses is a refusal.
fn common_dir(
    dirs: &Dirs,
    git: &Tool,
    record: &RunRecord,
    roots: &[&Path],
) -> Res<Option<PathBuf>> {
    let path = git.path(roots);
    let git = git.at(roots)?;
    let quiet = placement::quiet_git_args(dirs)?;
    let ask = |git_dir: Option<&Path>, cwd: &Path| -> Option<PathBuf> {
        let mut args = quiet.clone();
        if let Some(git_dir) = git_dir {
            let mut flag = OsString::from("--git-dir=");
            flag.push(git_dir);
            args.push(flag);
        }
        args.extend(
            ["rev-parse", "--path-format=absolute", "--git-common-dir"].map(OsString::from),
        );
        let output =
            spawn::run_helper_with_env(&git, &args, Some(cwd), GIT_DEADLINE, path.clone(), &[])
                .ok()?;
        if output.status != Some(0) {
            return None;
        }
        let line = output.bytes.strip_suffix(b"\n").unwrap_or(&output.bytes);
        let found = PathBuf::from(std::ffi::OsStr::from_bytes(line));
        (found.is_absolute() && found.is_dir()).then(|| fs::canonicalize(&found).unwrap_or(found))
    };
    let pinned = record
        .gitdir
        .as_deref()
        .filter(|gitdir| gitdir.is_dir())
        .and_then(|gitdir| ask(Some(gitdir), gitdir));
    Ok(pinned.or_else(|| {
        record
            .base
            .as_deref()
            .filter(|base| base.is_dir())
            .and_then(|base| ask(None, base))
    }))
}

/// Whether `task` can still be replayed: its base commit checked in its
/// repository's common git directory, which outlives a linked worktree.
/// Anything that stops the check is `Unknown`, with why.
pub fn rot(dirs: &Dirs, git: &Tool, task: &Task, invoker: &[&Path]) -> (Rot, Option<String>) {
    let mut roots = invoker.to_vec();
    roots.extend([task.repo.as_path(), task.git_common_dir.as_path()]);
    check(dirs, git, &task.git_common_dir, &task.base_commit, &roots)
        .unwrap_or_else(|fail| (Rot::Unknown, Some(fail.message)))
}

/// The rot check itself. An `Err` is a `git` that may not be run at all.
fn check(
    dirs: &Dirs,
    git: &Tool,
    common: &Path,
    commit: &Commit,
    roots: &[&Path],
) -> Res<(Rot, Option<String>)> {
    if !common.is_dir() {
        return Ok((Rot::RepositoryGone, None));
    }
    let path = git.path(roots);
    let git = git.at(roots)?;
    let mut args = placement::quiet_git_args(dirs)?;
    let mut flag = OsString::from("--git-dir=");
    flag.push(common);
    args.push(flag);
    args.extend(["rev-parse", "--verify", "--quiet", "--end-of-options"].map(OsString::from));
    args.push(OsString::from(format!("{}^{{commit}}", commit.as_str())));
    let output = spawn::run_helper_with_env(&git, &args, Some(common), GIT_DEADLINE, path, &[]);
    Ok(match output {
        Ok(output) => match output.status {
            Some(0) => (Rot::None, None),
            Some(1) => (Rot::CommitGone, None),
            Some(code) => (
                Rot::Unknown,
                Some(format!("git rev-parse exited with {code}")),
            ),
            None => (
                Rot::Unknown,
                Some("git rev-parse was stopped by a signal".to_string()),
            ),
        },
        Err(fail) => (Rot::Unknown, Some(fail.message)),
    })
}

/// A path as a patch prints it, without the one pair of quotes git puts
/// around a C-quoted path. The escapes inside are kept, never decoded.
fn unquoted(path: &str) -> &str {
    path.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(path)
}

/// A path as git wrote it in a header, as the bytes it names: one pair of
/// surrounding quotes taken off, and git's C escapes inside undone (`\t`,
/// `\"`, `\\`, `\303`, …). An escape git never writes is kept as it is.
fn decoded(path: &str) -> Vec<u8> {
    let Some(inner) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
        return path.as_bytes().to_vec();
    };
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'\\' || at + 1 == bytes.len() {
            out.push(bytes[at]);
            at += 1;
            continue;
        }
        let next = bytes[at + 1];
        let simple = match next {
            b'a' => Some(0x07),
            b'b' => Some(0x08),
            b't' => Some(b'\t'),
            b'n' => Some(b'\n'),
            b'v' => Some(0x0b),
            b'f' => Some(0x0c),
            b'r' => Some(b'\r'),
            b'"' => Some(b'"'),
            b'\\' => Some(b'\\'),
            _ => None,
        };
        if let Some(byte) = simple {
            out.push(byte);
            at += 2;
            continue;
        }
        let octal = bytes.get(at + 1..at + 4).filter(|digits| {
            digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) && digits[0] <= b'3'
        });
        match octal {
            Some(digits) => {
                out.push(
                    digits
                        .iter()
                        .fold(0u8, |byte, digit| byte * 8 + (digit - b'0')),
                );
                at += 4;
            }
            None => {
                out.push(b'\\');
                at += 1;
            }
        }
    }
    out
}

/// Whether two names, one component of a path each, may be one file on a
/// filesystem that does not tell case apart. ASCII names are compared
/// exactly, without case. Where either is not ASCII, std alone cannot rule
/// it out — Unicode case folding (a Kelvin sign is a `k`), and composed or
/// decomposed forms of one letter, which std cannot normalise — so they may.
fn may_be_one(a: &[u8], b: &[u8]) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    true
}

/// A hidden test and a file of the rest that collide: one under the other (a
/// file that became a directory, or the reverse, or a link in the way), or
/// the same path but for case (a rename that changed only case, where the
/// path rule's case-sensitive `FooTest` suffix tells the two names apart).
/// Applied apart, at the base, one of the two diffs would then fail, so such
/// a run makes no task. A change like that wholly on one side applies, in
/// git's order, within its one diff. Paths are compared as the bytes they
/// name, component by component (`may_be_one`): where a name is not ASCII,
/// a collision that cannot be ruled out counts as one, and the run is
/// refused rather than kept as a task that might not apply.
fn straddling<'a>(sections: &'a [patch::Section<'_>]) -> Option<(&'a str, &'a str)> {
    struct Named<'a> {
        path: &'a str,
        parts: Vec<Vec<u8>>,
        /// Lowercase and `/`-joined, when every byte is ASCII.
        key: Option<String>,
    }
    let named = |path: &'a str| {
        let bytes = decoded(path);
        let key = bytes
            .is_ascii()
            .then(|| String::from_utf8_lossy(&bytes).to_ascii_lowercase());
        let parts = bytes
            .split(|&byte| byte == b'/')
            .map(<[u8]>::to_vec)
            .collect();
        Named { path, parts, key }
    };
    // The shorter path, name by name, is where the longer one starts.
    let collide =
        |a: &Named, b: &Named| a.parts.iter().zip(&b.parts).all(|(a, b)| may_be_one(a, b));
    // Every directory an ASCII path is in, as a key: `a/b/c` is in `a` and `a/b`.
    let ancestors = |key: &str| -> Vec<String> {
        key.match_indices('/')
            .map(|(at, _)| key[..at].to_string())
            .collect()
    };
    let (tests, rest): (Vec<Named>, Vec<Named>) = sections
        .iter()
        .map(|section| named(&section.path))
        .partition(|named| is_test_path(named.path));
    let mut rest_files = std::collections::HashMap::new();
    let mut rest_dirs = std::collections::HashMap::new();
    for other in &rest {
        if let Some(key) = &other.key {
            rest_files.insert(key.clone(), other.path);
            for dir in ancestors(key) {
                rest_dirs.entry(dir).or_insert(other.path);
            }
        }
    }
    // Paths that are not all ASCII are held against every path on the other
    // side, name by name; there are few of them in a patch.
    let rest_not_ascii: Vec<&Named> = rest.iter().filter(|other| other.key.is_none()).collect();
    for test in &tests {
        let found = match &test.key {
            // A file of the rest at the test's own path, where the test's
            // directory is, or under the test.
            Some(key) => rest_files
                .get(key)
                .or_else(|| ancestors(key).iter().find_map(|dir| rest_files.get(dir)))
                .or_else(|| rest_dirs.get(key))
                .copied()
                .or_else(|| {
                    rest_not_ascii
                        .iter()
                        .find(|other| collide(test, other))
                        .map(|other| other.path)
                }),
            None => rest
                .iter()
                .find(|other| collide(test, other))
                .map(|other| other.path),
        };
        if let Some(other) = found {
            return Some((test.path, other));
        }
    }
    None
}

/// Whether a file in a patch is a test, by its path alone (as the patch
/// prints it, C-quoted when git quotes it). A directory named for tests, or
/// a file named like one in the common languages. Rust's inline
/// `#[cfg(test)]` modules live in source files, and count as solution.
pub fn is_test_path(path: &str) -> bool {
    let path = unquoted(path);
    let parts: Vec<&str> = path.split('/').collect();
    let (file, dirs) = parts.split_last().unwrap_or((&"", &[]));
    const DIRS: [&str; 6] = ["test", "tests", "__tests__", "spec", "specs", "testdata"];
    if dirs
        .iter()
        .any(|dir| DIRS.iter().any(|named| dir.eq_ignore_ascii_case(named)))
    {
        return true;
    }
    let lower = file.to_ascii_lowercase();
    const INFIXES: [&str; 7] = [
        "_test.", "_tests.", "_spec.", ".test.", ".spec.", "-test.", "-spec.",
    ];
    if lower.starts_with("test_")
        || INFIXES.iter().any(|infix| lower.contains(infix))
        || lower == "conftest.py"
    {
        return true;
    }
    let stem = file.split('.').next().unwrap_or(file);
    ["Test", "Tests", "Spec"].iter().any(|suffix| {
        stem.strip_suffix(suffix)
            .is_some_and(|before| !before.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_file_is_known_by_its_path() {
        for path in [
            "tests/a.rs",
            "src/test/X.java",
            "a/__tests__/b.js",
            "pkg/foo_test.go",
            "test_foo.py",
            "web/foo.test.ts",
            "web/foo.spec.js",
            "lib/foo_spec.rb",
            "FooTest.java",
            "FooTests.cs",
            "conftest.py",
            "\"tests/caf\\303\\251.rs\"",
            "Tests/A.swift",
        ] {
            assert!(is_test_path(path), "{path} is a test");
        }
        for path in [
            "src/lib.rs",
            "testing.md",
            "src/latest.rs",
            "Contest.java",
            "docs/testing/guide.md",
            "Test.java",
            "tests",
        ] {
            assert!(!is_test_path(path), "{path} is not a test");
        }
    }

    #[test]
    fn a_file_turned_directory_across_the_split_is_found() {
        let found = |paths: &[&str]| {
            let patch: String = paths
                .iter()
                .map(|path| match path.strip_prefix('"') {
                    Some(_) => {
                        let inner = &path[1..path.len() - 1];
                        format!("diff --git \"a/{inner}\" \"b/{inner}\"\n@@ -0,0 +1 @@\n+x\n")
                    }
                    None => format!("diff --git a/{path} b/{path}\n@@ -0,0 +1 @@\n+x\n"),
                })
                .collect();
            straddling(&patch::sections(patch.as_bytes()))
                .map(|(test, other)| (test.to_string(), other.to_string()))
        };
        let pair = |test: &str, other: &str| Some((test.to_string(), other.to_string()));
        // A file of the rest where a test's directory is.
        assert_eq!(
            found(&["tests", "tests/new_test.rs"]),
            pair("tests/new_test.rs", "tests")
        );
        // A file of the rest where a test was, under its directory.
        assert_eq!(
            found(&["spec", "spec/foo_spec.rb"]),
            pair("spec/foo_spec.rb", "spec")
        );
        // A test file where a directory of the rest is.
        assert_eq!(
            found(&["src/foo_test.go", "src/foo_test.go/x.rs"]),
            pair("src/foo_test.go", "src/foo_test.go/x.rs")
        );
        // Quotes are not part of the path, and case does not tell them apart.
        assert_eq!(
            found(&["Tests", "\"tests/caf\\303\\251.rs\""]),
            pair("\"tests/caf\\303\\251.rs\"", "Tests")
        );
        // Wholly on one side, or merely alike, it is nothing.
        assert_eq!(found(&["tests/fixtures", "tests/fixtures/x"]), None);
        assert_eq!(found(&["src/data", "src/data/x"]), None);
        assert_eq!(
            found(&["src/lib.rs", "src/lib.rs.orig", "tests/a.rs"]),
            None
        );
        assert_eq!(found(&["tests2", "tests/a.rs"]), None);
        // The same path but for case: a case-only rename across the split,
        // in either order, and one in which quotes are all that differ.
        assert_eq!(
            found(&["FooTest.java", "Footest.java"]),
            pair("FooTest.java", "Footest.java")
        );
        assert_eq!(
            found(&["Footest.java", "FooTest.java"]),
            pair("FooTest.java", "Footest.java")
        );
        assert_eq!(
            found(&["\"caf\\303\\251Test.java\"", "caf\\303\\251test.java"]),
            pair("\"caf\\303\\251Test.java\"", "caf\\303\\251test.java")
        );
        // Alike but for case on one side alone is nothing to it.
        assert_eq!(found(&["tests/A.rs", "tests/a.rs"]), None);
    }

    #[test]
    fn a_quoted_path_is_read_as_the_bytes_it_names() {
        assert_eq!(decoded("src/a b.rs"), b"src/a b.rs");
        assert_eq!(decoded(r#""caf\303\251.txt""#), "café.txt".as_bytes());
        assert_eq!(decoded(r#""q\"u\\o\tte""#), b"q\"u\\o\tte");
        assert_eq!(decoded(r#""\377x""#), b"\xffx");
        // What git never writes is kept as written.
        assert_eq!(decoded(r#""a\qb\9""#), b"a\\qb\\9");
        assert_eq!(decoded(r#""end\""#), b"end\\");
    }

    /// One section a path, as git writes a header for it, quoted when the
    /// path is given quoted.
    fn collision(paths: &[&str]) -> Option<(String, String)> {
        let patch: String = paths
            .iter()
            .map(|path| match path.strip_prefix('"') {
                Some(_) => {
                    let inner = &path[1..path.len() - 1];
                    format!("diff --git \"a/{inner}\" \"b/{inner}\"\n@@ -0,0 +1 @@\n+x\n")
                }
                None => format!("diff --git a/{path} b/{path}\n@@ -0,0 +1 @@\n+x\n"),
            })
            .collect();
        straddling(&patch::sections(patch.as_bytes()))
            .map(|(test, other)| (test.to_string(), other.to_string()))
    }

    #[test]
    fn names_that_are_not_ascii_collide_unless_they_can_be_told_apart() {
        let pair = |test: &str, other: &str| Some((test.to_string(), other.to_string()));
        // ÄTest.java (a test by its suffix) and ätest.java: one file where
        // case is not told apart, though git escapes them differently.
        let upper = r#""\303\204Test.java""#;
        let lower = r#""\303\244test.java""#;
        assert!(is_test_path(upper) && !is_test_path(lower));
        assert_eq!(collision(&[upper, lower]), pair(upper, lower));
        // The same letter composed (é) and decomposed (e + U+0301): std
        // cannot normalise, so it cannot rule the collision out.
        let decomposed = r#""e\314\201Test.java""#;
        let composed = r#""\303\251test.java""#;
        assert_eq!(
            collision(&[decomposed, composed]),
            pair(decomposed, composed)
        );
        // A Kelvin sign folds to an ASCII k.
        let kelvin = r#""\342\204\252Test.java""#;
        assert_eq!(
            collision(&[kelvin, "ktest.java"]),
            pair(kelvin, "ktest.java")
        );
        // Bytes that are not UTF-8 cannot be compared at all.
        let invalid = r#""\377Test.java""#;
        assert_eq!(
            collision(&[invalid, r#""\376test.java""#]),
            pair(invalid, r#""\376test.java""#)
        );
        // Under a directory of the rest whose name is not ASCII.
        assert_eq!(
            collision(&[
                r#""d\303\251j\303\240""#,
                r#""D\303\251j\303\240/tests/a.rs""#
            ]),
            pair(
                r#""D\303\251j\303\240/tests/a.rs""#,
                r#""d\303\251j\303\240""#
            )
        );
        // Told apart by an ASCII name on the way: nothing.
        assert_eq!(
            collision(&[r#""tests/\303\204Test.java""#, r#""src/\303\244.rs""#]),
            None
        );
        assert_eq!(collision(&["tests/a.rs", r#""src/\303\244.rs""#]), None);
        assert_eq!(collision(&[r#""\303\204/x_test.go""#, "b/x.go"]), None);
    }

    #[test]
    fn a_task_file_with_a_bad_commit_does_not_load() {
        let good = serde_json::json!({
            "v": 1,
            "task": "r1",
            "run": "r1",
            "added_at": 1,
            "role": "implement",
            "kind": null,
            "target": {"harness": "codex", "model": "gpt-5", "effort": "high"},
            "repo": "/w",
            "git_common_dir": "/w/.git",
            "base_commit": "0123456789abcdef0123456789abcdef01234567",
            "hidden_tests": [],
            "solution": ["src/a.rs"],
        });
        assert!(serde_json::from_value::<Task>(good.clone()).is_ok());
        for bad in [
            "--output=/tmp/x",
            "HEAD",
            "0123456789ABCDEF0123456789ABCDEF01234567",
        ] {
            let mut task = good.clone();
            task["base_commit"] = bad.into();
            assert!(serde_json::from_value::<Task>(task).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_task_on_disk_loads_only_when_it_is_what_it_says() {
        let tmp = tempfile::tempdir().unwrap();
        let task = Task {
            v: 1,
            task: "r1".to_string(),
            run: "r1".to_string(),
            added_at: 1,
            role: Role::Implement,
            kind: None,
            target: serde_json::from_value(
                serde_json::json!({"harness": "codex", "model": "gpt-5", "effort": "high"}),
            )
            .unwrap(),
            repo: PathBuf::from("/w"),
            git_common_dir: PathBuf::from("/w/.git"),
            base_commit: Commit::parse("0123456789abcdef0123456789abcdef01234567").unwrap(),
            hidden_tests: Vec::new(),
            solution: Vec::new(),
        };
        let dir = tmp.path().join("r1");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("task.json"), serde_json::to_vec(&task).unwrap()).unwrap();
        assert_eq!(load(&dir, "r1"), Some(task));
        // Under another name, it is not that task.
        assert_eq!(load(&dir, "r2"), None);
        // A link to a task is not one.
        std::os::unix::fs::symlink(&dir, tmp.path().join("r3")).unwrap();
        assert_eq!(load(&tmp.path().join("r3"), "r1"), None);
    }
}
