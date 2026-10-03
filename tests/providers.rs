//! What cuts a writer's worktree: the provider a person chose in config.toml
//! (`fork.provider`), never a flag and never the repository. git by default;
//! daft only where it is chosen and the repository has a `daft.yml`, and
//! then only the daft a person pinned (`fork.daft.binary`) — a chosen daft
//! that cannot be run fails the run, and never falls back to git. The run
//! records which cut its worktree, and says who removes it
//! (`worktree_owner`). Every `daft` here is the fake (`World::daft`).

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{Answer, World, alive, fake_at, path_str, wait_until};
use serde_json::{Value, json};

/// What every writer here is asked to do, so a test can tell where it ran.
const BRIEF: &str = "FAKE: write=callee-ran.txt";

fn fork(world: &World, extra: &[&str]) -> Answer {
    let brief = world.brief(BRIEF);
    let mut args = vec![
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--fork",
        "--brief",
        brief.to_str().unwrap(),
    ];
    args.extend(extra);
    world.ask(&args)
}

/// The fork was refused before the writer started: it ran neither in the
/// caller's tree nor at `printed`, no worktree is reported, and none is
/// owned.
fn assert_never_ran(world: &World, answer: &Answer, printed: &Path) {
    let data = answer.data();
    assert_eq!(data["state"], "failed", "{}", answer.json);
    assert!(data.get("worktree").is_none(), "{}", answer.json);
    assert!(data["worktree_owner"].is_null(), "{}", answer.json);
    let record = world.record(&answer.run_id());
    assert!(record["callee_pid"].is_null());
    assert!(record["worktree_provider"].is_null());
    assert!(
        !world.work.join("callee-ran.txt").exists(),
        "the writer ran in the caller's tree"
    );
    assert!(
        !printed.join("callee-ran.txt").exists(),
        "the writer ran in {}",
        printed.display()
    );
}

/// Nothing was cut under cahoots' own `worktrees`: no git fallback.
fn assert_no_git_cut(world: &World) {
    let worktrees = world.state.join("worktrees");
    assert!(
        !worktrees.exists() || fs::read_dir(&worktrees).unwrap().next().is_none(),
        "git cut a worktree after all"
    );
}

/// `fork.provider = "daft"`, with the daft at `binary` and the hooks as said.
fn chose_daft(world: &World, binary: &Path, hooks: bool) {
    world.fork(&format!(
        "fork.provider = \"daft\"\nfork.daft.binary = {binary:?}\nfork.daft.hooks = {hooks}"
    ));
}

fn pid_in(path: &Path) -> i64 {
    fs::read_to_string(path).unwrap().trim().parse().unwrap()
}

#[test]
fn git_cuts_by_default_even_where_there_is_a_daft_yml() {
    let world = World::new();
    world.daft(json!({"make": "worktree", "print": world.root.join("forks/f1")}));
    world.fork("fork.provider = \"git\"");
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let worktree = world.state.join("worktrees").join(answer.run_id());
    assert_eq!(answer.data()["worktree"], path_str(&worktree));
    assert!(worktree.join("callee-ran.txt").is_file());
    assert!(world.daft_calls().is_empty(), "daft cut");
    assert!(
        world.daft_versions().is_empty(),
        "daft was asked its version"
    );
    assert_eq!(answer.data()["worktree_owner"], "cahoots");
    assert_eq!(world.record(&answer.run_id())["worktree_provider"], "git");

    // With nothing in config.toml at all, the same.
    world.fork("");
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["worktree_owner"], "cahoots");
    assert!(world.daft_calls().is_empty(), "daft cut");
}

#[test]
fn daft_cuts_where_it_is_chosen_and_the_repository_has_a_daft_yml() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"make": "worktree", "print": f1}));
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["worktree"], path_str(&f1));
    assert!(f1.join("callee-ran.txt").is_file());
    assert_eq!(answer.data()["worktree_owner"], "daft");
    assert_eq!(world.record(&answer.run_id())["worktree_provider"], "daft");
    assert_eq!(world.daft_calls().len(), 1);
    assert_eq!(world.daft_versions().len(), 1, "asked its version once");
    assert_no_git_cut(&world);
}

#[test]
fn daft_chosen_without_a_daft_yml_cuts_with_git() {
    let world = World::new();
    world.daft(json!({"make": "worktree", "print": world.root.join("forks/f1")}));
    fs::remove_file(world.work.join("daft.yml")).unwrap();
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["worktree_owner"], "cahoots");
    assert_eq!(
        answer.data()["worktree"],
        path_str(&world.state.join("worktrees").join(answer.run_id()))
    );
    assert!(world.daft_calls().is_empty(), "daft cut");
    assert!(
        world.daft_versions().is_empty(),
        "daft was asked its version"
    );
}

#[test]
fn daft_chosen_with_none_pinned_is_refused_and_never_falls_back() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    // The fake is first on PATH, and would cut if it were run.
    world.daft(json!({"make": "worktree", "print": f1}));
    world.fork("fork.provider = \"daft\"");
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 34, "{}", answer.json);
    assert_eq!(
        answer.message(),
        "cahoots needs `daft`: none is chosen — fork.provider is \"daft\" and this repository \
         has a daft.yml; choose one with fork.daft.binary (`cahoots settings`)"
    );
    assert_never_ran(&world, &answer, &f1);
    assert_no_git_cut(&world);
    assert!(world.daft_calls().is_empty(), "the daft on PATH ran");
    assert!(
        world.daft_versions().is_empty(),
        "the daft on PATH was asked its version"
    );
}

#[test]
fn a_pinned_daft_that_is_not_there_fails_the_run() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"make": "worktree", "print": f1}));
    let nowhere = world.root.join("nowhere/daft");
    chose_daft(&world, &nowhere, false);
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 34, "{}", answer.json);
    assert!(
        answer
            .message()
            .starts_with(&format!("cahoots needs `daft`: {}", nowhere.display())),
        "{}",
        answer.json
    );
    assert_never_ran(&world, &answer, &f1);
    assert_no_git_cut(&world);
    assert!(world.daft_calls().is_empty());
}

#[test]
fn a_daft_that_does_not_identify_itself_fails_the_run() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"make": "worktree", "print": f1}));
    fs::write(world.bin.join("daft.version"), "not-daft 9.9.9\n").unwrap();
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 34, "{}", answer.json);
    assert_eq!(
        answer.message(),
        format!(
            "cahoots needs `daft`: {} does not identify itself as daft",
            world.bin.join("daft").display()
        )
    );
    assert_never_ran(&world, &answer, &f1);
    assert!(world.daft_calls().is_empty(), "it cut after all");
    assert_eq!(world.daft_versions().len(), 1);
}

#[test]
fn a_daft_older_than_tested_fails_the_run() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"make": "worktree", "print": f1}));
    fs::write(world.bin.join("daft.version"), "daft 1.20.0\n").unwrap();
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 34, "{}", answer.json);
    assert_eq!(
        answer.message(),
        "cahoots needs `daft`: daft 1.20.0 is older than the oldest version cahoots supports \
         (1.27.0)"
    );
    assert_never_ran(&world, &answer, &f1);
    assert!(world.daft_calls().is_empty(), "it cut after all");
}

#[test]
fn the_pinned_daft_is_the_one_run() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    // `bin/daft` stays first on PATH; the one config.toml names is elsewhere.
    world.daft(json!({"make": "worktree", "print": f1}));
    let pinned = world.root.join("pinned/daft");
    fs::create_dir_all(pinned.parent().unwrap()).unwrap();
    fake_at(&pinned);
    fs::write(
        world.root.join("pinned/daft.plan"),
        json!({"make": "worktree", "print": f1}).to_string(),
    )
    .unwrap();
    chose_daft(&world, &pinned, false);
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["worktree"], path_str(&f1));
    let calls = fs::read_to_string(world.root.join("pinned/daft.calls")).unwrap();
    assert_eq!(calls.lines().count(), 1, "{calls}");
    assert!(world.daft_calls().is_empty(), "the daft on PATH cut");
    assert!(
        world.daft_versions().is_empty(),
        "the daft on PATH was asked its version"
    );
}

#[test]
fn daft_hooks_on_hands_daft_its_hooks_in_the_foreground() {
    let world = World::new();
    world.hook_that_marks(&world.work.join(".git/hooks"));
    // The control: a worktree cut by hand runs the repository's hook.
    world.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        path_str(&world.root.join("control")),
        "HEAD",
    ]);
    assert!(world.hook_ran(), "the control never ran the hook");
    world.clear_hook_mark();

    let f1 = world.root.join("forks/f1");
    let daft = world.daft(json!({"make": "worktree", "print": f1}));
    chose_daft(&world, &daft, true);
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["worktree_owner"], "daft");
    let calls = world.daft_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        calls[0]["argv"],
        json!([
            "-C",
            path_str(&world.work),
            "start",
            "--fork",
            "--no-cd",
            "--hooks",
            "foreground",
            "--no-carry",
            answer.data()["base_commit"]
                .as_str()
                .expect("a base commit")
        ])
    );
    // daft's hooks are on; git's are not: the git daft runs is still told
    // its hooks are in an empty directory, and that there is no fsmonitor.
    let config = &calls[0]["git_config"];
    assert_eq!(config["GIT_CONFIG_KEY_0"], "core.hooksPath");
    let hooks = Path::new(config["GIT_CONFIG_VALUE_0"].as_str().unwrap());
    assert!(
        hooks.starts_with(&world.state)
            && hooks.is_dir()
            && fs::read_dir(hooks).unwrap().next().is_none(),
        "{}",
        hooks.display()
    );
    assert_eq!(config["GIT_CONFIG_KEY_1"], "core.fsmonitor");
    assert_eq!(config["GIT_CONFIG_VALUE_1"], "false");
    assert!(!world.hook_ran(), "a git hook of the repository's ran");
}

#[test]
fn daft_hooks_on_refuses_a_repository_whose_config_steers_them() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    let daft = world.daft(json!({"make": "worktree", "print": f1}));
    chose_daft(&world, &daft, true);
    world.git(&[
        "config",
        "daft.hooks.userDirectory",
        path_str(&world.work.join("h")),
    ]);
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 33, "{}", answer.json);
    // As git lists it: the section and the name lowercased.
    assert_eq!(
        answer.message(),
        "cannot cut a worktree: the repository's own git configuration sets \
         daft.hooks.userdirectory — with fork.daft.hooks on, it would choose what daft runs"
    );
    assert_never_ran(&world, &answer, &f1);
    assert!(world.daft_calls().is_empty(), "daft ran");

    // The control: with daft's hooks off, the key is moot, and daft cuts.
    chose_daft(&world, &daft, false);
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(world.daft_calls().len(), 1);
}

/// `cahoots <args>` from `dir`.
fn ask_from(world: &World, dir: &Path, args: &[&str]) -> Answer {
    let mut command = world.cahoots();
    command.current_dir(dir).args(args);
    common::answer(&mut command)
}

#[test]
fn the_worktree_owner_is_on_the_record_and_in_every_envelope() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"make": "worktree", "print": f1}));
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let id = answer.run_id();
    assert_eq!(answer.data()["worktree_owner"], "daft");
    assert_eq!(world.record(&id)["worktree_provider"], "daft");
    for verb in ["status", "result", "wait", "cancel"] {
        let told = world.ask(&[verb, &id]);
        assert_eq!(
            told.data()["worktree_owner"],
            "daft",
            "{verb}: {}",
            told.json
        );
    }
    // The list shows the runs under where it is asked: the fork works in
    // forks/f1, beside the repository.
    let listed = ask_from(&world, &world.root, &["status"]);
    let entry = listed.data()["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["run"] == id.as_str())
        .unwrap_or_else(|| panic!("{}", listed.json))
        .clone();
    assert_eq!(entry["worktree_owner"], "daft");

    // Nothing was cut for a reader, or for a writer in place.
    let advise = world.run("hello", &[]);
    assert_eq!(advise.code, 0, "{}", advise.json);
    assert_eq!(advise.data()["worktree_owner"], Value::Null);
    world.configure("limits.allow_in_place = true");
    let brief = world.brief("FAKE: write=in-place.txt");
    let in_place = world.ask(&[
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--in-place",
        "--brief",
        path_str(&brief),
    ]);
    assert_eq!(in_place.code, 0, "{}", in_place.json);
    assert_eq!(in_place.data()["worktree_owner"], Value::Null);
    assert!(world.record(&in_place.run_id())["worktree_provider"].is_null());
}

#[test]
fn a_blind_run_shows_its_worktree_owner() {
    let world = World::new();
    world.configure("review.blind = true");
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["blind"], true, "{}", answer.json);
    assert_eq!(answer.data()["worktree_owner"], "cahoots");
}

#[test]
fn a_fork_whose_cut_failed_has_no_owner() {
    let world = World::new();
    world.daft(json!({"make": "nothing"}));
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 40, "{}", answer.json);
    assert!(
        answer.message().contains("printed no path"),
        "{}",
        answer.json
    );
    assert_never_ran(&world, &answer, &world.root.join("forks"));
}

#[test]
fn a_resumed_fork_keeps_its_owner() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"make": "worktree", "print": f1}));
    let first = fork(&world, &[]);
    assert_eq!(first.code, 0, "{}", first.json);
    let brief = world.brief("FAKE: say=again");
    let resumed = world.ask(&[
        "resume",
        &first.run_id(),
        "--caller",
        "claude",
        "--brief",
        path_str(&brief),
    ]);
    assert_eq!(resumed.code, 0, "{}", resumed.json);
    assert_eq!(resumed.data()["worktree"], path_str(&f1));
    assert_eq!(resumed.data()["worktree_owner"], "daft");
    assert_eq!(world.record(&resumed.run_id())["worktree_provider"], "daft");
    assert_eq!(world.daft_calls().len(), 1, "a resume cut again");
}

#[test]
fn a_worktree_daft_cut_is_never_removed_by_cahoots() {
    let world = World::new();
    // Under cahoots' own `worktrees`, which the path check alone would take
    // for one of cahoots'.
    let f2 = world.state.join("worktrees/f2");
    world.daft(json!({"make": "worktree", "print": f2}));
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["worktree"], path_str(&f2));
    let id = answer.run_id();
    world.age(&id, 8 * 24 * 3600);
    world.ask(&["status"]);
    assert!(
        !world.state.join("runs").join(&id).exists(),
        "the run did not age out"
    );
    assert!(f2.is_dir(), "cahoots removed a worktree daft cut");

    // The control: an aged git fork's worktree goes.
    world.fork("fork.provider = \"git\"");
    let git = fork(&world, &[]);
    assert_eq!(git.code, 0, "{}", git.json);
    let worktree = world.state.join("worktrees").join(git.run_id());
    assert!(worktree.is_dir());
    world.age(&git.run_id(), 8 * 24 * 3600);
    world.ask(&["status"]);
    assert!(!worktree.exists(), "an aged git fork's worktree was kept");
    assert!(f2.is_dir());
}

#[test]
fn a_cut_that_leaves_a_process_behind_still_returns_and_the_process_is_killed() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"make": "worktree", "print": f1, "linger": 120}));
    let started = Instant::now();
    let answer = fork(&world, &[]);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "it waited {:?}",
        started.elapsed()
    );
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["worktree"], path_str(&f1));
    let lingered = pid_in(&world.bin.join("daft.linger.pid"));
    wait_until("what daft left behind is gone", || !alive(lingered));
}

#[test]
fn a_process_that_left_the_group_holding_the_output_fails_the_cut_without_a_wait() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"make": "worktree", "print": f1, "escape": 120}));
    let started = Instant::now();
    let answer = fork(&world, &[]);
    let elapsed = started.elapsed();
    // Out of the cut's reach by design, so this test stops it.
    let escaped = pid_in(&world.bin.join("daft.escape.pid"));
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(escaped as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
    assert!(elapsed < Duration::from_secs(30), "it waited {elapsed:?}");
    assert_eq!(answer.code, 40, "{}", answer.json);
    assert_eq!(
        answer.message(),
        "cannot cut a worktree: `daft start --fork` left a process holding its output"
    );
    assert_never_ran(&world, &answer, &f1);
}

#[test]
fn a_cut_past_its_deadline_kills_what_the_provider_started() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"sleep": 120, "linger": 120, "print": f1}));
    let started = Instant::now();
    let answer = fork(&world, &["--timeout", "2"]);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "it waited {:?}",
        started.elapsed()
    );
    assert_eq!(answer.code, 40, "{}", answer.json);
    assert!(
        answer.message().contains("did not finish within 2s"),
        "{}",
        answer.json
    );
    assert_never_ran(&world, &answer, &f1);
    let daft = pid_in(&world.bin.join("daft.pid"));
    let lingered = pid_in(&world.bin.join("daft.linger.pid"));
    wait_until("daft and what it started are gone", || {
        !alive(daft) && !alive(lingered)
    });
}

#[test]
fn a_provider_asked_its_version_finds_nothing_in_the_workspace() {
    let world = World::new();
    world.daft(json!({"make": "worktree", "print": world.root.join("forks/f1")}));
    let work_bin = world.work.join("bin");
    fs::create_dir_all(&work_bin).unwrap();
    fs::write(work_bin.join("README"), "not a tool\n").unwrap();
    world.prefix_path(&work_bin);
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let versions = world.daft_versions();
    assert_eq!(versions.len(), 1, "{versions:?}");
    let path = versions[0]["path"].as_str().expect("it was given a PATH");
    assert!(
        !path
            .split(':')
            .any(|entry| Path::new(entry).starts_with(&world.work)),
        "{path}"
    );
    assert_eq!(
        versions[0]["cwd"], "/",
        "asked where no repository can reach it"
    );
}

/// The `fork` check of a doctor's answer.
fn fork_check(answer: &Value) -> Value {
    answer["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "fork")
        .unwrap_or_else(|| panic!("no fork check: {answer}"))
        .clone()
}

#[test]
fn doctor_suggests_daft_where_the_repository_has_a_daft_yml() {
    let world = World::new();
    world.daft(json!({"make": "worktree", "print": world.root.join("forks/f1")}));
    world.fork("fork.provider = \"git\"");
    let doctor = world.ask(&["doctor"]);
    assert_eq!(doctor.code, 0, "{}", doctor.json);
    let check = fork_check(&doctor.json);
    assert_eq!(check["status"], "warn", "{check}");
    let detail = check["detail"].as_str().unwrap();
    assert!(
        detail.contains("this repository has a daft.yml")
            && detail.contains("fork.provider")
            && detail.contains("cahoots settings"),
        "{detail}"
    );

    fs::remove_file(world.work.join("daft.yml")).unwrap();
    let doctor = world.ask(&["doctor"]);
    assert_eq!(doctor.code, 0, "{}", doctor.json);
    let check = fork_check(&doctor.json);
    assert_eq!(check["status"], "ok", "{check}");
    let detail = check["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("git ") && detail.ends_with("(fork.provider = \"git\")"),
        "{detail}"
    );
    assert!(world.daft_versions().is_empty(), "doctor asked daft");

    // With no git at all, the provider's binary is missing: a failure.
    let after = world.with_stdin_elsewhere(&["doctor"]).finish();
    assert_eq!(after.code, 34, "{}", after.text());
    let json: Value = serde_json::from_str(after.text().trim()).unwrap();
    let check = fork_check(&json);
    assert_eq!(check["status"], "fail", "{check}");
    assert_eq!(
        check["detail"],
        "no git on PATH — cahoots cannot see a repository without one, so --fork is refused \
         until git is there"
    );
}

#[test]
fn doctor_names_the_daft_it_would_run_and_fails_without_one() {
    let world = World::new();
    let daft = world.daft(json!({"make": "worktree", "print": world.root.join("forks/f1")}));
    let doctor = world.ask(&["doctor"]);
    assert_eq!(doctor.code, 0, "{}", doctor.json);
    let check = fork_check(&doctor.json);
    assert_eq!(check["status"], "ok", "{check}");
    assert_eq!(
        check["detail"],
        format!(
            "daft 1.27.9 ({}) cuts each writer's worktree where the repository has a daft.yml, \
             git elsewhere; daft's hooks are off",
            daft.display()
        )
    );
    assert!(world.daft_calls().is_empty(), "doctor cut");

    chose_daft(&world, &daft, true);
    let doctor = world.ask(&["doctor"]);
    assert_eq!(doctor.code, 0, "{}", doctor.json);
    let check = fork_check(&doctor.json);
    assert_eq!(check["status"], "warn", "{check}");
    assert!(
        check["detail"]
            .as_str()
            .unwrap()
            .ends_with("daft runs the repository's hooks in each new worktree, when daft trusts the repository (fork.daft.hooks = on)"),
        "{check}"
    );

    fs::write(world.bin.join("daft.version"), "daft 1.20.0\n").unwrap();
    let doctor = world.ask(&["doctor"]);
    assert_eq!(doctor.code, 34, "{}", doctor.json);
    assert_eq!(fork_check(&doctor.json)["status"], "fail");

    world.fork("fork.provider = \"daft\"");
    let doctor = world.ask(&["doctor"]);
    assert_eq!(doctor.code, 34, "{}", doctor.json);
    let check = fork_check(&doctor.json);
    assert_eq!(check["status"], "fail", "{check}");
    assert!(
        check["detail"].as_str().unwrap().contains("none is chosen"),
        "{check}"
    );
}

#[test]
fn doctor_never_asks_a_git_the_workspace_supplies() {
    let world = World::new();
    let marker = world.root.join("planted-git-ran");
    world.script_at(
        &world.work.join("bin/git"),
        &format!(
            "#!/bin/sh\ntouch '{}'\necho 'git version 2.50.0'\n",
            marker.display()
        ),
    );
    world.prefix_path(&world.work.join("bin"));
    let doctor = world.ask(&["doctor"]);
    assert_eq!(doctor.code, 34, "{}", doctor.json);
    let check = fork_check(&doctor.json);
    assert_eq!(check["status"], "fail", "{check}");
    assert!(
        check["detail"]
            .as_str()
            .unwrap()
            .starts_with("refusing to run `git`: "),
        "{check}"
    );
    assert!(!marker.exists(), "the planted git ran");
}
