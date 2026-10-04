//! Cutting a fork runs no command the repository defines — no git hook, no
//! `daft.yml` job — and the place `daft` says it cut is used only if it is the
//! top of a worktree of the same repository, outside the caller's tree and
//! outside cahoots' own directories. No tool cahoots starts on the way comes
//! from the workspace. Every `daft` here is the fake (`World::daft`).

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Answer, World, alive, git_in, path_str};
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
/// caller's tree nor at `printed`, and no worktree is reported.
fn assert_never_ran(world: &World, answer: &Answer, printed: &Path) {
    let data = answer.data();
    assert!(data.get("worktree").is_none(), "{}", answer.json);
    assert!(data.get("changes").is_none(), "{}", answer.json);
    assert!(world.record(&answer.run_id())["callee_pid"].is_null());
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

/// A fork with the fake daft doing what `plan` says, refused with `code`.
fn refused(world: &World, plan: Value, printed: &Path, code: i32, says: &str) {
    let calls = world.daft_calls().len();
    world.daft(plan);
    let answer = fork(world, &[]);
    assert_eq!(answer.code, code, "{}", answer.json);
    assert!(answer.message().contains(says), "{}", answer.json);
    assert_never_ran(world, &answer, printed);
    assert_eq!(
        world.daft_calls().len(),
        calls + 1,
        "the fake daft was not the one run"
    );
}

#[test]
fn a_daft_fork_runs_no_hooks_and_is_used() {
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
    world.daft(json!({"make": "worktree", "print": f1}));
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["placement"], "fork");
    assert_eq!(answer.data()["worktree"], path_str(&f1));
    assert!(f1.join("callee-ran.txt").is_file());
    assert!(!world.work.join("callee-ran.txt").exists());
    assert!(
        answer.data()["changes"]
            .to_string()
            .contains("callee-ran.txt"),
        "{}",
        answer.json
    );
    assert!(!world.hook_ran(), "a hook of the repository's ran");

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
            "--skip-hooks",
            "all",
            "--no-carry",
            answer.data()["base_commit"]
                .as_str()
                .expect("a base commit")
        ])
    );
    // The git daft runs is told where its hooks are: nowhere. Every setting
    // after the two turns off a filter the configuration names, which on a
    // developer's machine may be their own (Git LFS's, say).
    let config = &calls[0]["git_config"];
    let count: usize = config["GIT_CONFIG_COUNT"]
        .as_str()
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("{config}"));
    assert!(count >= 2, "{config}");
    for n in 2..count {
        let key = config[format!("GIT_CONFIG_KEY_{n}")].as_str().unwrap();
        assert!(key.starts_with("filter."), "{key}: {config}");
    }
    assert_eq!(config["GIT_CONFIG_KEY_0"], "core.hooksPath");
    let hooks = Path::new(config["GIT_CONFIG_VALUE_0"].as_str().unwrap());
    assert!(hooks.starts_with(&world.state), "{}", hooks.display());
    assert!(
        hooks.is_dir() && fs::read_dir(hooks).unwrap().next().is_none(),
        "{} is not an empty directory",
        hooks.display()
    );
    assert_eq!(config["GIT_CONFIG_KEY_1"], "core.fsmonitor");
    assert_eq!(config["GIT_CONFIG_VALUE_1"], "false");
}

#[test]
fn a_daft_fork_under_cahoots_worktrees_is_accepted() {
    let world = World::new();
    let f2 = world.state.join("worktrees/f2");
    world.daft(json!({"make": "worktree", "print": f2}));
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["worktree"], path_str(&f2));
    assert!(f2.join("callee-ran.txt").is_file());
    assert_eq!(world.daft_calls().len(), 1);
}

#[test]
fn a_daft_path_that_is_not_absolute_is_refused() {
    let world = World::new();
    refused(
        &world,
        json!({"make": "nothing", "print": "forks/x"}),
        &world.root.join("forks/x"),
        33,
        "is not an absolute path",
    );
}

#[test]
fn a_daft_path_that_is_not_a_directory_is_refused() {
    let world = World::new();
    let file = world.root.join("forks/a-file");
    refused(
        &world,
        json!({"make": "file", "print": file}),
        &file,
        33,
        "is not a directory",
    );
    assert!(file.is_file(), "what daft made is not cahoots' to remove");
}

#[test]
fn a_daft_path_outside_the_repository_is_refused() {
    let world = World::new();
    // A plain directory.
    let plain = world.root.join("forks/plain");
    refused(
        &world,
        json!({"make": "dir", "print": plain}),
        &plain,
        33,
        "is not a worktree of the repository it was cut from",
    );

    // A worktree, of another repository.
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
            path_str(&theirs),
            "HEAD",
        ],
    );
    refused(
        &world,
        json!({"make": "nothing", "print": theirs}),
        &theirs,
        33,
        "is not a worktree of the repository it was cut from",
    );

    // A worktree of this repository, but not its top.
    let ours = world.root.join("ours");
    world.git(&["worktree", "add", "-q", "--detach", path_str(&ours), "HEAD"]);
    let below = ours.join("below");
    fs::create_dir_all(&below).unwrap();
    refused(
        &world,
        json!({"make": "nothing", "print": below}),
        &below,
        33,
        "is not a worktree of the repository it was cut from",
    );
}

#[test]
fn a_daft_path_that_was_a_worktree_already_is_refused() {
    let world = World::new();
    // A worktree of this repository that was there before the run: someone's
    // work, which a writer must never be let into.
    let existing = world.root.join("existing");
    world.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        path_str(&existing),
        "HEAD",
    ]);
    refused(
        &world,
        json!({"make": "nothing", "print": existing}),
        &existing,
        33,
        "was a worktree already",
    );

    // The caller's own checkout, when it forks another worktree (`--dir`).
    // In a daft repository every checkout is a linked worktree like this.
    world.git(&["add", "daft.yml"]);
    world.git(&["commit", "-q", "-m", "chore: opt into daft"]);
    let (own, other) = (world.root.join("own"), world.root.join("other"));
    for tree in [&own, &other] {
        world.git(&["worktree", "add", "-q", "--detach", path_str(tree), "HEAD"]);
    }
    world.daft(json!({"make": "nothing", "print": own}));
    let brief = own.join("brief.md");
    fs::write(&brief, BRIEF).unwrap();
    let mut command = world.cahoots();
    command.current_dir(&own).args([
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--fork",
        "--dir",
        path_str(&other),
        "--brief",
        path_str(&brief),
    ]);
    let answer = common::answer(&mut command);
    assert_eq!(answer.code, 33, "{}", answer.json);
    assert!(
        answer.message().contains("was a worktree already"),
        "{}",
        answer.json
    );
    assert_never_ran(&world, &answer, &own);
    assert_eq!(
        world.daft_calls().len(),
        2,
        "the fake daft was not the one run"
    );
}

/// Puts a directory's mode back when it goes out of scope, so a failed
/// assertion does not leave a world its temp directory cannot remove.
struct ModeBack(PathBuf, u32);

impl Drop for ModeBack {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(self.1));
    }
}

#[test]
fn a_fork_is_not_cut_when_the_repositorys_worktrees_cannot_be_listed() {
    let world = World::new();
    // A worktree that is someone's, and a daft that hands it back, while the
    // list a fresh one is told apart by cannot be read.
    let existing = world.root.join("existing");
    world.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        path_str(&existing),
        "HEAD",
    ]);
    world.daft(json!({"make": "nothing", "print": existing}));
    let listed = world.work.join(".git/worktrees");
    let _back = ModeBack(listed.clone(), 0o755);
    fs::set_permissions(&listed, fs::Permissions::from_mode(0o111)).unwrap();

    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 40, "{}", answer.json);
    assert!(answer.message().contains("cannot list"), "{}", answer.json);
    assert_never_ran(&world, &answer, &existing);
    assert!(world.daft_calls().is_empty(), "daft ran without the list");
}

#[test]
fn a_hooks_directory_that_is_not_empty_stops_the_cut() {
    // On the git path, then on daft's.
    for with_daft in [false, true] {
        let world = World::new();
        // Were the hooks directory used as it is, git would run what is in
        // it; the repository's own hook leaves the same mark.
        world.hook_that_marks(&world.state.join("no-hooks"));
        world.hook_that_marks(&world.work.join(".git/hooks"));
        let made = world.root.join("forks/f");
        if with_daft {
            world.daft(json!({"make": "worktree", "print": made}));
        }
        let answer = fork(&world, &[]);
        assert_eq!(answer.code, 40, "daft: {with_daft}: {}", answer.json);
        assert!(
            answer.message().contains("is not empty"),
            "daft: {with_daft}: {}",
            answer.json
        );
        assert_never_ran(&world, &answer, &made);
        assert!(!world.hook_ran(), "daft: {with_daft}: a hook ran");
        assert!(world.daft_calls().is_empty(), "daft ran");
    }
}

#[test]
fn a_daft_path_inside_the_callers_tree_is_refused() {
    let world = World::new();
    let nested = world.work.join("nested");
    refused(
        &world,
        json!({"make": "worktree", "print": nested}),
        &nested,
        33,
        "is the tree it was cut from, or inside it",
    );
}

#[test]
fn a_daft_path_that_is_the_callers_tree_is_refused() {
    let world = World::new();
    refused(
        &world,
        json!({"make": "nothing", "print": world.work}),
        &world.work,
        33,
        "is the tree it was cut from, or inside it",
    );
}

#[test]
fn a_daft_path_in_cahoots_own_directories_is_refused() {
    let world = World::new();
    for place in [world.state.join("elsewhere"), world.config.join("x")] {
        refused(
            &world,
            json!({"make": "dir", "print": place}),
            &place,
            33,
            "cahoots' own directories",
        );
    }
}

/// The data directory holds the eval suite, and a writer is never cut into
/// it: not as a real worktree of the repository, and not through a link
/// that resolves there.
#[test]
fn a_daft_worktree_in_the_data_directory_is_refused() {
    let world = World::new();
    let place = world.data.join("fork");
    refused(
        &world,
        json!({"make": "worktree", "print": place}),
        &place,
        33,
        "cahoots' own directories",
    );

    let by_hand = world.data.join("cut-by-hand");
    world.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        path_str(&by_hand),
        "HEAD",
    ]);
    let link = world.root.join("to-data");
    std::os::unix::fs::symlink(&by_hand, &link).unwrap();
    refused(
        &world,
        json!({"make": "nothing", "print": link}),
        &by_hand,
        33,
        "cahoots' own directories",
    );
}

#[test]
fn a_daft_that_fails_is_refused() {
    let world = World::new();
    let made = world.root.join("forks/f");
    refused(
        &world,
        json!({"make": "worktree", "print": made, "exit": 1}),
        &made,
        40,
        "exited with status 1",
    );
    assert!(made.is_dir(), "what daft made is not cahoots' to remove");
}

#[test]
fn a_daft_that_prints_nothing_is_refused() {
    let world = World::new();
    refused(
        &world,
        json!({"make": "nothing"}),
        &world.root.join("forks"),
        40,
        "printed no path",
    );
}

#[test]
fn a_daft_that_hangs_is_stopped() {
    let world = World::new();
    let made = world.root.join("forks/slow");
    world.daft(json!({"sleep": 120, "print": made}));
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
    assert_never_ran(&world, &answer, &made);
    let pid: i64 = fs::read_to_string(world.bin.join("daft.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!alive(pid), "daft {pid} is still running");
}

#[test]
fn a_git_cut_that_fails_its_pin_is_refused_and_left_where_it_is() {
    let world = World::new();
    // A git, pinned outside the workspace, that cuts as git does but says
    // the new worktree's git directory is not where git's layout puts it.
    let real = common::real("git");
    let wrap = world.root.join("wrap");
    world.script_at(
        &wrap.join("git"),
        &format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  [ \"$arg\" = --absolute-git-dir ] && \
             echo / && exit 0\ndone\nexec '{}' \"$@\"\n",
            real.display()
        ),
    );
    world.pin_tool("git", Some(&wrap.join("git")));
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 33, "{}", answer.json);
    let left = world.state.join("worktrees").join(answer.run_id());
    assert!(
        answer.message().contains(path_str(&left)),
        "the refusal does not name the worktree: {}",
        answer.json
    );
    assert!(answer.message().contains("left"), "{}", answer.json);
    assert!(left.is_dir(), "the worktree was not left where it is");
    assert_never_ran(&world, &answer, &left);
}

#[test]
fn a_git_fork_runs_no_hooks() {
    let world = World::new();
    let cut_by_hand = |name: &str| {
        world.git(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            path_str(&world.root.join(name)),
            "HEAD",
        ]);
    };

    // In the repository's own hooks directory.
    world.hook_that_marks(&world.work.join(".git/hooks"));
    cut_by_hand("control-a");
    assert!(world.hook_ran(), "the control never ran the hook");
    world.clear_hook_mark();
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(
        Path::new(answer.data()["worktree"].as_str().unwrap())
            .join("callee-ran.txt")
            .is_file()
    );
    assert!(!world.hook_ran(), "the hook in .git/hooks ran");

    // In the tree itself, where `core.hooksPath` points — content any agent
    // that can commit could have written.
    world.hook_that_marks(&world.work.join(".githooks"));
    world.git(&["add", ".githooks"]);
    world.git(&["commit", "-q", "-m", "chore: hooks in the tree"]);
    world.git(&["config", "core.hooksPath", ".githooks"]);
    cut_by_hand("control-b");
    assert!(world.hook_ran(), "the control never ran the hook");
    world.clear_hook_mark();
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(!world.hook_ran(), "the hook core.hooksPath names ran");
}

#[test]
fn a_daft_inside_the_workspace_is_not_run() {
    let world = World::new();
    // The fake is on PATH too, behind the planted one, which is the daft
    // config.toml names: if it were let through, it — not the fake — would
    // be what ran.
    world.daft(json!({"make": "worktree", "print": world.root.join("forks/f")}));
    let marker = world.root.join("planted-daft-ran");
    let planted = world.work.join("bin/daft");
    world.script_at(
        &planted,
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    world.prefix_path(&world.work.join("bin"));
    world.fork(&format!(
        "fork.provider = \"daft\"\nfork.daft.binary = {planted:?}"
    ));
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 33, "{}", answer.json);
    assert!(
        answer.message().contains("refusing to run `daft`"),
        "{}",
        answer.json
    );
    assert_never_ran(&world, &answer, &world.root.join("forks/f"));
    assert!(!marker.exists(), "the planted daft ran");
    assert!(world.daft_calls().is_empty(), "a daft ran");
}

#[test]
fn a_git_inside_the_workspace_on_path_is_never_run() {
    let world = World::new();
    let marker = world.root.join("planted-git-ran");
    world.script_at(
        &world.work.join("bin/git"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    world.prefix_path(&world.work.join("bin"));
    // First on PATH is nothing: the pinned git runs, and this one never.
    let answer = world.run("hello", &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(!marker.exists(), "the planted git ran");
}

/// A `git` planted at the top of the repository, and pinned there, that
/// leaves a mark when it runs.
fn plant_git(world: &World) -> PathBuf {
    let marker = world.root.join("planted-git-ran");
    world.script_at(
        &world.work.join("bin/git"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    world.pin_tool("git", Some(&world.work.join("bin/git")));
    marker
}

/// `cahoots run --role advise` from `dir`.
fn advise_from(world: &World, dir: &Path) -> Answer {
    let brief = world.brief("hello");
    let mut command = world.cahoots();
    command.current_dir(dir).args([
        "run",
        "--role",
        "advise",
        "--caller",
        "claude",
        "--brief",
        path_str(&brief),
    ]);
    common::answer(&mut command)
}

#[test]
fn a_git_at_the_top_of_the_repository_is_not_run_from_below_it() {
    let world = World::new();
    let marker = plant_git(&world);
    let below = world.work.join("sub");
    fs::create_dir_all(&below).unwrap();
    let answer = advise_from(&world, &below);
    assert_eq!(answer.code, 33, "{}", answer.json);
    assert!(
        answer.message().contains("inside the workspace"),
        "{}",
        answer.json
    );
    assert!(!marker.exists(), "the planted git ran");
    assert!(!world.state.join("runs").exists(), "a run was created");
}

#[test]
fn a_dot_git_that_git_looks_past_does_not_hide_the_repository_around_it() {
    // Each is a `.git` git does not take for a repository: it looks on up,
    // and finds the one around it, where the planted git is.
    // The last two have all a git directory needs but a HEAD git takes: one
    // whose ref begins past the 255 bytes git reads of it, and one whose ref
    // follows a form feed, which is no space to git.
    let far = format!("ref:{}refs/heads/main\n", " ".repeat(251));
    for mask in [
        "an empty directory",
        "a directory with only a HEAD",
        "a file naming nothing",
        "a ref past git's read",
        "a ref after a form feed",
    ] {
        let world = World::new();
        let marker = plant_git(&world);
        let below = world.work.join("sub");
        let dot_git = below.join(".git");
        let full = |head: &str| {
            for part in ["objects", "refs"] {
                fs::create_dir_all(dot_git.join(part)).unwrap();
            }
            fs::write(dot_git.join("HEAD"), head).unwrap();
        };
        match mask {
            "an empty directory" => fs::create_dir_all(&dot_git).unwrap(),
            "a directory with only a HEAD" => {
                fs::create_dir_all(&dot_git).unwrap();
                fs::write(dot_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
            }
            "a file naming nothing" => {
                fs::create_dir_all(&below).unwrap();
                fs::write(&dot_git, "gitdir: nowhere\n").unwrap();
            }
            "a ref past git's read" => full(&far),
            _ => full("ref:\u{c}refs/heads/main\n"),
        }
        // The control: git itself looks past it, to the repository around.
        let top = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&below)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        if mask != "a file naming nothing" {
            assert_eq!(
                String::from_utf8_lossy(&top.stdout).trim(),
                path_str(&world.work),
                "{mask}: git did not look past it"
            );
        }
        let answer = advise_from(&world, &below);
        assert_eq!(answer.code, 33, "{mask}: {}", answer.json);
        assert!(!marker.exists(), "{mask}: the planted git ran");
        assert!(
            !world.state.join("runs").exists(),
            "{mask}: a run was created"
        );
    }
}

#[test]
fn a_ps_in_the_workspace_on_path_is_never_run() {
    let world = World::new();
    let marker = world.root.join("planted-ps-ran");
    world.script_at(
        &world.work.join("bin/ps"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    world.prefix_path(&world.work.join("bin"));
    let answer = world.run("hello", &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(!marker.exists(), "the planted ps ran");
    // The pinned ps ran instead, and said when the callee started.
    assert!(!world.record(&answer.run_id())["callee_started"].is_null());
}

#[test]
fn a_harness_asked_its_version_finds_nothing_in_the_workspace() {
    let world = World::new();
    // Claude Code as a launcher installs it: a script that runs a tool of
    // its own, found on PATH, then answers.
    world.script_at(
        &world.bin.join("claude"),
        "#!/bin/sh\nharness-helper >/dev/null 2>&1\necho '2.1.278 (Claude Code)'\n",
    );
    let marker = world.root.join("planted-helper-ran");
    world.script_at(
        &world.work.join("bin/harness-helper"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    world.prefix_path(&world.work.join("bin"));
    // Picking runs the target's `--version`, and nothing else of it.
    let answer = world.ask(&["pick", "--role", "advise", "--caller", "codex"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.data()["target"]["harness"], "claude");
    assert!(
        !marker.exists(),
        "the harness's version check ran a tool from the workspace"
    );
}

#[test]
fn daft_gets_no_workspace_directory_on_its_path() {
    let world = World::new();
    world.daft(json!({"make": "worktree", "print": world.root.join("forks/f")}));
    // A directory of the workspace's, and the world's bin, first on the
    // caller's PATH: neither is daft's for being there.
    let work_bin = world.work.join("bin");
    fs::create_dir_all(&work_bin).unwrap();
    fs::write(work_bin.join("README"), "not a tool\n").unwrap();
    world.prefix_path(&work_bin);
    world.prefix_path(&world.bin);

    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let calls = world.daft_calls();
    let path = calls[0]["path"].as_str().expect("daft was given a PATH");
    // The pinned git's directories, then daft's own, then the system's:
    // nothing of the caller's PATH.
    let git = common::real("git");
    assert_eq!(
        path,
        common::own_path(&[&git, &world.bin.join("daft")], &[]),
        "{path}"
    );
}

// ── No filter runs during the cut ───────────────────────────────────────────
//
// A filter driver the configuration names, for a path the attributes give
// it, would run during the cut's checkout, outside every sandbox — and no
// commit is needed to plant one: `.git/config` and `.git/info/attributes`
// both apply to a new worktree. Each case first shows the route is live: a
// worktree cut by hand runs the filter.

/// Where a planted filter leaves its mark, the first time it runs.
fn filter_mark(world: &World) -> PathBuf {
    world.root.join("filter-ran")
}

/// A filter command that marks that it ran, and smudges what it is given.
fn filter_script(world: &World) -> PathBuf {
    let script = world.root.join("filter");
    world.script_at(
        &script,
        &format!(
            "#!/bin/sh\n[ -e '{mark}' ] || touch '{mark}'\necho smudged\n",
            mark = filter_mark(world).display()
        ),
    );
    script
}

/// A file a filter converts on checkout: the empty first commit has none.
fn commit_data(world: &World) {
    fs::write(world.work.join("data.txt"), "stored\n").unwrap();
    world.git(&["add", "data.txt"]);
    world.git(&["commit", "-q", "-m", "chore: data"]);
}

/// `* filter=<name>` for every path, in `.git/info/attributes`: no commit.
fn attribute_everything(world: &World, name: &str) {
    let info = world.work.join(".git/info");
    fs::create_dir_all(&info).unwrap();
    let mut attributes = fs::read_to_string(info.join("attributes")).unwrap_or_default();
    attributes.push_str(&format!("* filter={name}\n"));
    fs::write(info.join("attributes"), attributes).unwrap();
}

/// Adds `bytes` to the end of the file at `path`, as they are.
fn append_bytes(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

/// The control: a worktree cut by hand runs the planted filter. Its exit is
/// not asked — a process filter that speaks no protocol fails the checkout,
/// after it ran.
fn the_route_is_live(world: &World, case: &str) {
    let _ = std::process::Command::new("git")
        .args([
            "worktree",
            "add",
            "-q",
            "--detach",
            path_str(&world.root.join("control")),
            "HEAD",
        ])
        .current_dir(&world.work)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        filter_mark(world).exists(),
        "{case}: the control never ran the filter"
    );
    fs::remove_file(filter_mark(world)).unwrap();
}

/// Whether the planted filter ran before the writer did: during the cut.
fn ran_before_the_writer(world: &World, worktree: &Path) -> bool {
    let Ok(mark) = fs::metadata(filter_mark(world)) else {
        return false;
    };
    let writer = fs::metadata(worktree.join("callee-ran.txt")).unwrap();
    mark.modified().unwrap() <= writer.modified().unwrap()
}

#[test]
fn a_git_cut_runs_no_filter_the_repository_planted() {
    for case in [
        "a smudge in .git/config, for every path in .git/info/attributes",
        "a smudge in .git/config, for every path in a committed .gitattributes",
        "a process in .git/config",
        "a smudge in the worktree's own config",
        "a smudge in an included file, under a mixed-case dotted name",
        "a smudge under a name that is not UTF-8",
    ] {
        let world = World::new();
        let script = filter_script(&world);
        commit_data(&world);
        let script = path_str(&script);
        let mut name = "evil";
        match case {
            "a smudge in .git/config, for every path in .git/info/attributes" => {
                world.git(&["config", "filter.evil.smudge", script]);
                attribute_everything(&world, name);
            }
            "a smudge in .git/config, for every path in a committed .gitattributes" => {
                fs::write(world.work.join(".gitattributes"), "* filter=evil\n").unwrap();
                world.git(&["add", ".gitattributes"]);
                world.git(&["commit", "-q", "-m", "chore: attributes"]);
                world.git(&["config", "filter.evil.smudge", script]);
            }
            "a process in .git/config" => {
                world.git(&["config", "filter.evil.process", script]);
                attribute_everything(&world, name);
            }
            "a smudge in the worktree's own config" => {
                world.git(&["config", "extensions.worktreeConfig", "true"]);
                world.git(&["config", "--worktree", "filter.evil.smudge", script]);
                world.git(&["config", "--worktree", "filter.evil.required", "true"]);
                attribute_everything(&world, name);
            }
            "a smudge under a name that is not UTF-8" => {
                // Not text: a name made into text would turn off another.
                name = "not UTF-8";
                append_bytes(
                    &world.work.join(".git/config"),
                    &[
                        &b"[filter \"ev\xffil\"]\n\tsmudge = "[..],
                        script.as_bytes(),
                        b"\n\trequired = true\n",
                    ]
                    .concat(),
                );
                fs::create_dir_all(world.work.join(".git/info")).unwrap();
                append_bytes(
                    &world.work.join(".git/info/attributes"),
                    b"* filter=ev\xffil\n",
                );
            }
            _ => {
                name = "Inc.Name";
                let included = world.root.join("included.gitconfig");
                fs::write(
                    &included,
                    format!("[filter \"Inc.Name\"]\n\tsmudge = {script}\n\trequired = true\n"),
                )
                .unwrap();
                world.git(&["config", "include.path", path_str(&included)]);
                attribute_everything(&world, name);
            }
        }
        // Required: a filter that is turned off but still required fails
        // the checkout.
        if !case.contains("worktree's own") && name == "evil" {
            world.git(&["config", "filter.evil.required", "true"]);
        }
        the_route_is_live(&world, case);

        let answer = fork(&world, &[]);
        assert_eq!(answer.code, 0, "{case}: {}", answer.json);
        let worktree = PathBuf::from(answer.data()["worktree"].as_str().unwrap());
        assert_eq!(
            fs::read_to_string(worktree.join("data.txt")).unwrap(),
            "stored\n",
            "{case}: the file is not as git stores it"
        );
        if case == "a process in .git/config" {
            // A process filter cleans too, and what reads the writer's
            // change afterwards honours a clean filter (docs/THREAT-MODEL.md,
            // Writers): only the cut is held here.
            assert!(
                !ran_before_the_writer(&world, &worktree),
                "{case}: the filter ran during the cut"
            );
        } else {
            assert!(
                !filter_mark(&world).exists(),
                "{case}: a planted filter ran"
            );
        }
    }
}

#[test]
fn a_daft_cut_runs_no_filter_the_repository_planted() {
    let world = World::new();
    let script = filter_script(&world);
    commit_data(&world);
    world.git(&["config", "filter.evil.smudge", path_str(&script)]);
    world.git(&["config", "filter.evil.required", "true"]);
    attribute_everything(&world, "evil");
    the_route_is_live(&world, "daft");

    let f1 = world.root.join("forks/f1");
    world.daft(json!({"make": "worktree", "print": f1}));
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(!filter_mark(&world).exists(), "a planted filter ran");
    assert_eq!(fs::read_to_string(f1.join("data.txt")).unwrap(), "stored\n");
    // The git daft runs is told, above every config file, that the filter
    // has no command and is not required.
    let config = &world.daft_calls()[0]["git_config"];
    let count: usize = config["GIT_CONFIG_COUNT"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let told: Vec<(String, String)> = (0..count)
        .map(|n| {
            (
                config[format!("GIT_CONFIG_KEY_{n}")]
                    .as_str()
                    .unwrap()
                    .to_string(),
                config[format!("GIT_CONFIG_VALUE_{n}")]
                    .as_str()
                    .unwrap()
                    .to_string(),
            )
        })
        .collect();
    assert_eq!(told[0].0, "core.hooksPath");
    assert_eq!(told[1], ("core.fsmonitor".into(), "false".into()));
    for (var, value) in [
        ("smudge", ""),
        ("clean", ""),
        ("process", ""),
        ("required", "false"),
    ] {
        let setting = (format!("filter.evil.{var}"), value.to_string());
        assert!(told[2..].contains(&setting), "{setting:?} in {told:?}");
    }
}

#[test]
fn a_filter_that_appears_during_the_cut_fails_the_run() {
    let world = World::new();
    let script = filter_script(&world);
    commit_data(&world);
    let f1 = world.root.join("forks/f1");
    // A process writing the repository's git configuration while daft cuts:
    // the filter is named after the configuration was read.
    world.daft(json!({"make": "worktree", "print": f1, "plant_filter": script}));
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 33, "{}", answer.json);
    assert!(
        answer.message().contains("gained a filter (planted)"),
        "{}",
        answer.json
    );
    assert!(
        answer
            .message()
            .contains(&format!("{} is left for a person to remove", f1.display())),
        "{}",
        answer.json
    );
    assert!(answer.data()["worktree_owner"].is_null(), "{}", answer.json);
    assert_never_ran(&world, &answer, &f1);
    assert!(f1.is_dir(), "what was cut is left where it is");
    // The residual, held visible: in that window the filter could run.
    assert!(
        filter_mark(&world).exists(),
        "the planted filter never ran: this test shows nothing"
    );
}

#[test]
fn an_in_place_writer_cannot_plant_a_filter_for_the_next_cut() {
    let world = World::new();
    world.configure("limits.allow_in_place = true");
    let script = filter_script(&world);
    commit_data(&world);
    // A writer let into the caller's own tree, `.git` and all, names a
    // filter for every path of the repository's.
    let brief = world.brief(&format!(
        "FAKE: append=.git/config::[filter \"evil\"]\\n\\tsmudge = {}\\n\\trequired = true\\n\n\
         FAKE: append=.git/info/attributes::* filter=evil\\n",
        script.display()
    ));
    let planted = world.ask(&[
        "run",
        "--role",
        "implement",
        "--caller",
        "claude",
        "--in-place",
        "--brief",
        path_str(&brief),
    ]);
    assert_eq!(planted.code, 0, "{}", planted.json);
    the_route_is_live(&world, "in place");

    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(!filter_mark(&world).exists(), "the planted filter ran");
    let worktree = PathBuf::from(answer.data()["worktree"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(worktree.join("data.txt")).unwrap(),
        "stored\n"
    );
}

/// An include git reads for each worktree apart, naming a filter for a new
/// linked worktree alone: in the tree the worktree is cut from, nothing
/// names it. In the repository's own `.git/config`, where a test can put
/// it, it stands in for one in a person's own config, which a test may not
/// touch. The control shows the route is live on this machine's git.
fn plant_a_filter_for_new_worktrees_alone(world: &World) {
    let script = filter_script(world);
    commit_data(world);
    let included = world.root.join("per-worktree.gitconfig");
    fs::write(
        &included,
        format!(
            "[filter \"wt\"]\n\tsmudge = {}\n\trequired = true\n",
            script.display()
        ),
    )
    .unwrap();
    world.git(&[
        "config",
        "includeIf.gitdir:**/worktrees/**.path",
        path_str(&included),
    ]);
    attribute_everything(world, "wt");
    let listed = std::process::Command::new("git")
        .args(["config", "--list"])
        .current_dir(&world.work)
        .env_remove("GIT_DIR")
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&listed.stdout).contains("filter.wt"),
        "the base's own reading names the filter: this test shows nothing"
    );
    the_route_is_live(world, "for new worktrees alone");
}

#[test]
fn a_git_cut_turns_off_a_filter_only_the_new_worktree_names() {
    // git cuts with no checkout, reads the configuration as git reads it in
    // the new worktree, and checks out with what it names turned off.
    let world = World::new();
    plant_a_filter_for_new_worktrees_alone(&world);
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(!filter_mark(&world).exists(), "the filter ran");
    let worktree = PathBuf::from(answer.data()["worktree"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(worktree.join("data.txt")).unwrap(),
        "stored\n"
    );
    // Exit 0 also means #29's check held: it was checked out at the commit
    // the run recorded.
    assert!(
        worktree.join("callee-ran.txt").is_file(),
        "the writer ran there"
    );
}

#[test]
fn a_daft_cut_turns_off_a_filter_behind_a_condition_that_does_not_match_yet() {
    // daft checks out as it cuts, so cahoots reads, before it runs, every
    // file an include could reach, as if every condition held.
    let world = World::new();
    plant_a_filter_for_new_worktrees_alone(&world);
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"make": "worktree", "print": f1}));
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(!filter_mark(&world).exists(), "the filter ran");
    assert_eq!(fs::read_to_string(f1.join("data.txt")).unwrap(), "stored\n");
    let told = &world.daft_calls()[0]["git_config"];
    let keys: Vec<&str> = told
        .as_object()
        .unwrap()
        .iter()
        .filter(|(name, _)| name.starts_with("GIT_CONFIG_KEY_"))
        .map(|(_, key)| key.as_str().unwrap())
        .collect();
    for var in ["smudge", "clean", "process", "required"] {
        let key = format!("filter.wt.{var}");
        assert!(keys.contains(&key.as_str()), "{key} in {keys:?}");
    }
}

#[test]
fn a_daft_cut_turns_off_a_filter_behind_a_condition_that_matches() {
    // A condition that holds here and in the new worktree alike.
    let world = World::new();
    let script = filter_script(&world);
    commit_data(&world);
    let included = world.root.join("everywhere.gitconfig");
    fs::write(
        &included,
        format!(
            "[filter \"all\"]\n\tsmudge = {}\n\trequired = true\n",
            script.display()
        ),
    )
    .unwrap();
    world.git(&["config", "includeIf.gitdir:**.path", path_str(&included)]);
    attribute_everything(&world, "all");
    let listed = std::process::Command::new("git")
        .args(["config", "--list"])
        .current_dir(&world.work)
        .env_remove("GIT_DIR")
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("filter.all.smudge"),
        "the condition does not hold here: this test shows nothing"
    );
    the_route_is_live(&world, "a condition that matches");
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"make": "worktree", "print": f1}));
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(!filter_mark(&world).exists(), "the filter ran");
    assert_eq!(fs::read_to_string(f1.join("data.txt")).unwrap(), "stored\n");
}

#[test]
fn a_daft_cut_is_refused_where_an_include_cannot_be_read() {
    // A file not read is a filter that might not be turned off: refused,
    // never skipped, though git itself would skip a missing one.
    let world = World::new();
    let missing = world.root.join("missing.gitconfig");
    world.git(&["config", "include.path", path_str(&missing)]);
    let f1 = world.root.join("forks/f1");
    world.daft(json!({"make": "worktree", "print": f1}));
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 33, "{}", answer.json);
    let message = answer.message();
    assert!(
        message.starts_with(&format!(
            "cannot cut a worktree: {} includes {}, which cannot be read (",
            world.work.join(".git/config").display(),
            missing.display()
        )),
        "{message}"
    );
    assert!(
        message.ends_with(
            "— daft checks a new worktree out before cahoots can see what such an include names \
             there, so every file an include could reach is read first"
        ),
        "{message}"
    );
    assert_never_ran(&world, &answer, &f1);
    assert!(world.daft_calls().is_empty(), "daft ran");

    // git cuts with no checkout and reads the configuration as git does in
    // the new worktree, where git skips a missing include: it cuts.
    world.fork("fork.provider = \"git\"");
    let answer = fork(&world, &[]);
    assert_eq!(answer.code, 0, "{}", answer.json);
}
