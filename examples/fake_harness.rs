//! A stand-in for the harness CLIs, for the test suite (hard rule 8: tests
//! never call a real harness). Copied into a temp directory under the name
//! `claude` or `codex`, it answers `--version` with that CLI's fingerprint and
//! otherwise speaks that CLI's output stream. Named `usage-cli` or `ccusage`,
//! it is a usage meter instead, and named `daft`, it cuts a fork the way its
//! plan says (so no test ever runs the real daft).
//!
//! The brief (stdin) steers it, one directive per line:
//!
//! ```text
//! FAKE: say=<text>     the answer (default: pong)
//! FAKE: sleep=<secs>   think for a while first
//! FAKE: fail           report a failed run and exit 1
//! FAKE: dump           answer with this process's argv, cwd and environment
//! FAKE: child          leave a long-lived child in its own process group
//! FAKE: write=<name>   "edit": create <name> in the working directory
//! FAKE: append=<file>::<text>  add <text> to <file> there (`\n`, `\t`
//!                      for a newline and a tab); as many as the brief has
//! FAKE: leak=<name>    leave a child in the callee's group that, a moment
//!                      after the callee exits, writes <name> in the cwd
//! FAKE: remove=<name>  delete <name> (before `write` and `append`)
//! FAKE: commit         commit everything there (after `write` and `append`)
//! FAKE: bytes=<name>   write bytes that are not UTF-8, with a CRLF
//! FAKE: link=<name>=<target>  make <name> a link to <target>
//! FAKE: hardlink=<name>=<target>  make <name> a hard link to <target>
//! ```
//!
//! In this order: `child`, `leak`, `remove`, `write`, every `append`, then
//! `commit`, `bytes`, `link`, `hardlink`, and last `sleep`.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::time::Duration;

use serde_json::json;

fn emit(value: serde_json::Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

/// As `usage-cli`: answers `headroom` the way `usage-cli.plan` (next to the
/// binary) says, and logs each call to `usage-cli.calls`.
fn fake_meter(exe: &std::path::Path, argv: &[String]) {
    let calls = exe.with_file_name("usage-cli.calls");
    let is_watch = |line: &str| line.contains("--max-data-age") && !line.contains("--forecast");
    // How many watchdog calls came before this one — for "watch_sequence".
    let earlier_watches = std::fs::read_to_string(&calls)
        .unwrap_or_default()
        .lines()
        .filter(|line| is_watch(line))
        .count();
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&calls)
        .expect("calls");
    let _ = writeln!(log, "{}", argv[1..].join(" "));
    let plan: serde_json::Value = std::fs::read_to_string(exe.with_file_name("usage-cli.plan"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    // Three kinds of call: admission (guarded by data age, asks the forecast),
    // admission's stale re-ask (unguarded), and the mid-run watchdog (guarded,
    // no forecast) — answered by "watch", or by "watch_sequence" in turn, or
    // "under" when the plan says nothing about it.
    let guarded = argv.iter().any(|arg| arg == "--max-data-age");
    let forecast = argv.iter().any(|arg| arg == "--forecast");
    let under = json!({"code": 0, "percent": 1});
    let answer = match (guarded, forecast) {
        (true, true) => &plan["guarded"],
        (false, _) => &plan["unguarded"],
        (true, false) => match plan["watch_sequence"].as_array() {
            Some(sequence) if !sequence.is_empty() => &sequence[earlier_watches % sequence.len()],
            _ if plan["watch"].is_object() => &plan["watch"],
            _ => &under,
        },
    };
    if let Some(percent) = answer["percent"].as_f64() {
        println!(
            "{}",
            json!({"verdict": "from-the-fake", "percent": percent})
        );
    }
    std::process::exit(answer["code"].as_i64().unwrap_or(13) as i32);
}

/// As `ccusage`: answers `--version` (`ccusage.version` next to the binary
/// overrides it), `blocks --active --json` for Claude Code and `codex daily
/// --json` for Codex, from `ccusage.plan`, logging each call to
/// `ccusage.calls`. The plan: `{"claude": [reading, …], "codex": [reading,
/// …], "exit": 1, "garbage": true}`. A reading is `{"tokens": n, "projected":
/// n, "limit_notice": "<UTC time>", "idle": true}`; the list is served in
/// turn and its last reading repeats — so the first call is admission and the
/// rest are the watchdog.
fn fake_ccusage(exe: &std::path::Path, argv: &[String]) {
    if argv.iter().any(|arg| arg == "--version") {
        let version = std::fs::read_to_string(exe.with_file_name("ccusage.version"))
            .unwrap_or_else(|_| "ccusage 20.0.23".to_string());
        println!("{}", version.trim());
        return;
    }
    let harness = if argv.iter().any(|arg| arg == "codex") {
        "codex"
    } else {
        "claude"
    };
    let calls = exe.with_file_name("ccusage.calls");
    let earlier = std::fs::read_to_string(&calls)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("codex") == (harness == "codex"))
        .count();
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&calls)
        .expect("calls");
    let _ = writeln!(log, "{}", argv[1..].join(" "));
    // Where it was run from (ccusage reads a config file from there), and
    // the PATH it was given (a script meter's interpreter is found on it).
    if let Ok(cwd) = std::env::current_dir() {
        let _ = std::fs::write(
            exe.with_file_name("ccusage.cwd"),
            cwd.to_string_lossy().as_bytes(),
        );
    }
    let _ = std::fs::write(
        exe.with_file_name("ccusage.path"),
        std::env::var("PATH").unwrap_or_default(),
    );
    let plan: serde_json::Value = std::fs::read_to_string(exe.with_file_name("ccusage.plan"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    if let Some(code) = plan["exit"].as_i64() {
        std::process::exit(code as i32);
    }
    if plan["garbage"] == true {
        println!("Usage report (not JSON)");
        return;
    }
    let idle = json!({"idle": true});
    let reading = match plan[harness].as_array() {
        Some(list) if !list.is_empty() => &list[earlier.min(list.len() - 1)],
        _ => &idle,
    };
    let tokens = reading["tokens"].as_u64().unwrap_or(0);
    if harness == "codex" {
        let rows = if tokens > 0 {
            json!([{"date": "2026-09-21", "totalTokens": tokens}])
        } else {
            json!([])
        };
        emit(json!({"daily": rows, "totals": {"totalTokens": tokens}}));
        return;
    }
    if reading["idle"] == true {
        emit(json!({"blocks": []}));
        return;
    }
    let mut block = json!({
        "id": "2026-09-21T05:00:00.000Z",
        "isActive": true,
        "isGap": false,
        "totalTokens": tokens,
        "projection": {"totalTokens": reading["projected"].as_u64().unwrap_or(tokens)},
    });
    if let Some(notice) = reading["limit_notice"].as_str() {
        block["usageLimitResetTime"] = json!(notice);
    }
    emit(json!({"blocks": [block]}));
}

/// As `daft`: `daft -C <base> start --fork …`, done the way `daft.plan` (next
/// to the binary) says, and logged to `daft.calls`, one JSON object a call —
/// its argv, where it ran, its PATH and every `GIT_CONFIG_*` it was given —
/// with its pid in `daft.pid`. The plan: `{"sleep": s, "make":
/// "worktree"|"dir"|"file"|"nothing", "print": "<path>", "exit": n, "at":
/// "<commit-ish>"}`; it makes a worktree at the commit it was given (`at`
/// overrides it) and exits 0 unless it says otherwise.
fn fake_daft(exe: &Path, argv: &[String]) {
    let git_config: std::collections::BTreeMap<String, String> = std::env::vars()
        .filter(|(name, _)| name.starts_with("GIT_CONFIG"))
        .collect();
    let call = json!({
        "argv": argv[1..],
        "cwd": std::env::current_dir().ok(),
        "path": std::env::var("PATH").ok(),
        "git_config": git_config,
    });
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(exe.with_file_name("daft.calls"))
        .expect("calls");
    let _ = writeln!(log, "{call}");
    let _ = std::fs::write(
        exe.with_file_name("daft.pid"),
        std::process::id().to_string(),
    );
    let plan: serde_json::Value = std::fs::read_to_string(exe.with_file_name("daft.plan"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    if let Some(secs) = plan["sleep"].as_u64() {
        std::thread::sleep(Duration::from_secs(secs));
    }
    let print = plan["print"].as_str().unwrap_or_default();
    match plan["make"].as_str().unwrap_or("worktree") {
        "worktree" => {
            let base = argv
                .iter()
                .position(|arg| arg == "-C")
                .and_then(|at| argv.get(at + 1))
                .expect("daft -C <base>");
            // At the commit it was given, as the real daft forks at its last
            // positional; HEAD when there is none.
            let last = argv.last().map(String::as_str).unwrap_or_default();
            let commit = matches!(last.len(), 40 | 64)
                && last
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
            // Or where the plan's `at` says, whatever it was given: a daft
            // that cuts somewhere else.
            let at = match plan["at"].as_str() {
                Some(at) => at,
                None if commit => last,
                None => "HEAD",
            };
            // The environment cahoots gave daft, so the git it runs gets it too.
            let made = std::process::Command::new("git")
                .args(["-C", base, "worktree", "add", "--detach", print, at])
                .stdout(std::io::stderr())
                .status()
                .expect("git");
            if !made.success() {
                eprintln!("daft (fake): git worktree add failed");
                std::process::exit(1);
            }
        }
        "dir" => std::fs::create_dir_all(print).expect("a directory"),
        "file" => {
            if let Some(parent) = Path::new(print).parent() {
                std::fs::create_dir_all(parent).expect("its directory");
            }
            std::fs::write(print, "not a directory\n").expect("a file");
        }
        _ => {}
    }
    eprintln!("daft (fake): forked {print}");
    println!("{print}");
    std::process::exit(plan["exit"].as_i64().unwrap_or(0) as i32);
}

fn main() {
    let exe = std::env::current_exe().expect("current_exe");
    let flavor = exe.file_name().unwrap().to_string_lossy().to_string();
    let argv: Vec<String> = std::env::args().collect();

    if flavor == "usage-cli" {
        return fake_meter(&exe, &argv);
    }
    if flavor == "ccusage" {
        return fake_ccusage(&exe, &argv);
    }
    if flavor == "daft" {
        return fake_daft(&exe, &argv);
    }

    if argv.iter().any(|arg| arg == "--version") {
        let overridden = std::fs::read_to_string(exe.with_file_name(format!("{flavor}.version")));
        match (overridden, flavor.as_str()) {
            (Ok(text), _) => println!("{}", text.trim()),
            (_, "claude") => println!("2.1.278 (Claude Code)"),
            _ => println!("codex-cli 0.155.1"),
        }
        return;
    }

    let mut brief = String::new();
    std::io::stdin()
        .read_to_string(&mut brief)
        .expect("brief on stdin");
    let directive = |name: &str| -> Option<String> {
        brief.lines().find_map(|line| {
            let rest = line.trim().strip_prefix("FAKE:")?.trim();
            if rest == name {
                Some(String::new())
            } else {
                rest.strip_prefix(&format!("{name}=")).map(str::to_string)
            }
        })
    };

    // A resumed session keeps its id: `claude --resume <id>`, `codex exec resume <id>`.
    let after = |flag: &str| {
        argv.iter()
            .position(|arg| arg == flag)
            .and_then(|at| argv.get(at + 1).cloned())
    };
    let session = after("--session-id")
        .or_else(|| after("--resume"))
        .or_else(|| after("resume"))
        .unwrap_or_else(|| "01a0bf61-0000-7000-8000-00000000fa4e".to_string());
    if flavor == "claude" {
        emit(
            json!({"type": "system", "subtype": "init", "session_id": session, "model": "claude-fake"}),
        );
    } else {
        println!("Reading additional input from stdin...");
        emit(json!({"type": "thread.started", "thread_id": session}));
        emit(json!({"type": "turn.started"}));
    }

    if directive("child").is_some() {
        // Deliberately never waited on: the point is an orphan for the
        // supervisor to sweep.
        #[allow(clippy::zombie_processes)]
        let child = std::process::Command::new("sleep")
            .arg("300")
            .process_group(0)
            .spawn()
            .expect("sleep");
        eprintln!("left child {}", child.id());
    }
    if let Some(name) = directive("leak") {
        // A child left in the callee's OWN process group (no `process_group`),
        // stdio detached so it does not hold the supervisor's pipes open. It
        // waits, then writes `name` in the working directory: a writer that
        // tries to touch the tree after the callee is gone. The supervisor
        // kills the group when the run ends, so it never gets to write.
        use std::process::Stdio;
        #[allow(clippy::zombie_processes)]
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("sleep 1; printf leaked > '{name}'"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("leak");
        eprintln!("left a child in the group: {}", child.id());
    }
    if let Some(name) = directive("remove") {
        std::fs::remove_file(&name).expect("remove in cwd");
    }
    if let Some(name) = directive("write") {
        std::fs::write(&name, "written by the callee\n").expect("write in cwd");
    }
    for (file, text) in brief.lines().filter_map(|line| {
        line.trim()
            .strip_prefix("FAKE:")?
            .trim()
            .strip_prefix("append=")?
            .split_once("::")
    }) {
        let text = text.replace("\\n", "\n").replace("\\t", "\t");
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
            .expect("append in cwd");
        out.write_all(text.as_bytes()).expect("append");
    }
    if directive("commit").is_some() {
        for args in [
            &["add", "-A"][..],
            &[
                "-c",
                "user.name=Fake",
                "-c",
                "user.email=fake@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "fake: commit",
            ],
        ] {
            let done = std::process::Command::new("git")
                .args(args)
                .stdout(std::io::stderr())
                .status()
                .expect("git");
            assert!(done.success(), "git {args:?} failed");
        }
    }
    if let Some(name) = directive("bytes") {
        std::fs::write(&name, b"caf\xe9\r\nna\xefve\n").expect("bytes in cwd");
    }
    if let Some(spec) = directive("link") {
        let (name, target) = spec.split_once('=').expect("link=<name>=<target>");
        std::os::unix::fs::symlink(target, name).expect("a link in cwd");
    }
    if let Some(spec) = directive("hardlink") {
        let (name, target) = spec.split_once('=').expect("hardlink=<name>=<target>");
        std::fs::hard_link(target, name).expect("a hard link in cwd");
    }
    if let Some(secs) = directive("sleep").and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(Duration::from_secs(secs));
    }

    let answer = if directive("dump").is_some() {
        json!({
            "argv": argv,
            "cwd": std::env::current_dir().ok(),
            "env": std::env::vars().collect::<std::collections::BTreeMap<_, _>>(),
        })
        .to_string()
    } else {
        directive("say").unwrap_or_else(|| "pong".to_string())
    };
    let failed = directive("fail").is_some();

    if flavor == "claude" {
        emit(json!({"type": "assistant", "session_id": session,
            "message": {"model": "claude-fake", "content": [{"type": "text", "text": answer}]}}));
        emit(
            json!({"type": "result", "subtype": if failed { "error_during_execution" } else { "success" },
            "is_error": failed, "result": if failed { "the fake was told to fail".to_string() } else { answer },
            "session_id": session, "total_cost_usd": 0.01,
            "usage": {"input_tokens": 100, "cache_read_input_tokens": 50, "output_tokens": 7}}),
        );
    } else if failed {
        emit(json!({"type": "turn.failed", "error": {"message": "the fake was told to fail"}}));
    } else {
        emit(
            json!({"type": "item.completed", "item": {"id": "item_0", "type": "agent_message", "text": answer}}),
        );
        emit(json!({"type": "turn.completed",
            "usage": {"input_tokens": 100, "cached_input_tokens": 50, "output_tokens": 7}}));
    }
    if failed {
        std::process::exit(1);
    }
}
