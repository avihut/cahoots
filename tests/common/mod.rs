//! A throwaway world for one test: fake `claude` and `codex` binaries, a
//! config and a state directory of its own, and a small git repository to
//! stand in. Nothing here touches the real `~/.config` or `~/.local/state`:
//! the binary under test is a dev build, and every command gets both
//! directory overrides.
#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use serde_json::Value;

pub struct World {
    _root: tempfile::TempDir,
    pub bin: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    /// Stands in for the user's home, where the agent homes live.
    pub home: PathBuf,
    pub work: PathBuf,
}

/// The `fake_harness` example, which cargo builds next to the test binaries.
fn fake_harness() -> PathBuf {
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
            bin: base.join("bin"),
            config: base.join("config"),
            state: base.join("state"),
            home: base.join("home"),
            work: base.join("work"),
            _root: root,
        };
        for dir in [
            &world.bin,
            &world.config,
            &world.state,
            &world.home,
            &world.work,
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        for name in ["claude", "codex"] {
            let path = world.bin.join(name);
            fs::copy(fake_harness(), &path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
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
        for args in [
            vec!["config", "user.name", "World"],
            vec!["config", "user.email", "world@example.invalid"],
            vec!["config", "commit.gpgsign", "false"],
            vec![
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "chore: a first commit",
            ],
        ] {
            let done = StdCommand::new("git")
                .args(&args)
                .current_dir(&world.work)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .status()
                .unwrap();
            assert!(done.success(), "git {args:?}");
        }
        world
    }

    pub fn enable(&self, harnesses: &[&str]) {
        let json = serde_json::json!({ "v": 1, "enabled": harnesses });
        fs::write(self.config.join("enabled.json"), json.to_string()).unwrap();
    }

    /// Writes config.toml: the fake binaries, a fast kill ladder, then `extra` —
    /// dotted keys (`harness.codex.cap = 80`) first, new tables after.
    pub fn configure(&self, extra: &str) {
        let text = format!(
            "schema = 1\nharness.claude.binary = {:?}\nharness.codex.binary = {:?}\n\
             limits.int_grace_secs = 1\nlimits.term_grace_secs = 1\n{extra}\n",
            self.bin.join("claude"),
            self.bin.join("codex"),
        );
        fs::write(self.config.join("config.toml"), text).unwrap();
    }

    /// Installs a fake `usage-cli` and points the config at it. `plan` says how
    /// it answers: `{"guarded": {"code": 21}, "unguarded": {"code": 0, "percent": 40}}`
    /// — `guarded` is the call that carries `--max-data-age`.
    pub fn meter(&self, plan: serde_json::Value, extra: &str) {
        let path = self.bin.join("usage-cli");
        fs::copy(fake_harness(), &path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(self.bin.join("usage-cli.plan"), plan.to_string()).unwrap();
        self.configure(&format!(
            "{extra}\n[meter.agent-usage]\nbinary = {path:?}\n"
        ));
    }

    /// Every command line the fake meter was called with.
    pub fn meter_calls(&self) -> Vec<String> {
        fs::read_to_string(self.bin.join("usage-cli.calls"))
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
        let mut command = Command::cargo_bin("cahoots").unwrap();
        command
            .current_dir(&self.work)
            .env("CAHOOTS_CONFIG_DIR", &self.config)
            .env("CAHOOTS_STATE_DIR", &self.state)
            .env("CAHOOTS_HOME_DIR", &self.home)
            .env_remove("CAHOOTS_DEV_REAL_DIRS")
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
