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
use crate::registry::Registry;
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
    role: Role,
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
    let list: Vec<Candidate> = registry.roles[&role]
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
            format!(
                "no enabled target for the {role} role — a person enables one with `cahoots enable <harness>`"
            ),
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

pub fn choose(
    dirs: &Dirs,
    registry: &Registry,
    role: Role,
    caller: Option<HarnessId>,
    to: Option<HarnessId>,
    workspace: &[&Path],
) -> Res<Choice> {
    let mut skipped: Vec<(Candidate, Fail)> = Vec::new();
    for candidate in candidates(registry, role, caller, to)? {
        let attempt = (|| {
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
            let admission = gate::admit(dirs, registry, &candidate, role)?;
            Ok((binary, version, admission))
        })();
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
        .map(|(c, fail)| format!("{} ({}): {}", c.harness, c.model.as_str(), fail.message))
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
