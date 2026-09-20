//! `resume`: a finished run's conversation, continued. The harness's own
//! session is picked up — and everything else is a NEW run: gated, slotted,
//! recorded, fenced.

mod common;

use common::World;

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
