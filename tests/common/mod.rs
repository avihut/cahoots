//! A throwaway world for one test: fake `claude` and `codex` binaries, a
//! config and a state directory of its own, and a small git repository to
//! stand in. Nothing here touches the real `~/.config` or `~/.local/state`:
//! the binary under test is a dev build, and every command gets both
//! directory overrides.
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
    pub bin: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    /// Stands in for the user's home, where the agent homes live.
    pub home: PathBuf,
    pub work: PathBuf,
    /// The targets `enable` last named, and what `configure` was last given:
    /// config.toml is written from both.
    enabled: RefCell<Vec<String>>,
    extra: RefCell<String>,
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
/// binary. Rewritten in place just after it ran, a binary's next run on macOS
/// failed now and then.
pub fn fake_at(path: &Path) {
    let _ = fs::remove_file(path);
    fs::copy(fake_harness(), path).unwrap();
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
            bin: base.join("bin"),
            config: base.join("config"),
            state: base.join("state"),
            home: base.join("home"),
            work: base.join("work"),
            enabled: RefCell::new(Vec::new()),
            extra: RefCell::new(String::new()),
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

    /// Turns these targets on, and every other one off, in config.toml.
    pub fn enable(&self, harnesses: &[&str]) {
        *self.enabled.borrow_mut() = harnesses.iter().map(|h| h.to_string()).collect();
        self.write_config();
    }

    /// Writes config.toml: the fake binaries, the targets `enable` named, a
    /// fast kill ladder, then `extra` — dotted keys (`harness.codex.cap = 80`)
    /// first, new tables after.
    pub fn configure(&self, extra: &str) {
        *self.extra.borrow_mut() = extra.to_string();
        self.write_config();
    }

    fn write_config(&self) {
        let enabled: String = self
            .enabled
            .borrow()
            .iter()
            .map(|id| format!("harness.{id}.enabled = true\n"))
            .collect();
        let text = format!(
            "schema = 1\nharness.claude.binary = {:?}\nharness.codex.binary = {:?}\n{enabled}\
             limits.int_grace_secs = 1\nlimits.term_grace_secs = 1\n{}\n",
            self.bin.join("claude"),
            self.bin.join("codex"),
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
        let mut command = self.cahoots_std();
        command
            .args(args)
            .env("PATH", &self.bin)
            .env("NO_COLOR", "1")
            .env("TERM", "xterm-256color")
            .envs(env.iter().copied());
        AtTerminal::start(command)
    }

    fn cahoots_std(&self) -> StdCommand {
        let mut command = StdCommand::new(env!("CARGO_BIN_EXE_cahoots"));
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

/// A command run at a terminal of its own: a pseudo-terminal on its stdin and
/// stderr, and stdout piped apart, as in `cahoots install | jq`. What the
/// terminal shows is collected as it comes, so a test can wait for a question,
/// press keys, and then read the screen, the JSON, and the terminal's mode.
pub struct AtTerminal {
    child: std::process::Child,
    master: Arc<OwnedFd>,
    /// Kept open, to read the terminal's mode once the command is gone.
    slave: OwnedFd,
    shown: Arc<Mutex<Vec<u8>>>,
    done: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

/// What a command left at its terminal.
pub struct Finished {
    pub code: i32,
    pub json: Value,
    /// Everything the terminal was sent, escapes and all.
    pub screen: String,
    /// The terminal's mode after the command was gone: what it was given back.
    pub mode: Termios,
}

impl AtTerminal {
    pub fn start(mut command: StdCommand) -> AtTerminal {
        let size = Winsize {
            ws_row: 24,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(&size, None::<&Termios>).expect("a pseudo-terminal");
        command
            .stdin(Stdio::from(pty.slave.try_clone().unwrap()))
            .stderr(Stdio::from(pty.slave.try_clone().unwrap()))
            .stdout(Stdio::piped());
        let child = command.spawn().expect("cahoots starts");
        let master = Arc::new(pty.master);
        let shown = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(AtomicBool::new(false));
        let reader = {
            let (master, shown, done) = (master.clone(), shown.clone(), done.clone());
            std::thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    drain(&master, &shown, 50);
                }
            })
        };
        AtTerminal {
            child,
            master,
            slave: pty.slave,
            shown,
            done,
            reader: Some(reader),
        }
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
        self.child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        self.done.store(true, Ordering::Relaxed);
        self.reader.take().unwrap().join().unwrap();
        // What it drew last may still be on its way.
        drain(&self.master, &self.shown, 200);
        let screen = self.screen();
        let json = serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
            panic!("stdout is not one JSON envelope ({error}): {stdout:?}\nscreen: {screen:?}")
        });
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
