//! Repository data cannot make git fetch. In a partial clone, git fetches a
//! missing object on demand, through the repository's own remote
//! configuration — which can name a remote helper, a command, run outside
//! every sandbox. Every git cahoots starts is told never to (and the git
//! `daft` starts, and the harness's own): a step that needs a missing object
//! fails as it would for one that cannot be read, and no transport starts.
//!
//! The repository here is a partial clone whose promisor remote is a local
//! marker script (`World::promisor`). Each test loses one object, drives the
//! binary, and checks the marker never ran — then replays the same git
//! command without the variable, which does run it: the step really reads
//! that object, so the test could have failed.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Answer, World, path_str};
use serde_json::json;

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

/// A world whose repository is a partial clone with one committed file,
/// `a.txt`; returns it with the commit.
fn promisor_world() -> (World, String) {
    let world = World::new();
    fs::write(world.work.join("a.txt"), "hello\n").unwrap();
    world.git(&["add", "a.txt"]);
    world.git(&["commit", "-q", "-m", "chore: a file"]);
    world.promisor();
    let head = git_out(&world.work, &["rev-parse", "HEAD"]);
    (world, head)
}

fn fork(world: &World, brief: &str) -> Answer {
    let brief = world.brief(brief);
    world.ask(&[
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--fork",
        "--brief",
        brief.to_str().unwrap(),
    ])
}

fn worktree(answer: &Answer) -> PathBuf {
    PathBuf::from(
        answer.data()["worktree"]
            .as_str()
            .unwrap_or_else(|| panic!("no worktree: {}", answer.json)),
    )
}

#[test]
fn the_base_commit_is_never_fetched() {
    let (world, head) = promisor_world();
    world.lose(&head);
    let answer = world.run("hello", &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(!world.fetch_tried(), "recording the base commit fetched it");
    assert!(answer.data()["base_commit"].is_null(), "{}", answer.json);
    assert!(world.fetches_without_the_variable(
        &world.work,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]
    ));
}

#[test]
fn a_git_cut_never_fetches_a_missing_blob() {
    let (world, head) = promisor_world();
    world.lose(&git_out(&world.work, &["rev-parse", "HEAD:a.txt"]));
    let answer = fork(&world, "FAKE: write=x.txt");
    assert_ne!(answer.code, 0, "{}", answer.json);
    assert!(!world.fetch_tried(), "the cut fetched the missing blob");
    assert!(
        answer.message().contains("`git worktree add` refused"),
        "{}",
        answer.json
    );
    assert!(world.record(&answer.run_id())["callee_pid"].is_null());
    let elsewhere = world.root.join("replay");
    assert!(world.fetches_without_the_variable(
        &world.work,
        &["worktree", "add", "--detach", path_str(&elsewhere), &head]
    ));
}

#[test]
fn a_daft_cut_never_fetches_a_missing_blob() {
    let (world, head) = promisor_world();
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"print": f1}));
    world.lose(&git_out(&world.work, &["rev-parse", "HEAD:a.txt"]));
    let answer = fork(&world, "FAKE: write=x.txt");
    assert_ne!(answer.code, 0, "{}", answer.json);
    assert!(!world.fetch_tried(), "the git daft runs fetched the blob");
    assert!(
        answer
            .message()
            .contains("`daft start --fork` exited with status 1"),
        "{}",
        answer.json
    );
    // daft got the variable, whatever cahoots' caller had set, so the git it
    // starts does too.
    let calls = world.daft_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0]["no_lazy_fetch"], "1", "{calls:?}");
    let elsewhere = world.root.join("replay");
    assert!(world.fetches_without_the_variable(
        &world.work,
        &["worktree", "add", "--detach", path_str(&elsewhere), &head]
    ));
}

#[test]
fn the_post_cut_check_never_fetches() {
    let (world, head) = promisor_world();
    let f1 = world.root.join("forks/f1");
    // daft cuts at the commit, then the commit is gone: the check that the
    // worktree was cut there reads it.
    world.daft(json!({"print": f1, "lose": world.object_path(&head)}));
    let answer = fork(&world, "FAKE: write=x.txt");
    assert_eq!(answer.code, 40, "{}", answer.json);
    assert!(
        !world.fetch_tried(),
        "the post-cut check fetched the commit"
    );
    assert_eq!(
        answer.message(),
        format!("cannot cut a worktree: it was not cut at {head}")
    );
    assert!(world.record(&answer.run_id())["callee_pid"].is_null());
    assert!(!f1.join("x.txt").exists(), "the writer ran");
    assert!(
        world.fetches_without_the_variable(
            &f1,
            &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]
        )
    );
}

#[test]
fn patch_capture_never_fetches_a_missing_blob() {
    let (world, head) = promisor_world();
    let blob = git_out(&world.work, &["rev-parse", "HEAD:a.txt"]);
    // The writer changes a.txt and loses its base blob: the diff that keeps
    // the patch needs it.
    let answer = fork(
        &world,
        &format!(
            "FAKE: remove={}\nFAKE: append=a.txt::more\\n",
            world.object_path(&blob).display()
        ),
    );
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(!world.fetch_tried(), "the patch capture fetched the blob");
    assert!(answer.data()["patch"].is_null(), "{}", answer.json);
    let log = fs::read_to_string(world.run_file(&answer.run_id(), "supervisor.log")).unwrap();
    assert!(log.contains("patch not kept"), "{log}");
    assert!(world.fetches_without_the_variable(
        &worktree(&answer),
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            &head,
            "--",
            "a.txt"
        ]
    ));
}

#[test]
fn git_status_never_fetches_a_missing_tree() {
    let (world, head) = promisor_world();
    let tree = git_out(&world.work, &["rev-parse", &format!("{head}^{{tree}}")]);
    let answer = fork(
        &world,
        &format!("FAKE: remove={}", world.object_path(&tree).display()),
    );
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(!world.fetch_tried(), "git status fetched the tree");
    assert!(
        answer.data()["changes_error"]
            .as_str()
            .is_some_and(|why| why.contains("`git status` failed")),
        "{}",
        answer.json
    );
    assert!(world.fetches_without_the_variable(
        &worktree(&answer),
        &[
            "status",
            "--short",
            "--untracked-files=all",
            "--ignore-submodules=all"
        ]
    ));
}

#[test]
fn a_git_below_the_floor_is_never_run() {
    let world = World::new();
    let ran = world.root.join("old-git-ran");
    let old = world.root.join("old-git/git");
    world.script_at(
        &old,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'git version 2.45.0'; exit 0; fi\n\
             echo \"$@\" >> '{}'\nexit 1\n",
            ran.display()
        ),
    );
    world.prefix_path(old.parent().unwrap());
    let answer = world.run("hello", &[]);
    assert_eq!(answer.code, 34, "{}", answer.json);
    assert!(
        answer.message().contains("never fetches lazily")
            && answer.message().contains("found `git version 2.45.0`"),
        "{}",
        answer.json
    );
    assert!(!ran.exists(), "the old git ran for more than its version");
}

/// The `git` first on this test's own PATH, as found.
fn installed_git() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git on PATH")
}

#[test]
fn the_git_daft_finds_is_held_to_the_floor_too() {
    let world = World::new();
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"print": f1}));
    // An old git daft would find, and never cahoots: before it on PATH, a
    // directory inside the workspace whose `git` links to the installed one.
    // The policy judges that link by where it points, so cahoots takes it;
    // daft's PATH drops workspace directories, so daft would not.
    let ran = world.root.join("old-git-ran");
    let old = world.root.join("old-git/git");
    world.script_at(
        &old,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'git version 2.45.0'; exit 0; fi\n\
             echo \"$@\" >> '{}'\nexit 1\n",
            ran.display()
        ),
    );
    world.prefix_path(old.parent().unwrap());
    let tools = world.work.join("tools");
    fs::create_dir_all(&tools).unwrap();
    std::os::unix::fs::symlink(
        fs::canonicalize(installed_git()).unwrap(),
        tools.join("git"),
    )
    .unwrap();
    world.prefix_path(&tools);
    let answer = fork(&world, "FAKE: write=x.txt");
    assert_eq!(answer.code, 34, "{}", answer.json);
    assert!(
        answer.message().contains("found `git version 2.45.0`"),
        "{}",
        answer.json
    );
    assert!(!ran.exists(), "the old git ran for more than its version");
    assert!(world.daft_calls().is_empty(), "daft started");
    assert!(world.record(&answer.run_id())["callee_pid"].is_null());
    assert!(!f1.exists(), "a worktree was cut");
}

/// The eval suite's checks of a task's base commit (`evals list`, and
/// `evals add` before it writes a task) read that commit, and never fetch it.
#[test]
fn an_eval_tasks_base_commit_is_never_fetched() {
    let (world, head) = promisor_world();
    let brief = "FAKE: append=a.txt::more\\n\n";
    let added = world.accepted_writer(brief, &[]).run_id();
    let other = world.accepted_writer(brief, &[]).run_id();
    let at = |args: &[&str]| {
        world
            .at_terminal_with(args, &[("PATH", &world.path_with_git())])
            .finish()
    };
    let first = at(&["evals", "add", &added]);
    assert_eq!(first.code, 0, "{}", first.json);
    world.lose(&head);

    let listed = at(&["evals", "list"]);
    assert_eq!(listed.code, 0, "{}", listed.json);
    assert_eq!(
        listed.json["data"]["tasks"][0]["rot"], "commit_gone",
        "{}",
        listed.json
    );
    assert!(!world.fetch_tried(), "evals list tried to fetch");
    let refused = at(&["evals", "add", &other]);
    assert_eq!(refused.code, 2, "{}", refused.json);
    assert!(!world.fetch_tried(), "evals add tried to fetch");

    let probe = format!("{head}^{{commit}}");
    assert!(world.fetches_without_the_variable(
        &world.work,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &probe
        ]
    ));
}
