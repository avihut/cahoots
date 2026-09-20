//! Codex, headless: `codex exec --json -`, brief on stdin, JSONL out.

use serde_json::Value;

use super::{Harness, Progress, RunSpec, Version, value_of};
use crate::model::{Effort, HarnessId, Role};

pub struct Codex;

/// Codex takes the effort only as a config override. This is the ONE `-c` key
/// cahoots ever emits, and `check_argv` holds `-c` to exactly this shape.
const EFFORT_KEY: &str = "model_reasoning_effort=";

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
        let mut argv: Vec<String> = ["exec", "--json"].map(String::from).to_vec();
        if spec.role.is_read_only() {
            // No repo check for a run that cannot write: advice about a plain
            // directory is a fine thing to ask for.
            argv.extend(["--sandbox", "read-only", "--skip-git-repo-check"].map(String::from));
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
                if !effort_ok {
                    return Err(format!(
                        "-c {value} is not the one config override cahoots emits"
                    ));
                }
            }
            if arg == "-p" || arg == "-C" || arg == "--cd" || arg == "--oss" || arg == "--worktree"
            {
                return Err(format!("{arg} is not a flag cahoots emits"));
            }
        }
        if role.is_read_only() && value_of(argv, "--sandbox") != Some("read-only") {
            return Err("a read-only role must run with --sandbox read-only".to_string());
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
            Some("item.completed") => {
                let item = &event["item"];
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
                            progress.notes.push(message.to_string());
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
                    progress.notes.push(message.to_string());
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            ["exec", "--json", "--sandbox", "read-only", "-c", value, "-"]
                .map(String::from)
                .to_vec()
        };
        assert!(
            Codex
                .check_argv(Role::Advise, &argv("model_reasoning_effort=high"))
                .is_ok()
        );
        for bad in [
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
