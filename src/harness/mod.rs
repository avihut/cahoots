//! One definition per harness: how its headless CLI is invoked, what proves an
//! invocation read-only, and how its output stream is read.
//!
//! Commands are built HERE, in code, from typed values (hard rule 3). The only
//! way to get a command line out of this module is [`command_line`], which
//! builds it and then puts it through [`validate`] — so a future edit to a
//! builder that drops the read-only flag, or adds a flag that widens
//! authority, fails closed at run time and fails a test before that.

mod claude;
mod codex;

use serde::{Deserialize, Serialize};

use crate::exit::{Exit, Fail, Res};
use crate::model::{Candidate, HarnessId, Role};

/// Everything a harness needs to build one invocation. The brief is not here:
/// it travels on the callee's stdin, never in argv.
#[derive(Debug, Clone)]
pub struct RunSpec {
    pub role: Role,
    pub target: Candidate,
    /// Preset by cahoots for harnesses that accept one (Claude), so a run is
    /// resumable even if it is killed before it prints anything.
    pub session_id: Option<String>,
}

/// What has been learned from a callee's output stream so far.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub session_id: Option<String>,
    /// The model the stream itself reported — a silent fallback shows up as a
    /// mismatch with the model that was requested.
    pub model_reported: Option<String>,
    pub final_text: Option<String>,
    pub tokens_input: u64,
    pub tokens_cached: u64,
    pub tokens_output: u64,
    pub cost_usd: Option<f64>,
    /// Set when the STREAM says the run failed; the exit status is the other
    /// failure signal, and the supervisor's business.
    pub failure: Option<String>,
    /// The callee's own budget flag stopped it.
    pub budget_stop: bool,
    /// Tool calls the callee's permission mode refused — evidence that the
    /// read-only fence was leaned on.
    pub permission_denials: u32,
    /// Warnings worth keeping, never failures.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// The first `a.b.c` in a string.
    pub fn find_in(text: &str) -> Option<Version> {
        text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .find_map(|word| {
                let mut parts = word.split('.').map(str::parse::<u32>);
                match (parts.next(), parts.next(), parts.next(), parts.next()) {
                    (Some(Ok(a)), Some(Ok(b)), Some(Ok(c)), None) => Some(Version(a, b, c)),
                    _ => None,
                }
            })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

pub trait Harness: Sync {
    fn id(&self) -> HarnessId;
    /// The executable's name on PATH.
    fn binary_name(&self) -> &'static str;
    /// Recognises this harness in its `--version` output. A binary with the
    /// right name and the wrong fingerprint is something else (on some Macs
    /// `agy` is an IDE launcher, not an agent CLI).
    fn fingerprint(&self, version_output: &str) -> Option<Version>;
    /// Oldest version the builders and parsers were written against, and the
    /// first version they have NOT been checked against.
    fn tested(&self) -> (Version, Version);
    fn build_argv(&self, spec: &RunSpec) -> Vec<String>;
    /// Harness-specific half of [`validate`]: `Err` names what is wrong.
    fn check_argv(&self, role: Role, argv: &[String]) -> Result<(), String>;
    /// Folds one stdout line into `progress`. Lines that do not parse are
    /// skipped: harnesses print plain text on stdout too (docs/SPIKE.md S3).
    fn parse_line(&self, line: &str, progress: &mut Progress);
}

pub fn harness(id: HarnessId) -> &'static dyn Harness {
    match id {
        HarnessId::Claude => &claude::Claude,
        HarnessId::Codex => &codex::Codex,
    }
}

/// The one way to obtain a command line: build, then validate.
pub fn command_line(spec: &RunSpec) -> Res<Vec<String>> {
    let harness = harness(spec.target.harness);
    let argv = harness.build_argv(spec);
    validate(harness, spec.role, &argv)?;
    Ok(argv)
}

/// Flags no cahoots invocation may ever carry, whatever the harness: they
/// widen what the callee may touch, or swap its configuration for another.
const NEVER: [&str; 11] = [
    "--add-dir",
    "--settings",
    "--mcp-config",
    "--plugin-dir",
    "--agents",
    "--profile",
    "--config",
    "--enable",
    "--disable",
    "--approve-for-me",
    "--ephemeral",
];

fn validate(harness: &dyn Harness, role: Role, argv: &[String]) -> Res<()> {
    let refuse = |why: String| {
        Err(Fail::new(
            Exit::Internal,
            format!(
                "refusing to run {}: {why} (this is a bug in cahoots, not in your setup)",
                harness.id()
            ),
        ))
    };
    for arg in argv {
        let flag = arg.split('=').next().unwrap_or(arg);
        if flag.starts_with("--dangerously") || flag.starts_with("--allow-dangerously") {
            return refuse(format!("{arg} bypasses the callee's own permissions"));
        }
        if NEVER.contains(&flag) {
            return refuse(format!(
                "{flag} widens or replaces the callee's configuration"
            ));
        }
        if arg.contains("bypassPermissions") || arg.contains("danger-full-access") {
            return refuse(format!("{arg} turns the callee's fence off"));
        }
    }
    harness.check_argv(role, argv).or_else(refuse)
}

/// The value that follows `flag`, if the flag is present.
fn value_of<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    argv.iter()
        .position(|arg| arg == flag)
        .and_then(|at| argv.get(at + 1))
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Effort, ModelName};

    pub(super) fn spec(harness: HarnessId, role: Role) -> RunSpec {
        RunSpec {
            role,
            target: Candidate {
                harness,
                model: ModelName::try_from("some-model".to_string()).unwrap(),
                effort: Effort::High,
            },
            session_id: Some("4cb89a87-0000-4000-8000-000000000001".to_string()),
        }
    }

    #[test]
    fn every_harness_builds_a_valid_command_for_every_role() {
        for id in HarnessId::ALL {
            for role in Role::ALL {
                command_line(&spec(id, role)).unwrap();
            }
        }
    }

    /// The validator is the last line, so it is tested against a builder that
    /// has gone wrong: every one of these must be refused for every harness.
    #[test]
    fn a_command_that_widens_authority_is_refused() {
        for id in HarnessId::ALL {
            let good = command_line(&spec(id, Role::Review)).unwrap();
            for extra in [
                vec!["--dangerously-skip-permissions"],
                vec!["--dangerously-bypass-approvals-and-sandbox"],
                vec!["--allow-dangerously-skip-permissions"],
                vec!["--add-dir", "/"],
                vec!["--add-dir=/"],
                vec!["--settings", "{}"],
                vec!["--mcp-config", "x.json"],
                vec!["--config", "sandbox_mode=\"danger-full-access\""],
                vec!["-c", "sandbox_mode=\"danger-full-access\""],
                vec!["--permission-mode", "bypassPermissions"],
                vec!["--sandbox", "danger-full-access"],
                vec!["--ephemeral"],
                vec!["--profile", "yolo"],
            ] {
                let mut argv = good.clone();
                argv.extend(extra.iter().map(|s| s.to_string()));
                assert!(
                    validate(harness(id), Role::Review, &argv).is_err(),
                    "{id}: {extra:?} was let through"
                );
            }
        }
    }

    #[test]
    fn a_role_without_its_fence_is_refused() {
        for id in HarnessId::ALL {
            for role in Role::ALL {
                let good = command_line(&spec(id, role)).unwrap();
                // Drop each argument of the fence in turn; every result must fail.
                let fence: &[&str] = match (id, role.is_read_only()) {
                    (HarnessId::Claude, true) => &[
                        "--tools",
                        "Read,Grep,Glob",
                        "--strict-mcp-config",
                        "dontAsk",
                    ],
                    (HarnessId::Claude, false) => &[
                        "--tools",
                        "Read,Grep,Glob,Edit,Write",
                        "--strict-mcp-config",
                        "acceptEdits",
                    ],
                    (HarnessId::Codex, true) => &[
                        "--sandbox",
                        "read-only",
                        "--ignore-rules",
                        "approval_policy=\"never\"",
                    ],
                    (HarnessId::Codex, false) => &[
                        "--sandbox",
                        "workspace-write",
                        "--ignore-rules",
                        "approval_policy=\"never\"",
                    ],
                };
                for part in fence {
                    let argv: Vec<String> = good.iter().filter(|a| a != part).cloned().collect();
                    assert!(
                        validate(harness(id), role, &argv).is_err(),
                        "{id} {role}: dropping {part} went unnoticed"
                    );
                }
            }
        }
    }

    /// A reader never gets a writer's command line, whatever a builder does:
    /// the validator is told the ROLE, and holds the argv to that role's fence.
    #[test]
    fn a_writers_command_is_refused_for_a_reader_and_the_reverse() {
        for id in HarnessId::ALL {
            let writer = command_line(&spec(id, Role::Implement)).unwrap();
            let reader = command_line(&spec(id, Role::Review)).unwrap();
            assert!(
                validate(harness(id), Role::Review, &writer).is_err(),
                "{id}"
            );
            assert!(
                validate(harness(id), Role::Implement, &reader).is_err(),
                "{id}"
            );
            assert!(
                !writer.iter().any(|arg| arg.contains("Bash")),
                "{id}: a writer got a shell outside a sandbox"
            );
        }
    }

    /// Drift: every flag cahoots emits exists in the harness's real `--help`,
    /// captured at the tested version (tests/fixtures/help).
    #[test]
    fn every_emitted_flag_exists_in_the_captured_help() {
        let help = |id| match id {
            HarnessId::Claude => include_str!("../../tests/fixtures/help/claude-2.1.278.txt"),
            HarnessId::Codex => include_str!("../../tests/fixtures/help/codex-exec-0.155.1.txt"),
        };
        for id in HarnessId::ALL {
            for arg in command_line(&spec(id, Role::Advise)).unwrap() {
                if arg.starts_with('-') && arg != "-" {
                    assert!(help(id).contains(&arg), "{id} help has no {arg}");
                }
            }
        }
    }

    #[test]
    fn versions_are_found_in_real_version_strings() {
        assert_eq!(
            Version::find_in("2.1.278 (Claude Code)"),
            Some(Version(2, 1, 278))
        );
        assert_eq!(
            Version::find_in("codex-cli 0.155.1"),
            Some(Version(0, 155, 1))
        );
        assert_eq!(Version::find_in("Antigravity 1.107"), None);
        for id in HarnessId::ALL {
            let (min, next) = harness(id).tested();
            assert!(min < next);
        }
    }
}
