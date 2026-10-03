//! Choosing who to ask. The role's candidates are walked in order; the first
//! one that is enabled, is not the caller, has a free slot, has a real
//! harness binary behind it and passes the gate is the target. `pick` and
//! `run` share this function, so what `pick` says is what `run` does.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::dirs::{Dirs, ensure_private_dir};
use crate::exit::{Exit, Fail, Res};
use crate::gate::{self, Admission};
use crate::harness::{self, Version};
use crate::model::{Candidate, HarnessId, Role};
use crate::registry::{Registry, Routing};
use crate::run::record::try_lock_file;
use crate::spawn;

#[derive(Debug, Serialize)]
pub struct Skip {
    pub candidate: Candidate,
    pub code: u8,
    pub class: &'static str,
    pub reason: String,
}

#[derive(Debug)]
pub struct Choice {
    pub target: Candidate,
    pub binary: PathBuf,
    pub version: Version,
    pub admission: Admission,
    pub skipped: Vec<Skip>,
}

/// Enabled candidates for the role, without the caller. `--to` narrows the
/// list to one harness, and turns "not enabled" into its own refusal.
fn candidates(
    registry: &Registry,
    routing: &Routing<'_>,
    caller: Option<HarnessId>,
    to: Option<HarnessId>,
) -> Res<Vec<Candidate>> {
    if let Some(to) = to {
        if Some(to) == caller {
            return Err(Fail::policy(format!(
                "{to} is the caller; a harness does not delegate to itself"
            )));
        }
        if !registry.harness(to).enabled {
            return Err(Fail::new(
                Exit::TargetUnavailable,
                format!(
                    "{to} is not enabled as a target — a person enables it with `cahoots enable {to}`"
                ),
            ));
        }
    }
    let list: Vec<Candidate> = routing
        .candidates
        .iter()
        .filter(|c| Some(c.harness) != caller)
        .filter(|c| to.is_none_or(|to| c.harness == to))
        .filter(|c| registry.harness(c.harness).enabled)
        .cloned()
        .collect();
    if list.is_empty() {
        return Err(Fail::new(
            Exit::NoEligibleTarget,
            match routing.kind {
                Some(name) => format!(
                    "no enabled candidate for task kind {:?} after applying --caller and --to — a person checks its candidates and enabled targets in config.toml",
                    name.as_str()
                ),
                None => format!(
                    "no enabled target for the {} role — a person enables one with `cahoots enable <harness>`",
                    routing.role
                ),
            },
        ));
    }
    Ok(list)
}

fn slot_free(dirs: &Dirs, registry: &Registry, harness: HarnessId) -> bool {
    let _ = ensure_private_dir(&dirs.slots());
    (0..registry.harness(harness).max_concurrent).any(|n| {
        let path = dirs.slots().join(format!("{harness}.{n}.lock"));
        matches!(try_lock_file(&path), Ok(Some(_)))
    })
}

/// The harness binary, held to the binary policy and to its fingerprint.
pub fn locate(registry: &Registry, id: HarnessId, workspace: &[&Path]) -> Res<(PathBuf, Version)> {
    let tool = harness::harness(id);
    let configured = registry.harness(id).binary.as_deref();
    let binary = spawn::resolve_binary(tool.binary_name(), configured, workspace)?;
    let version = spawn::run_helper(&binary, &["--version"], None, Duration::from_secs(15))
        .ok()
        .and_then(|output| tool.fingerprint(&output.stdout))
        .ok_or_else(|| {
            Fail::new(
                Exit::TargetUnavailable,
                format!("{} does not identify itself as {id}", binary.display()),
            )
        })?;
    if version < tool.tested().0 {
        return Err(Fail::new(
            Exit::TargetUnavailable,
            format!(
                "{id} {version} is older than the oldest version cahoots supports ({})",
                tool.tested().0
            ),
        ));
    }
    Ok((binary, version))
}

/// Whether ONE candidate can take a run right now: a free slot, a real binary,
/// and the gate. `resume` uses this directly — a session belongs to the
/// harness and model that started it, so there is nobody to fall through to.
pub fn eligible(
    dirs: &Dirs,
    registry: &Registry,
    candidate: &Candidate,
    role: Role,
    workspace: &[&Path],
) -> Res<(PathBuf, Version, Admission)> {
    if !slot_free(dirs, registry, candidate.harness) {
        return Err(Fail::new(
            Exit::Busy,
            format!(
                "{} is already running as many jobs as it may",
                candidate.harness
            ),
        ));
    }
    let (binary, version) = locate(registry, candidate.harness, workspace)?;
    let admission = gate::admit(dirs, registry, candidate, role)?;
    Ok((binary, version, admission))
}

pub fn choose(
    dirs: &Dirs,
    registry: &Registry,
    routing: &Routing<'_>,
    caller: Option<HarnessId>,
    to: Option<HarnessId>,
    workspace: &[&Path],
) -> Res<Choice> {
    let mut skipped: Vec<(Candidate, Fail)> = Vec::new();
    for candidate in candidates(registry, routing, caller, to)? {
        let attempt = eligible(dirs, registry, &candidate, routing.role, workspace);
        match attempt {
            Ok((binary, version, admission)) => {
                return Ok(Choice {
                    target: candidate,
                    binary,
                    version,
                    admission,
                    skipped: skipped.into_iter().map(skip).collect(),
                });
            }
            Err(fail) => skipped.push((candidate, fail)),
        }
    }
    // One candidate: its own refusal is the most useful answer. Several: none
    // was eligible, and the message says why each was not.
    if skipped.len() == 1 {
        return Err(skipped.remove(0).1);
    }
    let reasons: Vec<String> = skipped
        .iter()
        .map(|(c, fail)| {
            format!(
                "{} ({}, {}): {}",
                c.harness,
                c.model.as_str(),
                c.effort,
                fail.message
            )
        })
        .collect();
    Err(Fail::new(Exit::NoEligibleTarget, reasons.join("; ")))
}

fn skip((candidate, fail): (Candidate, Fail)) -> Skip {
    Skip {
        candidate,
        code: fail.exit.code(),
        class: fail.exit.class(),
        reason: fail.message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UserConfig;

    fn registry() -> Registry {
        Registry::effective(
            &UserConfig::parse(
                r#"schema = 1
harness.codex.enabled = true
harness.claude.enabled = true
[kinds.rust-review]
description = "Review Rust."
role = "review"
candidates = [
 { harness = "codex", model = "m", effort = "high" },
 { harness = "codex", model = "m", effort = "medium" },
 { harness = "claude", model = "other", effort = "low" },
]
"#,
            )
            .unwrap(),
        )
    }

    #[test]
    fn kind_candidates_only_narrow_and_keep_distinct_efforts() {
        let mut registry = registry();
        let routing = registry.routing(None, Some("rust-review")).unwrap();
        assert_eq!(
            candidates(&registry, &routing, None, None).unwrap(),
            routing.candidates
        );
        assert_eq!(
            candidates(&registry, &routing, None, Some(HarnessId::Codex)).unwrap(),
            routing.candidates[..2]
        );
        assert_eq!(
            candidates(&registry, &routing, Some(HarnessId::Codex), None).unwrap(),
            routing.candidates[2..]
        );
        registry
            .harnesses
            .get_mut(&HarnessId::Claude)
            .unwrap()
            .enabled = false;
        let routing = registry.routing(None, Some("rust-review")).unwrap();
        assert_eq!(
            candidates(&registry, &routing, None, None).unwrap(),
            routing.candidates[..2]
        );
    }

    #[test]
    fn kind_target_refusals_keep_existing_precedence() {
        let mut registry = registry();
        registry
            .harnesses
            .get_mut(&HarnessId::Claude)
            .unwrap()
            .enabled = false;
        let routing = registry.routing(None, Some("rust-review")).unwrap();
        assert_eq!(
            candidates(
                &registry,
                &routing,
                Some(HarnessId::Claude),
                Some(HarnessId::Claude)
            )
            .unwrap_err()
            .exit,
            Exit::Policy
        );
        assert_eq!(
            candidates(&registry, &routing, None, Some(HarnessId::Claude))
                .unwrap_err()
                .exit,
            Exit::TargetUnavailable
        );
        let fail = candidates(&registry, &routing, Some(HarnessId::Codex), None).unwrap_err();
        assert_eq!(fail.exit, Exit::NoEligibleTarget);
        assert_eq!(
            fail.message,
            "no enabled candidate for task kind \"rust-review\" after applying --caller and --to — a person checks its candidates and enabled targets in config.toml"
        );
        let role = registry.routing(Some(Role::Review), None).unwrap();
        assert!(
            candidates(&registry, &role, Some(HarnessId::Codex), None)
                .unwrap_err()
                .message
                .starts_with("no enabled target for the review role")
        );
        registry
            .kinds
            .values_mut()
            .next()
            .unwrap()
            .candidates
            .retain(|c| c.harness == HarnessId::Codex);
        registry
            .harnesses
            .get_mut(&HarnessId::Claude)
            .unwrap()
            .enabled = true;
        assert_eq!(
            candidates(
                &registry,
                &registry.routing(None, Some("rust-review")).unwrap(),
                None,
                Some(HarnessId::Claude)
            )
            .unwrap_err()
            .exit,
            Exit::NoEligibleTarget
        );
    }
}
