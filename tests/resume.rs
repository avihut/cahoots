//! `resume`: a finished run's conversation, continued. The harness's own
//! session is picked up — and everything else is a NEW run: gated, slotted,
//! recorded, fenced.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{World, git_in, path_str};

fn argv_of(answer: &common::Answer) -> Vec<String> {
    let dump: serde_json::Value = serde_json::from_str(answer.text()).unwrap();
    dump["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_resumed_run_continues_the_session_under_the_same_fence() {
    let world = World::new();
    for (caller, target) in [("claude", "codex"), ("codex", "claude")] {
        let first = world.run("hello", &["--caller", caller]);
        assert_eq!(first.code, 0, "{}", first.json);
        let session = world.record(&first.run_id())["progress"]["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let brief = world.brief("FAKE: dump");
        let again = world.ask(&[
            "resume",
            &first.run_id(),
            "--caller",
            caller,
            "--brief",
            brief.to_str().unwrap(),
        ]);
        assert_eq!(again.code, 0, "{}", again.json);
        assert_ne!(again.run_id(), first.run_id(), "a resume is a new run");
        assert_eq!(again.data()["resumed_from"], first.run_id().as_str());
        assert_eq!(again.data()["target"]["harness"], target);

        let argv = argv_of(&again).join(" ");
        assert!(
            argv.contains(&session),
            "the session was not picked up: {argv}"
        );
        match target {
            "codex" => {
                assert!(argv.contains(&format!("exec resume {session}")), "{argv}");
                assert!(argv.contains(r#"-c sandbox_mode="read-only""#), "{argv}");
                assert!(
                    argv.contains(r#"-c approval_policy="never""#)
                        && argv.contains("--ignore-rules")
                );
                assert!(
                    !argv.contains("--sandbox"),
                    "`codex exec resume` has no --sandbox: {argv}"
                );
            }
            _ => {
                assert!(argv.contains(&format!("--resume {session}")), "{argv}");
                assert!(!argv.contains("--session-id"), "{argv}");
                assert!(argv.contains("--tools Read,Grep,Glob "), "{argv}");
            }
        }
        // The resumed run is itself resumable, on the same session.
        assert_eq!(
            world.record(&again.run_id())["progress"]["session_id"],
            session.as_str()
        );
    }
}

#[test]
fn resuming_is_not_a_way_around_the_gate() {
    let world = World::new();
    world.configure("[meter.ledger]\nmax_runs_per_hour = 1");
    let first = world.run("hello", &[]);
    assert_eq!(first.code, 0, "{}", first.json);
    let brief = world.brief("and another thing");
    let again = world.ask(&[
        "resume",
        &first.run_id(),
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(again.code, 24, "{}", again.json);
}

#[test]
fn only_a_finished_run_that_has_a_session_can_be_resumed() {
    let world = World::new();
    let brief = world.brief("more");
    let brief = brief.to_str().unwrap();
    assert_eq!(
        world
            .ask(&[
                "resume",
                "0198c0de-0000-7000-8000-000000000000",
                "--brief",
                brief
            ])
            .code,
        50
    );

    let running = world.run("FAKE: sleep=120", &["--wait", "0"]).run_id();
    common::wait_until("it is running", || {
        world.record(&running)["state"] == "running"
    });
    let early = world.ask(&["resume", &running, "--caller", "claude", "--brief", brief]);
    assert_eq!(early.code, 51, "{}", early.json);
    assert_eq!(world.ask(&["cancel", &running]).code, 42);

    // Cancelled runs ARE resumable: that is what the session id on disk is for.
    let after = world.ask(&["resume", &running, "--caller", "claude", "--brief", brief]);
    assert_eq!(after.code, 0, "{}", after.json);
}

#[test]
fn a_run_is_resumed_from_its_own_workspace_and_by_a_harness_that_is_not_its_target() {
    let world = World::new();
    let first = world.run("hello", &[]); // claude asked codex
    let elsewhere = tempfile::tempdir().unwrap();
    let brief = elsewhere.path().join("brief.md");
    std::fs::write(&brief, "more").unwrap();
    let mut command = world.cahoots();
    command.current_dir(elsewhere.path()).args([
        "resume",
        &first.run_id(),
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    let answer = common::answer(&mut command);
    assert_eq!(answer.code, 33, "{}", answer.json);
    assert!(answer.message().contains("another workspace"));

    let brief = world.brief("more");
    let own = world.ask(&[
        "resume",
        &first.run_id(),
        "--caller",
        "codex",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(
        own.code, 33,
        "codex resumed a session of codex's: {}",
        own.json
    );
}

#[test]
fn a_resumed_writer_goes_back_into_its_own_worktree() {
    let world = World::new();
    let brief = world.brief("FAKE: write=first.txt");
    let first = world.ask(&[
        "run",
        "--role",
        "implement",
        "--fork",
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(first.code, 0, "{}", first.json);
    let worktree = first.data()["worktree"].as_str().unwrap().to_string();

    let brief = world.brief("FAKE: write=second.txt\nFAKE: dump");
    let again = world.ask(&[
        "resume",
        &first.run_id(),
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(again.code, 0, "{}", again.json);
    assert_eq!(
        again.data()["worktree"],
        worktree.as_str(),
        "a resume cut a second worktree"
    );
    for file in ["first.txt", "second.txt"] {
        assert!(
            std::path::Path::new(&worktree).join(file).is_file(),
            "{file}"
        );
    }
    assert!(!world.work.join("second.txt").exists());
    let argv = argv_of(&again).join(" ");
    assert!(
        argv.contains(r#"-c sandbox_mode="workspace-write""#),
        "{argv}"
    );
}

#[test]
fn resume_keeps_the_original_kind_after_config_changes() {
    let world = World::new();
    let definition = r#"[kinds.rust-review]
description = "Review Rust."
role = "review"
candidates = [{ harness = "codex", model = "custom", effort = "medium" }]
"#;
    world.configure(definition);
    let brief = world.brief("hello");
    let first = world.ask(&[
        "run",
        "--kind",
        "rust-review",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(first.code, 0, "{}", first.json);
    let session = world.record(&first.run_id())["progress"]["session_id"].clone();
    for new_config in [
        String::new(),
        definition.replace("rust-review", "renamed"),
        definition
            .replace("review\"", "implement\"")
            .replace("codex", "claude"),
    ] {
        world.configure(&new_config);
        let brief = world.brief("FAKE: dump");
        let again = world.ask(&[
            "resume",
            &first.run_id(),
            "--brief",
            brief.to_str().unwrap(),
        ]);
        assert_eq!(again.code, 0, "{}", again.json);
        assert_eq!(again.data()["kind"], "rust-review");
        assert_eq!(again.data()["role"], "review");
        assert_eq!(again.data()["target"], first.data()["target"]);
        assert_eq!(
            world.record(&again.run_id())["progress"]["session_id"],
            session
        );
        assert!(
            argv_of(&again)
                .join(" ")
                .contains(r#"sandbox_mode="read-only""#)
        );
    }
    world.enable(&["claude"]);
    assert_eq!(
        world
            .ask(&[
                "resume",
                &first.run_id(),
                "--brief",
                brief.to_str().unwrap()
            ])
            .code,
        31
    );
    world.enable(&["claude", "codex"]);
    world.meter(
        serde_json::json!({"guarded": {"code": 24, "percent": 95}}),
        "",
    );
    assert_eq!(
        world
            .ask(&[
                "resume",
                &first.run_id(),
                "--brief",
                brief.to_str().unwrap()
            ])
            .code,
        24
    );
}

/// The worktrees the repository knows of, as `git worktree list` has them.
fn worktrees(world: &World) -> String {
    let listed = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&world.work)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(listed.status.success());
    String::from_utf8(listed.stdout).unwrap()
}

#[test]
fn a_shared_worktree_outlives_the_run_that_cut_it() {
    let world = World::new();
    let brief = world.brief("FAKE: write=first.txt");
    let first = world.ask(&[
        "run",
        "--role",
        "implement",
        "--fork",
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(first.code, 0, "{}", first.json);
    let worktree = PathBuf::from(first.data()["worktree"].as_str().unwrap());
    let brief = world.brief("FAKE: write=second.txt");
    let again = world.ask(&[
        "resume",
        &first.run_id(),
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(again.code, 0, "{}", again.json);
    assert_eq!(again.data()["worktree"], path_str(&worktree));
    assert_eq!(
        world.record(&again.run_id())["gitdir"],
        world.record(&first.run_id())["gitdir"],
        "a resumed run reads its changes against another git directory"
    );

    // The run that cut it ages out; the run that continues it is still here.
    let week_and_a_day = 8 * 24 * 3600;
    world.age(&first.run_id(), week_and_a_day);
    assert_eq!(world.ask(&["status"]).code, 0);
    assert!(
        !world.state.join("runs").join(first.run_id()).exists(),
        "a run past retention was kept"
    );
    assert!(
        worktree.is_dir(),
        "a worktree a run still works in was removed"
    );
    assert!(worktrees(&world).contains(path_str(&worktree)));
    let result = world.ask(&["result", &again.run_id()]);
    assert_eq!(result.code, 0, "{}", result.json);
    for file in ["first.txt", "second.txt"] {
        assert!(
            result.data()["changes"].to_string().contains(file),
            "{file}: {}",
            result.json
        );
    }

    // Now the last run in it ages out too.
    world.age(&again.run_id(), week_and_a_day);
    assert_eq!(world.ask(&["status"]).code, 0);
    assert!(!worktree.exists(), "the worktree outlived every run in it");
    assert!(!worktrees(&world).contains(path_str(&worktree)));
}

#[test]
fn a_fork_that_was_never_cut_cannot_be_resumed() {
    let world = World::new();
    let empty = world.root.join("no-commits");
    fs::create_dir_all(&empty).unwrap();
    git_in(&empty, &["init", "-q"]);
    let ask_in = |args: &[&str]| {
        let mut command = world.cahoots();
        command.current_dir(&empty).args(args);
        common::answer(&mut command)
    };
    // Codex asks Claude, whose session cahoots presets: the failed run looks
    // resumable.
    let brief = empty.join("first.md");
    fs::write(&brief, "FAKE: write=first.txt").unwrap();
    let first = ask_in(&[
        "run",
        "--role",
        "implement",
        "--caller",
        "codex",
        "--fork",
        "--brief",
        path_str(&brief),
    ]);
    assert_eq!(first.code, 40, "{}", first.json);
    assert_eq!(first.data()["target"]["harness"], "claude");
    assert!(first.data()["resumable"] == true, "{}", first.json);

    let brief = empty.join("second.md");
    fs::write(&brief, "FAKE: write=second.txt").unwrap();
    let again = ask_in(&[
        "resume",
        &first.run_id(),
        "--caller",
        "codex",
        "--brief",
        path_str(&brief),
    ]);
    assert_eq!(again.code, 2, "{}", again.json);
    assert!(
        again.message().contains("never got a worktree of its own"),
        "{}",
        again.json
    );
    let runs = fs::read_dir(world.state.join("runs")).unwrap().count();
    assert_eq!(runs, 1, "a resume that was refused left a run behind");
    assert!(!empty.join("second.txt").exists());
    assert!(!Path::new(&empty).join("first.txt").exists());
}

#[test]
fn a_resume_from_another_worktree_keeps_both_workspaces_out_of_its_tools() {
    let world = World::new();
    // The first run is asked for from a subdirectory: its workspace is that
    // directory and the top of the repository, `work`.
    let sub = world.work.join("sub");
    fs::create_dir_all(&sub).unwrap();
    let brief = world.brief("hello");
    let mut command = world.cahoots();
    command.current_dir(&sub).args([
        "run",
        "--role",
        "advise",
        "--caller",
        "claude",
        "--brief",
        path_str(&brief),
    ]);
    let first = common::answer(&mut command);
    assert_eq!(first.code, 0, "{}", first.json);

    // It is resumed from another worktree of the repository, by a caller with
    // a `ps` from the first run's workspace first on its PATH — which the
    // resumed run's supervisor inherits.
    let second = world.root.join("second");
    world.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        path_str(&second),
        "HEAD",
    ]);
    let marker = world.root.join("planted-ps-ran");
    world.script_at(
        &world.work.join("bin/ps"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    world.prefix_path(&world.work.join("bin"));
    let brief = second.join("more.md");
    fs::write(&brief, "and more").unwrap();
    let mut command = world.cahoots();
    command.current_dir(&second).args([
        "resume",
        &first.run_id(),
        "--caller",
        "claude",
        "--brief",
        path_str(&brief),
    ]);
    let again = common::answer(&mut command);
    assert_eq!(again.code, 0, "{}", again.json);
    assert!(
        !marker.exists(),
        "a ps from the workspace that started the run ran for its resume"
    );
    let record = world.record(&again.run_id());
    let roots = record["roots"].as_array().unwrap();
    for workspace in [&world.work, &second] {
        assert!(
            roots.iter().any(|root| root == path_str(workspace)),
            "{} is not among the resumed run's roots: {roots:?}",
            workspace.display()
        );
    }

    // Pinned in the worktree it is resumed from, a git is refused, and never
    // run: no new run.
    let git_marker = world.root.join("pinned-git-ran");
    world.script_at(
        &second.join("bin/git"),
        &format!("#!/bin/sh\ntouch '{}'\n", git_marker.display()),
    );
    world.pin_tool("git", Some(&second.join("bin/git")));
    let runs = fs::read_dir(world.state.join("runs")).unwrap().count();
    let refused = common::answer(&mut command);
    assert_eq!(refused.code, 33, "{}", refused.json);
    assert!(!git_marker.exists(), "the pinned git in the workspace ran");
    assert_eq!(
        fs::read_dir(world.state.join("runs")).unwrap().count(),
        runs
    );
}

#[test]
fn a_resumed_in_place_run_is_held_to_the_original_runs_configuration() {
    let world = World::new();
    world.configure("limits.allow_in_place = true");
    fs::write(world.work.join("tracked.txt"), "written by the callee\n").unwrap();
    world.git(&["add", "tracked.txt"]);
    world.git(&["commit", "-q", "-m", "chore: a tracked file"]);
    let marker = world.root.join("filter-ran");
    let filter = world.root.join("filter");
    world.script_at(
        &filter,
        &format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
    );

    // The first in-place writer names a filter in the tree's own config and
    // sets a tracked file to go through it. Its own result is refused — the
    // configuration changed while it ran.
    let brief = world.brief(&format!(
        "FAKE: append=.git/config::[filter \"evil\"]\\n\\tclean = {}\\n\n\
         FAKE: append=.gitattributes::* filter=evil\\n",
        filter.display()
    ));
    let first = world.ask(&[
        "run",
        "--role",
        "implement",
        "--in-place",
        "--caller",
        "claude",
        "--brief",
        path_str(&brief),
    ]);
    assert_eq!(first.code, 0, "{}", first.json);
    assert!(first.data()["changes"].is_null(), "{}", first.json);

    // The resume leaves the config poisoned. It is compared against the
    // ORIGINAL run's snapshot — taken before any writer touched the tree —
    // not a fresh one of the poisoned config, so it still differs: status is
    // refused, and the first writer's filter never runs.
    let brief = world.brief("FAKE: write=tracked.txt");
    let again = world.ask(&[
        "resume",
        &first.run_id(),
        "--caller",
        "claude",
        "--brief",
        path_str(&brief),
    ]);
    assert_eq!(again.code, 0, "the run keeps its own code: {}", again.json);
    assert!(again.data()["changes"].is_null(), "{}", again.json);
    assert!(
        again.data()["changes_error"]
            .as_str()
            .unwrap_or_default()
            .contains("changed during the run"),
        "{}",
        again.json
    );
    assert!(!marker.exists(), "the resume ran the first writer's filter");
    // The control: git status there, as anyone runs it, does run it.
    git_in(&world.work, &["status", "--short"]);
    assert!(marker.exists(), "the control never ran the filter");
}

/// An in-place resume whose configuration never changed still reads its
/// changes: the original run's snapshot matches, so status runs.
#[test]
fn a_clean_in_place_resume_still_reads_its_changes() {
    let world = World::new();
    world.configure("limits.allow_in_place = true");
    let brief = world.brief("FAKE: write=first.txt");
    let first = world.ask(&[
        "run",
        "--role",
        "implement",
        "--in-place",
        "--caller",
        "claude",
        "--brief",
        path_str(&brief),
    ]);
    assert_eq!(first.code, 0, "{}", first.json);
    let brief = world.brief("FAKE: write=second.txt");
    let again = world.ask(&[
        "resume",
        &first.run_id(),
        "--caller",
        "claude",
        "--brief",
        path_str(&brief),
    ]);
    assert_eq!(again.code, 0, "{}", again.json);
    assert!(
        again.data()["changes"]
            .as_array()
            .unwrap_or_else(|| panic!("no changes: {}", again.json))
            .iter()
            .any(|line| line.as_str().unwrap().ends_with("second.txt")),
        "{}",
        again.json
    );
}
