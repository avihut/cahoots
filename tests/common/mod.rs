//! A throwaway world for one test: fake `claude` and `codex` binaries, a
//! config, a state and a data directory of its own, and a small git
//! repository to stand in. Nothing here touches the real `~/.config`,
//! `~/.local/state` or `~/.local/share`: the binary under test is a dev
//! build, and every command gets every directory override.
#![allow(dead_code)]

use std::cell::RefCell;
use std::fs;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use assert_cmd::Command;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::{Winsize, openpty};
use nix::sys::termios::{Termios, tcgetattr};
use serde_json::Value;

pub struct World {
    _root: tempfile::TempDir,
    /// Where all of this world lives: its own directory, canonical.
    pub root: PathBuf,
    pub bin: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    /// What a person keeps: the eval suite.
    pub data: PathBuf,
    /// Stands in for the user's home, where the agent homes live.
    pub home: PathBuf,
    pub work: PathBuf,
    /// The targets `enable` last named, and what `configure` was last given:
    /// config.toml is written from both.
    enabled: RefCell<Vec<String>>,
    extra: RefCell<String>,
    /// What cuts a writer's worktree: dotted `fork.*` keys, written before
    /// `extra` so that neither a `configure` nor a table in it can take them.
    fork: RefCell<String>,
    /// Directories put before the inherited PATH, first one first: `daft`
    /// puts `bin` there, `prefix_path` anything else.
    path_prefix: RefCell<Vec<PathBuf>>,
}

/// The `fake_harness` example, which cargo builds next to the test binaries.
pub fn fake_harness() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let path = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/fake_harness");
    assert!(
        path.is_file(),
        "{} is missing — run the suite with `cargo test`",
        path.display()
    );
    path
}

/// Puts the fake at `path` as a new file, the way an update replaces a
/// binary, written by `cp` rather than by this process. Rewritten in place
/// just after it ran, a binary's next run on macOS failed now and then.
/// Written here, it is open for writing while other tests start processes;
/// each of those children holds the handle until its own exec, and Linux
/// refuses to run a file anything has open for writing ("Text file busy"),
/// so the fake's first run failed now and then too. In `cp`, the handle is in
/// no process another test can fork, and it is closed once `cp` exits.
pub fn fake_at(path: &Path) {
    let _ = fs::remove_file(path);
    let copied = StdCommand::new("cp")
        .arg(fake_harness())
        .arg(path)
        .status()
        .unwrap();
    assert!(
        copied.success(),
        "cp could not put a fake at {}",
        path.display()
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

pub struct Answer {
    pub code: i32,
    pub json: Value,
}

impl Answer {
    pub fn data(&self) -> &Value {
        &self.json["data"]
    }
    pub fn run_id(&self) -> String {
        self.data()["run"].as_str().expect("a run id").to_string()
    }
    pub fn text(&self) -> &str {
        self.data()["result"]["text"].as_str().unwrap_or_default()
    }
    pub fn message(&self) -> &str {
        self.json["message"].as_str().unwrap_or_default()
    }
}

impl World {
    /// Both harnesses enabled, fast kill ladder.
    pub fn new() -> World {
        let world = World::bare();
        world.enable(&["claude", "codex"]);
        world.configure("");
        world
    }

    pub fn bare() -> World {
        let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let base = fs::canonicalize(root.path()).unwrap();
        let world = World {
            root: base.clone(),
            bin: base.join("bin"),
            config: base.join("config"),
            state: base.join("state"),
            data: base.join("data"),
            home: base.join("home"),
            work: base.join("work"),
            enabled: RefCell::new(Vec::new()),
            extra: RefCell::new(String::new()),
            fork: RefCell::new(String::new()),
            path_prefix: RefCell::new(Vec::new()),
            _root: root,
        };
        for dir in [
            &world.bin,
            &world.config,
            &world.state,
            &world.data,
            &world.home,
            &world.work,
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        for name in ["claude", "codex"] {
            fake_at(&world.bin.join(name));
        }
        // Its own repository, so the workspace ends at `work/` and the fake
        // binaries beside it are outside of it.
        let git = StdCommand::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&world.work)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .status()
            .unwrap();
        assert!(git.success());
        // A HEAD to cut worktrees from. Local identity, never signed.
        world.git(&["config", "user.name", "World"]);
        world.git(&["config", "user.email", "world@example.invalid"]);
        world.git(&["config", "commit.gpgsign", "false"]);
        world.git(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "chore: a first commit",
        ]);
        world
    }

    /// `git <args>` in this world's repository, which must succeed.
    pub fn git(&self, args: &[&str]) {
        git_in(&self.work, args);
    }

    /// Installs a fake `daft` beside the harnesses, writes its `plan` (see the
    /// fake), chooses it in the config (`fork.provider = "daft"` and
    /// `fork.daft.binary` pinned to it), puts `bin` first on PATH and opts the
    /// repository into daft with a `daft.yml`. All of it together, always: a
    /// `daft.yml` with the real daft chosen would have the developer's daft
    /// cut — and catalog — this world's repository. Nothing else writes a
    /// `daft.yml`. On PATH too, so a test can show that what runs is the
    /// pinned daft, and never one PATH finds.
    pub fn daft(&self, plan: Value) -> PathBuf {
        let path = self.bin.join("daft");
        fake_at(&path);
        fs::write(self.bin.join("daft.plan"), plan.to_string()).unwrap();
        if !self.path_prefix.borrow().contains(&self.bin) {
            self.path_prefix.borrow_mut().push(self.bin.clone());
        }
        fs::write(self.work.join("daft.yml"), "hooks: {}\n").unwrap();
        self.fork(&format!(
            "fork.provider = \"daft\"\nfork.daft.binary = {:?}",
            path
        ));
        path
    }

    /// Replaces what cuts a writer's worktree: dotted `fork.*` keys only —
    /// a `[fork.daft]` table would be refused after them.
    pub fn fork(&self, keys: &str) {
        *self.fork.borrow_mut() = keys.to_string();
        self.write_config();
    }

    /// Every call the fake daft took, as it logged them (`--version` aside).
    pub fn daft_calls(&self) -> Vec<Value> {
        self.calls("daft.calls")
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Every time the fake daft was asked its version: its PATH and where it
    /// ran.
    pub fn daft_versions(&self) -> Vec<Value> {
        self.calls("daft.versions")
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Puts `dir` first on PATH for every later command.
    pub fn prefix_path(&self, dir: &Path) {
        self.path_prefix.borrow_mut().insert(0, dir.to_path_buf());
    }

    /// Makes `run` look `secs` older than it is: created and finished then.
    pub fn age(&self, run: &str, secs: u64) {
        let path = self.state.join("runs").join(run).join("run.json");
        let mut record: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        record["created_at"] = (now - secs).into();
        record["finished_at"] = (now - secs).into();
        fs::write(&path, record.to_string()).unwrap();
    }

    /// Makes `run`'s history `secs` older than it is: every line about it
    /// (finished, outcome, survival) moved back by as much, so a window can
    /// pass without a clock.
    pub fn age_history(&self, run: &str, secs: u64) {
        let path = self.state.join("history.jsonl");
        let text: String = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| {
                let mut event: Value = serde_json::from_str(line).unwrap();
                if event["run"] == run {
                    event["t"] = (event["t"].as_u64().unwrap() - secs).into();
                }
                format!("{event}\n")
            })
            .collect();
        fs::write(&path, text).unwrap();
    }

    /// A shell script at `path`, runnable, written the way `fake_at` writes a
    /// fake: by `cp`, from a file beside it.
    pub fn script_at(&self, path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let source = path.with_extension("src");
        fs::write(&source, text).unwrap();
        let _ = fs::remove_file(path);
        let copied = StdCommand::new("cp")
            .arg(&source)
            .arg(path)
            .status()
            .unwrap();
        assert!(
            copied.success(),
            "cp could not put a script at {}",
            path.display()
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A `post-checkout` hook in `hooks` that leaves a mark when it runs.
    pub fn hook_that_marks(&self, hooks: &Path) {
        self.script_at(
            &hooks.join("post-checkout"),
            &format!(
                "#!/bin/sh\necho ran >> '{}'\n",
                self.root.join("hook-ran").display()
            ),
        );
    }

    /// Whether a hook from `hook_that_marks` has run since the last `clear_hook_mark`.
    pub fn hook_ran(&self) -> bool {
        self.root.join("hook-ran").exists()
    }

    pub fn clear_hook_mark(&self) {
        let _ = fs::remove_file(self.root.join("hook-ran"));
    }

    /// Makes this world's repository a partial clone whose promisor remote is
    /// a marker: a local script, run by git's `ext::` transport, that notes
    /// it ran and fetches nothing. Any lazy fetch of a missing object runs
    /// it, so a missing object (`lose`) and no mark (`fetch_tried`) is a fetch
    /// that never started. No network: the remote is the script.
    pub fn promisor(&self) {
        let marker = self.root.join("promisor-remote");
        self.script_at(
            &marker,
            &format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\nexit 1\n",
                self.root.join("fetch-tried").display()
            ),
        );
        for (key, value) in [
            ("core.repositoryformatversion", "1".to_string()),
            ("extensions.partialClone", "origin".to_string()),
            ("remote.origin.url", format!("ext::{}", marker.display())),
            ("remote.origin.promisor", "true".to_string()),
            ("protocol.ext.allow", "always".to_string()),
        ] {
            self.git(&["config", key, &value]);
        }
    }

    /// Where the repository keeps `oid` as a loose object.
    pub fn object_path(&self, oid: &str) -> PathBuf {
        self.work
            .join(".git/objects")
            .join(&oid[..2])
            .join(&oid[2..])
    }

    /// Removes the loose object `oid`: the repository is missing it now, as a
    /// partial clone is missing what it never fetched.
    pub fn lose(&self, oid: &str) {
        fs::remove_file(self.object_path(oid)).unwrap();
    }

    /// Whether the `promisor` remote has run — a lazy fetch was tried.
    pub fn fetch_tried(&self) -> bool {
        self.root.join("fetch-tried").exists()
    }

    /// Runs `git <args>` in `dir` as cahoots would but WITHOUT
    /// `GIT_NO_LAZY_FETCH`, and says whether that tried to fetch. The control
    /// of a test that says cahoots' own `git` did not: it shows the step does
    /// read the missing object, so the test could have failed.
    pub fn fetches_without_the_variable(&self, dir: &Path, args: &[&str]) -> bool {
        let _ = fs::remove_file(self.root.join("fetch-tried"));
        let _ = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_NO_LAZY_FETCH")
            .output()
            .unwrap();
        self.fetch_tried()
    }

    /// Turns these targets on, and every other one off, in config.toml.
    pub fn enable(&self, harnesses: &[&str]) {
        *self.enabled.borrow_mut() = harnesses.iter().map(|h| h.to_string()).collect();
        self.write_config();
    }

    /// Writes config.toml: the fake binaries, the targets `enable` named, a
    /// fast kill ladder, what `fork` chose, then `extra` — dotted keys
    /// (`harness.codex.cap = 80`) first, new tables after.
    pub fn configure(&self, extra: &str) {
        *self.extra.borrow_mut() = extra.to_string();
        self.write_config();
    }

    fn write_config(&self) {
        let enabled = self.enabled.borrow();
        let harness: String = ["claude", "codex"]
            .iter()
            .map(|id| {
                let on = if enabled.iter().any(|e| e == id) {
                    format!("harness.{id}.enabled = true\n")
                } else {
                    String::new()
                };
                format!("harness.{id}.binary = {:?}\n{on}", self.bin.join(id))
            })
            .collect();
        let text = format!(
            "schema = 1\n{harness}limits.int_grace_secs = 1\nlimits.term_grace_secs = 1\n{}\n{}\n",
            self.fork.borrow(),
            self.extra.borrow(),
        );
        fs::write(self.config.join("config.toml"), text).unwrap();
    }

    /// Installs a fake `usage-cli`, chooses it as the meter and points the
    /// config at it. `plan` says how it answers:
    /// `{"guarded": {"code": 21}, "unguarded": {"code": 0, "percent": 40}}` —
    /// `guarded` is the call that carries `--max-data-age`.
    pub fn meter(&self, plan: serde_json::Value, extra: &str) {
        let path = self.bin.join("usage-cli");
        fake_at(&path);
        fs::write(self.bin.join("usage-cli.plan"), plan.to_string()).unwrap();
        self.configure(&format!(
            "meter.use = \"agent-usage\"\n{extra}\n[meter.agent-usage]\nbinary = {path:?}\n"
        ));
    }

    /// Every command line the fake meter was called with.
    pub fn meter_calls(&self) -> Vec<String> {
        self.calls("usage-cli.calls")
    }

    /// Installs a fake `ccusage` beside the harnesses and writes its `plan`
    /// (`{"claude": [{"tokens": 40}], "codex": […]}` — see the fake). With
    /// `table` it is chosen in the config (`meter.use`), with
    /// `[meter.ccusage]` plus `table`; with `None` it is only there to be
    /// found.
    pub fn ccusage(&self, plan: Value, extra: &str, table: Option<&str>) -> PathBuf {
        let path = self.bin.join("ccusage");
        fake_at(&path);
        fs::write(self.bin.join("ccusage.plan"), plan.to_string()).unwrap();
        match table {
            Some(table) => self.configure(&format!(
                "meter.use = \"ccusage\"\n{extra}\n[meter.ccusage]\nbinary = {path:?}\n{table}\n"
            )),
            None => self.configure(extra),
        }
        path
    }

    /// Every command line the fake ccusage was called with (`--version` aside).
    pub fn ccusage_calls(&self) -> Vec<String> {
        self.calls("ccusage.calls")
    }

    fn calls(&self, file: &str) -> Vec<String> {
        fs::read_to_string(self.bin.join(file))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn brief(&self, text: &str) -> PathBuf {
        let path = self.work.join(format!("brief-{}.md", uuid_like()));
        fs::write(&path, text).unwrap();
        path
    }

    /// A `cahoots` command with a clean slate: this world's directories, and
    /// none of the harness markers the suite itself may be running under.
    pub fn cahoots(&self) -> Command {
        Command::from_std(self.cahoots_std())
    }

    /// `cahoots <args>` at a terminal of its own (`AtTerminal`), with only this
    /// world's bin directory on PATH, and color off so the screen reads as text.
    pub fn at_terminal(&self, args: &[&str]) -> AtTerminal {
        self.at_terminal_with(args, &[])
    }

    /// `at_terminal`, with `env` set on top.
    pub fn at_terminal_with(&self, args: &[&str], env: &[(&str, &str)]) -> AtTerminal {
        AtTerminal::start(
            self.terminal_command(args, env),
            Duration::ZERO,
            Stdout::Piped,
        )
    }

    /// `at_terminal`, at a terminal that answers where its cursor is only
    /// after `delay`, as one over a slow connection does.
    pub fn at_slow_terminal(&self, args: &[&str], delay: Duration) -> AtTerminal {
        AtTerminal::start(self.terminal_command(args, &[]), delay, Stdout::Piped)
    }

    /// `cahoots <args>` as a person runs it: stdout on the terminal too, so a
    /// verb a person reads ends in words, and there is no JSON to read.
    pub fn as_a_person(&self, args: &[&str]) -> AtTerminal {
        self.as_a_person_with(args, &[])
    }

    /// `as_a_person`, with `env` set on top.
    pub fn as_a_person_with(&self, args: &[&str], env: &[(&str, &str)]) -> AtTerminal {
        AtTerminal::start(
            self.terminal_command(args, env),
            Duration::ZERO,
            Stdout::Terminal,
        )
    }

    /// `cahoots <args>` with stdout and stderr on a terminal, and nothing on
    /// stdin: a terminal on stdout alone, which proves no person is there.
    pub fn with_stdin_elsewhere(&self, args: &[&str]) -> AtTerminal {
        AtTerminal::start_with(
            self.terminal_command(args, &[]),
            Duration::ZERO,
            Stdin::Nothing,
            Stdout::Terminal,
        )
    }

    /// Writes a history in which, for `advise`, codex (the default first
    /// choice) kept being thrown away and claude (second) kept being
    /// accepted: `each` runs of each.
    pub fn history_where_the_second_choice_does_better(&self, each: usize) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut lines = String::new();
        for (harness, model, outcome) in [
            ("codex", "gpt-6-astra", "discarded"),
            ("claude", "opus", "accepted"),
        ] {
            for n in 0..each {
                let run = format!("0198c0de-0000-7000-8000-{harness:0>6}{n:06}");
                lines.push_str(&format!(
                    "{{\"kind\":\"finished\",\"t\":{now},\"run\":\"{run}\",\"role\":\"advise\",\"caller\":null,\
                     \"target\":{{\"harness\":\"{harness}\",\"model\":\"{model}\",\"effort\":\"high\"}},\
                     \"dir\":\"/w\",\"state\":\"done\",\"exit\":0,\"tokens_in\":1,\"tokens_out\":1,\"secs\":1,\"sampled\":false}}\n\
                     {{\"kind\":\"outcome\",\"t\":{now},\"run\":\"{run}\",\"outcome\":\"{outcome}\"}}\n"
                ));
            }
        }
        fs::create_dir_all(&self.state).unwrap();
        fs::write(self.state.join("history.jsonl"), lines).unwrap();
    }

    fn terminal_command(&self, args: &[&str], env: &[(&str, &str)]) -> StdCommand {
        let mut command = self.cahoots_std();
        command
            .args(args)
            .env("PATH", &self.bin)
            .env("NO_COLOR", "1")
            .env("TERM", "xterm-256color")
            .envs(env.iter().copied());
        command
    }

    fn cahoots_std(&self) -> StdCommand {
        let mut command = StdCommand::new(env!("CARGO_BIN_EXE_cahoots"));
        let prefix = self.path_prefix.borrow();
        if !prefix.is_empty() {
            let inherited = std::env::var_os("PATH").unwrap_or_default();
            let path = std::env::join_paths(
                prefix
                    .iter()
                    .cloned()
                    .chain(std::env::split_paths(&inherited)),
            )
            .unwrap();
            command.env("PATH", path);
        }
        command
            .current_dir(&self.work)
            .env("CAHOOTS_CONFIG_DIR", &self.config)
            .env("CAHOOTS_STATE_DIR", &self.state)
            .env("CAHOOTS_DATA_DIR", &self.data)
            .env("CAHOOTS_HOME_DIR", &self.home)
            .env_remove("CAHOOTS_DEV_REAL_DIRS")
            // Whatever the caller says, cahoots' own git never fetches
            // lazily: a caller that says otherwise proves it.
            .env("GIT_NO_LAZY_FETCH", "0")
            .env_remove("CLAUDECODE")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("CODEX_SANDBOX")
            .env_remove("CAHOOTS_DEPTH")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY");
        command
    }

    pub fn ask(&self, args: &[&str]) -> Answer {
        answer(self.cahoots().args(args))
    }

    /// `cahoots run --role advise --caller claude --brief <file with text>`.
    pub fn run(&self, brief: &str, extra: &[&str]) -> Answer {
        let brief = self.brief(brief);
        let mut args = vec![
            "run",
            "--role",
            "advise",
            "--brief",
            brief.to_str().unwrap(),
        ];
        if !extra.contains(&"--caller") {
            args.extend(["--caller", "claude"]);
        }
        args.extend(extra);
        self.ask(&args)
    }

    /// A PATH for a command at a terminal that starts `git`: this world's
    /// bin first, then the inherited PATH, where git is.
    pub fn path_with_git(&self) -> String {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        std::env::join_paths(
            std::iter::once(self.bin.clone()).chain(std::env::split_paths(&inherited)),
        )
        .unwrap()
        .into_string()
        .unwrap()
    }

    /// Commits the files an eval task is made from: `src/lib.rs` and the
    /// test file `tests/old_test.rs`.
    pub fn evals_fixture(&self) {
        fs::create_dir_all(self.work.join("src")).unwrap();
        fs::create_dir_all(self.work.join("tests")).unwrap();
        fs::write(self.work.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        fs::write(self.work.join("tests/old_test.rs"), "#[test] fn old() {}\n").unwrap();
        self.git(&["add", "--", "src/lib.rs", "tests/old_test.rs"]);
        self.git(&["commit", "-q", "-m", "chore: a library and its test"]);
    }

    /// A writer's run in a fork (`--role implement`, unless `extra` names a
    /// kind), from `dir` with its brief there, which must succeed.
    pub fn writer_in(&self, dir: &Path, brief: &str, extra: &[&str]) -> Answer {
        let path = dir.join(format!("brief-{}.md", uuid_like()));
        fs::write(&path, brief).unwrap();
        let mut args = vec!["run", "--fork", "--caller", "claude"];
        if !extra.contains(&"--kind") {
            args.extend(["--role", "implement"]);
        }
        args.extend(["--brief", path.to_str().unwrap()]);
        args.extend(extra);
        let answer = answer(self.cahoots().current_dir(dir).args(&args));
        assert_eq!(answer.code, 0, "{}", answer.json);
        answer
    }

    /// `writer_in` this world's repository, then its result recorded as
    /// accepted.
    pub fn accepted_writer(&self, brief: &str, extra: &[&str]) -> Answer {
        self.accepted_writer_in(&self.work.clone(), brief, extra)
    }

    pub fn accepted_writer_in(&self, dir: &Path, brief: &str, extra: &[&str]) -> Answer {
        let run = self.writer_in(dir, brief, extra);
        let outcome = self.ask(&["outcome", &run.run_id(), "accepted"]);
        assert_eq!(outcome.code, 0, "{}", outcome.json);
        run
    }

    /// Makes the commit every run in `forks` started from unreachable, and
    /// gone: they were cut from a branch `scratch`, which is deleted along
    /// with their worktrees, and the repository is collected.
    pub fn lose_the_scratch_commit(&self, forks: &[&Answer]) {
        self.git(&["switch", "-q", "main"]);
        self.git(&["branch", "-q", "-D", "scratch"]);
        for fork in forks {
            let worktree = fork.data()["worktree"].as_str().expect("a worktree");
            self.git(&["worktree", "remove", "--force", worktree]);
        }
        self.git(&["reflog", "expire", "--expire=now", "--all"]);
        self.git(&["gc", "-q", "--prune=now"]);
    }

    pub fn record(&self, run: &str) -> Value {
        let path = self.state.join("runs").join(run).join("run.json");
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    pub fn run_file(&self, run: &str, name: &str) -> PathBuf {
        self.state.join("runs").join(run).join(name)
    }
}

pub fn answer(command: &mut Command) -> Answer {
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!(
            "stdout is not one JSON envelope ({error}): {stdout:?}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    Answer {
        code: output.status.code().expect("an exit code"),
        json,
    }
}

pub fn alive(pid: i64) -> bool {
    StdCommand::new("ps")
        .args(["-p", &pid.to_string()])
        .output()
        .is_ok_and(|output| output.status.success())
}

pub fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !check() {
        assert!(
            started.elapsed().as_secs() < 30,
            "timed out waiting until {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// `git <args>` in `dir`, which must succeed — with none of the variables git
/// exports to a hook, which would point it at another repository.
pub fn git_in(dir: &Path, args: &[&str]) {
    let done = StdCommand::new("git")
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
}

/// Where a command at a terminal reads its stdin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stdin {
    /// The terminal, as a person types at it.
    Terminal,
    /// Nothing at all (`/dev/null`).
    Nothing,
}

/// Where a command at a terminal writes its stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stdout {
    /// Piped apart, as in `cahoots install | jq`: the JSON envelope.
    Piped,
    /// On the terminal, as a person runs it: the words of a verb a person reads.
    Terminal,
}

/// A command run at a terminal of its own: a pseudo-terminal on its stdin and
/// stderr, and stdout piped apart, as in `cahoots install | jq`, or on the
/// terminal too. What the terminal shows is collected as it comes, so a test
/// can wait for a question, press keys, and then read the screen, the JSON,
/// and the terminal's mode. Like a real terminal, it answers when it is asked
/// where its cursor is (`ESC [ 6 n`): at its far corner, which is its size.
pub struct AtTerminal {
    child: std::process::Child,
    stdout: Stdout,
    master: Arc<OwnedFd>,
    /// Kept open, to read the terminal's mode once the command is gone.
    slave: OwnedFd,
    shown: Arc<Mutex<Vec<u8>>>,
    /// Rows and columns.
    size: Arc<Mutex<(u16, u16)>>,
    done: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

/// What a command left at its terminal.
pub struct Finished {
    pub code: i32,
    /// The envelope on a piped stdout; `Null` when stdout was the terminal.
    pub json: Value,
    /// Everything the terminal was sent, escapes and all.
    pub screen: String,
    /// The terminal's mode after the command was gone: what it was given back.
    pub mode: Termios,
}

impl Finished {
    /// What stayed on the screen, as a person reads it: without what was
    /// drawn on the alternate screen, which is gone when the page closes, and
    /// without escape codes or carriage returns.
    pub fn text(&self) -> String {
        let mut rest = self.screen.as_str();
        let mut kept = String::new();
        while let Some(start) = rest.find("\x1b[?1049h") {
            kept.push_str(&rest[..start]);
            rest = match rest[start..].find("\x1b[?1049l") {
                Some(end) => &rest[start + end..],
                None => "",
            };
        }
        kept.push_str(rest);
        plain(&kept)
    }
}

/// `text` without its escape codes (`ESC [ … letter`) or carriage returns.
pub fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' if chars.peek() == Some(&'[') => {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() || next == '~' {
                        break;
                    }
                }
            }
            '\r' => {}
            ch => out.push(ch),
        }
    }
    out
}

impl AtTerminal {
    /// Starts `command` at the terminal, which answers a size question after
    /// `delay`, with its stdout piped apart or on the terminal too.
    pub fn start(command: StdCommand, delay: Duration, stdout: Stdout) -> AtTerminal {
        AtTerminal::start_with(command, delay, Stdin::Terminal, stdout)
    }

    /// `start`, with stdin on the terminal or on nothing at all.
    pub fn start_with(
        mut command: StdCommand,
        delay: Duration,
        stdin: Stdin,
        stdout: Stdout,
    ) -> AtTerminal {
        let size = Winsize {
            ws_row: 24,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(&size, None::<&Termios>).expect("a pseudo-terminal");
        command
            .stdin(match stdin {
                Stdin::Terminal => Stdio::from(pty.slave.try_clone().unwrap()),
                Stdin::Nothing => Stdio::null(),
            })
            .stderr(Stdio::from(pty.slave.try_clone().unwrap()))
            .stdout(match stdout {
                Stdout::Piped => Stdio::piped(),
                Stdout::Terminal => Stdio::from(pty.slave.try_clone().unwrap()),
            });
        let child = command.spawn().expect("cahoots starts");
        let master = Arc::new(pty.master);
        let shown = Arc::new(Mutex::new(Vec::new()));
        let size = Arc::new(Mutex::new((size.ws_row, size.ws_col)));
        let done = Arc::new(AtomicBool::new(false));
        let reader = {
            let (master, shown, size, done) =
                (master.clone(), shown.clone(), size.clone(), done.clone());
            std::thread::spawn(move || {
                let mut answered = 0;
                while !done.load(Ordering::Relaxed) {
                    drain(&master, &shown, 50);
                    let asked = shown
                        .lock()
                        .unwrap()
                        .windows(4)
                        .filter(|bytes| bytes == b"\x1b[6n")
                        .count();
                    for _ in answered..asked {
                        std::thread::sleep(delay);
                        let (rows, cols) = *size.lock().unwrap();
                        let answer = format!("\x1b[{rows};{cols}R");
                        let _ = nix::unistd::write(&*master, answer.as_bytes());
                    }
                    answered = asked;
                }
            })
        };
        AtTerminal {
            child,
            stdout,
            master,
            slave: pty.slave,
            shown,
            size,
            done,
            reader: Some(reader),
        }
    }

    /// The terminal is `rows` by `cols` now: it says so the way a terminal
    /// does, with SIGWINCH, and answers the next size question with it.
    pub fn resize(&self, rows: u16, cols: u16) {
        *self.size.lock().unwrap() = (rows, cols);
        let pid = nix::unistd::Pid::from_raw(self.child.id() as i32);
        nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGWINCH).expect("SIGWINCH");
    }

    pub fn screen(&self) -> String {
        String::from_utf8_lossy(&self.shown.lock().unwrap()).into_owned()
    }

    /// Waits until the terminal has shown `text`.
    pub fn wait_for(&self, text: &str) {
        wait_until(&format!("the terminal shows {text:?}"), || {
            self.screen().contains(text)
        });
    }

    /// Types `keys` as a keyboard sends them: `b"\x1b[B"` is ↓, `b"\r"` Enter.
    pub fn press(&self, keys: &[u8]) {
        nix::unistd::write(&*self.master, keys).expect("the keys are typed");
    }

    /// Waits for the command to exit, and says what it left.
    pub fn finish(mut self) -> Finished {
        wait_until("the command exits", || {
            self.child.try_wait().unwrap().is_some()
        });
        let code = self.child.wait().unwrap().code().expect("an exit code");
        let mut stdout = String::new();
        if let Some(mut piped) = self.child.stdout.take() {
            piped.read_to_string(&mut stdout).unwrap();
        }
        self.done.store(true, Ordering::Relaxed);
        self.reader.take().unwrap().join().unwrap();
        // What it drew last may still be on its way.
        drain(&self.master, &self.shown, 200);
        let screen = self.screen();
        let json = match self.stdout {
            Stdout::Piped => serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
                panic!("stdout is not one JSON envelope ({error}): {stdout:?}\nscreen: {screen:?}")
            }),
            Stdout::Terminal => Value::Null,
        };
        Finished {
            code,
            json,
            screen,
            mode: tcgetattr(self.slave.as_fd()).expect("the terminal's mode"),
        }
    }
}

/// Reads what the terminal was sent until nothing more comes within `wait_ms`.
fn drain(master: &OwnedFd, shown: &Mutex<Vec<u8>>, wait_ms: u16) {
    let mut buf = [0u8; 4096];
    loop {
        let mut ready = [PollFd::new(master.as_fd(), PollFlags::POLLIN)];
        if !matches!(poll(&mut ready, PollTimeout::from(wait_ms)), Ok(n) if n > 0) {
            return;
        }
        match nix::unistd::read(master.as_fd(), &mut buf) {
            Ok(read) if read > 0 => shown.lock().unwrap().extend_from_slice(&buf[..read]),
            _ => {
                std::thread::sleep(Duration::from_millis(wait_ms.into()));
                return;
            }
        }
    }
}
