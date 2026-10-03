//! Choosing who to ask. The role's candidates are walked in order; the first
//! one that is enabled, is not the caller, has a free slot, has a real
//! harness binary behind it and passes the gate is the target. `pick` and
//! `run` share this function and its admission, so what `pick` says is what
//! `run` does — with one difference: `run` alone may, on a configured share of
//! new runs, swap the first two entries BEFORE that walk (`explore.rs`). Every
//! candidate then passes the same checks, in the order that swap leaves.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::dirs::{Dirs, ensure_private_dir};
use crate::exit::{Exit, Fail, Res};
use crate::explore;
use crate::gate::{self, Admission};
use crate::harness::{self, Version};
use crate::model::{Candidate, HarnessId, Role};
use crate::registry::{Registry, Routing};
use crate::run::record::try_lock_file;
use crate::spawn;
use crate::tools::{Tool, Tools};

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
    /// The share in effect for THIS selection: the list's own, or zero where
    /// exploration cannot apply (`--to`, fewer than two candidates left, an
    /// identical second entry).
    pub exploration_share: f64,
    /// The draw promoted the second entry, and it is the one chosen. A
    /// fallback to any other entry is not exploration.
    pub exploration: bool,
}

/// Whether this selection may try the next candidate first. Only `run` draws,
/// from the id it is about to record; `pick` is a preview of ordinary routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Ordinary,
    /// The result of the draw for the run's own id.
    Drawn(bool),
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

/// The harness binary: the one a person pinned (`harness.<id>.binary`),
/// never a PATH lookup, held to the binary policy and to its fingerprint,
/// and its settings folder, if one is set. Unpinned is the person's setup to
/// fix (34); missing or not the harness is 31, and a candidate is skipped
/// for it. `git` is the located git, whose directories come first on the
/// PATH (`None` where git cannot be located: doctor).
pub fn locate(
    registry: &Registry,
    git: Option<&Tool>,
    id: HarnessId,
    workspace: &[&Path],
) -> Res<(PathBuf, Version)> {
    let entry = registry.harness(id);
    let pinned = entry.binary.as_deref().ok_or_else(|| {
        Fail::config(format!(
            "{id} has no pinned program — `cahoots enable {id}` or `cahoots install` pins the one \
             on your PATH, or choose one with `cahoots settings` (harness.{id}.binary)"
        ))
    })?;
    let git = git.map(Tool::pinned);
    let recorded = registry.tools.recorded();
    locate_at(
        id,
        pinned,
        entry.home.as_deref(),
        git,
        recorded.as_deref(),
        workspace,
    )
}

/// The harness at `path`, for a person choosing one (`settings`, `enable`,
/// `install`): held to exactly what a run holds it to. `pins` give the PATH
/// it is asked its version on: the pinned git's directory, if that pin
/// passes the binary policy, and the recorded PATH.
pub fn locate_pinned(
    id: HarnessId,
    path: &Path,
    home: Option<&Path>,
    pins: &crate::tools::Pins,
) -> Res<(PathBuf, Version)> {
    let git = pins
        .git
        .as_deref()
        .filter(|git| spawn::pinned_system_tool("git", git, &[]).is_ok());
    let recorded = pins.recorded();
    locate_at(id, path, home, git, recorded.as_deref(), &[])
}

fn locate_at(
    id: HarnessId,
    pinned: &Path,
    home: Option<&Path>,
    git: Option<&Path>,
    recorded: Option<&OsStr>,
    workspace: &[&Path],
) -> Res<(PathBuf, Version)> {
    let tool = harness::harness(id);
    let binary = spawn::resolve_binary(pinned, workspace)?;
    if let Some(home) = home {
        usable_home(id, home, workspace)?;
    }
    // The PATH and the settings folder the callee will get: a harness that
    // cannot start under them fails here, not halfway through a run. From
    // `/`, so that no repository's files shape the answer.
    let binaries: Vec<&Path> = git.into_iter().chain([pinned]).collect();
    let version = spawn::run_helper_with_env(
        &binary,
        &["--version"],
        Some(Path::new("/")),
        Duration::from_secs(15),
        spawn::own_path(&binaries, recorded, workspace),
        &spawn::home_vars(id, home),
    )
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

/// A harness's settings folder, before a run uses it: not inside the
/// workspace or a temp directory (33), and there, this user's and writable
/// by no one else (31).
pub fn usable_home(id: HarnessId, home: &Path, workspace: &[&Path]) -> Res<()> {
    let key = format!("harness.{id}.home");
    let canonical = std::fs::canonicalize(home).map_err(|error| {
        Fail::new(
            Exit::TargetUnavailable,
            format!("{key} {}: {error}", home.display()),
        )
    })?;
    let inside = |root: &Path| {
        canonical.starts_with(root)
            || std::fs::canonicalize(root).is_ok_and(|root| canonical.starts_with(root))
    };
    if let Some(root) = workspace.iter().find(|root| inside(root)) {
        return Err(Fail::policy(format!(
            "refusing to use {key} {}: it is inside the workspace {}",
            home.display(),
            root.display()
        )));
    }
    if let Some(root) = crate::tools::fixed_temp_roots()
        .iter()
        .find(|root| inside(root))
    {
        return Err(Fail::policy(format!(
            "refusing to use {key} {}: it is inside the temp directory {}",
            home.display(),
            root.display()
        )));
    }
    crate::tools::check_home(home).map_err(|why| {
        Fail::new(
            Exit::TargetUnavailable,
            format!("{key} {}: {why}", home.display()),
        )
    })
}

/// Whether ONE candidate can take a run right now: a free slot, a real binary,
/// and the gate. `resume` uses this directly — a session belongs to the
/// harness and model that started it, so there is nobody to fall through to.
pub fn eligible(
    dirs: &Dirs,
    registry: &Registry,
    tools: &Tools,
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
    let (binary, version) = locate(registry, Some(&tools.git), candidate.harness, workspace)?;
    let admission = gate::admit(dirs, registry, candidate, role)?;
    Ok((binary, version, admission))
}

/// The target for `routing`: who asks (`caller`) and who it must be, if
/// anyone (`to`), then the first candidate `eligible` admits.
pub fn choose(
    dirs: &Dirs,
    registry: &Registry,
    tools: &Tools,
    routing: &Routing<'_>,
    (caller, to): (Option<HarnessId>, Option<HarnessId>),
    selection: Selection,
    workspace: &[&Path],
) -> Res<Choice> {
    let list = candidates(registry, routing, caller, to)?;
    let share = if to.is_none() && explore::can_explore(&list) {
        routing.exploration_share
    } else {
        0.0
    };
    let promote = share > 0.0 && selection == Selection::Drawn(true);
    let walked = walk(list, promote, |candidate| {
        eligible(dirs, registry, tools, candidate, routing.role, workspace)
    })?;
    let (binary, version, admission) = walked.admitted;
    Ok(Choice {
        target: walked.target,
        binary,
        version,
        admission,
        skipped: walked.skipped,
        exploration_share: share,
        exploration: promote && walked.at == 1,
    })
}

struct Walked<T> {
    target: Candidate,
    /// Where the target stood in the list before any swap.
    at: usize,
    admitted: T,
    /// What was refused on the way, in the order it was tried.
    skipped: Vec<Skip>,
}

/// Tries `list` in order — or with its first two swapped — until `attempt`
/// admits one. Nothing is bypassed: a promoted candidate that is refused is
/// skipped like any other, and the walk goes on.
fn walk<T>(
    list: Vec<Candidate>,
    promote: bool,
    mut attempt: impl FnMut(&Candidate) -> Res<T>,
) -> Res<Walked<T>> {
    let mut skipped: Vec<(usize, Candidate, Fail)> = Vec::new();
    for at in explore::order(list.len(), promote) {
        let candidate = &list[at];
        match attempt(candidate) {
            Ok(admitted) => {
                return Ok(Walked {
                    target: candidate.clone(),
                    at,
                    admitted,
                    skipped: skipped
                        .into_iter()
                        .map(|(_, candidate, fail)| skip((candidate, fail)))
                        .collect(),
                });
            }
            Err(fail) => skipped.push((at, candidate.clone(), fail)),
        }
    }
    // One candidate: its own refusal is the most useful answer. Several: none
    // was eligible, and the message says why each was not — in the order the
    // list names them, whatever order they were tried in.
    if skipped.len() == 1 {
        return Err(skipped.remove(0).2);
    }
    skipped.sort_by_key(|(at, ..)| *at);
    let reasons: Vec<String> = skipped
        .iter()
        .map(|(_, c, fail)| {
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

    fn three() -> Vec<Candidate> {
        let routing_list = |harness, model: &str, effort| Candidate {
            harness,
            model: crate::model::ModelName::try_from(model.to_string()).unwrap(),
            effort,
        };
        vec![
            routing_list(HarnessId::Codex, "m", crate::model::Effort::High),
            routing_list(HarnessId::Codex, "m", crate::model::Effort::Medium),
            routing_list(HarnessId::Claude, "n", crate::model::Effort::Low),
        ]
    }

    /// `refused` are the entries (by original index) that cannot take a run.
    fn walked(promote: bool, refused: &[usize]) -> Res<Walked<usize>> {
        let list = three();
        walk(list.clone(), promote, |candidate| {
            let at = list.iter().position(|c| c == candidate).unwrap();
            if refused.contains(&at) {
                Err(Fail::new(
                    if at == 1 {
                        Exit::Busy
                    } else {
                        Exit::TargetUnavailable
                    },
                    format!("refused {at}"),
                ))
            } else {
                Ok(at)
            }
        })
    }

    fn skipped_reasons(walked: &Walked<usize>) -> Vec<String> {
        walked.skipped.iter().map(|s| s.reason.clone()).collect()
    }

    #[test]
    fn exploration_is_at_most_one_adjacent_swap() {
        let list = three();
        // Ordinary: the first admissible, in the list's own order.
        let ordinary = walked(false, &[]).unwrap();
        assert_eq!((ordinary.at, ordinary.target.clone()), (0, list[0].clone()));
        // Promoted: the second entry is tried first, and taken when admitted.
        let promoted = walked(true, &[]).unwrap();
        assert_eq!((promoted.at, promoted.target.clone()), (1, list[1].clone()));
        assert!(promoted.skipped.is_empty());
        // Promoted but refused: it is not bypassed. The first is then tried,
        // and no refusal is invented for it.
        let fallback = walked(true, &[1]).unwrap();
        assert_eq!(fallback.at, 0);
        assert_eq!(skipped_reasons(&fallback), ["refused 1"]);
        assert_eq!(fallback.skipped[0].code, Exit::Busy.code());
        assert_eq!(fallback.skipped[0].candidate, list[1]);
        // Both leading entries refused: the tail is untouched and is reached.
        let tail = walked(true, &[0, 1]).unwrap();
        assert_eq!(tail.at, 2);
        assert_eq!(
            skipped_reasons(&tail),
            ["refused 1", "refused 0"],
            "attempt order"
        );
        // Ordinary routing is unchanged by the same refusals.
        let ordinary_tail = walked(false, &[0, 1]).unwrap();
        assert_eq!(skipped_reasons(&ordinary_tail), ["refused 0", "refused 1"]);
    }

    #[test]
    fn an_exhausted_list_says_why_in_the_lists_own_order() {
        for promote in [false, true] {
            let fail = walked(promote, &[0, 1, 2]).err().unwrap();
            assert_eq!(fail.exit, Exit::NoEligibleTarget);
            assert_eq!(
                fail.message,
                "codex (m, high): refused 0; codex (m, medium): refused 1; claude (n, low): refused 2",
                "promote = {promote}"
            );
        }
        // One candidate: its own refusal, with its own code.
        let one = walk(three()[..1].to_vec(), false, |_| {
            Err::<(), _>(Fail::new(Exit::Busy, "held"))
        });
        assert_eq!(one.err().unwrap().exit, Exit::Busy);
        // One candidate cannot be swapped with anything.
        let alone = walk(three()[..1].to_vec(), true, |_| Ok(())).unwrap();
        assert_eq!(alone.at, 0);
    }
}
