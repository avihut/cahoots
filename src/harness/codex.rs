//! Codex, headless: `codex exec --json -`, brief on stdin, JSONL out.

use serde_json::Value;

use super::{Activity, Harness, Progress, RunSpec, ToolLabel, Version, value_of};
use crate::model::{Effort, HarnessId, Role};

pub struct Codex;

/// Codex takes the effort only as a config override.
const EFFORT_KEY: &str = "model_reasoning_effort=";

/// A callee NEVER escalates out of its sandbox. A callee inherits the user's
/// Codex configuration, and that configuration can hand approvals to an
/// automated reviewer (`approvals_reviewer = "auto_review"`): a blocked write
/// then becomes an approval request that is GRANTED, and a `--sandbox
/// read-only` run writes wherever it likes. Found against the real CLI on
/// 2026-09-20 (docs/SPIKE.md S7) — the sandbox flag alone is not a fence.
///
/// These are the only two `-c` overrides cahoots ever emits, and `check_argv`
/// holds `-c` to exactly these two shapes.
const NEVER_ESCALATE: &str = "approval_policy=\"never\"";

/// The user's exec-policy rules do not apply to a callee either: a rule with
/// `decision="allow"` runs its command OUTSIDE the sandbox (that is how a
/// Codex CALLER reaches cahoots at all), so a callee that inherited them
/// would hold exactly the authority the user granted their own sessions —
/// not the role's.
const NO_USER_RULES: &str = "--ignore-rules";

/// `codex exec resume` has no `--sandbox` flag, and a resumed session
/// otherwise INHERITS the sandbox it was started with (docs/SPIKE.md S8). So
/// on a resume the mode is stated again, as the third and last `-c` shape.
const SANDBOX_KEY: &str = "sandbox_mode=";

fn sandbox_of(role: Role) -> &'static str {
    if role.is_read_only() {
        "read-only"
    } else {
        "workspace-write"
    }
}

impl Harness for Codex {
    fn id(&self) -> HarnessId {
        HarnessId::Codex
    }

    fn binary_name(&self) -> &'static str {
        "codex"
    }

    fn fingerprint(&self, version_output: &str) -> Option<Version> {
        version_output
            .contains("codex-cli")
            .then(|| Version::find_in(version_output))
            .flatten()
    }

    fn tested(&self) -> (Version, Version) {
        (Version(0, 155, 0), Version(0, 200, 0))
    }

    fn build_argv(&self, spec: &RunSpec) -> Vec<String> {
        let mut argv = vec!["exec".to_string()];
        if let Some(thread) = &spec.resume {
            argv.extend(["resume".to_string(), thread.clone()]);
        }
        argv.extend(["--json", NO_USER_RULES, "-c", NEVER_ESCALATE].map(String::from));
        // A reader: no repo check — advice about a plain directory is a fine
        // thing to ask for. A writer runs inside Codex's own sandbox: it may
        // write under its working directory (a worktree of its own) and the
        // temp directories, nowhere else, with no network, and Codex's
        // repository check stays ON.
        if spec.resume.is_some() {
            argv.extend([
                "-c".to_string(),
                format!("{SANDBOX_KEY}\"{}\"", sandbox_of(spec.role)),
            ]);
        } else {
            argv.extend(["--sandbox", sandbox_of(spec.role)].map(String::from));
        }
        if spec.role.is_read_only() {
            argv.push("--skip-git-repo-check".to_string());
        }
        argv.extend([
            "-m".to_string(),
            spec.target.model.as_str().to_string(),
            "-c".to_string(),
            format!("{EFFORT_KEY}{}", spec.target.effort.as_str()),
            // The brief is read from stdin.
            "-".to_string(),
        ]);
        argv
    }

    fn check_argv(&self, role: Role, argv: &[String]) -> Result<(), String> {
        if argv.first().map(String::as_str) != Some("exec") {
            return Err("not a headless (exec) invocation".to_string());
        }
        for (at, arg) in argv.iter().enumerate() {
            if arg == "-c" {
                let value = argv.get(at + 1).map(String::as_str).unwrap_or_default();
                let effort_ok = value
                    .strip_prefix(EFFORT_KEY)
                    .is_some_and(|level| level.parse::<Effort>().is_ok());
                let sandbox_ok = value == format!("{SANDBOX_KEY}\"{}\"", sandbox_of(role));
                if !effort_ok && !sandbox_ok && value != NEVER_ESCALATE {
                    return Err(format!(
                        "-c {value} is not one of the config overrides cahoots emits for {role}"
                    ));
                }
            }
            let flag = arg.split('=').next().unwrap_or(arg);
            if ["-p", "-C", "--cd", "--oss", "--worktree"].contains(&flag) {
                return Err(format!("{arg} is not a flag cahoots emits"));
            }
        }
        // The fence has three parts, and every role needs all of them: the
        // sandbox, no escalation out of it, and none of the user's allow-rules.
        if !argv
            .windows(2)
            .any(|pair| pair[0] == "-c" && pair[1] == NEVER_ESCALATE)
        {
            return Err(format!("every Codex run must carry -c {NEVER_ESCALATE}"));
        }
        if !argv.iter().any(|arg| arg == NO_USER_RULES) {
            return Err(format!("every Codex run must carry {NO_USER_RULES}"));
        }
        // The mode is a flag on a fresh run and a `-c` on a resumed one —
        // exactly one of the two, and it must be the ROLE's mode.
        let sandbox = sandbox_of(role);
        let resuming = argv.get(1).map(String::as_str) == Some("resume");
        let stated = if resuming {
            let wanted = format!("{SANDBOX_KEY}\"{sandbox}\"");
            argv.windows(2)
                .any(|pair| pair[0] == "-c" && pair[1] == wanted)
                && value_of(argv, "--sandbox").is_none()
        } else {
            value_of(argv, "--sandbox") == Some(sandbox)
        };
        if !stated {
            return Err(format!("the {role} role must run in the {sandbox} sandbox"));
        }
        if !role.is_read_only() && argv.iter().any(|arg| arg == "--skip-git-repo-check") {
            return Err("a writer must not skip Codex's repository check".to_string());
        }
        Ok(())
    }

    fn parse_line(&self, line: &str, progress: &mut Progress) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match event["type"].as_str() {
            Some("thread.started") => {
                if let Some(thread) = event["thread_id"].as_str() {
                    progress.session_id = Some(thread.to_string());
                }
            }
            Some("item.started") => {
                if let Some(activity) = activity_of(&event["item"]) {
                    progress.activity = Some(activity);
                }
            }
            Some("item.completed") => {
                let item = &event["item"];
                if let Some(activity) = activity_of(item) {
                    progress.activity = Some(activity);
                }
                match item["type"].as_str() {
                    // The last message is the answer; earlier ones are narration.
                    Some("agent_message") => {
                        if let Some(text) = item["text"].as_str() {
                            progress.final_text = Some(text.to_string());
                        }
                    }
                    // An `error` ITEM is a warning (docs/SPIKE.md S3), never
                    // the run's failure.
                    Some("error") => {
                        if let Some(message) = item["message"].as_str() {
                            progress.note(message);
                        }
                    }
                    _ => {}
                }
            }
            Some("turn.completed") => {
                let usage = &event["usage"];
                let count = |key: &str| usage[key].as_u64().unwrap_or(0);
                progress.tokens_input += count("input_tokens");
                progress.tokens_cached += count("cached_input_tokens");
                progress.tokens_output += count("output_tokens");
            }
            Some("turn.failed") => {
                progress.failure = Some(
                    event["error"]["message"]
                        .as_str()
                        .unwrap_or("the turn failed")
                        .to_string(),
                );
            }
            // A top-level `error` is often a retry notice; `turn.failed` and
            // the exit status are what decide failure.
            Some("error") => {
                if let Some(message) = event["message"].as_str() {
                    progress.note(message);
                }
            }
            _ => {}
        }
    }
}

/// One item as a step: a message it wrote, or a tool it called, under
/// cahoots' label, with what it was called on. Never an item's output
/// (`aggregated_output` and the like), which is the tool's, not the callee's,
/// and never its reasoning.
fn activity_of(item: &Value) -> Option<Activity> {
    match item["type"].as_str()? {
        "agent_message" => Activity::said(item["text"].as_str()?),
        "command_execution" => Some(Activity::tool(ToolLabel::Command, item["command"].as_str())),
        "file_change" => Some(Activity::tool(
            ToolLabel::Edit,
            item["changes"][0]["path"].as_str(),
        )),
        "web_search" => Some(Activity::tool(ToolLabel::Web, item["query"].as_str())),
        "mcp_tool_call" => Some(Activity::tool(ToolLabel::Mcp, None)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(stream: &str) -> Progress {
        let mut progress = Progress::default();
        for line in stream.lines() {
            Codex.parse_line(line, &mut progress);
        }
        progress
    }

    #[test]
    fn a_successful_stream() {
        let progress = read(include_str!("../../tests/fixtures/streams/codex-ok.jsonl"));
        assert_eq!(
            progress.final_text.as_deref(),
            Some("The gate is sound.\n\nOne nit: the reserve is unused.")
        );
        assert_eq!(
            progress.session_id.as_deref(),
            Some("01a0bf61-0000-7000-8000-000000000001")
        );
        assert_eq!(
            (progress.tokens_input, progress.tokens_output),
            (18465, 805)
        );
        assert!(
            progress.failure.is_none(),
            "a warning item is not a failure"
        );
        assert_eq!(progress.notes.len(), 1);
    }

    #[test]
    fn the_latest_step_is_the_callees_own_message_or_tool_call() {
        let stream = include_str!("../../tests/fixtures/streams/codex-ok.jsonl");
        let at = |n: usize| {
            let cut: String = stream.lines().take(n).collect::<Vec<_>>().join("\n");
            read(&cut).activity.map(|a| a.shown(None))
        };
        assert_eq!(at(4).unwrap()["text"], "Looking at the module first.");
        // Started: the command is what it is doing now.
        let running = at(5).unwrap();
        assert_eq!(running["tool"], "command");
        assert_eq!(running["text"], "/bin/zsh -lc 'rg -n gate src'");
        // Its output is the repository's text, never the callee's step.
        let done = at(6).unwrap();
        assert_eq!(done, running);
        assert!(!done.to_string().contains("IGNORE"), "{done}");
        // Reasoning is not shown; a file change is, by its first path.
        assert_eq!(at(7), at(6));
        assert_eq!(at(8).unwrap()["tool"], "edit");
        assert_eq!(at(8).unwrap()["text"], "src/gate.rs");
        let last = read(stream).activity.unwrap().shown(None);
        assert_eq!(
            (last["kind"].clone(), last["text"].clone()),
            ("said".into(), "The gate is sound.".into())
        );
        assert_eq!(last["truncated"], true);
    }

    #[test]
    fn notes_are_bounded_and_few() {
        let mut progress = Progress::default();
        for n in 0..100 {
            Codex.parse_line(
                &json!({"type": "error", "message": format!("retry {n}\u{1b}[2J {}", "z".repeat(1000))})
                    .to_string(),
                &mut progress,
            );
        }
        assert_eq!(progress.notes.len(), crate::harness::MAX_NOTES);
        assert!(progress.notes[0].starts_with("retry 0 "));
        for note in &progress.notes {
            // A string on disk, one past the bound, so it shows as cut.
            assert_eq!(note.chars().count(), crate::harness::NOTE_CHARS + 1);
            assert!(!note.contains('\u{1b}'));
            let shown = crate::harness::CalleeText::bound(
                note,
                crate::harness::NOTE_CHARS,
                crate::harness::Keep::Start,
            )
            .unwrap();
            assert!(shown.truncated());
        }
        let on_disk = serde_json::to_value(&progress).unwrap();
        assert!(on_disk["notes"][0].is_string(), "{on_disk}");
    }

    #[test]
    fn a_failed_turn_is_a_failure_and_a_retry_notice_is_not() {
        let progress = read(include_str!(
            "../../tests/fixtures/streams/codex-failed.jsonl"
        ));
        assert!(progress.failure.as_deref().unwrap().contains("usage limit"));
        assert_eq!(progress.notes.len(), 1);
    }

    #[test]
    fn only_the_effort_override_may_follow_dash_c() {
        let argv = |value: &str| -> Vec<String> {
            [
                "exec",
                "--json",
                "--ignore-rules",
                "-c",
                "approval_policy=\"never\"",
                "--sandbox",
                "read-only",
                "-c",
                value,
                "-",
            ]
            .map(String::from)
            .to_vec()
        };
        assert!(
            Codex
                .check_argv(Role::Advise, &argv("model_reasoning_effort=high"))
                .is_ok()
        );
        for bad in [
            "approval_policy=\"on-request\"",
            "approval_policy=\"on-failure\"",
            "approvals_reviewer=\"auto_review\"",
            "model_reasoning_effort=ultra",
            "sandbox_mode=\"danger-full-access\"",
            "model_reasoning_effort=high\nsandbox_mode=x",
            "",
        ] {
            assert!(
                Codex.check_argv(Role::Advise, &argv(bad)).is_err(),
                "{bad:?}"
            );
        }
    }
}
