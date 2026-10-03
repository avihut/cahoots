//! How much of a fork writer's diff survived: what share of its blocks are
//! still part of the repository's change since the base commit, measured
//! when `outcome` is recorded and once more by `report` after the window. It
//! reads commits only, against the repository pinned when the run started,
//! and whatever it finds, no exit code changes.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Answer, World, git_in, path_str};
use serde_json::Value;

const DAY: u64 = 24 * 3600;
/// A block of two lines, which counts.
const EDIT: &str = "one\ntwo\n";

/// Writes `files` in `dir` and commits them there; returns HEAD.
fn commit_in(dir: &Path, files: &[(&str, &[u8])]) -> String {
    let mut add = vec!["add", "--"];
    for (name, bytes) in files {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        add.push(name);
    }
    git_in(dir, &add);
    git_in(
        dir,
        &[
            "-c",
            "user.name=World",
            "-c",
            "user.email=world@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "chore: the caller's work",
        ],
    );
    head(dir)
}

/// `git <args>` in `dir`, which must succeed; its stdout, trimmed.
fn git_out(dir: &Path, args: &[&str]) -> String {
    let done = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        done.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&done.stderr)
    );
    String::from_utf8(done.stdout).unwrap().trim().to_string()
}

fn head(dir: &Path) -> String {
    git_out(dir, &["rev-parse", "HEAD"])
}

fn history(world: &World) -> Vec<Value> {
    fs::read_to_string(world.state.join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn lines_of(world: &World, kind: &str, run: &str) -> Vec<Value> {
    history(world)
        .into_iter()
        .filter(|event| event["kind"] == kind && event["run"] == run)
        .collect()
}

fn survivals(world: &World, run: &str) -> Vec<Value> {
    lines_of(world, "survival", run)
}

/// A fork writer started from `dir` with `brief`, finished, its history line
/// written; its id.
fn writer_in(world: &World, dir: &Path, brief: &str) -> String {
    let file = dir.join(format!("brief-{}.md", fs::read_dir(dir).unwrap().count()));
    fs::write(&file, brief).unwrap();
    let mut command = world.cahoots();
    command.current_dir(dir).args([
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--fork",
        "--brief",
        path_str(&file),
    ]);
    let answer = common::answer(&mut command);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(answer.data()["patch"].is_object(), "{}", answer.json);
    let id = answer.run_id();
    common::wait_until("the finished history event", || {
        !lines_of(world, "finished", &id).is_empty()
    });
    id
}

/// A fork writer from the world's repository that appends `EDIT` to
/// `keep.txt`, which the base commit has.
fn writer(world: &World) -> String {
    commit_in(&world.work, &[("keep.txt", b"kept\n")]);
    writer_in(world, &world.work, "FAKE: append=keep.txt::one\\ntwo\\n")
}

/// The caller takes the writer's edit: the same bytes, committed in `dir`.
fn accept_in(dir: &Path) -> String {
    let path = dir.join("keep.txt");
    let mut text = fs::read(&path).unwrap();
    text.extend_from_slice(EDIT.as_bytes());
    commit_in(dir, &[("keep.txt", &text)])
}

fn outcome_from(world: &World, dir: &Path, run: &str) -> Answer {
    let mut command = world.cahoots();
    command.current_dir(dir).args(["outcome", run, "accepted"]);
    common::answer(&mut command)
}

/// `outcome <run> accepted`, which must succeed with today's envelope.
fn outcome(world: &World, run: &str) -> Answer {
    let answer = outcome_from(world, &world.work, run);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(
        answer.data(),
        &serde_json::json!({"run": run, "outcome": "accepted"})
    );
    answer
}

fn counts(line: &Value) -> (u64, u64) {
    (
        line["kept"].as_u64().unwrap(),
        line["counted"].as_u64().unwrap(),
    )
}

fn report(world: &World) -> Answer {
    report_from(world, &world.work)
}

fn report_from(world: &World, dir: &Path) -> Answer {
    let mut command = world.cahoots();
    command.current_dir(dir).args(["report", "--days", "30"]);
    let answer = common::answer(&mut command);
    assert_eq!(answer.code, 0, "{}", answer.json);
    answer
}

/// The `survival` of the implement row, which must be there.
fn row_survival(answer: &Answer) -> Value {
    let rows = answer.data()["by_role_and_target"].as_object().unwrap();
    let (_, row) = rows
        .iter()
        .find(|(key, _)| key.starts_with("implement"))
        .unwrap_or_else(|| panic!("no implement row: {}", answer.json));
    row["survival"].clone()
}

/// A marker script at `path` that leaves `mark` in the world's root when
/// anything runs it.
fn marker(world: &World, path: &Path) -> PathBuf {
    world.script_at(
        path,
        &format!(
            "#!/bin/sh\necho \"$0 $*\" >> '{}'\n",
            world.root.join("mark").display()
        ),
    );
    path.to_path_buf()
}

fn marked(world: &World) -> String {
    fs::read_to_string(world.root.join("mark")).unwrap_or_default()
}

/// A linked worktree of the world's repository, outside it, at
/// `<root>/<name>`; its path, canonical.
fn linked(world: &World, name: &str) -> PathBuf {
    let path = world.root.join(name);
    world.git(&["worktree", "add", "-q", path_str(&path)]);
    fs::canonicalize(path).unwrap()
}

fn common_dir(world: &World) -> PathBuf {
    fs::canonicalize(world.work.join(".git")).unwrap()
}

#[test]
fn an_accepted_diff_that_was_committed_survives() {
    let world = World::new();
    let run = writer(&world);
    let tip = accept_in(&world.work);
    outcome(&world, &run);
    let lines = survivals(&world, &run);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(counts(&lines[0]), (1, 1));
    assert_eq!(lines[0]["tip"], tip.as_str());
    assert!(lines[0].get("unknown").is_none(), "{}", lines[0]);
}

#[test]
fn a_diff_nobody_committed_has_not_survived() {
    let world = World::new();
    let run = writer(&world);
    outcome(&world, &run);
    let lines = survivals(&world, &run);
    assert_eq!(counts(&lines[0]), (0, 1));
    assert_eq!(lines[0]["tip"], head(&world.work).as_str());
}

#[test]
fn a_writers_own_commits_are_not_survival() {
    let world = World::new();
    commit_in(&world.work, &[("keep.txt", b"kept\n")]);
    let run = writer_in(
        &world,
        &world.work,
        "FAKE: append=keep.txt::one\\ntwo\\n\nFAKE: commit",
    );
    outcome(&world, &run);
    assert_eq!(counts(&survivals(&world, &run)[0]), (0, 1));

    // A fork daft cuts is on a branch of its own: the writer's commit is in
    // the repository's refs, and still not survival.
    let fork = world.root.join("forks/f1");
    world.daft(serde_json::json!({"make": "worktree", "print": fork}));
    let run = writer_in(
        &world,
        &world.work,
        "FAKE: append=keep.txt::three\\nfour\\n\nFAKE: commit",
    );
    assert_eq!(
        world.daft_calls().len(),
        1,
        "the fake daft was not the one run"
    );
    outcome(&world, &run);
    assert_eq!(counts(&survivals(&world, &run)[0]), (0, 1));
}

#[test]
fn a_block_kept_then_rewritten_scores_one_then_zero() {
    let world = World::new();
    let run = writer(&world);
    accept_in(&world.work);
    outcome(&world, &run);
    assert_eq!(counts(&survivals(&world, &run)[0]), (1, 1));

    commit_in(&world.work, &[("keep.txt", b"kept\nuno\ndos\n")]);
    world.age_history(&run, 15 * DAY);
    let answer = report(&world);
    let lines = survivals(&world, &run);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(counts(&lines[1]), (0, 1));
    let survival = row_survival(&answer);
    assert_eq!(survival["settled"]["share"], 0.0);
    assert_eq!(survival["settled"]["fell"], 1);
    assert!(survival.get("early").is_none(), "{survival}");
    assert_eq!(answer.data()["survival"]["measured"], 1);
}

#[test]
fn report_measures_a_run_nobody_gave_an_outcome() {
    let world = World::new();
    let run = writer(&world);
    accept_in(&world.work);
    world.age_history(&run, 15 * DAY);
    let answer = report(&world);
    assert_eq!(
        answer.data()["survival"],
        serde_json::json!({"window_days": 14, "measured": 1, "pending": 0})
    );
    let survival = row_survival(&answer);
    assert_eq!(survival["settled"]["runs"], 1);
    assert_eq!(survival["settled"]["share"], 1.0);
    assert_eq!(counts(&survivals(&world, &run)[0]), (1, 1));
}

#[test]
fn report_before_the_window_measures_nothing() {
    let world = World::new();
    let run = writer(&world);
    let answer = report(&world);
    assert_eq!(answer.data()["survival"]["measured"], 0);
    assert_eq!(answer.data()["survival"]["pending"], 0);
    assert!(survivals(&world, &run).is_empty());
    assert_eq!(row_survival(&answer)["unmeasured"], 1);
}

/// Rewrites `run`'s finished line in the history with `change`.
fn edit_finished(world: &World, run: &str, change: impl Fn(&mut Value)) {
    let path = world.state.join("history.jsonl");
    let text: String = history(world)
        .into_iter()
        .map(|mut event| {
            if event["kind"] == "finished" && event["run"] == run {
                change(&mut event);
            }
            format!("{event}\n")
        })
        .collect();
    fs::write(path, text).unwrap();
}

#[test]
fn a_run_is_measured_once_after_the_window() {
    let world = World::new();
    let kept = writer(&world);
    let lost = writer_in(
        &world,
        &world.work,
        "FAKE: append=keep.txt::three\\nfour\\n",
    );
    // A base commit that is no commit of the repository: unknown.
    edit_finished(&world, &lost, |event| {
        event["base_commit"] = "0123456789abcdef0123456789abcdef01234567".into();
    });
    world.age_history(&kept, 15 * DAY);
    world.age_history(&lost, 15 * DAY);
    let first = report(&world);
    assert_eq!(first.data()["survival"]["measured"], 2);
    let second = report(&world);
    assert_eq!(second.data()["survival"]["measured"], 0);
    assert_eq!(survivals(&world, &kept).len(), 1);
    let lost_lines = survivals(&world, &lost);
    assert_eq!(lost_lines.len(), 1, "an unknown result is not retried");
    assert_eq!(lost_lines[0]["unknown"], "base_gone");
    let survival = row_survival(&second);
    assert_eq!(survival["settled"]["runs"], 1);
    assert_eq!(survival["unknown"], 1);
}

#[test]
fn a_change_of_a_single_line_is_not_counted() {
    let world = World::new();
    commit_in(&world.work, &[("keep.txt", b"kept\n")]);
    let run = writer_in(&world, &world.work, "FAKE: append=keep.txt::one\\n");
    outcome(&world, &run);
    assert_eq!(counts(&survivals(&world, &run)[0]), (0, 0));
    world.age_history(&run, 15 * DAY);
    let survival = row_survival(&report(&world));
    assert_eq!(survival["settled"]["runs"], 1);
    assert!(survival["settled"]["share"].is_null(), "{survival}");
}

#[test]
fn a_path_with_spaces_and_quotes_is_matched() {
    let world = World::new();
    let run = writer_in(
        &world,
        &world.work,
        "FAKE: append=a b.txt::one\\ntwo\\n\nFAKE: append=\u{e9}.txt::three\\nfour\\n",
    );
    commit_in(
        &world.work,
        &[("a b.txt", b"one\ntwo\n"), ("\u{e9}.txt", b"three\nfour\n")],
    );
    outcome(&world, &run);
    let finished = &lines_of(&world, "finished", &run)[0];
    let mut paths: Vec<&str> = finished["patch"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    paths.sort();
    assert_eq!(paths, ["\"\\303\\251.txt\"", "a b.txt"]);
    assert_eq!(counts(&survivals(&world, &run)[0]), (2, 2));
}

#[test]
fn a_resumed_run_stands_for_the_chain() {
    let world = World::new();
    let first = writer_in(&world, &world.work, "FAKE: append=a.txt::one\\ntwo\\n");
    let brief = world.brief("FAKE: append=b.txt::three\\nfour\\n");
    let again = world.ask(&[
        "resume",
        &first,
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(again.code, 0, "{}", again.json);
    let second = again.run_id();
    common::wait_until("the resumed run's history event", || {
        !lines_of(&world, "finished", &second).is_empty()
    });
    let finished = &lines_of(&world, "finished", &second)[0];
    assert_eq!(finished["resumed_from"], first.as_str());
    assert_eq!(
        finished["base_repo"],
        lines_of(&world, "finished", &first)[0]["base_repo"]
    );

    commit_in(
        &world.work,
        &[("a.txt", b"one\ntwo\n"), ("b.txt", b"three\nfour\n")],
    );
    outcome(&world, &first);
    assert!(survivals(&world, &first).is_empty());
    world.age_history(&first, 15 * DAY);
    world.age_history(&second, 15 * DAY);
    let survival = row_survival(&report(&world));
    assert_eq!(survival["settled"]["runs"], 1, "{survival}");
    assert_eq!(survival["settled"]["share"], 1.0);
    assert!(survivals(&world, &first).is_empty());
    assert_eq!(counts(&survivals(&world, &second)[0]), (2, 2));
}

/// The settings and attributes that would run a command, or shape the diff,
/// if git honoured them: each runs a marker. Set after every commit the test
/// makes, and after the run, whose capture honours clean filters.
fn trap_the_configuration(world: &World) {
    let at = |name: &str| marker(world, &world.root.join("traps").join(name));
    let hooks = world.root.join("traps/hooks");
    marker(world, &hooks.join("post-checkout"));
    marker(world, &hooks.join("pre-auto-gc"));
    for (key, value) in [
        ("diff.external", path_str(&at("external")).to_string()),
        ("diff.evil.textconv", path_str(&at("textconv")).to_string()),
        ("diff.evil.command", path_str(&at("command")).to_string()),
        ("filter.evil.clean", path_str(&at("clean")).to_string()),
        ("filter.evil.process", path_str(&at("process")).to_string()),
        ("core.fsmonitor", path_str(&at("fsmonitor")).to_string()),
        ("core.hooksPath", path_str(&hooks).to_string()),
        ("core.pager", path_str(&at("pager")).to_string()),
        ("diff.algorithm", "histogram".to_string()),
        ("diff.noprefix", "true".to_string()),
        ("color.diff", "always".to_string()),
    ] {
        world.git(&["config", key, &value]);
    }
    let attributes = "* diff=evil filter=evil\n";
    fs::write(world.work.join(".gitattributes"), attributes).unwrap();
    fs::create_dir_all(world.work.join(".git/info")).unwrap();
    fs::write(world.work.join(".git/info/attributes"), attributes).unwrap();
}

#[test]
fn survival_reads_the_repository_without_its_configuration() {
    let world = World::new();
    let run = writer(&world);
    accept_in(&world.work);
    outcome(&world, &run);
    trap_the_configuration(&world);
    outcome(&world, &run);
    let lines = survivals(&world, &run);
    assert_eq!(lines.len(), 2);
    assert_eq!(counts(&lines[1]), (1, 1));
    assert_eq!(lines[1]["tip"], lines[0]["tip"]);
    assert_eq!(marked(&world), "", "a command the repository names ran");
}

#[test]
fn a_dirty_worktree_is_never_read() {
    let world = World::new();
    let run = writer(&world);
    trap_the_configuration(&world);
    // The writer's edit, in the caller's tree, never committed.
    let mut text = fs::read(world.work.join("keep.txt")).unwrap();
    text.extend_from_slice(EDIT.as_bytes());
    fs::write(world.work.join("keep.txt"), text).unwrap();
    outcome(&world, &run);
    assert_eq!(counts(&survivals(&world, &run)[0]), (0, 1));
    assert_eq!(marked(&world), "", "a command the repository names ran");
}

#[test]
fn report_words_show_survival_at_a_terminal() {
    let world = World::new();
    let run = writer(&world);
    accept_in(&world.work);
    outcome(&world, &run);
    let piped = report(&world);
    assert_eq!(row_survival(&piped)["early"]["share"], 1.0);
    assert_eq!(piped.data()["survival"]["window_days"], 14);
    let words = world.as_a_person(&["report"]).finish();
    assert_eq!(words.code, 0);
    assert!(
        words.text().contains("Kept: 100% of 1 still settling"),
        "{}",
        words.text()
    );

    let readers = World::new();
    readers.run("hello", &[]);
    let answer = report(&readers);
    for (_, row) in answer.data()["by_role_and_target"].as_object().unwrap() {
        assert!(row.get("survival").is_none(), "{row}");
    }
    assert_eq!(answer.data()["survival"]["measured"], 0);
}

#[test]
fn report_inside_the_codex_sandbox_still_answers() {
    let world = World::new();
    let run = writer(&world);
    world.age_history(&run, 15 * DAY);
    let mut command = world.cahoots();
    command
        .env("CODEX_SANDBOX", "seatbelt")
        .args(["report", "--days", "30"]);
    let answer = common::answer(&mut command);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["survival"]["measured"], 0);
    assert_eq!(answer.data()["survival"]["pending"], 1);
    assert!(survivals(&world, &run).is_empty());
}

#[test]
fn report_answers_when_the_history_cannot_be_written() {
    let world = World::new();
    let run = writer(&world);
    accept_in(&world.work);
    world.age_history(&run, 15 * DAY);
    let path = world.state.join("history.jsonl");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    let answer = report(&world);
    assert_eq!(answer.data()["survival"]["measured"], 1);
    assert_eq!(row_survival(&answer)["settled"]["share"], 1.0);
    assert!(survivals(&world, &run).is_empty(), "nothing is stored");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
}

/// The outcome is in the history, whatever survival said.
fn outcome_recorded(world: &World, run: &str) {
    assert!(
        !lines_of(world, "outcome", run).is_empty(),
        "no outcome for {run}"
    );
}

#[test]
fn survival_unknown_when_the_repository_is_gone() {
    let world = World::new();
    let run = writer(&world);
    fs::remove_dir_all(&world.work).unwrap();
    let answer = outcome_from(&world, &world.root, &run);
    assert_eq!(answer.code, 0, "{}", answer.json);
    outcome_recorded(&world, &run);
    let lines = survivals(&world, &run);
    assert_eq!(lines[0]["unknown"], "repository_gone");
    assert_eq!(counts(&lines[0]), (0, 0));
    assert!(lines[0].get("tip").is_none(), "{}", lines[0]);
    let survival = row_survival(&report_from(&world, &world.root));
    assert_eq!(survival["unknown"], 1);
    assert!(survival.get("early").is_none() && survival.get("settled").is_none());
}

#[test]
fn survival_unknown_when_the_base_commit_is_gone() {
    let world = World::new();
    commit_in(&world.work, &[("keep.txt", b"kept\n")]);
    let base = commit_in(&world.work, &[("throwaway.txt", b"soon gone\n")]);
    let run = writer_in(&world, &world.work, "FAKE: append=keep.txt::one\\ntwo\\n");
    // Nothing may keep the base reachable: not the fork's worktree, nor a
    // reflog.
    let fork = PathBuf::from(world.record(&run)["cwd"].as_str().unwrap());
    world.git(&["worktree", "remove", "--force", path_str(&fork)]);
    world.git(&["reset", "-q", "--hard", "HEAD~1"]);
    world.git(&["reflog", "expire", "--expire=now", "--all"]);
    world.git(&["gc", "-q", "--prune=now"]);
    let gone = Command::new("git")
        .args(["cat-file", "-e", &base])
        .current_dir(&world.work)
        .status()
        .unwrap();
    assert!(!gone.success(), "the base commit is still there");

    outcome(&world, &run);
    outcome_recorded(&world, &run);
    assert_eq!(survivals(&world, &run)[0]["unknown"], "base_gone");
}

/// A `git` at `dir/tools/git` that logs its argv and then is the real git.
fn planted_git(world: &World, dir: &Path) -> PathBuf {
    let real = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let tools = dir.join("tools");
    world.script_at(
        &tools.join("git"),
        &format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            world.root.join("git-calls").display(),
            real.trim()
        ),
    );
    world.prefix_path(&tools);
    tools
}

#[test]
fn a_planted_git_does_not_change_outcomes_exit_code() {
    let world = World::new();
    let wt = linked(&world, "wt");
    let run = writer_in(&world, &wt, "FAKE: write=x.txt");
    planted_git(&world, &wt);
    outcome(&world, &run);
    outcome_recorded(&world, &run);
    assert_eq!(survivals(&world, &run)[0]["unknown"], "git_refused");
    let calls = fs::read_to_string(world.root.join("git-calls")).unwrap_or_default();
    for call in calls.lines() {
        assert!(
            !call.contains("--git-dir=") && !call.contains(" diff ") && !call.contains("HEAD^{"),
            "survival ran the planted git: {call}"
        );
    }
}

#[test]
fn a_git_planted_in_the_asking_workspace_refuses_outcome_as_before() {
    let world = World::new();
    let run = writer(&world);
    planted_git(&world, &world.work);
    let answer = outcome_from(&world, &world.work, &run);
    assert_eq!(answer.code, 33, "{}", answer.json);
    assert!(lines_of(&world, "outcome", &run).is_empty());
    assert!(survivals(&world, &run).is_empty());
}

#[test]
fn a_huge_net_diff_is_too_large_not_zero() {
    let world = World::new();
    let run = writer(&world);
    // A net diff past 32 MiB: one big text file, committed.
    let line = "x".repeat(1023) + "\n";
    let big = line.repeat(33 * 1024);
    commit_in(&world.work, &[("big.txt", big.as_bytes())]);
    outcome(&world, &run);
    let lines = survivals(&world, &run);
    assert_eq!(lines[0]["unknown"], "too_large");
    assert_eq!(counts(&lines[0]), (0, 0));
}

#[test]
fn the_pin_is_recorded_before_the_writer_runs() {
    let world = World::new();
    let common = common_dir(&world);
    let wt = linked(&world, "wt");
    let brief = wt.join("brief.md");
    fs::write(&brief, "FAKE: write=x.txt").unwrap();
    let mut command = world.cahoots();
    command.current_dir(&wt).args([
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--fork",
        "--brief",
        path_str(&brief),
    ]);
    let answer = common::answer(&mut command);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let run = answer.run_id();
    common::wait_until("the finished history event", || {
        !lines_of(&world, "finished", &run).is_empty()
    });
    let pin = serde_json::json!({
        "tree": path_str(&common.join("worktrees/wt")),
        "common": path_str(&common),
    });
    assert_eq!(world.record(&run)["base_repo"], pin);
    assert_eq!(lines_of(&world, "finished", &run)[0]["base_repo"], pin);
    for shown in [
        answer.json.to_string(),
        world.ask(&["status", &run]).json.to_string(),
        world.ask(&["result", &run]).json.to_string(),
    ] {
        assert!(!shown.contains("base_repo"), "{shown}");
    }

    let main = writer(&world);
    let pin = &world.record(&main)["base_repo"];
    assert_eq!(pin["tree"], path_str(&common));
    assert_eq!(pin["common"], path_str(&common));
    // A reader has none.
    let reader = world.run("hello", &[]).run_id();
    assert!(
        world
            .record(&reader)
            .get("base_repo")
            .is_none_or(Value::is_null)
    );
    common::wait_until("the reader's history event", || {
        !lines_of(&world, "finished", &reader).is_empty()
    });
    assert!(
        lines_of(&world, "finished", &reader)[0]
            .get("base_repo")
            .is_none()
    );
}

/// The HEAD git keeps for linked worktree `name`, read from its git
/// directory.
fn linked_head(world: &World, name: &str) -> String {
    let gitdir = common_dir(world).join("worktrees").join(name);
    git_out(
        &world.work,
        &["--git-dir", path_str(&gitdir), "rev-parse", "HEAD"],
    )
}

#[test]
fn a_redirected_dot_git_is_not_followed() {
    let world = World::new();
    let wt = linked(&world, "wt");
    commit_in(&wt, &[("keep.txt", b"kept\n")]);
    let run = writer_in(&world, &wt, "FAKE: append=keep.txt::one\\ntwo\\n");

    // A repository of the agent's, holding the base and the writer's block,
    // armed; the tree's `.git` now names it.
    let evil = world.root.join("evil");
    git_in(
        &world.root,
        &["clone", "-q", path_str(&wt), path_str(&evil)],
    );
    accept_in(&evil);
    for (key, script) in [
        ("core.fsmonitor", "fsmonitor"),
        ("diff.external", "external"),
        ("filter.evil.clean", "clean"),
    ] {
        let path = marker(&world, &world.root.join("traps").join(script));
        git_in(&evil, &["config", key, path_str(&path)]);
    }
    fs::write(evil.join(".gitattributes"), "* filter=evil\n").unwrap();
    fs::write(
        wt.join(".git"),
        format!("gitdir: {}\n", evil.join(".git").display()),
    )
    .unwrap();

    outcome(&world, &run);
    let lines = survivals(&world, &run);
    assert!(lines[0].get("unknown").is_none(), "{}", lines[0]);
    assert_eq!(counts(&lines[0]), (0, 1));
    assert_eq!(lines[0]["tip"], linked_head(&world, "wt").as_str());
    assert_eq!(
        marked(&world),
        "",
        "a command the agent's repository names ran"
    );
}

#[test]
fn a_swapped_pin_is_refused_and_nothing_is_read() {
    let world = World::new();
    let wt = linked(&world, "wt");
    let run = writer_in(&world, &wt, "FAKE: write=x.txt");
    let gitdir = common_dir(&world).join("worktrees/wt");

    // (i) Its `commondir` names another repository.
    let other = world.root.join("other");
    fs::create_dir_all(&other).unwrap();
    git_in(&other, &["init", "-q"]);
    let original = fs::read(gitdir.join("commondir")).unwrap();
    fs::write(
        gitdir.join("commondir"),
        format!("{}\n", other.join(".git").display()),
    )
    .unwrap();
    planted_git(&world, &world.root.join("outside"));
    outcome(&world, &run);
    assert_eq!(survivals(&world, &run)[0]["unknown"], "pin_changed");
    fs::write(gitdir.join("commondir"), original).unwrap();

    // (ii) A standalone git directory where the linked one was.
    fs::remove_dir_all(&gitdir).unwrap();
    git_in(&world.root, &["init", "-q", "--bare", path_str(&gitdir)]);
    outcome(&world, &run);
    outcome_recorded(&world, &run);
    assert_eq!(survivals(&world, &run)[1]["unknown"], "pin_changed");

    let calls = fs::read_to_string(world.root.join("git-calls")).unwrap_or_default();
    assert!(!calls.contains("--git-dir="), "something was read: {calls}");
}

#[test]
fn a_removed_worktree_falls_back_to_the_repositorys_head() {
    let world = World::new();
    commit_in(&world.work, &[("keep.txt", b"kept\n")]);
    let wt = linked(&world, "wt");
    let run = writer_in(&world, &wt, "FAKE: append=keep.txt::one\\ntwo\\n");
    let tip = accept_in(&world.work);
    world.git(&["worktree", "remove", "--force", path_str(&wt)]);
    assert!(!common_dir(&world).join("worktrees/wt").exists());

    outcome(&world, &run);
    let lines = survivals(&world, &run);
    assert!(lines[0].get("unknown").is_none(), "{}", lines[0]);
    assert_eq!(lines[0]["tip"], tip.as_str());
    assert_eq!(counts(&lines[0]), (1, 1));

    world.age_history(&run, 15 * DAY);
    let survival = row_survival(&report(&world));
    assert_eq!(survival["settled"]["share"], 1.0);
    assert_eq!(survivals(&world, &run)[1]["tip"], tip.as_str());
}

#[test]
fn a_run_without_a_pin_is_not_measured() {
    let world = World::new();
    let run = writer(&world);
    edit_finished(&world, &run, |event| {
        event.as_object_mut().unwrap().remove("base_repo");
    });
    let finished = &lines_of(&world, "finished", &run)[0];
    assert!(finished["patch"].is_object() && finished["base_commit"].is_string());
    outcome(&world, &run);
    assert!(survivals(&world, &run).is_empty());
    world.age_history(&run, 15 * DAY);
    let answer = report(&world);
    let rows = answer.data()["by_role_and_target"].as_object().unwrap();
    for (_, row) in rows {
        assert!(row.get("survival").is_none(), "{row}");
    }
    assert_eq!(answer.data()["survival"]["measured"], 0);
}

#[test]
fn readers_and_in_place_writers_are_never_measured() {
    let world = World::new();
    world.configure("limits.allow_in_place = true");
    let reader = world.run("hello", &[]).run_id();
    let brief = world.brief("FAKE: append=keep.txt::one\\ntwo\\n");
    let in_place = world.ask(&[
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--in-place",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(in_place.code, 0, "{}", in_place.json);
    let in_place = in_place.run_id();
    for run in [&reader, &in_place] {
        common::wait_until("the finished history event", || {
            !lines_of(&world, "finished", run).is_empty()
        });
        outcome(&world, run);
        assert!(survivals(&world, run).is_empty(), "{run}");
        assert!(
            lines_of(&world, "finished", run)[0]
                .get("base_repo")
                .is_none()
        );
    }
}

#[test]
fn outcome_is_recorded_whatever_survival_says() {
    let world = World::new();
    let run = writer(&world);
    // A pin whose tree is now a plain directory: refused, nothing read.
    let pinned = PathBuf::from(
        lines_of(&world, "finished", &run)[0]["base_repo"]["common"]
            .as_str()
            .unwrap(),
    );
    edit_finished(&world, &run, |event| {
        event["base_repo"]["tree"] = path_str(&pinned.join("info")).into();
    });
    fs::create_dir_all(pinned.join("info")).unwrap();
    for answer in ["accepted", "discarded"] {
        let mut command = world.cahoots();
        command.args(["outcome", &run, answer]);
        let answered = common::answer(&mut command);
        assert_eq!(answered.code, 0, "{}", answered.json);
    }
    assert_eq!(lines_of(&world, "outcome", &run).len(), 2);
    let lines = survivals(&world, &run);
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|line| line["unknown"] == "pin_changed"));
}

/// A writer's edit, accepted, in a partial clone that has lost its copy of
/// the accepted file (`World::promisor`). The control shows the survival
/// diff reads that object: without `GIT_NO_LAZY_FETCH`, it fetches.
fn accepted_in_a_partial_clone(world: &World) -> String {
    let run = writer(world);
    let tip = accept_in(&world.work);
    let blob = git_out(&world.work, &["rev-parse", "HEAD:keep.txt"]);
    let base = lines_of(world, "finished", &run)[0]["base_commit"]
        .as_str()
        .unwrap()
        .to_string();
    world.promisor();
    world.lose(&blob);
    assert!(
        world.fetches_without_the_variable(&world.work, &["diff", &base, &tip]),
        "the diff does not read the lost object, so this test could not fail"
    );
    let _ = fs::remove_file(world.root.join("fetch-tried"));
    run
}

#[test]
fn outcome_never_fetches_a_missing_object() {
    let world = World::new();
    let run = accepted_in_a_partial_clone(&world);
    outcome(&world, &run);
    outcome_recorded(&world, &run);
    assert_eq!(survivals(&world, &run)[0]["unknown"], "git_failed");
    assert!(
        !world.fetch_tried(),
        "survival started the promisor's transport"
    );
}

#[test]
fn report_never_fetches_a_missing_object() {
    let world = World::new();
    let run = accepted_in_a_partial_clone(&world);
    world.age_history(&run, 15 * DAY);
    let answer = report(&world);
    assert_eq!(answer.data()["survival"]["measured"], 1);
    assert_eq!(row_survival(&answer)["unknown"], 1);
    assert_eq!(survivals(&world, &run)[0]["unknown"], "git_failed");
    assert!(
        !world.fetch_tried(),
        "survival started the promisor's transport"
    );
}
