//! Claude Code, headless: `claude -p`, brief on stdin, `stream-json` out.
//!
//! `stream-json` and not `json`: a killed `json` run prints nothing at all, so
//! the session, the tokens and the text so far would be lost (docs/SPIKE.md).

use serde_json::Value;

use super::{Activity, Harness, Progress, RunSpec, ToolLabel, Version, value_of};
use crate::model::{HarnessId, Role};

pub struct Claude;

/// The whole tool set of a read-only role. No Bash, no Edit, no Write, no
/// WebFetch — and `--strict-mcp-config` with no config means no MCP tools,
/// which could otherwise write on the callee's behalf.
const READ_ONLY_TOOLS: &str = "Read,Grep,Glob";

/// A writer's tool set: it may edit files, and Claude Code itself confines
/// Edit/Write to the working directory. Still NO Bash — without a sandbox a
/// shell could write anywhere, so a Claude writer cannot run the tests it
/// breaks; the caller does that. (Codex writers run inside its sandbox.)
const WRITER_TOOLS: &str = "Read,Grep,Glob,Edit,Write";

impl Harness for Claude {
    fn id(&self) -> HarnessId {
        HarnessId::Claude
    }

    fn binary_name(&self) -> &'static str {
        "claude"
    }

    fn fingerprint(&self, version_output: &str) -> Option<Version> {
        version_output
            .contains("Claude Code")
            .then(|| Version::find_in(version_output))
            .flatten()
    }

    fn tested(&self) -> (Version, Version) {
        (Version(2, 1, 0), Version(3, 0, 0))
    }

    fn build_argv(&self, spec: &RunSpec) -> Vec<String> {
        let mut argv: Vec<String> = [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--model",
            spec.target.model.as_str(),
            "--effort",
            spec.target.effort.as_str(),
        ]
        .map(String::from)
        .to_vec();
        if let Some(session) = &spec.resume {
            // Continues that session under the same id.
            argv.extend(["--resume".to_string(), session.clone()]);
        } else if let Some(session) = &spec.session_id {
            argv.extend(["--session-id".to_string(), session.clone()]);
        }
        let (tools, mode) = if spec.role.is_read_only() {
            // Anything that would need a permission is denied, not asked.
            (READ_ONLY_TOOLS, "dontAsk")
        } else {
            // Edits inside the working directory are accepted; anything else
            // that would need a permission is denied.
            (WRITER_TOOLS, "acceptEdits")
        };
        argv.extend(
            [
                "--tools",
                tools,
                "--strict-mcp-config",
                "--permission-mode",
                mode,
            ]
            .map(String::from),
        );
        argv
    }

    fn check_argv(&self, role: Role, argv: &[String]) -> Result<(), String> {
        if argv.first().map(String::as_str) != Some("-p") {
            return Err("not a headless (-p) invocation".to_string());
        }
        if argv.iter().any(|arg| arg == "--no-session-persistence") {
            return Err("--no-session-persistence makes the run unresumable".to_string());
        }
        // `-w`/`--worktree` would have the callee cut a tree of its own, with
        // its own hooks and outside cahoots' checks on where a writer works.
        if let Some(arg) = argv.iter().find(|arg| {
            let flag = arg.split('=').next().unwrap_or(arg);
            ["-c", "--continue", "--bare", "-w", "--worktree"].contains(&flag)
        }) {
            return Err(format!("{arg} is not a flag cahoots emits"));
        }
        // The fence is the PAIR (tool set, permission mode), and each role has
        // exactly one. Anything else — a missing flag, Bash in the tools, a
        // writer's mode on a reader — is refused.
        let (tools, mode) = if role.is_read_only() {
            (READ_ONLY_TOOLS, "dontAsk")
        } else {
            (WRITER_TOOLS, "acceptEdits")
        };
        if value_of(argv, "--tools") != Some(tools) {
            return Err(format!("the {role} role must run with --tools {tools}"));
        }
        if value_of(argv, "--permission-mode") != Some(mode) {
            return Err(format!(
                "the {role} role must run with --permission-mode {mode}"
            ));
        }
        if !argv.iter().any(|arg| arg == "--strict-mcp-config") {
            return Err("every role must run with --strict-mcp-config".to_string());
        }
        Ok(())
    }

    fn parse_line(&self, line: &str, progress: &mut Progress) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if let Some(session) = event["session_id"].as_str() {
            progress.session_id = Some(session.to_string());
        }
        match event["type"].as_str() {
            Some("assistant") => {
                if let Some(model) = event["message"]["model"].as_str() {
                    progress.model_reported = Some(model.to_string());
                }
                let text: Vec<&str> = event["message"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|block| block["text"].as_str())
                    .collect();
                if !text.is_empty() {
                    // Provisional: the `result` event is authoritative, but a
                    // killed run never prints one.
                    progress.final_text = Some(text.join("\n"));
                }
                // The last thing it said or called, in this message. Only its
                // own blocks: a tool's result comes back in a `user` event,
                // which is never read for this.
                if let Some(activity) = event["message"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .rev()
                    .find_map(activity_of)
                {
                    progress.activity = Some(activity);
                }
            }
            Some("result") => {
                if let Some(text) = event["result"].as_str() {
                    progress.final_text = Some(text.to_string());
                }
                let usage = &event["usage"];
                let count = |key: &str| usage[key].as_u64().unwrap_or(0);
                progress.tokens_cached = count("cache_read_input_tokens");
                progress.tokens_input = count("input_tokens")
                    + count("cache_creation_input_tokens")
                    + progress.tokens_cached;
                progress.tokens_output = count("output_tokens");
                progress.cost_usd = event["total_cost_usd"].as_f64();
                progress.permission_denials = event["permission_denials"]
                    .as_array()
                    .map_or(0, |d| d.len() as u32);
                let subtype = event["subtype"].as_str().unwrap_or_default();
                if subtype.contains("budget") {
                    progress.budget_stop = true;
                }
                if event["is_error"].as_bool() == Some(true) {
                    progress.failure = Some(
                        event["result"]
                            .as_str()
                            .filter(|text| !text.is_empty())
                            .unwrap_or(subtype)
                            .to_string(),
                    );
                }
            }
            _ => {}
        }
    }
}

/// One content block as a step: a `text` block, or a `tool_use` block under
/// cahoots' label for its tool, with the one input that says what it was
/// called on. A `thinking` block is not shown.
fn activity_of(block: &Value) -> Option<Activity> {
    match block["type"].as_str()? {
        "text" => Activity::said(block["text"].as_str()?),
        "tool_use" => {
            let name = block["name"].as_str().unwrap_or_default();
            let (label, on) = match name {
                "Read" => (ToolLabel::Read, Some("file_path")),
                "Grep" | "Glob" => (ToolLabel::Search, Some("pattern")),
                "Edit" | "Write" | "MultiEdit" => (ToolLabel::Edit, Some("file_path")),
                "NotebookEdit" => (ToolLabel::Edit, Some("notebook_path")),
                "Bash" => (ToolLabel::Command, Some("command")),
                "WebFetch" => (ToolLabel::Web, Some("url")),
                "WebSearch" => (ToolLabel::Web, Some("query")),
                _ if name.starts_with("mcp__") => (ToolLabel::Mcp, None),
                _ => (ToolLabel::Other, None),
            };
            let on = on.and_then(|key| block["input"][key].as_str());
            Some(Activity::tool(label, on))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(stream: &str) -> Progress {
        let mut progress = Progress::default();
        for line in stream.lines() {
            Claude.parse_line(line, &mut progress);
        }
        progress
    }

    #[test]
    fn a_successful_stream() {
        let progress = read(include_str!("../../tests/fixtures/streams/claude-ok.jsonl"));
        assert_eq!(progress.final_text.as_deref(), Some("The gate is sound."));
        assert_eq!(
            progress.session_id.as_deref(),
            Some("4cb89a87-0000-4000-8000-000000000001")
        );
        // Requested haiku, answered by something else: recorded, not hidden.
        assert_eq!(progress.model_reported.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(progress.tokens_input, 12 + 18333 + 200);
        assert_eq!(progress.tokens_cached, 200);
        assert_eq!(progress.tokens_output, 40);
        assert_eq!(progress.permission_denials, 1);
        assert!(progress.failure.is_none());
    }

    #[test]
    fn an_error_result_is_a_failure_and_junk_lines_are_skipped() {
        let progress = read(include_str!(
            "../../tests/fixtures/streams/claude-error.jsonl"
        ));
        assert_eq!(
            progress.failure.as_deref(),
            Some("Not logged in · Please run /login")
        );
    }

    #[test]
    fn the_latest_step_is_the_callees_own_text_or_tool_call() {
        let stream = include_str!("../../tests/fixtures/streams/claude-tools.jsonl");
        let at = |n: usize| {
            let cut: String = stream.lines().take(n).collect::<Vec<_>>().join("\n");
            read(&cut).activity.map(|a| a.shown(None))
        };
        // A text block, then a tool call in the same message: the call is later.
        assert_eq!(at(2).unwrap()["text"], "src/gate.rs");
        assert_eq!(at(2).unwrap()["tool"], "read");
        // A tool's result never becomes activity: the step stays the call.
        assert_eq!(at(3), at(2));
        let search = at(4).unwrap();
        assert_eq!(
            (search["tool"].clone(), search["text"].clone()),
            ("search".into(), "fn admit".into())
        );
        // A tool cahoots has no label for is `other`, and its name is not shown.
        let other = at(5).unwrap();
        assert_eq!(other["tool"], "other");
        assert!(!other.to_string().contains("Exfiltrate"), "{other}");
        // A thinking block says nothing; the answer's first line does.
        assert_eq!(at(6), at(5));
        let said = read(stream).activity.unwrap().shown(None);
        assert_eq!(said["kind"], "said");
        assert_eq!(said["text"], "The gate is sound.");
        assert_eq!(said["truncated"], true);
        assert!(!read(stream).final_text.unwrap().is_empty());
    }

    #[test]
    fn a_killed_run_keeps_what_it_had() {
        let stream = include_str!("../../tests/fixtures/streams/claude-ok.jsonl");
        let cut: String = stream.lines().take(3).collect::<Vec<_>>().join("\n");
        let progress = read(&cut);
        assert_eq!(progress.final_text.as_deref(), Some("Reading the gate."));
        assert!(progress.session_id.is_some());
    }
}
