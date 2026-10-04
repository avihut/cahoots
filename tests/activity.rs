//! A run's words on the agent verbs, end to end against the fake harness:
//! what it is doing now on `status`, and its notes and its account of a
//! failure wherever a run is shown — each bounded, and marked as the
//! callee's (docs/THREAT-MODEL.md, "A run's words are the callee's").

mod common;

use std::fs;

use common::{World, wait_until};
use serde_json::{Value, json};

/// `--to <target>`, from the other harness.
fn toward(target: &str) -> [&str; 4] {
    let caller = if target == "claude" {
        "codex"
    } else {
        "claude"
    };
    ["--to", target, "--caller", caller]
}

fn activity(world: &World, run: &str) -> Value {
    world.ask(&["status", run]).data()["activity"].clone()
}

fn listed(world: &World, run: &str) -> Value {
    world.ask(&["status"]).data()["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|listed| listed["run"] == run)
        .cloned()
        .expect("the run is listed")
}

#[test]
fn status_shows_what_a_run_is_doing_now_as_the_callees_words() {
    let world = World::new();
    for (target, label) in [("claude", "read"), ("codex", "command")] {
        let mut extra = vec!["--wait", "0"];
        extra.extend(toward(target));
        let started = world.run(
            "FAKE: said=Looking at the gate first\nFAKE: step=src/gate.rs\nFAKE: sleep=120",
            &extra,
        );
        let run = started.run_id();
        assert!(started.data().get("activity").is_none(), "{}", started.json);
        wait_until("the step reaches status", || {
            activity(&world, &run)["text"] == "src/gate.rs"
        });
        let now = activity(&world, &run);
        assert!(now["at"].as_u64().is_some(), "{now}");
        assert_eq!(
            now,
            json!({"untrusted": true, "kind": "tool", "tool": label,
                   "text": "src/gate.rs", "truncated": false, "at": now["at"]}),
            "{target}"
        );
        assert_eq!(listed(&world, &run)["activity"], now);
        // The tool's output is the repository's text, never the callee's step.
        let shown = world.ask(&["status", &run]).json.to_string();
        assert!(!shown.contains("not the fake's"), "{shown}");

        let cancelled = world.ask(&["cancel", &run]);
        assert!(
            cancelled.data().get("activity").is_none(),
            "{}",
            cancelled.json
        );
    }
}

#[test]
fn only_status_shows_it_and_nothing_learned_keeps_it() {
    let world = World::new();
    for target in ["claude", "codex"] {
        let answer = world.run(
            "FAKE: step=a-step-only-status-shows\nFAKE: say=done",
            &toward(target),
        );
        assert_eq!(answer.code, 0, "{}", answer.json);
        let run = answer.run_id();
        // Finished, the last thing it did was say its answer.
        let last = activity(&world, &run);
        assert_eq!(
            (last["kind"].clone(), last["text"].clone()),
            ("said".into(), "done".into())
        );
        assert!(answer.data().get("activity").is_none(), "{}", answer.json);
        for verb in ["wait", "result"] {
            let shown = world.ask(&[verb, &run]);
            assert!(
                shown.data().get("activity").is_none(),
                "{verb}: {}",
                shown.json
            );
        }
    }
    let history = fs::read_to_string(world.state.join("history.jsonl")).unwrap();
    assert!(!history.contains("a-step-only-status-shows"), "{history}");
}

#[test]
fn a_hostile_step_is_one_bounded_line_on_status() {
    let world = World::new();
    for target in ["claude", "codex"] {
        let mut extra = vec!["--wait", "0"];
        extra.extend(toward(target));
        let step = format!("\\e[2J\\e]0;owned first{}\\nsecond line", "x".repeat(1000));
        let run = world
            .run(&format!("FAKE: step={step}\nFAKE: sleep=120"), &extra)
            .run_id();
        wait_until("the step reaches status", || {
            !activity(&world, &run)["text"].is_null()
        });
        let now = activity(&world, &run);
        let text = now["text"].as_str().unwrap();
        assert_eq!(text.chars().count(), 200, "{target}: {text}");
        assert!(text.starts_with("[2J ]0;owned first"), "{target}: {text}");
        assert!(
            !text.contains('\u{1b}') && !text.contains("second"),
            "{text}"
        );
        assert_eq!(now["truncated"], true);
        assert_eq!(now["untrusted"], true);
        world.ask(&["cancel", &run]);
    }
}

#[test]
fn notes_and_a_failure_are_bounded_and_marked_wherever_a_run_is_shown() {
    let world = World::new();
    let reason = format!("\\e]0;owned{}", "f".repeat(2000));
    let answer = world.run(
        &format!(
            "FAKE: warn=\\e[31mretrying{}\\nmore\nFAKE: fail={reason}",
            "w".repeat(1000)
        ),
        &toward("codex"),
    );
    assert_eq!(answer.code, 40, "{}", answer.json);
    // cahoots' own words in the message; the callee's under `data`.
    assert_eq!(
        answer.message(),
        "the run failed — the callee's own account is in data.failure"
    );
    let run = answer.run_id();
    for (verb, shown) in [
        ("run", answer.data().clone()),
        ("status", world.ask(&["status", &run]).data().clone()),
        ("wait", world.ask(&["wait", &run]).data().clone()),
        ("result", world.ask(&["result", &run]).data().clone()),
    ] {
        let failure = &shown["failure"];
        let text = failure["text"].as_str().expect(verb);
        assert_eq!(failure["untrusted"], true, "{verb}");
        assert_eq!(failure["truncated"], true, "{verb}");
        assert_eq!(text.chars().count(), 500, "{verb}");
        assert!(!text.contains('\u{1b}'), "{verb}: {text}");
        let notes = shown["notes"].as_array().expect(verb);
        assert_eq!(notes.len(), 1, "{verb}");
        let note = &notes[0];
        let text = note["text"].as_str().unwrap();
        assert_eq!(note["untrusted"], true, "{verb}");
        assert_eq!(note["truncated"], true, "{verb}");
        assert_eq!(text.chars().count(), 300, "{verb}");
        assert!(text.starts_with("[31mretrying"), "{verb}: {text}");
        assert!(!text.contains('\u{1b}'), "{verb}: {text}");
    }

    // A run that succeeds has no failure to account for.
    let fine = world.run("FAKE: warn=just a warning\nFAKE: say=ok", &toward("codex"));
    assert_eq!(fine.code, 0, "{}", fine.json);
    assert!(fine.data()["failure"].is_null(), "{}", fine.json);
    assert_eq!(
        fine.data()["notes"],
        json!([{"untrusted": true, "text": "just a warning", "truncated": false}])
    );
}

/// Makes a run's directory read-only while it lives, and writable again when
/// it drops, so a failing test still leaves a directory it can remove.
struct ReadOnly(std::path::PathBuf);

impl ReadOnly {
    fn new(dir: std::path::PathBuf) -> ReadOnly {
        set_mode(&dir, 0o500);
        ReadOnly(dir)
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        set_mode(&self.0, 0o700);
    }
}

fn set_mode(dir: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn a_step_whose_save_failed_reaches_status_once_a_save_can_land() {
    let world = World::new();
    let gate = world.root.join("step-gate");
    let mut extra = vec!["--wait", "0"];
    extra.extend(toward("claude"));
    let run = world
        .run(
            &format!(
                "FAKE: hold={}\nFAKE: step=src/later.rs\nFAKE: sleep=120",
                gate.display()
            ),
            &extra,
        )
        .run_id();
    wait_until("the callee is running", || {
        world.record(&run)["state"] == "running"
    });

    // The run's directory cannot be written: the save of the step fails.
    let read_only = ReadOnly::new(world.run_file(&run, "run.json").parent().unwrap().into());
    fs::write(&gate, "").unwrap();
    wait_until("a save of the step fails", || {
        fs::read_to_string(world.run_file(&run, "supervisor.log"))
            .unwrap_or_default()
            .contains("activity not saved, trying again")
    });
    assert!(
        activity(&world, &run).is_null(),
        "{}",
        activity(&world, &run)
    );

    // The callee says nothing more, and the step still lands.
    drop(read_only);
    wait_until("the step reaches status", || {
        activity(&world, &run)["text"] == "src/later.rs"
    });
    assert_eq!(listed(&world, &run)["activity"]["text"], "src/later.rs");
    assert_eq!(listed(&world, &run)["activity"]["tool"], "read");
    world.ask(&["cancel", &run]);
}

#[test]
fn an_older_failed_record_reads_as_the_callees_words_on_every_verb() {
    let world = World::new();
    let id = "01legacy-failed-run";
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let text = include_str!("fixtures/records/legacy-failed.json")
        .replace("@ID@", id)
        .replace("@CWD@", world.work.to_str().unwrap());
    let mut record: Value = serde_json::from_str(&text).unwrap();
    for at in ["created_at", "started_at", "finished_at"] {
        record[at] = now.into();
    }
    let dir = world.state.join("runs").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("run.json"), record.to_string()).unwrap();

    for verb in ["wait", "result"] {
        let shown = world.ask(&[verb, id]);
        assert_eq!(shown.code, 40, "{verb}: {}", shown.json);
        assert_eq!(
            shown.message(),
            "the run failed — the callee's own account is in data.failure",
            "{verb}"
        );
        assert!(
            !shown.json.to_string().contains("\\u001b"),
            "{verb}: {}",
            shown.json
        );
    }
    for (verb, data) in [
        ("wait", world.ask(&["wait", id]).data().clone()),
        ("result", world.ask(&["result", id]).data().clone()),
        ("status", world.ask(&["status", id]).data().clone()),
        ("status list", listed(&world, id)),
    ] {
        let failure = &data["failure"];
        let text = failure["text"].as_str().expect(verb);
        assert_eq!(failure["untrusted"], true, "{verb}");
        assert_eq!(failure["truncated"], true, "{verb}");
        assert_eq!(text.chars().count(), 500, "{verb}");
        assert!(
            text.starts_with("]0;owned You've hit your usage limit."),
            "{verb}: {text}"
        );
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{7}'),
            "{verb}"
        );
        assert_eq!(
            data["notes"],
            json!([{"untrusted": true, "truncated": false,
                    "text": "stream disconnected before completion; retrying 1/5 [2J"}]),
            "{verb}"
        );
    }
}
