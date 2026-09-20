//! The one role that writes. It never works in the caller's tree unless a
//! person's config says it may; it gets a worktree of its own, and the caller
//! is told where the change is and what it touched.

mod common;

use std::path::Path;

use common::World;
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
