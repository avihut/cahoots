//! The one role that writes. It never works in the caller's tree unless a
//! person's config says it may; it gets a worktree of its own, and the caller
//! is told where the change is and what it touched.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{World, git_in};
use serde_json::json;

fn implement(world: &World, brief: &str, extra: &[&str]) -> common::Answer {
    let brief = world.brief(brief);
    let mut args = vec![
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ];
    args.extend(extra);
    world.ask(&args)
}

#[test]
fn a_writer_needs_a_place_of_its_own() {
    let world = World::new();
    let answer = implement(&world, "FAKE: write=oops.txt", &[]);
    assert_eq!(answer.code, 33, "{}", answer.json);
    assert!(answer.message().contains("--fork"));
    assert!(!world.work.join("oops.txt").exists());
    assert!(
        !world.state.join("runs").exists(),
        "a refusal left a run behind"
    );
}

#[test]
fn a_reader_takes_no_placement_flag() {
    let world = World::new();
    let brief = world.brief("hello");
    for flag in ["--fork", "--in-place"] {
        let answer = world.ask(&[
            "run",
            "--role",
            "review",
            "--caller",
            "claude",
            "--brief",
            brief.to_str().unwrap(),
            flag,
        ]);
        assert_eq!(answer.code, 2, "{flag}: {}", answer.json);
    }
}

#[test]
fn in_place_is_off_until_a_person_turns_it_on() {
    let world = World::new();
    let refused = implement(&world, "FAKE: write=edit.txt", &["--in-place"]);
    assert_eq!(refused.code, 33, "{}", refused.json);
    assert!(refused.message().contains("allow_in_place"));
    assert!(!world.work.join("edit.txt").exists());

    world.configure("limits.allow_in_place = true");
    let allowed = implement(&world, "FAKE: write=edit.txt", &["--in-place"]);
    assert_eq!(allowed.code, 0, "{}", allowed.json);
    assert!(world.work.join("edit.txt").is_file());
    assert_eq!(allowed.data()["placement"], "in_place");
    assert!(allowed.data()["changes"].to_string().contains("edit.txt"));
}

#[test]
fn a_fork_keeps_the_callers_tree_untouched() {
    let world = World::new();
    let answer = implement(&world, "FAKE: write=made-by-the-callee.txt", &["--fork"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["placement"], "fork");

    let worktree = Path::new(answer.data()["worktree"].as_str().expect("a worktree path"));
    assert!(
        worktree.starts_with(world.state.join("worktrees")),
        "{}",
        worktree.display()
    );
    assert!(
        !worktree.starts_with(&world.work),
        "the fork is inside the caller's tree"
    );
    assert!(worktree.join("made-by-the-callee.txt").is_file());
    assert!(
        !world.work.join("made-by-the-callee.txt").exists(),
        "the writer reached the caller's own tree"
    );
    // The caller is told what changed, so it knows what to review.
    let changes = answer.data()["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert!(
        changes[0]
            .as_str()
            .unwrap()
            .ends_with("made-by-the-callee.txt")
    );

    let record = world.record(&answer.run_id());
    assert_eq!(record["base"], world.work.to_str().unwrap());
    assert_eq!(record["cwd"], worktree.to_str().unwrap());
}

#[test]
fn a_fork_needs_a_repository() {
    let world = World::new();
    let elsewhere = tempfile::tempdir().unwrap();
    let brief = elsewhere.path().join("brief.md");
    std::fs::write(&brief, "FAKE: write=x.txt").unwrap();
    let mut command = world.cahoots();
    command.current_dir(elsewhere.path()).args([
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--fork",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    let answer = common::answer(&mut command);
    assert_eq!(answer.code, 33, "{}", answer.json);
    assert!(answer.message().contains("git repository"));
}

#[test]
fn each_harness_gets_its_writers_fence() {
    let world = World::new();
    for (caller, target) in [("claude", "codex"), ("codex", "claude")] {
        let brief = world.brief("FAKE: dump");
        let answer = world.ask(&[
            "run",
            "--role",
            "implement",
            "--caller",
            caller,
            "--fork",
            "--brief",
            brief.to_str().unwrap(),
        ]);
        assert_eq!(answer.code, 0, "{}", answer.json);
        assert_eq!(answer.data()["target"]["harness"], target);
        let dump: serde_json::Value = serde_json::from_str(answer.text()).unwrap();
        let argv: Vec<&str> = dump["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        match target {
            "codex" => {
                assert!(
                    argv.windows(2)
                        .any(|w| w == ["--sandbox", "workspace-write"]),
                    "{argv:?}"
                );
                assert!(!argv.contains(&"--skip-git-repo-check"));
            }
            _ => {
                assert!(
                    argv.windows(2)
                        .any(|w| w == ["--tools", "Read,Grep,Glob,Edit,Write"]),
                    "{argv:?}"
                );
                assert!(
                    argv.windows(2)
                        .any(|w| w == ["--permission-mode", "acceptEdits"])
                );
            }
        }
        // It really ran in the fork, not where the caller is.
        assert_eq!(dump["cwd"], answer.data()["worktree"]);
    }
}

#[test]
fn a_writer_is_held_to_a_larger_reserve() {
    let world = World::new();
    world.meter(
        json!({"guarded": {"code": 0, "percent": 10}}),
        "harness.codex.cap = 80",
    );
    let answer = implement(&world, "hello", &["--fork"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    // cap 80 minus the writer's 8-point reserve; a reader's would be 77.
    assert!(
        world.meter_calls()[0].contains("--cap 72"),
        "{:?}",
        world.meter_calls()
    );
}

/// A fork in a repository with no commit has no HEAD to be cut from.
fn a_repository_with_no_commit(world: &World) -> PathBuf {
    let empty = world.root.join("no-commits");
    fs::create_dir_all(&empty).unwrap();
    git_in(&empty, &["init", "-q"]);
    empty
}

#[test]
fn a_status_that_fails_is_not_reported_as_no_changes() {
    let world = World::new();
    // The writer overwrites its worktree's `.git`. Its changes are read
    // against the git directory pinned when the worktree was cut, so they
    // are still read — and never as the `.git` it wrote says.
    let rewritten = implement(&world, "FAKE: write=.git", &["--fork"]);
    assert_eq!(rewritten.code, 0, "{}", rewritten.json);
    assert!(rewritten.data()["changes"].is_array(), "{}", rewritten.json);
    assert!(
        rewritten.data().get("changes_error").is_none(),
        "{}",
        rewritten.json
    );

    // The worktree is gone by the time the caller asks: that is not "no
    // changes".
    let good = implement(&world, "FAKE: write=x.txt", &["--fork"]);
    assert_eq!(good.code, 0, "{}", good.json);
    fs::remove_dir_all(good.data()["worktree"].as_str().unwrap()).unwrap();
    let later = world.ask(&["result", &good.run_id()]);
    assert_eq!(later.code, 0, "{}", later.json);
    assert!(later.data()["changes"].is_null(), "{}", later.json);
    assert!(
        later.data()["changes_error"]
            .as_str()
            .is_some_and(|why| !why.is_empty()),
        "{}",
        later.json
    );
}

#[test]
fn a_fork_that_was_never_cut_reports_no_worktree() {
    let world = World::new();
    let empty = a_repository_with_no_commit(&world);
    let brief = empty.join("brief.md");
    fs::write(&brief, "FAKE: write=x.txt").unwrap();
    let mut command = world.cahoots();
    command.current_dir(&empty).args([
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--fork",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    let answer = common::answer(&mut command);
    assert_eq!(answer.code, 40, "{}", answer.json);
    // Its `cwd` is still the caller's own tree, which is not the writer's work.
    assert!(answer.data().get("worktree").is_none(), "{}", answer.json);
    assert!(answer.data().get("changes").is_none(), "{}", answer.json);
    assert!(!empty.join("x.txt").exists());
}

#[test]
fn a_rewritten_dot_git_does_not_steer_changes() {
    let world = World::new();
    fs::write(world.work.join("tracked.txt"), "tracked\n").unwrap();
    world.git(&["add", "tracked.txt"]);
    world.git(&["commit", "-q", "-m", "chore: a tracked file"]);
    let answer = implement(&world, "FAKE: write=callee-ran.txt", &["--fork"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let worktree = PathBuf::from(answer.data()["worktree"].as_str().unwrap());
    let common_dir = fs::canonicalize(world.work.join(".git")).unwrap();
    let record = world.record(&answer.run_id());
    let pinned = PathBuf::from(record["gitdir"].as_str().expect("a pinned git directory"));
    assert_eq!(
        pinned.parent(),
        Some(common_dir.join("worktrees").as_path()),
        "{}",
        pinned.display()
    );

    // The test plays the writer: a git directory of its own that shares the
    // repository's objects and refs, with a config of its own — whose clean
    // filter is a program — and the worktree's `.git` pointing at it.
    world.git(&["config", "extensions.worktreeConfig", "true"]);
    let evil = world.root.join("evil");
    fs::create_dir_all(&evil).unwrap();
    fs::copy(pinned.join("HEAD"), evil.join("HEAD")).unwrap();
    fs::copy(pinned.join("index"), evil.join("index")).unwrap();
    fs::write(
        evil.join("commondir"),
        format!("{}\n", common_dir.display()),
    )
    .unwrap();
    let marker = world.root.join("filter-ran");
    let filter = world.root.join("filter");
    world.script_at(
        &filter,
        &format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
    );
    fs::write(
        evil.join("config.worktree"),
        format!("[filter \"evil\"]\n\tclean = {}\n", filter.display()),
    )
    .unwrap();
    fs::write(worktree.join(".gitattributes"), "* filter=evil\n").unwrap();
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", evil.display()),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(worktree.join("tracked.txt"), "tracked\n").unwrap();

    // The control: git, run there as anyone would, runs the writer's filter.
    git_in(&worktree, &["status", "--short"]);
    assert!(marker.exists(), "the control never ran the filter");
    fs::remove_file(&marker).unwrap();

    let later = world.ask(&["result", &answer.run_id()]);
    assert_eq!(later.code, 0, "{}", later.json);
    let changes = later.data()["changes"]
        .as_array()
        .unwrap_or_else(|| panic!("no changes: {}", later.json));
    assert!(
        changes
            .iter()
            .any(|line| line.as_str().unwrap().ends_with("callee-ran.txt")),
        "{changes:?}"
    );
    assert!(!marker.exists(), "cahoots ran the writer's filter");

    // A record from before the pin has no git directory of its own: the one
    // its `.git` names now must be one git keeps for this repository, and the
    // writer's is not — the same-repository check alone would let it through.
    edit_record(&world, &answer.run_id(), |record| {
        let fields = record.as_object_mut().unwrap();
        fields.remove("gitdir");
        fields.remove("roots");
    });
    let old = world.ask(&["result", &answer.run_id()]);
    assert!(old.data()["changes"].is_null(), "{}", old.json);
    assert!(
        old.data()["changes_error"]
            .as_str()
            .unwrap()
            .contains("no longer a worktree of the repository"),
        "{}",
        old.json
    );
    assert!(
        !marker.exists(),
        "cahoots ran the writer's filter for an old record"
    );
}

/// Rewrites run.json the way `edit` says.
fn edit_record(world: &World, run: &str, edit: impl FnOnce(&mut serde_json::Value)) {
    let mut record = world.record(run);
    edit(&mut record);
    fs::write(world.run_file(run, "run.json"), record.to_string()).unwrap();
}

#[test]
fn a_pinned_git_directory_that_moved_is_not_followed() {
    let world = World::new();
    let answer = implement(&world, "FAKE: write=callee-ran.txt", &["--fork"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    // Not where git keeps a worktree of this repository.
    edit_record(&world, &answer.run_id(), |record| {
        record["gitdir"] = json!(world.config)
    });
    let result = world.ask(&["result", &answer.run_id()]);
    assert_eq!(result.code, 0, "{}", result.json);
    assert!(result.data()["changes"].is_null(), "{}", result.json);
    assert_eq!(
        result.data()["changes_error"],
        "the worktree's git directory has moved"
    );
}

#[test]
fn a_record_from_before_the_pin_still_gets_the_same_repository_check() {
    let world = World::new();
    let answer = implement(&world, "FAKE: write=callee-ran.txt", &["--fork"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    // run.json as it was written before the pin: no `gitdir`, no `roots`.
    edit_record(&world, &answer.run_id(), |record| {
        let fields = record.as_object_mut().unwrap();
        fields.remove("gitdir");
        fields.remove("roots");
    });
    let old = world.ask(&["result", &answer.run_id()]);
    assert_eq!(old.code, 0, "{}", old.json);
    assert!(
        old.data()["changes"].to_string().contains("callee-ran.txt"),
        "{}",
        old.json
    );

    // Its `.git` now names a worktree of another repository: read as it
    // says, the changes would be that repository's.
    let worktree = PathBuf::from(old.data()["worktree"].as_str().unwrap());
    let other = world.root.join("other");
    fs::create_dir_all(&other).unwrap();
    git_in(&other, &["init", "-q"]);
    git_in(
        &other,
        &[
            "-c",
            "user.name=Other",
            "-c",
            "user.email=other@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "chore: another repository",
        ],
    );
    let theirs = world.root.join("other-worktree");
    git_in(
        &other,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            theirs.to_str().unwrap(),
            "HEAD",
        ],
    );
    fs::copy(theirs.join(".git"), worktree.join(".git")).unwrap();
    let elsewhere = world.ask(&["result", &answer.run_id()]);
    assert!(elsewhere.data()["changes"].is_null(), "{}", elsewhere.json);
    assert!(
        elsewhere.data()["changes_error"]
            .as_str()
            .unwrap()
            .contains("no longer a worktree of the repository"),
        "{}",
        elsewhere.json
    );

    // And a `.git` that leads nowhere is said too, never "no changes".
    fs::write(worktree.join(".git"), "broken\n").unwrap();
    let broken = world.ask(&["result", &answer.run_id()]);
    assert!(broken.data()["changes"].is_null(), "{}", broken.json);
    assert!(
        broken.data()["changes_error"]
            .as_str()
            .unwrap()
            .contains("no longer a worktree of the repository"),
        "{}",
        broken.json
    );
}

#[test]
fn a_status_git_cannot_read_is_said_and_the_run_keeps_its_code() {
    let world = World::new();
    let answer = implement(&world, "FAKE: write=callee-ran.txt", &["--fork"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let worktree = PathBuf::from(answer.data()["worktree"].as_str().unwrap());
    let pinned = PathBuf::from(
        world.record(&answer.run_id())["gitdir"]
            .as_str()
            .expect("a pinned git directory"),
    );
    fs::write(pinned.join("index"), "not an index").unwrap();
    // The control: git itself cannot read this worktree's status now.
    let control = std::process::Command::new("git")
        .args(["status", "--short"])
        .current_dir(&worktree)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(!control.status.success(), "the control status succeeded");

    let later = world.ask(&["result", &answer.run_id()]);
    assert_eq!(later.code, 0, "the run's own code: {}", later.json);
    assert!(later.data()["changes"].is_null(), "{}", later.json);
    assert!(
        later.data()["changes_error"]
            .as_str()
            .unwrap_or_default()
            .contains("`git status` failed"),
        "{}",
        later.json
    );
}
