//! git, ps and every harness run only from the paths a person pinned, never
//! from a PATH lookup: the caller sets PATH, and cahoots runs outside every
//! sandbox (docs/THREAT-MODEL.md, Binaries). Each test plants or pins, drives
//! the binary, and looks at what ran — markers, and wrappers that log the
//! PATH and the directory each tool was started with.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::{Answer, World, path_str};
use serde_json::Value;

/// A reader's run from the world's repository, asked by codex: claude takes it.
fn reader(world: &World) -> Answer {
    world.run("hello", &["--caller", "codex"])
}

/// A writer's run in a fork, which must succeed.
fn writer(world: &World) -> Answer {
    world.writer_in(&world.work.clone(), "FAKE: write=x.txt", &[])
}

fn runs(world: &World) -> usize {
    fs::read_dir(world.state.join("runs")).map_or(0, |dir| dir.count())
}

fn check<'a>(doctor: &'a Answer, name: &str) -> &'a Value {
    doctor.data()["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == name)
        .unwrap_or_else(|| panic!("no {name} check: {}", doctor.json))
}

/// A script at `path` that logs its arguments to `<path>.args`, answers
/// `--version` with `says`, and leaves a mark for anything else.
fn imposter(world: &World, path: &Path, says: &str) {
    world.script_at(
        path,
        &format!(
            "#!/bin/sh\necho \"$*\" >> '{log}.args'\nif [ \"$1\" = --version ]; then echo '{says}'; \
             exit 0; fi\nexit 1\n",
            log = path.display()
        ),
    );
}

#[test]
fn a_git_planted_first_on_path_never_runs() {
    let world = World::new();
    let marker = world.planted("git");
    let run = reader(&world);
    assert_eq!(run.code, 0, "{}", run.json);
    let id = run.run_id();
    for args in [
        vec!["wait", &id],
        vec!["status", &id],
        vec!["result", &id],
        vec!["outcome", &id, "accepted"],
        vec!["pick", "--role", "advise", "--caller", "codex"],
    ] {
        let answer = world.ask(&args);
        assert_eq!(answer.code, 0, "{args:?}: {}", answer.json);
    }
    let fork = writer(&world);
    let worktree = fork.data()["worktree"].as_str().expect("a worktree");
    assert!(
        Path::new(worktree).starts_with(world.state.join("worktrees")),
        "{worktree}"
    );
    assert!(!marker.exists(), "the planted git ran");
}

#[test]
fn a_ps_planted_first_on_path_never_runs() {
    let world = World::new();
    let marker = world.planted("ps");
    let run = reader(&world);
    assert_eq!(run.code, 0, "{}", run.json);
    // The pinned ps ran, and said when the callee started.
    assert!(!world.record(&run.run_id())["callee_started"].is_null());
    assert!(!marker.exists(), "the planted ps ran");
}

#[test]
fn a_harness_planted_first_on_path_never_runs() {
    let world = World::new();
    let dir = world.root.join("planted");
    let marker = dir.join("claude.ran");
    world.script_at(
        &dir.join("claude"),
        &format!(
            "#!/bin/sh\ntouch '{}'\necho '2.1.278 (Claude Code)'\n",
            marker.display()
        ),
    );
    world.prefix_path(&dir);
    let pick = world.ask(&["pick", "--role", "advise", "--caller", "codex"]);
    assert_eq!(pick.code, 0, "{}", pick.json);
    assert_eq!(pick.data()["target"]["harness"], "claude");
    let run = reader(&world);
    assert_eq!(run.code, 0, "{}", run.json);
    assert!(!marker.exists(), "the planted claude ran");
}

#[test]
fn an_unpinned_harness_is_refused_and_never_looked_up() {
    let world = World::new();
    world.unpin_harness("claude");
    // The fake claude is still first on PATH, and logs any `--version`.
    world.prefix_path(&world.bin.clone());
    let asked = world.bin.join("claude.version-env");
    fs::write(&asked, "").unwrap();
    let pick = world.ask(&["pick", "--role", "advise", "--caller", "codex"]);
    assert_eq!(pick.code, 34, "{}", pick.json);
    assert!(
        pick.message().contains("`cahoots enable claude`")
            && pick.message().contains("harness.claude.binary"),
        "{}",
        pick.json
    );
    assert_eq!(fs::read_to_string(&asked).unwrap(), "", "claude was asked");

    // Among others, it is skipped with that code, and the next is taken.
    world.configure(
        "[roles.advise]\ncandidates = [\n  { harness = \"claude\", model = \"opus\", effort = \
         \"high\" },\n  { harness = \"codex\", model = \"gpt-6-astra\", effort = \"high\" },\n]",
    );
    let pick = world.ask(&["pick", "--role", "advise"]);
    assert_eq!(pick.code, 0, "{}", pick.json);
    assert_eq!(pick.data()["target"]["harness"], "codex");
    let skipped = &pick.data()["skipped"][0];
    assert_eq!(skipped["candidate"]["harness"], "claude", "{skipped}");
    assert_eq!(skipped["code"], 34, "{skipped}");
    assert_eq!(fs::read_to_string(&asked).unwrap(), "", "claude was asked");
}

/// Every agent verb that starts a tool, against a finished run `id`.
fn tool_verbs(world: &World, id: &str) -> Vec<Vec<String>> {
    let brief = world.brief("again");
    [
        vec!["pick", "--role", "advise", "--caller", "codex"],
        vec![
            "run",
            "--role",
            "advise",
            "--caller",
            "codex",
            "--brief",
            path_str(&brief),
        ],
        vec![
            "resume",
            id,
            "--caller",
            "codex",
            "--brief",
            path_str(&brief),
        ],
        vec!["wait", id],
        vec!["result", id],
        vec!["status"],
        vec!["status", id],
        vec!["cancel", id],
        vec!["outcome", id, "accepted"],
    ]
    .into_iter()
    .map(|args| args.into_iter().map(str::to_string).collect())
    .collect()
}

#[test]
fn a_missing_pin_refuses_every_agent_verb_that_starts_a_tool() {
    for (tool, key) in [("git", "tools.git.binary"), ("ps", "tools.ps.binary")] {
        let world = World::new();
        let id = reader(&world).run_id();
        world.pin_tool(tool, None);
        let before = runs(&world);
        for args in tool_verbs(&world, &id) {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let answer = world.ask(&args);
            assert_eq!(answer.code, 34, "{tool}: {args:?}: {}", answer.json);
            assert!(
                answer
                    .message()
                    .contains(&format!("cahoots needs `{tool}`"))
                    && answer.message().contains("cahoots install")
                    && answer.message().contains(key),
                "{tool}: {args:?}: {}",
                answer.json
            );
        }
        assert_eq!(runs(&world), before, "{tool}: a run was created");
        // A verb that starts no tool has no need of one.
        let notes = world.ask(&["notes", "--role", "advise"]);
        assert_eq!(notes.code, 0, "{tool}: {}", notes.json);
    }
}

#[test]
fn a_moved_tool_refuses_until_it_is_pinned_again() {
    for tool in ["git", "ps"] {
        let world = World::new();
        let (first, second) = (world.root.join("t1"), world.root.join("t2"));
        let (wrapper, _) = world.wrapper(&first, tool);
        world.pin_tool(tool, Some(&wrapper));
        let run = reader(&world);
        assert_eq!(run.code, 0, "{tool}: {}", run.json);
        assert!(
            !world.wrapper_log(&first, tool).is_empty(),
            "{tool} never ran"
        );

        fs::rename(&first, &second).unwrap();
        let run = reader(&world);
        assert_eq!(run.code, 34, "{tool}: {}", run.json);
        assert!(
            run.message().contains("No such file or directory")
                && run.message().contains("pin it again"),
            "{tool}: {}",
            run.json
        );

        let key = format!("tools.{tool}.binary");
        let moved = second.join(tool);
        let set = world
            .as_a_person(&["settings", "set", &key, path_str(&moved)])
            .finish();
        assert_eq!(set.code, 0, "{tool}: {}", set.text());
        let run = reader(&world);
        assert_eq!(run.code, 0, "{tool}: {}", run.json);
    }
}

#[test]
fn a_moved_harness_refuses_until_it_is_pinned_again() {
    let world = World::new();
    let (first, second) = (world.root.join("h1"), world.root.join("h2"));
    fs::create_dir_all(&first).unwrap();
    common::fake_at(&first.join("claude"));
    world.unpin_harness("claude");
    world.configure(&format!(
        "harness.claude.binary = {:?}",
        first.join("claude")
    ));
    let pick = || world.ask(&["pick", "--role", "advise", "--caller", "codex"]);
    assert_eq!(pick().code, 0);
    fs::rename(&first, &second).unwrap();
    let moved = pick();
    assert_eq!(moved.code, 31, "{}", moved.json);
    assert!(moved.message().contains("No such file"), "{}", moved.json);
    let set = world
        .as_a_person(&[
            "settings",
            "set",
            "harness.claude.binary",
            path_str(&second.join("claude")),
        ])
        .finish();
    assert_eq!(set.code, 0, "{}", set.text());
    assert_eq!(pick().code, 0);
}

#[test]
fn a_pinned_git_that_is_not_git_or_too_old_refuses() {
    for (says, refused) in [
        ("not-git 1.0", "does not identify itself as git"),
        (
            "git version 2.20.0",
            "older than the oldest version cahoots supports (2.31.0)",
        ),
    ] {
        let world = World::new();
        let git = world.root.join("odd/git");
        imposter(&world, &git, says);
        world.pin_tool("git", Some(&git));
        let run = reader(&world);
        assert_eq!(run.code, 34, "{says}: {}", run.json);
        assert!(run.message().contains(refused), "{says}: {}", run.json);
        let args = fs::read_to_string(git.with_extension("args")).unwrap_or_default();
        assert!(
            args.lines().all(|line| line == "--version"),
            "{says}: it ran for more than its version: {args}"
        );
    }
}

#[test]
fn a_pinned_ps_that_does_not_answer_as_ps_refuses() {
    let world = World::new();
    let ps = world.root.join("odd/ps");
    world.script_at(&ps, "#!/bin/sh\necho '1 1'\n");
    world.pin_tool("ps", Some(&ps));
    let run = reader(&world);
    assert_eq!(run.code, 34, "{}", run.json);
    assert!(
        run.message().contains("does not answer as ps"),
        "{}",
        run.json
    );
}

#[test]
fn a_pinned_git_inside_the_workspace_is_refused() {
    let world = World::new();
    let marker = world.root.join("planted-git-ran");
    let git = world.work.join("bin/git");
    world.script_at(&git, &format!("#!/bin/sh\ntouch '{}'\n", marker.display()));
    world.pin_tool("git", Some(&git));
    let run = reader(&world);
    assert_eq!(run.code, 33, "{}", run.json);
    assert!(
        run.message().contains("refusing to run `git`"),
        "{}",
        run.json
    );
    assert!(!marker.exists(), "the pinned git in the workspace ran");
    assert_eq!(runs(&world), 0, "a run was created");
}

#[test]
fn every_tool_runs_on_a_path_of_its_own() {
    let world = World::new();
    let tools = world.root.join("tools");
    let (git, _) = world.wrapper(&tools, "git");
    let (ps, _) = world.wrapper(&tools, "ps");
    world.pin_tool("git", Some(&git));
    world.pin_tool("ps", Some(&ps));
    world.planted("git");
    let work_bin = world.work.join("bin");
    fs::create_dir_all(&work_bin).unwrap();
    world.prefix_path(&work_bin);
    let fork = writer(&world);
    assert_eq!(world.ask(&["status", &fork.run_id()]).code, 0);
    let expected = common::own_path(&[&git], &[]);
    for tool in ["git", "ps"] {
        let log = world.wrapper_log(&tools, tool);
        assert!(!log.is_empty(), "{tool} never ran");
        for (path, _) in log {
            assert_eq!(path, expected, "{tool}");
        }
    }
}

#[test]
fn the_callee_gets_a_path_of_its_own() {
    let world = World::new();
    world.planted("git");
    let run = world.run("FAKE: dump", &["--caller", "codex"]);
    assert_eq!(run.code, 0, "{}", run.json);
    let dump: Value = serde_json::from_str(run.text()).unwrap();
    assert_eq!(
        dump["env"]["PATH"],
        common::own_path(&[&common::real("git"), &world.bin.join("claude")], &[]).as_str()
    );
}

#[test]
fn the_recorded_path_comes_last_and_never_from_the_caller() {
    let world = World::new();
    let (mine, gone, open) = (
        world.root.join("mine"),
        world.root.join("gone"),
        world.root.join("open"),
    );
    fs::create_dir_all(&mine).unwrap();
    fs::create_dir_all(&open).unwrap();
    fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
    let mine_git = world.root.join("mine-git.ran");
    world.script_at(
        &mine.join("git"),
        &format!("#!/bin/sh\ntouch '{}'\n", mine_git.display()),
    );
    world.configure(&format!(
        "tools.path = \"{}:{}:{}\"",
        mine.display(),
        gone.display(),
        open.display()
    ));
    let tools = world.root.join("tools");
    let (git, _) = world.wrapper(&tools, "git");
    world.pin_tool("git", Some(&git));
    world.planted("git");
    let run = world.run("FAKE: dump", &["--caller", "codex"]);
    assert_eq!(run.code, 0, "{}", run.json);
    let dump: Value = serde_json::from_str(run.text()).unwrap();
    assert_eq!(
        dump["env"]["PATH"],
        common::own_path(&[&git, &world.bin.join("claude")], &[&mine]).as_str()
    );
    for (path, _) in world.wrapper_log(&tools, "git") {
        assert_eq!(path, common::own_path(&[&git], &[&mine]));
    }
    assert!(!mine_git.exists(), "a git on the recorded PATH ran");
}

#[test]
fn a_harness_is_asked_its_version_from_root_on_the_callees_path() {
    let world = World::new();
    let asked = world.bin.join("claude.version-env");
    fs::write(&asked, "").unwrap();
    let pick = world.ask(&["pick", "--role", "advise", "--caller", "codex"]);
    assert_eq!(pick.code, 0, "{}", pick.json);
    let calls: Vec<Value> = fs::read_to_string(&asked)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!calls.is_empty());
    for call in calls {
        assert_eq!(call["cwd"], "/", "{call}");
        assert_eq!(
            call["path"],
            common::own_path(&[&common::real("git"), &world.bin.join("claude")], &[]).as_str()
        );
    }
}

#[test]
fn doctor_reports_each_pin() {
    let world = World::new();
    let doctor = world.ask(&["doctor"]);
    assert_eq!(doctor.code, 0, "{}", doctor.json);
    let git = check(&doctor, "tools: git");
    assert_eq!(git["status"], "ok", "{git}");
    let detail = git["detail"].as_str().unwrap();
    let version = detail
        .strip_prefix(path_str(&common::real("git")))
        .expect("the pinned git, then its version");
    assert!(version.trim().split('.').count() == 3, "{git}");
    let ps = check(&doctor, "tools: ps");
    assert_eq!(ps["status"], "ok", "{ps}");
    assert_eq!(ps["detail"], path_str(&common::real("ps")));

    world.pin_tool("git", None);
    let doctor = world.ask(&["doctor"]);
    assert_eq!(doctor.code, 34, "{}", doctor.json);
    let git = check(&doctor, "tools: git");
    assert_eq!(git["status"], "fail", "{git}");
    assert!(
        git["detail"].as_str().unwrap().contains("cahoots install"),
        "{git}"
    );

    world.pin_tool("git", Some(&common::real("git")));
    world.pin_tool("ps", Some(&world.root.join("moved/ps")));
    let doctor = world.ask(&["doctor"]);
    assert_eq!(check(&doctor, "tools: ps")["status"], "fail");

    world.pin_tool("ps", Some(&common::real("ps")));
    world.unpin_harness("claude");
    let doctor = world.ask(&["doctor"]);
    assert_eq!(check(&doctor, "claude: binary")["status"], "fail");
    world.enable(&["codex"]);
    let doctor = world.ask(&["doctor"]);
    assert_eq!(check(&doctor, "claude: binary")["status"], "warn");
}

#[test]
fn enable_pins_the_harness_it_finds_and_keeps_the_pin() {
    let world = World::bare();
    world.configure(&format!("tools.path = {:?}", world.bin));
    world.unpin_harness("claude");
    let file = world.config.join("config.toml");
    let after = world.as_a_person(&["enable", "claude"]).finish();
    assert_eq!(after.code, 0, "{}", after.text());
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.contains(&format!("binary = {:?}", world.bin.join("claude")))
            && text.contains("enabled = true"),
        "{text}"
    );
    assert!(after.text().contains("Pinned"), "{}", after.text());
    // Again: the pin is kept, and nothing is said of pinning.
    let again = world.as_a_person(&["enable", "claude"]).finish();
    assert_eq!(again.code, 0, "{}", again.text());
    assert!(!again.text().contains("Pinned"), "{}", again.text());
    assert_eq!(fs::read_to_string(&file).unwrap(), text);
}

#[test]
fn enable_with_no_harness_on_path_writes_nothing() {
    let world = World::bare();
    world.configure("");
    world.unpin_harness("claude");
    let file = world.config.join("config.toml");
    let before = fs::read(&file).unwrap();
    let empty = world.root.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let after = world
        .as_a_person_with(&["enable", "claude"], &[("PATH", path_str(&empty))])
        .finish();
    assert_eq!(after.code, 34, "{}", after.text());
    assert!(
        after.text().contains("cahoots settings"),
        "{}",
        after.text()
    );
    assert_eq!(fs::read(&file).unwrap(), before, "something was written");
}

#[test]
fn enable_records_the_path_once() {
    let world = World::bare();
    world.configure("");
    let file = world.config.join("config.toml");
    let after = world.as_a_person(&["enable", "claude"]).finish();
    assert_eq!(after.code, 0, "{}", after.text());
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.contains(&format!("path = {:?}", path_str(&world.bin))),
        "{text}"
    );
    let other = world.root.join("other");
    fs::create_dir_all(&other).unwrap();
    let path = format!("{}:{}", other.display(), world.bin.display());
    let again = world
        .as_a_person_with(&["enable", "codex"], &[("PATH", &path)])
        .finish();
    assert_eq!(again.code, 0, "{}", again.text());
    let now = fs::read_to_string(&file).unwrap();
    assert!(
        !now.contains(path_str(&other)),
        "the PATH was recorded again: {now}"
    );
}

// ── install pins, settings chooses ──────────────────────────────────────────

/// A world with nothing pinned at all: no git, no ps, no harness.
fn unpinned() -> World {
    let world = World::bare();
    world.pin_tool("git", None);
    world.pin_tool("ps", None);
    world.unpin_harness("claude");
    world.unpin_harness("codex");
    world.configure("");
    world
}

/// `cahoots install --meter none` at a terminal whose PATH is `path`: the
/// envelope.
fn install(world: &World, path: &str, extra: &[&str]) -> common::Finished {
    let mut args = vec!["install", "--meter", "none"];
    args.extend(extra);
    let after = world.at_terminal_with(&args, &[("PATH", path)]).finish();
    assert_eq!(after.code, 0, "{}", after.screen);
    after
}

#[test]
fn install_pins_git_ps_and_the_harnesses_it_finds() {
    let world = unpinned();
    let tools = world.root.join("w");
    let (git, _) = world.wrapper(&tools, "git");
    let (ps, _) = world.wrapper(&tools, "ps");
    let path = format!("{}:{}", tools.display(), world.bin.display());
    let after = install(&world, &path, &[]);
    let pins = &after.json["data"]["pins"];
    for (name, binary) in [
        ("git", &git),
        ("ps", &ps),
        ("claude", &world.bin.join("claude")),
        ("codex", &world.bin.join("codex")),
    ] {
        assert_eq!(pins[name]["decision"], "pinned", "{name}: {pins}");
        assert_eq!(pins[name]["binary"], path_str(binary), "{name}: {pins}");
    }
    let config = cahoots::config::UserConfig::load(&world.config.join("config.toml")).unwrap();
    assert_eq!(config.tools.git.unwrap().binary.as_deref(), Some(&*git));
    assert_eq!(config.tools.ps.unwrap().binary.as_deref(), Some(&*ps));
    for id in ["claude", "codex"] {
        let id: cahoots::model::HarnessId = id.parse().unwrap();
        assert!(config.harness[&id].binary.is_some(), "{id}");
    }
}

#[test]
fn the_tools_section_chooses_git() {
    let world = World::new();
    world.pin_tool("git", None);
    let (git, _) = world.wrapper(&world.bin.clone(), "git");
    let terminal = world.at_terminal(&["settings"]);
    terminal.wait_for("❯ Enabled");
    terminal.resize(40, 180);
    // Claude, Codex, meter, runs, worktrees, then the tools.
    for _ in 0..5 {
        terminal.press(b"\t");
    }
    terminal.wait_for("❯ git program");
    terminal.wait_for("Not chosen: agents' runs are refused until one is.");
    terminal.press(b"\r");
    terminal.wait_for("Tools · git program");
    terminal.wait_for("↑↓ choose · enter save");
    terminal.press(b"\r");
    terminal.wait_for("Saved to config.toml: [tools.git] binary =");
    terminal.press(b"\x1b");
    let after = terminal.finish();
    assert_eq!(after.code, 0, "{}", after.screen);
    let config = cahoots::config::UserConfig::load(&world.config.join("config.toml")).unwrap();
    assert_eq!(config.tools.git.unwrap().binary.as_deref(), Some(&*git));
}

#[test]
fn install_keeps_pins_that_still_run_and_repins_one_that_moved() {
    let world = unpinned();
    let (first, second) = (world.root.join("w1"), world.root.join("w2"));
    world.wrapper(&first, "git");
    world.wrapper(&first, "ps");
    let path = |dir: &Path| format!("{}:{}", dir.display(), world.bin.display());
    install(&world, &path(&first), &[]);
    let again = install(&world, &path(&first), &[]);
    assert_eq!(again.json["data"]["pins"]["git"]["decision"], "kept");

    fs::rename(&first, &second).unwrap();
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    let dry = install(&world, &path(&second), &["--dry-run"]);
    assert_eq!(dry.json["data"]["pins"]["git"]["decision"], "repinned");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        before,
        "a dry run wrote"
    );

    let moved = install(&world, &path(&second), &[]);
    let git = &moved.json["data"]["pins"]["git"];
    assert_eq!(git["decision"], "repinned", "{git}");
    assert_eq!(git["was"], path_str(&first.join("git")), "{git}");
    assert_eq!(git["binary"], path_str(&second.join("git")), "{git}");
    assert!(
        git["why"].as_str().unwrap().contains("No such file"),
        "{git}"
    );
}

#[test]
fn install_with_no_git_on_path_says_what_is_left_to_do() {
    let world = unpinned();
    let after = world
        .as_a_person_with(
            &["install", "--meter", "none"],
            &[("PATH", path_str(&world.bin))],
        )
        .finish();
    assert_eq!(after.code, 0, "{}", after.text());
    assert!(
        after
            .text()
            .contains("No git is pinned, so agents' runs are refused until one is"),
        "{}",
        after.text()
    );
    let enveloped = install(&world, path_str(&world.bin), &[]);
    let git = &enveloped.json["data"]["pins"]["git"];
    assert_eq!(git["decision"], "none_found", "{git}");
    assert_eq!(git["why"], "no `git` on your PATH", "{git}");
}

#[test]
fn install_records_the_path_and_drops_what_does_not_qualify() {
    let world = unpinned();
    let (a, b, open) = (
        world.root.join("a"),
        world.root.join("b"),
        world.root.join("open"),
    );
    for dir in [&a, &b, &open] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
    let path = [
        path_str(&a),
        "relative",
        path_str(&world.root.join("missing")),
        path_str(&open),
        "/tmp",
        "/usr/bin",
        path_str(&a),
    ]
    .join(":");
    let after = install(&world, &path, &[]);
    let recorded = &after.json["data"]["pins"]["path"];
    assert_eq!(recorded["decision"], "recorded", "{recorded}");
    assert_eq!(recorded["path"], path_str(&a), "{recorded}");
    let why = |dir: &str| {
        recorded["dropped"]
            .as_array()
            .unwrap()
            .iter()
            .find(|dropped| dropped["dir"] == dir)
            .map(|dropped| dropped["why"].as_str().unwrap().to_string())
    };
    assert_eq!(why("relative").as_deref(), Some("not absolute"));
    assert_eq!(
        why(path_str(&world.root.join("missing"))).as_deref(),
        Some("not a directory")
    );
    assert_eq!(why(path_str(&open)).as_deref(), Some("writable by others"));
    assert_eq!(why("/tmp").as_deref(), Some("a temp directory"));
    assert_eq!(why("/usr/bin").as_deref(), Some("already searched"));
    assert_eq!(
        recorded["dropped"].as_array().unwrap().len(),
        5,
        "{recorded}"
    );

    let again = install(&world, &format!("{path}:{}", b.display()), &[]);
    let recorded = &again.json["data"]["pins"]["path"];
    assert_eq!(recorded["decision"], "rerecorded", "{recorded}");
    assert_eq!(recorded["added"], serde_json::json!([path_str(&b)]));
}

#[test]
fn install_records_a_config_home_the_terminal_sets_and_never_overwrites_it() {
    let world = unpinned();
    let (home, other) = (world.root.join("ch"), world.root.join("other"));
    for dir in [&home, &other] {
        fs::create_dir_all(dir).unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let at = |codex_home: &Path| {
        let after = world
            .at_terminal_with(
                &["install", "--meter", "none"],
                &[
                    ("PATH", path_str(&world.bin)),
                    ("CODEX_HOME", path_str(codex_home)),
                ],
            )
            .finish();
        assert_eq!(after.code, 0, "{}", after.screen);
        after.json["data"]["pins"]["homes"]["codex"].clone()
    };
    let first = at(&home);
    assert_eq!(first["decision"], "recorded", "{first}");
    let file = world.config.join("config.toml");
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.contains(&format!("home = {:?}", path_str(&home))),
        "{text}"
    );
    let second = at(&other);
    assert_eq!(second["decision"], "kept", "{second}");
    assert_eq!(fs::read_to_string(&file).unwrap(), text);
}

#[test]
fn a_tool_or_harness_program_that_does_not_locate_is_refused() {
    let world = World::new();
    let not_git = world.root.join("odd/git");
    world.script_at(&not_git, "#!/bin/sh\necho 'not-git 1.0'\n");
    let sticky = world.root.join("sticky");
    fs::create_dir_all(&sticky).unwrap();
    let sticky_claude = sticky.join("claude");
    common::fake_at(&sticky_claude);
    fs::set_permissions(&sticky, fs::Permissions::from_mode(0o1777)).unwrap();
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    for (key, value) in [
        ("tools.git.binary", "relative/git".to_string()),
        (
            "tools.git.binary",
            path_str(&world.root.join("missing/git")).to_string(),
        ),
        (
            "tools.git.binary",
            path_str(&world.bin.join("codex")).to_string(),
        ),
        ("tools.ps.binary", path_str(&not_git).to_string()),
        ("harness.claude.binary", path_str(&not_git).to_string()),
        // A real claude, in a directory everyone can write (sticky, so the
        // binary policy alone would let it run): held to what install pins.
        (
            "harness.claude.binary",
            path_str(&sticky_claude).to_string(),
        ),
    ] {
        let after = world
            .as_a_person(&["settings", "set", key, &value])
            .finish();
        assert_eq!(after.code, 2, "{key} = {value}: {}", after.text());
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            before,
            "{key} = {value}"
        );
    }
}

#[test]
fn a_path_or_home_that_does_not_qualify_is_refused() {
    let world = World::new();
    let a = world.root.join("a");
    let open = world.root.join("open");
    for dir in [&a, &open] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
    let file = world.config.join("config.toml");
    let before = fs::read_to_string(&file).unwrap();
    for (key, value, says) in [
        (
            "tools.path",
            format!("/tmp/x:{}", a.display()),
            "a temp directory",
        ),
        ("harness.codex.home", "relative".to_string(), "absolute"),
        (
            "harness.codex.home",
            path_str(&open).to_string(),
            "writable",
        ),
    ] {
        let after = world
            .as_a_person(&["settings", "set", key, &value])
            .finish();
        assert_eq!(after.code, 2, "{key} = {value}: {}", after.text());
        assert!(
            after.text().contains(says),
            "{key} = {value}: {}",
            after.text()
        );
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            before,
            "{key} = {value}"
        );
    }
    // One that qualifies is set.
    let set = world
        .as_a_person(&["settings", "set", "tools.path", path_str(&a)])
        .finish();
    assert_eq!(set.code, 0, "{}", set.text());
}

#[test]
fn install_takes_nothing_from_the_repository_it_is_run_in() {
    // A repository's own bin first on the person's PATH, as direnv or mise
    // put it: agents working there can write it.
    let world = unpinned();
    let marker = world.root.join("repo-git.ran");
    world.script_at(
        &world.work.join("bin/git"),
        &format!(
            "#!/bin/sh\ntouch '{}'\necho 'git version 2.50.1'\n",
            marker.display()
        ),
    );
    let tools = world.root.join("w");
    let (git, _) = world.wrapper(&tools, "git");
    let path = format!(
        "{}:{}:{}",
        world.work.join("bin").display(),
        tools.display(),
        world.bin.display()
    );
    let after = install(&world, &path, &[]);
    let pins = &after.json["data"]["pins"];
    assert_eq!(pins["git"]["binary"], path_str(&git), "{pins}");
    assert!(!marker.exists(), "install ran the repository's git");
    let dropped = pins["path"]["dropped"].as_array().unwrap();
    assert!(
        dropped
            .iter()
            .any(|d| d["dir"] == path_str(&world.work.join("bin"))
                && d["why"] == "inside the workspace"),
        "{pins}"
    );
    assert!(
        !pins["path"]["path"]
            .as_str()
            .unwrap()
            .contains(path_str(&world.work)),
        "{pins}"
    );
}
