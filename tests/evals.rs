//! The private eval suite: an accepted writer's run becomes a task — its
//! brief, its base commit, and its patch split into the test files (the
//! hidden tests) and the rest — kept under the data directory; `list` flags
//! a task that can no longer be replayed, and `remove` takes one out. All of
//! it from a terminal only, with fake harnesses and throwaway directories.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::{Finished, World, git_in};
use serde_json::Value;

/// A tracked change to the solution, a tracked change to a test, and a new,
/// untracked test.
const B: &str = "FAKE: append=src/lib.rs::pub fn b() {}\\n\n\
                 FAKE: append=tests/old_test.rs::#[test] fn more() {}\\n\n\
                 FAKE: append=tests/new_test.rs::#[test] fn new() {}\\n\n";

/// `cahoots <args>` at a terminal, with the envelope on a piped stdout.
fn at(world: &World, args: &[&str]) -> Finished {
    world
        .at_terminal_with(args, &[("PATH", &world.path_with_git())])
        .finish()
}

fn message(after: &Finished) -> &str {
    after.json["message"].as_str().unwrap_or_default()
}

fn task_dir(world: &World, id: &str) -> PathBuf {
    world.data.join("evals/tasks").join(id)
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

/// Every path under `dir`, sorted.
fn tree(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            found.push(path);
        }
    }
    found.sort();
    found
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("not a list: {value}"))
        .iter()
        .map(|item| item.as_str().unwrap())
        .collect()
}

#[test]
fn an_accepted_writer_run_becomes_a_task() {
    let world = World::new();
    world.evals_fixture();
    let run = world.accepted_writer(B, &[]);
    let id = run.run_id();
    let config_before = tree(&world.config);

    let added = at(&world, &["evals", "add", &id]);
    assert_eq!(added.code, 0, "{}", added.json);
    let data = &added.json["data"];
    assert_eq!(data["task"], id.as_str());
    assert_eq!(data["run"], id.as_str());
    assert_eq!(data["v"], 1);
    assert_eq!(data["role"], "implement");
    assert!(data["kind"].is_null(), "{data}");
    assert_eq!(data["base_commit"], world.record(&id)["base_commit"]);
    assert_eq!(data["target"], world.record(&id)["target"]);
    assert_eq!(data["repo"], world.work.to_str().unwrap());
    let common = fs::canonicalize(world.work.join(".git")).unwrap();
    assert_eq!(data["git_common_dir"], common.to_str().unwrap());
    assert_eq!(
        strings(&data["hidden_tests"]),
        ["tests/old_test.rs", "tests/new_test.rs"]
    );
    assert_eq!(strings(&data["solution"]), ["src/lib.rs"]);

    let dir = task_dir(&world, &id);
    assert_eq!(data["path"], dir.to_str().unwrap());
    let on_disk: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("task.json")).unwrap()).unwrap();
    let mut without_path = data.clone();
    without_path.as_object_mut().unwrap().remove("path");
    assert_eq!(on_disk, without_path);
    assert_eq!(
        fs::read(dir.join("brief")).unwrap(),
        fs::read(world.run_file(&id, "brief")).unwrap()
    );
    assert_eq!(mode(&dir), 0o700);
    for file in ["task.json", "brief", "tests.diff", "solution.diff"] {
        assert_eq!(mode(&dir.join(file)), 0o600, "{file}");
    }
    assert_eq!(tree(&dir).len(), 4, "{:?}", tree(&dir));
    assert_eq!(tree(&world.config), config_before);
}

#[test]
fn the_hidden_tests_and_the_rest_rebuild_the_writers_change() {
    let world = World::new();
    world.evals_fixture();
    let run = world.accepted_writer(B, &[]);
    let id = run.run_id();
    let patch = fs::read(world.run_file(&id, "patch.diff")).unwrap();
    let added = at(&world, &["evals", "add", &id]);
    assert_eq!(added.code, 0, "{}", added.json);
    let dir = task_dir(&world, &id);
    let tests = fs::read(dir.join("tests.diff")).unwrap();
    let solution = fs::read(dir.join("solution.diff")).unwrap();
    assert_eq!(fs::read(world.run_file(&id, "patch.diff")).unwrap(), patch);

    // Every section of the patch, once, in one file or the other.
    let mut kept: Vec<Vec<u8>> = cahoots::patch::sections(&tests)
        .into_iter()
        .chain(cahoots::patch::sections(&solution))
        .map(|section| section.bytes.to_vec())
        .collect();
    let mut all: Vec<Vec<u8>> = cahoots::patch::sections(&patch)
        .into_iter()
        .map(|section| section.bytes.to_vec())
        .collect();
    kept.sort();
    all.sort();
    assert_eq!(kept, all);
    assert_eq!(tests.len() + solution.len(), patch.len());

    // At the base commit, the tests apply on their own, and the two
    // together are the writer's tree.
    let base = added.json["data"]["base_commit"].as_str().unwrap();
    let replay = world.root.join("replay");
    world.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        replay.to_str().unwrap(),
        base,
    ]);
    for (name, bytes) in [("tests.diff", &tests), ("solution.diff", &solution)] {
        fs::write(world.root.join(name), bytes).unwrap();
    }
    let tests_file = world.root.join("tests.diff");
    let solution_file = world.root.join("solution.diff");
    git_in(&replay, &["apply", "--check", tests_file.to_str().unwrap()]);
    git_in(&replay, &["apply", tests_file.to_str().unwrap()]);
    git_in(&replay, &["apply", solution_file.to_str().unwrap()]);
    let writer = PathBuf::from(run.data()["worktree"].as_str().unwrap());
    for file in ["src/lib.rs", "tests/old_test.rs", "tests/new_test.rs"] {
        assert_eq!(
            fs::read(replay.join(file)).unwrap(),
            fs::read(writer.join(file)).unwrap(),
            "{file}"
        );
    }
}

#[test]
fn a_kinds_run_keeps_its_kind() {
    let world = World::new();
    world.configure(
        "[kinds.fix]\ndescription = \"Fix a bug.\"\nrole = \"implement\"\ncandidates = [\n \
         { harness = \"codex\", model = \"custom\", effort = \"medium\" },\n]\n",
    );
    world.evals_fixture();
    let run = world.accepted_writer(B, &["--kind", "fix"]);
    let added = at(&world, &["evals", "add", &run.run_id()]);
    assert_eq!(added.code, 0, "{}", added.json);
    assert_eq!(added.json["data"]["kind"], "fix");
}

#[test]
fn a_run_with_no_test_file_has_no_hidden_tests() {
    let world = World::new();
    world.evals_fixture();
    let run = world.accepted_writer("FAKE: append=src/lib.rs::pub fn b() {}\\n\n", &[]);
    let id = run.run_id();
    let added = at(&world, &["evals", "add", &id]);
    assert_eq!(added.code, 0, "{}", added.json);
    assert_eq!(added.json["data"]["hidden_tests"], serde_json::json!([]));
    assert_eq!(strings(&added.json["data"]["solution"]), ["src/lib.rs"]);
    assert_eq!(
        fs::metadata(task_dir(&world, &id).join("tests.diff"))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn only_a_run_accepted_as_it_came_becomes_a_task() {
    let world = World::new();
    world.evals_fixture();
    let id = world.writer_in(&world.work, B, &[]).run_id();
    let refused = at(&world, &["evals", "add", &id]);
    assert_eq!(refused.code, 2, "{}", refused.json);
    assert!(message(&refused).contains("no outcome"), "{}", refused.json);
    for (outcome, said) in [("reworked", "reworked"), ("discarded", "discarded")] {
        assert_eq!(world.ask(&["outcome", &id, outcome]).code, 0);
        let refused = at(&world, &["evals", "add", &id]);
        assert_eq!(refused.code, 2, "{}", refused.json);
        assert!(message(&refused).contains(said), "{}", refused.json);
        assert!(!task_dir(&world, &id).exists());
    }
    assert_eq!(world.ask(&["outcome", &id, "accepted"]).code, 0);
    assert_eq!(at(&world, &["evals", "add", &id]).code, 0);
}

#[test]
fn a_readers_run_is_refused() {
    let world = World::new();
    let id = world.run("hello", &[]).run_id();
    assert_eq!(world.ask(&["outcome", &id, "accepted"]).code, 0);
    let refused = at(&world, &["evals", "add", &id]);
    assert_eq!(refused.code, 2, "{}", refused.json);
    assert!(
        message(&refused).contains("only a writer's run"),
        "{}",
        refused.json
    );
}

#[test]
fn an_in_place_writers_run_is_refused() {
    let world = World::new();
    world.configure("limits.allow_in_place = true");
    let brief = world.brief("FAKE: write=x.txt");
    let run = world.ask(&[
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--in-place",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(run.code, 0, "{}", run.json);
    let id = run.run_id();
    assert_eq!(world.ask(&["outcome", &id, "accepted"]).code, 0);
    let refused = at(&world, &["evals", "add", &id]);
    assert_eq!(refused.code, 2, "{}", refused.json);
    assert!(message(&refused).contains("in place"), "{}", refused.json);
}

#[test]
fn a_writer_that_changed_nothing_is_refused() {
    let world = World::new();
    let id = world.accepted_writer("hello", &[]).run_id();
    let refused = at(&world, &["evals", "add", &id]);
    assert_eq!(refused.code, 2, "{}", refused.json);
    assert!(
        message(&refused).contains("changed nothing"),
        "{}",
        refused.json
    );
}

#[test]
fn a_resumed_run_is_refused() {
    let world = World::new();
    world.evals_fixture();
    let first = world.writer_in(&world.work, "hello", &[]).run_id();
    let brief = world.brief(B);
    let again = world.ask(&[
        "resume",
        &first,
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(again.code, 0, "{}", again.json);
    let id = again.run_id();
    assert_eq!(world.ask(&["outcome", &id, "accepted"]).code, 0);
    let refused = at(&world, &["evals", "add", &id]);
    assert_eq!(refused.code, 2, "{}", refused.json);
    assert!(
        message(&refused).contains("continues run"),
        "{}",
        refused.json
    );
}

#[test]
fn an_unfinished_run_is_refused() {
    let world = World::new();
    let brief = world.brief("FAKE: sleep=30");
    let run = world.ask(&[
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--fork",
        "--wait",
        "0",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    let id = run.run_id();
    let refused = at(&world, &["evals", "add", &id]);
    assert_eq!(refused.code, 51, "{}", refused.json);
    assert!(
        message(&refused).contains("has not finished"),
        "{}",
        refused.json
    );
    world.ask(&["cancel", &id]);
}

#[test]
fn an_unknown_run_is_refused() {
    let world = World::new();
    let unknown = at(
        &world,
        &["evals", "add", "0198c0de-0000-7000-8000-000000000000"],
    );
    assert_eq!(unknown.code, 50, "{}", unknown.json);
    assert_eq!(
        message(&unknown),
        "no run 0198c0de-0000-7000-8000-000000000000"
    );
    let bad = at(&world, &["evals", "add", "../x"]);
    assert_eq!(bad.code, 2, "{}", bad.json);
}

#[test]
fn a_run_past_retention_is_refused() {
    let world = World::new();
    world.evals_fixture();
    let id = world.accepted_writer(B, &[]).run_id();
    world.age(&id, 8 * 24 * 3600);
    let refused = at(&world, &["evals", "add", &id]);
    assert_eq!(refused.code, 50, "{}", refused.json);
    assert!(message(&refused).contains("7 days"), "{}", refused.json);
    assert!(!world.state.join("runs").join(&id).exists());
    assert!(tree(&world.data.join("evals/tasks")).is_empty());
}

#[test]
fn a_run_already_in_the_suite_is_refused() {
    let world = World::new();
    world.evals_fixture();
    let id = world.accepted_writer(B, &[]).run_id();
    assert_eq!(at(&world, &["evals", "add", &id]).code, 0);
    let task_json = task_dir(&world, &id).join("task.json");
    let before = fs::read(&task_json).unwrap();
    let again = at(&world, &["evals", "add", &id]);
    assert_eq!(again.code, 2, "{}", again.json);
    assert!(
        message(&again).contains("already in the suite"),
        "{}",
        again.json
    );
    assert_eq!(fs::read(&task_json).unwrap(), before);
}

#[test]
fn list_shows_every_task_oldest_first() {
    let world = World::new();
    let empty = at(&world, &["evals", "list"]);
    assert_eq!(empty.code, 0, "{}", empty.json);
    assert_eq!(empty.json["data"], serde_json::json!({"tasks": []}));

    world.evals_fixture();
    let first = world.accepted_writer(B, &[]).run_id();
    let second = world.accepted_writer(B, &[]).run_id();
    // Added newest first: the list still goes by age.
    for id in [&second, &first] {
        assert_eq!(at(&world, &["evals", "add", id]).code, 0);
    }
    let listed = at(&world, &["evals", "list"]);
    assert_eq!(listed.code, 0, "{}", listed.json);
    let tasks = listed.json["data"]["tasks"].as_array().unwrap();
    let ids: Vec<&str> = tasks.iter().map(|t| t["task"].as_str().unwrap()).collect();
    assert_eq!(ids, [first.as_str(), second.as_str()]);
    for task in tasks {
        assert_eq!(task["rot"], "none", "{task}");
        assert!(task.get("rot_error").is_none(), "{task}");
        let dir = task_dir(&world, task["task"].as_str().unwrap());
        assert_eq!(task["path"], dir.to_str().unwrap());
        let on_disk: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("task.json")).unwrap()).unwrap();
        for (key, value) in on_disk.as_object().unwrap() {
            assert_eq!(&task[key], value, "{key}");
        }
    }
}

#[test]
fn a_task_whose_base_commit_is_gone_is_flagged_and_such_a_run_is_refused() {
    let world = World::new();
    world.evals_fixture();
    world.git(&["switch", "-q", "-c", "scratch"]);
    fs::write(world.work.join("f.txt"), "scratch\n").unwrap();
    world.git(&["add", "f.txt"]);
    world.git(&["commit", "-q", "-m", "chore: scratch"]);
    let a = world.accepted_writer(B, &[]);
    let b = world.accepted_writer(B, &[]);
    assert_eq!(at(&world, &["evals", "add", &a.run_id()]).code, 0);
    world.lose_the_scratch_commit(&[&a, &b]);

    let listed = at(&world, &["evals", "list"]);
    assert_eq!(listed.code, 0, "{}", listed.json);
    assert_eq!(
        listed.json["data"]["tasks"][0]["rot"], "commit_gone",
        "{}",
        listed.json
    );
    let refused = at(&world, &["evals", "add", &b.run_id()]);
    assert_eq!(refused.code, 2, "{}", refused.json);
    assert!(
        message(&refused).contains("no longer in"),
        "{}",
        refused.json
    );
}

/// A second repository beside the world's, with a commit holding a test.
fn other_repository(world: &World) -> PathBuf {
    let other = world.root.join("other");
    fs::create_dir_all(other.join("tests")).unwrap();
    git_in(&other, &["init", "-q", "-b", "main"]);
    git_in(&other, &["config", "user.name", "World"]);
    git_in(&other, &["config", "user.email", "world@example.invalid"]);
    git_in(&other, &["config", "commit.gpgsign", "false"]);
    fs::write(other.join("tests/old_test.rs"), "#[test] fn old() {}\n").unwrap();
    git_in(&other, &["add", "--", "tests/old_test.rs"]);
    git_in(&other, &["commit", "-q", "-m", "chore: a test"]);
    other
}

#[test]
fn a_task_whose_repository_is_gone_is_flagged() {
    let world = World::new();
    let other = other_repository(&world);
    let run = world.accepted_writer_in(
        &other,
        "FAKE: append=tests/old_test.rs::#[test] fn more() {}\\n\n",
        &[],
    );
    assert_eq!(at(&world, &["evals", "add", &run.run_id()]).code, 0);
    fs::remove_dir_all(&other).unwrap();
    let listed = at(&world, &["evals", "list"]);
    assert_eq!(listed.code, 0, "{}", listed.json);
    assert_eq!(
        listed.json["data"]["tasks"][0]["rot"], "repository_gone",
        "{}",
        listed.json
    );
}

#[test]
fn a_task_from_a_linked_worktree_outlives_the_worktree() {
    let world = World::new();
    world.evals_fixture();
    let linked = world.root.join("linked");
    world.git(&[
        "worktree",
        "add",
        "-q",
        linked.to_str().unwrap(),
        "-b",
        "side",
    ]);
    let run = world.accepted_writer_in(&linked, B, &[]);
    let added = at(&world, &["evals", "add", &run.run_id()]);
    assert_eq!(added.code, 0, "{}", added.json);
    let common = fs::canonicalize(world.work.join(".git")).unwrap();
    assert_eq!(
        added.json["data"]["git_common_dir"],
        common.to_str().unwrap()
    );
    assert_eq!(added.json["data"]["repo"], linked.to_str().unwrap());
    world.git(&["worktree", "remove", "--force", linked.to_str().unwrap()]);
    let listed = at(&world, &["evals", "list"]);
    assert_eq!(
        listed.json["data"]["tasks"][0]["rot"], "none",
        "{}",
        listed.json
    );
}

#[test]
fn remove_takes_a_task_out() {
    let world = World::new();
    world.evals_fixture();
    let id = world.accepted_writer(B, &[]).run_id();
    assert_eq!(at(&world, &["evals", "add", &id]).code, 0);
    let removed = at(&world, &["evals", "remove", &id]);
    assert_eq!(removed.code, 0, "{}", removed.json);
    assert_eq!(
        removed.json["data"],
        serde_json::json!({"task": id, "removed": true})
    );
    assert!(!task_dir(&world, &id).exists());
    assert!(tree(&world.data.join("evals/tasks")).is_empty());
    assert_eq!(
        at(&world, &["evals", "list"]).json["data"]["tasks"],
        serde_json::json!([])
    );
    let again = at(&world, &["evals", "remove", &id]);
    assert_eq!(again.code, 50, "{}", again.json);
    assert_eq!(message(&again), format!("no task {id} in the suite"));
    assert_eq!(at(&world, &["evals", "remove", "../x"]).code, 2);
}

#[test]
fn remove_does_not_follow_a_link() {
    let world = World::new();
    let elsewhere = world.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("keep.txt"), "kept\n").unwrap();
    let tasks = world.data.join("evals/tasks");
    fs::create_dir_all(&tasks).unwrap();
    let id = "0198c0de-0000-7000-8000-000000000001";
    std::os::unix::fs::symlink(&elsewhere, tasks.join(id)).unwrap();
    let refused = at(&world, &["evals", "remove", id]);
    assert_eq!(refused.code, 50, "{}", refused.json);
    assert!(fs::symlink_metadata(tasks.join(id)).is_ok());
    assert!(elsewhere.join("keep.txt").exists());
    assert_eq!(
        at(&world, &["evals", "list"]).json["data"]["tasks"],
        serde_json::json!([])
    );
}

#[test]
fn a_tampered_task_cannot_put_text_into_gits_argv() {
    let world = World::new();
    world.evals_fixture();
    let id = world.accepted_writer(B, &[]).run_id();
    assert_eq!(at(&world, &["evals", "add", &id]).code, 0);
    let task_json = task_dir(&world, &id).join("task.json");
    let mut task: Value = serde_json::from_str(&fs::read_to_string(&task_json).unwrap()).unwrap();
    let planted = world.root.join("planted");
    task["base_commit"] = format!("--output={}", planted.display()).into();
    fs::write(&task_json, task.to_string()).unwrap();
    let listed = at(&world, &["evals", "list"]);
    assert_eq!(listed.code, 0, "{}", listed.json);
    assert_eq!(listed.json["data"]["tasks"], serde_json::json!([]));
    assert!(!planted.exists());
    assert_eq!(at(&world, &["evals", "remove", &id]).code, 0);
}

#[test]
fn the_evals_verbs_need_a_terminal() {
    let world = World::new();
    let id = "0198c0de-0000-7000-8000-000000000000";
    for args in [
        vec!["evals", "list"],
        vec!["evals", "add", id],
        vec!["evals", "remove", id],
    ] {
        let refused = world.ask(&args);
        assert_eq!(refused.code, 33, "{args:?}: {}", refused.json);
        assert_eq!(
            refused.message(),
            "this verb changes what cahoots may do, so it only runs from a terminal"
        );
    }
    assert!(tree(&world.data).is_empty());
}

#[test]
fn a_task_that_is_not_what_it_says_is_not_listed_and_remove_still_clears_it() {
    let world = World::new();
    world.evals_fixture();
    let linked = world.accepted_writer(B, &[]).run_id();
    let renamed = world.accepted_writer(B, &[]).run_id();
    for id in [&linked, &renamed] {
        assert_eq!(at(&world, &["evals", "add", id]).code, 0);
    }
    // A task.json that is a link to a valid task file elsewhere.
    let file = task_dir(&world, &linked).join("task.json");
    let elsewhere = world.root.join("elsewhere.json");
    fs::rename(&file, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &file).unwrap();
    // A task.json that names another task.
    let file = task_dir(&world, &renamed).join("task.json");
    let mut task: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    task["task"] = linked.as_str().into();
    fs::write(&file, task.to_string()).unwrap();

    let listed = at(&world, &["evals", "list"]);
    assert_eq!(listed.code, 0, "{}", listed.json);
    assert_eq!(listed.json["data"]["tasks"], serde_json::json!([]));
    for id in [&linked, &renamed] {
        let removed = at(&world, &["evals", "remove", id]);
        assert_eq!(removed.code, 0, "{}", removed.json);
        assert!(!task_dir(&world, id).exists());
    }
    assert!(elsewhere.exists(), "the link's target is not removed");
}
