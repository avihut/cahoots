//! Every run records the commit it started from, and a fork writer's patch
//! is kept in its run directory, summed up in the history — which outlives
//! the patch. The patch is git's, against the pinned git directory, shaped by
//! nothing in anyone's git config, and taken only after the writer's process
//! group is gone.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Answer, World, git_in, path_str};
use serde_json::{Value, json};

/// Writes `files` in the world's repository and commits them; returns HEAD.
fn commit_files(world: &World, files: &[(&str, &[u8])]) -> String {
    let mut add = vec!["add", "--"];
    for (name, bytes) in files {
        let path = world.work.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        add.push(name);
    }
    world.git(&add);
    world.git(&["commit", "-q", "-m", "chore: fixtures"]);
    head(&world.work)
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

/// The history's `finished` event for `run`, once it is there: the
/// supervisor publishes the terminal record first and appends the line after.
fn history_line(world: &World, run: &str) -> Value {
    let mut found = None;
    common::wait_until("the finished history event", || {
        found = fs::read_to_string(world.state.join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|event| event["kind"] == "finished" && event["run"] == run);
        found.is_some()
    });
    found.unwrap()
}

fn fork(world: &World, brief: &str, extra: &[&str]) -> Answer {
    let brief = world.brief(brief);
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

fn worktree(answer: &Answer) -> PathBuf {
    PathBuf::from(
        answer.data()["worktree"]
            .as_str()
            .unwrap_or_else(|| panic!("no worktree: {}", answer.json)),
    )
}

fn paths(answer: &Answer) -> Vec<String> {
    answer.data()["patch"]["files"]
        .as_array()
        .unwrap_or_else(|| panic!("no patch: {}", answer.json))
        .iter()
        .map(|file| file["path"].as_str().unwrap().to_string())
        .collect()
}

fn file<'a>(answer: &'a Answer, path: &str) -> &'a Value {
    answer.data()["patch"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == path)
        .unwrap_or_else(|| panic!("{path} is not in the patch: {}", answer.json))
}

fn counts(answer: &Answer, path: &str) -> (u64, u64) {
    let file = file(answer, path);
    (
        file["added"].as_u64().unwrap(),
        file["removed"].as_u64().unwrap(),
    )
}

#[test]
fn a_run_records_the_commit_it_started_from() {
    let world = World::new();
    let answer = world.run("hello", &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let id = answer.run_id();
    let sha = head(&world.work);
    assert_eq!(answer.data()["base_commit"], sha.as_str());
    for verb in ["status", "result"] {
        let later = world.ask(&[verb, &id]);
        assert_eq!(later.data()["base_commit"], sha.as_str(), "{verb}");
    }
    assert_eq!(world.record(&id)["base_commit"], sha.as_str());
    assert_eq!(history_line(&world, &id)["base_commit"], sha.as_str());
    assert!(answer.data()["patch"].is_null(), "{}", answer.json);
    assert!(!world.run_file(&id, "patch.diff").exists());
    let all = world.ask(&["status"]);
    assert_eq!(all.data()["runs"][0]["base_commit"], sha.as_str());
}

/// A reader run from `dir`, with its brief there.
fn advise_in(world: &World, dir: &Path) -> Answer {
    let brief = dir.join("brief.md");
    fs::write(&brief, "hello").unwrap();
    let mut command = world.cahoots();
    command.current_dir(dir).args([
        "run",
        "--role",
        "advise",
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    common::answer(&mut command)
}

#[test]
fn outside_a_repository_there_is_no_base_commit() {
    let world = World::new();
    let elsewhere = tempfile::tempdir().unwrap();
    let answer = advise_in(&world, elsewhere.path());
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(answer.data()["base_commit"].is_null(), "{}", answer.json);
    let line = history_line(&world, &answer.run_id());
    assert!(line.get("base_commit").is_none(), "{line}");
    assert!(line.get("patch").is_none(), "{line}");
}

#[test]
fn a_repository_with_no_commit_has_no_base_commit() {
    let world = World::new();
    let fresh = tempfile::tempdir().unwrap();
    git_in(fresh.path(), &["init", "-q"]);
    let answer = advise_in(&world, fresh.path());
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(answer.data()["base_commit"].is_null(), "{}", answer.json);
}

#[test]
fn a_fork_is_cut_at_the_base_commit() {
    let world = World::new();
    let first = fork(&world, "FAKE: write=x.txt", &[]);
    assert_eq!(first.code, 0, "{}", first.json);
    let sha = head(&world.work);
    assert_eq!(first.data()["base_commit"], sha.as_str());
    assert_eq!(head(&worktree(&first)), sha);

    let newer = commit_files(&world, &[("later.txt", b"later\n")]);
    assert_ne!(newer, sha);
    let second = fork(&world, "FAKE: write=x.txt", &[]);
    assert_eq!(second.code, 0, "{}", second.json);
    assert_eq!(second.data()["base_commit"], newer.as_str());
    assert_eq!(head(&worktree(&second)), newer);
}

#[test]
fn a_writers_patch_replays_at_its_base_commit() {
    let world = World::new();
    let base = commit_files(
        &world,
        &[("keep.txt", b"kept\n"), ("gone.txt", b"one\ntwo\nthree\n")],
    );
    let answer = fork(
        &world,
        "FAKE: append=keep.txt::edited by the callee\\n\nFAKE: remove=gone.txt\nFAKE: write=new.txt\nFAKE: bytes=latin1.txt",
        &[],
    );
    assert_eq!(answer.code, 0, "{}", answer.json);
    let id = answer.run_id();
    assert_eq!(answer.data()["base_commit"], base.as_str());
    let patch = world.run_file(&id, "patch.diff");
    assert_eq!(
        fs::metadata(&patch).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let replay = world.root.join("replay");
    world.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        path_str(&replay),
        &base,
    ]);
    git_in(&replay, &["apply", "--check", path_str(&patch)]);
    git_in(&replay, &["apply", path_str(&patch)]);
    let writer = worktree(&answer);
    for name in ["keep.txt", "new.txt", "latin1.txt"] {
        assert_eq!(
            fs::read(replay.join(name)).unwrap(),
            fs::read(writer.join(name)).unwrap(),
            "{name}"
        );
    }
    assert!(!replay.join("gone.txt").exists());

    let mut listed = paths(&answer);
    listed.sort();
    assert_eq!(listed, ["gone.txt", "keep.txt", "latin1.txt", "new.txt"]);
    assert_eq!(counts(&answer, "keep.txt"), (1, 0));
    assert_eq!(counts(&answer, "gone.txt"), (0, 3));
    assert_eq!(counts(&answer, "new.txt"), (1, 0));
    assert_eq!(counts(&answer, "latin1.txt"), (2, 0));
    assert_eq!(answer.data()["patch"]["added"], 4);
    assert_eq!(answer.data()["patch"]["removed"], 3);

    assert_eq!(answer.data()["patch"], world.record(&id)["patch"]);
    assert_eq!(answer.data()["patch"], history_line(&world, &id)["patch"]);
}

#[test]
fn a_change_the_writer_committed_is_in_its_patch() {
    let world = World::new();
    let base = commit_files(&world, &[("keep.txt", b"kept\n")]);
    let answer = fork(
        &world,
        "FAKE: append=keep.txt::edited by the callee\\n\nFAKE: commit\nFAKE: bytes=after.bin",
        &[],
    );
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_ne!(head(&worktree(&answer)), base, "the writer did not commit");
    let mut listed = paths(&answer);
    listed.sort();
    assert_eq!(listed, ["after.bin", "keep.txt"]);
    assert_eq!(counts(&answer, "keep.txt"), (1, 0));
}

#[test]
fn a_symlink_is_kept_as_a_link_never_as_what_it_points_at() {
    let world = World::new();
    let secret = world.home.join("secret");
    fs::write(&secret, "TOP-SECRET-MARKER\n").unwrap();
    let answer = fork(
        &world,
        &format!("FAKE: link=leak={}", secret.display()),
        &[],
    );
    assert_eq!(answer.code, 0, "{}", answer.json);
    let patch = fs::read_to_string(world.run_file(&answer.run_id(), "patch.diff")).unwrap();
    assert!(patch.contains(path_str(&secret)), "{patch}");
    assert!(patch.contains("new file mode 120000"), "{patch}");
    assert!(!patch.contains("TOP-SECRET-MARKER"), "{patch}");

    // A link to a directory: `git diff --no-index` would take it for the
    // directory, and read the `null` in it.
    let vault = world.home.join("vault");
    fs::create_dir_all(&vault).unwrap();
    fs::write(vault.join("null"), "TOP-SECRET-MARKER\n").unwrap();
    let to_dir = fork(&world, &format!("FAKE: link=door={}", vault.display()), &[]);
    assert_eq!(to_dir.code, 0, "{}", to_dir.json);
    assert_eq!(paths(&to_dir), ["door"]);
    assert_eq!(counts(&to_dir, "door"), (1, 0));
    let patch = fs::read_to_string(world.run_file(&to_dir.run_id(), "patch.diff")).unwrap();
    assert!(patch.contains("new file mode 120000"), "{patch}");
    assert!(patch.contains(path_str(&vault)), "{patch}");
    assert!(!patch.contains("TOP-SECRET-MARKER"), "{patch}");
    // And it replays as the link it is.
    let replay = world.root.join("replay");
    let base = to_dir.data()["base_commit"].as_str().unwrap().to_string();
    world.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        path_str(&replay),
        &base,
    ]);
    let patch_path = world.run_file(&to_dir.run_id(), "patch.diff");
    git_in(&replay, &["apply", path_str(&patch_path)]);
    assert_eq!(fs::read_link(replay.join("door")).unwrap(), vault);

    let history = fs::read_to_string(world.state.join("history.jsonl")).unwrap();
    assert!(!history.contains("TOP-SECRET-MARKER"));
}

#[test]
fn a_writer_that_changes_nothing_has_an_empty_patch() {
    let world = World::new();
    let answer = fork(&world, "hello", &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let patch = world.run_file(&answer.run_id(), "patch.diff");
    assert_eq!(fs::metadata(&patch).unwrap().len(), 0);
    assert_eq!(
        answer.data()["patch"],
        json!({"files": [], "added": 0, "removed": 0})
    );
}

#[test]
fn the_same_change_hashes_the_same_in_two_runs() {
    let world = World::new();
    commit_files(&world, &[("keep.txt", b"kept\n")]);
    let brief = "FAKE: append=keep.txt::more\\n\nFAKE: write=x.txt\nFAKE: bytes=y.bin";
    let one = fork(&world, brief, &[]);
    let two = fork(&world, brief, &[]);
    assert_eq!((one.code, two.code), (0, 0), "{} {}", one.json, two.json);
    assert_ne!(one.run_id(), two.run_id());
    assert!(!one.data()["patch"]["files"].as_array().unwrap().is_empty());
    assert_eq!(one.data()["patch"], two.data()["patch"]);
}

#[test]
fn the_patch_ignores_how_git_is_configured() {
    let world = World::new();
    // Two blocks seven lines apart (merged by a wide inter-hunk context), a
    // blank context line, and a second tracked file for an order file to put
    // first. `core.autocrlf` is not among the settings: it is honoured, as
    // checkout honours it.
    let lines: String = (1..=30)
        .map(|n| {
            if n == 4 {
                "\n".to_string()
            } else {
                format!("line {n}\n")
            }
        })
        .collect();
    commit_files(
        &world,
        &[("lines.txt", lines.as_bytes()), ("z.txt", b"zed\n")],
    );
    let changed = lines
        .replace("line 5\n", "line five\n")
        .replace("line 13\n", "line thirteen\n")
        .replace('\n', "\\n");
    // Past a 1 KiB `core.bigFileThreshold`.
    let big: String = (0..100).map(|n| format!("a line of text {n}\\n")).collect();
    let brief = format!(
        "FAKE: remove=lines.txt\nFAKE: append=lines.txt::{changed}\n\
         FAKE: append=big.txt::{big}\nFAKE: write=new.txt\nFAKE: bytes=z.txt"
    );

    let a = fork(&world, &brief, &[]);
    assert_eq!(a.code, 0, "{}", a.json);
    assert_eq!(file(&a, "lines.txt")["hunks"].as_array().unwrap().len(), 2);

    let order = world.root.join("order");
    fs::write(&order, "z.txt\n").unwrap();
    let ext_ran = world.bin.join("ext-ran");
    let fsmon_ran = world.bin.join("fsmon-ran");
    let ext = world.bin.join("ext-diff");
    let fsmon = world.bin.join("fsmon");
    world.script_at(&ext, &format!("#!/bin/sh\ntouch '{}'\n", ext_ran.display()));
    world.script_at(
        &fsmon,
        &format!("#!/bin/sh\ntouch '{}'\n", fsmon_ran.display()),
    );
    for (key, value) in [
        ("diff.algorithm", "histogram"),
        ("diff.noprefix", "true"),
        ("diff.mnemonicPrefix", "true"),
        ("diff.orderFile", path_str(&order)),
        ("diff.interHunkContext", "10"),
        ("diff.suppressBlankEmpty", "true"),
        ("color.diff", "always"),
        ("core.bigFileThreshold", "1k"),
        ("core.abbrev", "4"),
        ("diff.external", path_str(&ext)),
        ("core.fsmonitor", path_str(&fsmon)),
    ] {
        world.git(&["config", key, value]);
    }

    let b = fork(&world, &brief, &[]);
    assert_eq!(b.code, 0, "{}", b.json);
    assert_eq!(a.data()["patch"], b.data()["patch"]);
    let read = |answer: &Answer| fs::read(world.run_file(&answer.run_id(), "patch.diff")).unwrap();
    assert_eq!(read(&a), read(&b), "the patch's bytes differ");
    let patch = read(&b);
    assert!(patch.starts_with(b"diff --git a/"));
    assert!(!patch.contains(&0x1b));
    assert!(!ext_ran.exists(), "the configured diff helper ran");
    assert!(!fsmon_ran.exists(), "the configured fsmonitor ran");
}

#[test]
fn readers_and_in_place_writers_keep_no_patch() {
    let world = World::new();
    world.configure("limits.allow_in_place = true");
    let brief = world.brief("FAKE: write=edit.txt");
    let answer = world.ask(&[
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--in-place",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let id = answer.run_id();
    assert_eq!(answer.data()["base_commit"], head(&world.work).as_str());
    assert!(answer.data()["patch"].is_null(), "{}", answer.json);
    assert!(!world.run_file(&id, "patch.diff").exists());
    let line = history_line(&world, &id);
    assert!(line["base_commit"].is_string(), "{line}");
    assert!(line.get("patch").is_none(), "{line}");
}

#[test]
fn a_cancelled_writer_keeps_what_it_had_written() {
    let world = World::new();
    let started = fork(
        &world,
        "FAKE: write=half.txt\nFAKE: sleep=30",
        &["--wait", "0"],
    );
    let id = started.run_id();
    common::wait_until("the writer has written", || {
        let record = world.record(&id);
        record["state"] == "running"
            && Path::new(record["cwd"].as_str().unwrap())
                .join("half.txt")
                .exists()
    });
    let cancelled = world.cancel_settled(&id);
    assert_eq!(cancelled.code, 42, "{}", cancelled.json);
    assert_eq!(cancelled.data()["state"], "cancelled");
    assert_eq!(paths(&cancelled), ["half.txt"]);
}

#[test]
fn a_resumed_fork_keeps_its_base_and_its_patch_grows() {
    let world = World::new();
    let first = fork(&world, "FAKE: write=a.txt", &[]);
    assert_eq!(first.code, 0, "{}", first.json);
    // The caller moves on; the worktree does not.
    commit_files(&world, &[("later.txt", b"later\n")]);
    let brief = world.brief("FAKE: write=b.txt");
    let again = world.ask(&[
        "resume",
        &first.run_id(),
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(again.code, 0, "{}", again.json);
    assert_eq!(again.data()["base_commit"], first.data()["base_commit"]);
    assert_eq!(paths(&again), ["a.txt", "b.txt"]);
}

#[test]
fn the_summary_outlives_the_content() {
    let world = World::new();
    let answer = fork(&world, "FAKE: write=x.txt", &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let id = answer.run_id();
    world.age(&id, 8 * 24 * 3600);
    world.ask(&["status"]);
    assert!(!world.state.join("runs").join(&id).exists());
    let line = history_line(&world, &id);
    assert_eq!(line["base_commit"], answer.data()["base_commit"]);
    assert_eq!(line["patch"], answer.data()["patch"]);
}

#[test]
fn a_daft_fork_is_cut_at_the_base_commit_without_carry() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"print": f1}));
    let answer = fork(&world, "FAKE: write=x.txt", &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let sha = answer.data()["base_commit"].as_str().unwrap().to_string();
    let calls = world.daft_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    let argv: Vec<&str> = calls[0]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let skip = argv.iter().position(|arg| *arg == "--skip-hooks").unwrap();
    assert_eq!(argv[skip..], ["--skip-hooks", "all", "--no-carry", &sha]);
    assert_eq!(head(&f1), sha);
    assert_eq!(paths(&answer), ["x.txt"]);
}

#[test]
fn a_fork_cut_anywhere_else_never_runs_the_writer() {
    let world = World::new();
    commit_files(&world, &[("later.txt", b"later\n")]);
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"print": f1, "at": "HEAD~1"}));
    let answer = fork(&world, "FAKE: write=x.txt", &[]);
    assert_eq!(answer.code, 40, "{}", answer.json);
    let sha = answer.data()["base_commit"].as_str().unwrap().to_string();
    assert_eq!(sha, head(&world.work));
    assert_eq!(
        answer.message(),
        format!("cannot cut a worktree: it was not cut at {sha}")
    );
    assert_ne!(head(&f1), sha, "the fake cut at the base after all");
    assert!(world.record(&answer.run_id())["callee_pid"].is_null());
    assert!(!f1.join("x.txt").exists(), "the writer ran");
    assert!(answer.data()["patch"].is_null(), "{}", answer.json);
}

#[test]
fn a_rewritten_dot_git_does_not_steer_the_patch() {
    let world = World::new();
    let sha = head(&world.work);
    let common_dir = fs::canonicalize(world.work.join(".git")).unwrap();
    world.git(&["config", "extensions.worktreeConfig", "true"]);
    let evil = world.root.join("evil");
    fs::create_dir_all(&evil).unwrap();
    fs::write(evil.join("HEAD"), format!("{sha}\n")).unwrap();
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

    let answer = fork(
        &world,
        &format!(
            "FAKE: remove=.git\nFAKE: append=.git::gitdir: {}\\n\n\
             FAKE: append=.gitattributes::* filter=evil\\n\nFAKE: write=x.txt",
            evil.display()
        ),
        &[],
    );
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(!marker.exists(), "cahoots ran the writer's filter");
    let mut listed = paths(&answer);
    listed.sort();
    assert_eq!(listed, [".gitattributes", "x.txt"]);

    // The control: git, following the rewritten `.git` as anyone's would,
    // runs the writer's filter on the same untracked file.
    let control = Command::new("git")
        .args(["diff", "--no-index", "--", "/dev/null", "x.txt"])
        .current_dir(worktree(&answer))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert_eq!(control.status.code(), Some(1));
    assert!(marker.exists(), "the control never ran the filter");
}

#[test]
fn a_submodule_the_writer_populates_is_not_entered() {
    let world = World::new();
    let commit_in = |dir: &Path, message: &str| {
        git_in(
            dir,
            &[
                "-c",
                "user.name=Sub",
                "-c",
                "user.email=sub@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                message,
            ],
        );
    };
    let src = world.root.join("subsrc");
    fs::create_dir_all(&src).unwrap();
    git_in(&src, &["init", "-q"]);
    commit_in(&src, "chore: a submodule");
    git_in(
        &world.work,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            "../subsrc",
            "sub",
        ],
    );
    world.git(&["commit", "-q", "-m", "chore: add a submodule"]);

    // A repository of the writer's, whose filter is a program. Its `f.txt`
    // and the writer's are the same size, so git hashes the file to compare
    // them, and runs the filter to hash it.
    let evil = world.root.join("evil");
    fs::create_dir_all(&evil).unwrap();
    git_in(&evil, &["init", "-q"]);
    fs::write(evil.join("f.txt"), "original\n").unwrap();
    fs::write(evil.join(".gitattributes"), "* filter=evil\n").unwrap();
    git_in(&evil, &["add", "f.txt", ".gitattributes"]);
    commit_in(&evil, "chore: evil");
    let marker = world.root.join("filter-ran");
    let filter = world.root.join("filter");
    world.script_at(
        &filter,
        &format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
    );
    git_in(&evil, &["config", "filter.evil.clean", path_str(&filter)]);

    let answer = fork(
        &world,
        &format!(
            "FAKE: append=sub/.git::gitdir: {}\\n\nFAKE: append=sub/f.txt::changed!\\n\n\
             FAKE: write=x.txt",
            evil.join(".git").display()
        ),
        &[],
    );
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(!marker.exists(), "cahoots entered the submodule");
    assert_eq!(paths(&answer), ["x.txt"]);

    // The control: a plain git status there does enter it.
    git_in(&worktree(&answer), &["status", "--short"]);
    assert!(marker.exists(), "the control never ran the filter");
}

#[test]
fn a_child_left_behind_does_not_change_the_patch() {
    let world = World::new();
    let brief = format!(
        "FAKE: leak=late.txt\nFAKE: leak_gate={}\nFAKE: write=x.txt",
        world.leak_gate().display()
    );
    let answer = fork(&world, &brief, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(paths(&answer), ["x.txt"]);
    // The child is gone with the run; only then is it let go to write.
    let child = world.leaked_child(&answer.run_id());
    common::wait_until("the leaked child is gone", || !common::alive(child));
    world.open_leak_gate();
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert!(!worktree(&answer).join("late.txt").exists());
}

#[test]
fn a_blind_run_still_shows_its_base_commit_and_patch() {
    let world = World::new();
    world.configure("review.blind = true");
    let answer = fork(&world, "FAKE: write=x.txt", &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["blind"], true);
    assert_eq!(answer.data()["base_commit"], head(&world.work).as_str());
    assert_eq!(paths(&answer), ["x.txt"]);
}

#[test]
fn old_records_load_and_a_forged_base_commit_does_not() {
    let world = World::new();
    let run = world.run("hello", &[]);
    let id = run.run_id();
    let mut record = world.record(&id);
    record.as_object_mut().unwrap().remove("base_commit");
    record.as_object_mut().unwrap().remove("patch");
    let old: cahoots::run::record::RunRecord = serde_json::from_value(record.clone()).unwrap();
    assert!(old.base_commit.is_none() && old.patch.is_none());
    fs::write(
        world.run_file(&id, "run.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let later = world.ask(&["result", &id]);
    assert_eq!(later.code, 0, "{}", later.json);
    assert!(later.data()["base_commit"].is_null());
    assert!(later.data()["patch"].is_null());
    record["base_commit"] = json!("--exec=x");
    assert!(serde_json::from_value::<cahoots::run::record::RunRecord>(record).is_err());
}

#[test]
fn autocrlf_shapes_the_patch_as_a_commit_would() {
    let world = World::new();
    commit_files(&world, &[("keep.txt", b"one\ntwo\n")]);
    world.git(&["config", "core.autocrlf", "true"]);
    let answer = fork(&world, "FAKE: append=keep.txt::edited\\n", &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    // Checked out with CRLF, as the person's setting says; the patch is what
    // committing it would record.
    let checked_out = fs::read(worktree(&answer).join("keep.txt")).unwrap();
    assert!(checked_out.windows(2).any(|pair| pair == b"\r\n"));
    assert_eq!(paths(&answer), ["keep.txt"]);
    assert_eq!(counts(&answer, "keep.txt"), (1, 0));
}

#[test]
fn a_failed_writer_keeps_its_patch() {
    let world = World::new();
    let answer = fork(&world, "FAKE: write=x.txt\nFAKE: fail", &[]);
    assert_eq!(answer.code, 40, "{}", answer.json);
    assert_eq!(answer.data()["state"], "failed");
    assert_eq!(paths(&answer), ["x.txt"]);
}

/// A tracked file's directory turned into a file: the path git lists from
/// the index is under something that is no longer a directory, and that is
/// nothing there, not a reason to keep no patch.
#[test]
fn a_directory_that_became_a_file_keeps_its_patch() {
    let world = World::new();
    commit_files(&world, &[("d/f.txt", b"in a directory\n")]);
    let answer = fork(
        &world,
        "FAKE: remove=d/f.txt\nFAKE: rmdir=d\nFAKE: write=d",
        &[],
    );
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(paths(&answer), ["d/f.txt", "d"]);
    let id = answer.run_id();
    let patch = fs::read(world.run_file(&id, "patch.diff")).unwrap();
    let base = answer.data()["base_commit"].as_str().unwrap();
    let replay = world.root.join("replay");
    world.git(&["worktree", "add", "-q", "--detach", path_str(&replay), base]);
    fs::write(world.root.join("patch.diff"), patch).unwrap();
    git_in(
        &replay,
        &["apply", path_str(&world.root.join("patch.diff"))],
    );
    assert_eq!(
        fs::read(replay.join("d")).unwrap(),
        fs::read(worktree(&answer).join("d")).unwrap()
    );
}

#[test]
fn a_timed_out_writer_keeps_its_patch() {
    let world = World::new();
    let answer = fork(
        &world,
        "FAKE: write=x.txt\nFAKE: sleep=30",
        &["--timeout", "1"],
    );
    assert_eq!(answer.code, 41, "{}", answer.json);
    assert_eq!(answer.data()["state"], "timed_out");
    assert_eq!(paths(&answer), ["x.txt"]);
}

#[test]
fn a_resumed_live_tree_reads_its_new_base_commit() {
    let world = World::new();
    let first = world.run("hello", &[]);
    assert_eq!(first.code, 0, "{}", first.json);
    let newer = commit_files(&world, &[("new.txt", b"new\n")]);
    let brief = world.brief("hello again");
    let again = world.ask(&[
        "resume",
        &first.run_id(),
        "--caller",
        "claude",
        "--brief",
        brief.to_str().unwrap(),
    ]);
    assert_eq!(again.code, 0, "{}", again.json);
    assert_eq!(again.data()["base_commit"], newer.as_str());
    assert_ne!(first.data()["base_commit"], again.data()["base_commit"]);
    assert!(again.data()["patch"].is_null());
}

/// The `git` on the suite's own PATH.
fn real_git() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|git| git.is_file())
        .expect("git on PATH")
}

/// A `git` of the test's own, pinned and outside the workspace, that logs
/// each call to `bin/git.calls` and passes it to the real one — except
/// what it is told to do instead when its argv holds `diff` or `ls-files`,
/// the index's own listing (`ls-files --cached`) aside.
fn capture_git(world: &World, diff: &str, ls_files: &str) {
    capture_git_with(world, diff, ls_files, None);
}

/// [`capture_git`], with the index's own listing (`ls-files --cached`) done
/// as `cached` says instead of by the real git.
fn capture_git_with(world: &World, diff: &str, ls_files: &str, cached: Option<&str>) {
    let git = real_git();
    let cached = cached.map_or_else(
        || format!("exec '{}' \"$@\"", git.display()),
        str::to_string,
    );
    world.script_at(
        &world.bin.join("git"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{calls}'\n\
             for arg do\n  if [ \"$arg\" = --cached ]; then\n    {cached}\n  fi\ndone\n\
             for arg do\n  case \"$arg\" in\n    diff) {diff} ;;\n    ls-files) {ls_files} ;;\n  \
             esac\ndone\nexec '{git}' \"$@\"\n",
            git = git.display(),
            calls = world.bin.join("git.calls").display(),
        ),
    );
    world.pin_tool("git", Some(&world.bin.join("git")));
}

/// The writer did what it did and its run ended as it would have; only the
/// patch was not kept — no file, nothing in the history, and a line saying
/// why.
fn assert_no_patch(world: &World, answer: &Answer, why: &str) {
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["state"], "done");
    assert!(answer.data()["patch"].is_null(), "{}", answer.json);
    assert!(!world.run_file(&answer.run_id(), "patch.diff").exists());
    assert!(worktree(answer).join("x.txt").is_file());
    let line = history_line(world, &answer.run_id());
    assert!(line.get("patch").is_none(), "{line}");
    assert!(line["base_commit"].is_string(), "{line}");
    let log = fs::read_to_string(world.run_file(&answer.run_id(), "supervisor.log")).unwrap();
    assert!(log.contains("[supervisor] patch not kept:"), "{log}");
    assert!(log.contains(why), "{log}");
}

#[test]
fn a_capture_that_fails_keeps_no_partial_patch() {
    let world = World::new();
    capture_git(&world, "printf partial; exit 2", ":");
    let answer = fork(&world, "FAKE: write=x.txt", &[]);
    assert_no_patch(&world, &answer, "git diff exited with 2");
}

#[test]
fn a_capture_past_the_cap_keeps_no_partial_patch() {
    let world = World::new();
    capture_git(&world, "exec head -c 33554433 /dev/zero", ":");
    let answer = fork(&world, "FAKE: write=x.txt", &[]);
    assert_no_patch(&world, &answer, "past 33554432 bytes");
}

#[test]
fn the_cap_is_on_every_step_together() {
    // The diff fits, and so does the list of untracked files; the two
    // together are 4 bytes past the cap. (`nested/` is a repository of its
    // own, and adds nothing more.)
    let world = World::new();
    capture_git(
        &world,
        "head -c 33554428 /dev/zero; exit 0",
        "printf 'nested/\\0'; exit 0",
    );
    let answer = fork(&world, "FAKE: write=x.txt", &[]);
    assert_no_patch(&world, &answer, "past 33554432 bytes");
}

#[test]
fn a_capture_of_exactly_the_cap_is_kept() {
    let world = World::new();
    capture_git(
        &world,
        "head -c 33554424 /dev/zero; exit 0",
        "printf 'nested/\\0'; exit 0",
    );
    let answer = fork(&world, "FAKE: write=x.txt", &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(
        answer.data()["patch"],
        json!({"files": [], "added": 0, "removed": 0})
    );
    let kept = fs::metadata(world.run_file(&answer.run_id(), "patch.diff")).unwrap();
    assert_eq!(kept.len(), 33554424);
}

#[test]
fn a_capture_past_the_deadline_keeps_no_partial_patch() {
    let world = World::new();
    capture_git(&world, "exec sleep 20", ":");
    let started = Instant::now();
    let answer = fork(&world, "FAKE: write=x.txt", &[]);
    assert!(started.elapsed() < Duration::from_secs(18));
    assert_no_patch(&world, &answer, "did not finish within");
}

#[test]
fn a_git_that_exits_but_leaves_its_stdout_open_is_not_waited_on() {
    // git exits at once, with a child that holds its stdout for longer than
    // the whole capture may take. The capture ends at its deadline all the
    // same.
    let world = World::new();
    capture_git(&world, "sleep 40 & exit 0", ":");
    let started = Instant::now();
    let answer = fork(&world, "FAKE: write=x.txt", &[]);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the capture waited on the pipe"
    );
    assert_no_patch(&world, &answer, "its stdout stayed open");
}

#[test]
fn an_untracked_hard_link_is_never_read() {
    let world = World::new();
    let secret = world.home.join("secret");
    fs::write(&secret, "TOP-SECRET-MARKER\n").unwrap();
    let answer = fork(
        &world,
        &format!(
            "FAKE: write=x.txt\nFAKE: hardlink=leak={}",
            secret.display()
        ),
        &[],
    );
    assert_no_patch(&world, &answer, "has more than one hard link");
    let history = fs::read_to_string(world.state.join("history.jsonl")).unwrap();
    assert!(!history.contains("TOP-SECRET-MARKER"));
}

#[test]
fn a_tracked_file_made_a_hard_link_is_never_read() {
    let world = World::new();
    commit_files(&world, &[("notes.txt", b"notes\n")]);
    let secret = world.home.join("secret");
    fs::write(&secret, "TOP-SECRET-MARKER\n").unwrap();
    let answer = fork(
        &world,
        &format!(
            "FAKE: remove=notes.txt\nFAKE: write=x.txt\nFAKE: hardlink=notes.txt={}",
            secret.display()
        ),
        &[],
    );
    assert_no_patch(&world, &answer, "has more than one hard link");
}

#[test]
fn a_hard_link_is_refused_before_git_reads_any_file() {
    // The refusal comes from the index's listing and `lstat` alone: no git
    // that reads a file in the tree — a `diff` of any kind — ever runs.
    let world = World::new();
    commit_files(&world, &[("notes.txt", b"notes\n")]);
    let secret = world.home.join("secret");
    fs::write(&secret, "TOP-SECRET-MARKER\n").unwrap();
    capture_git(&world, ":", ":");
    let answer = fork(
        &world,
        &format!(
            "FAKE: remove=notes.txt\nFAKE: write=x.txt\nFAKE: hardlink=notes.txt={}",
            secret.display()
        ),
        &[],
    );
    assert_no_patch(&world, &answer, "has more than one hard link");
    let calls = fs::read_to_string(world.bin.join("git.calls")).unwrap();
    let words = |line: &str| line.split(' ').map(str::to_string).collect::<Vec<_>>();
    assert!(
        calls
            .lines()
            .any(|line| words(line).contains(&"--cached".to_string())),
        "the index was never listed: {calls}"
    );
    assert!(
        !calls
            .lines()
            .any(|line| words(line).contains(&"diff".to_string())),
        "a git diff ran before the refusal: {calls}"
    );
}

#[test]
fn looking_at_a_large_tree_stops_at_the_deadline() {
    // The index lists five million paths, after most of the time is gone:
    // looking at them all would run seconds past the deadline, and the look
    // stops there instead. The listing is written beforehand, so that git
    // answers in time however busy the machine is.
    let world = World::new();
    let listing = world.root.join("index-listing");
    fs::write(&listing, b"p\0".repeat(5_000_000)).unwrap();
    capture_git_with(
        &world,
        ":",
        ":",
        Some(&format!("sleep 6.5; cat '{}'; exit 0", listing.display())),
    );
    let answer = fork(&world, "FAKE: write=x.txt", &[]);
    assert_no_patch(&world, &answer, "looking at the tree's files");
}
