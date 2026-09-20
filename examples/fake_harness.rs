//! A stand-in for the harness CLIs, for the test suite (hard rule 8: tests
//! never call a real harness). Copied into a temp directory under the name
//! `claude` or `codex`, it answers `--version` with that CLI's fingerprint and
//! otherwise speaks that CLI's output stream.
//!
//! The brief (stdin) steers it, one directive per line:
//!
//! ```text
//! FAKE: say=<text>     the answer (default: pong)
//! FAKE: sleep=<secs>   think for a while first
//! FAKE: fail           report a failed run and exit 1
//! FAKE: dump           answer with this process's argv, cwd and environment
//! FAKE: child          leave a long-lived child in its own process group
//! ```

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
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
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(calls)
        .expect("calls");
    let _ = writeln!(log, "{}", argv[1..].join(" "));
    let plan: serde_json::Value = std::fs::read_to_string(exe.with_file_name("usage-cli.plan"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let guarded = argv.iter().any(|arg| arg == "--max-data-age");
    let answer = &plan[if guarded { "guarded" } else { "unguarded" }];
    if let Some(percent) = answer["percent"].as_f64() {
        println!(
            "{}",
            json!({"verdict": "from-the-fake", "percent": percent})
        );
    }
    std::process::exit(answer["code"].as_i64().unwrap_or(13) as i32);
}

fn main() {
    let exe = std::env::current_exe().expect("current_exe");
    let flavor = exe.file_name().unwrap().to_string_lossy().to_string();
    let argv: Vec<String> = std::env::args().collect();

    if flavor == "usage-cli" {
        return fake_meter(&exe, &argv);
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

    let session = argv
        .iter()
        .position(|arg| arg == "--session-id")
        .and_then(|at| argv.get(at + 1).cloned())
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
