//! Where a run works. A reader works where the caller is. A WRITER never
//! does, unless a person's config says it may: it gets a worktree of its own,
//! cut from the caller's HEAD, so that what it writes is a proposal the
//! caller reviews — not an edit that lands in the middle of the caller's own
//! work.
//!
//! The client decides and checks (`decide`); the detached supervisor does the
//! cutting (`cut`), because in a daft repository a new worktree runs its
//! setup hooks and that can take longer than a caller's tool call may.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::dirs::{Dirs, ensure_private_dir};
use crate::exit::{Exit, Fail, Res};
use crate::model::Role;
use crate::paths::{self, Workspace};
use crate::registry::Registry;
use crate::spawn;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// The caller's directory (or `--dir`). Readers only.
    #[default]
    Caller,
    /// A writer in the caller's own tree — only if the config allows it.
    InPlace,
    /// A writer in a fresh worktree cut from the caller's HEAD.
    Fork,
}

/// Decides where a run will work and refuses what may not happen. For a fork
/// the returned directory is the BASE it will be cut from.
pub fn decide(
    role: Role,
    fork: bool,
    in_place: bool,
    dir: Option<&Path>,
    workspace: &Workspace,
    registry: &Registry,
) -> Res<(Placement, PathBuf)> {
    let base = paths::run_dir(dir, workspace)?;
    if role.is_read_only() {
        if fork || in_place {
            return Err(Fail::new(
                Exit::Usage,
                format!("--fork and --in-place are for a role that writes; {role} only reads"),
            ));
        }
        return Ok((Placement::Caller, base));
    }
    match (fork, in_place) {
        (true, false) => {
            if spawn::git_roots(&base).is_none() {
                return Err(Fail::policy(format!(
                    "--fork needs a git repository to cut a worktree from, and {} is not in one",
                    base.display()
                )));
            }
            Ok((Placement::Fork, base))
        }
        (false, true) if registry.limits.allow_in_place => Ok((Placement::InPlace, base)),
        (false, true) => Err(Fail::policy(
            "--in-place lets another agent edit the tree you are working in, so it is off unless \
             a person sets `limits.allow_in_place = true` in cahoots' config — use --fork",
        )),
        _ => Err(Fail::policy(format!(
            "the {role} role writes, so it needs a place of its own: pass --fork (a fresh \
             worktree cut from your HEAD, which you then review and bring over)"
        ))),
    }
}

/// Cuts the worktree a writer will work in, and returns its path.
///
/// In a repository that opted into daft (a `daft.yml` at its top) the fork is
/// daft's — `daft start --fork` — so it comes up with the repo's own setup
/// hooks run. Anywhere else it is a plain detached `git worktree` under
/// cahoots' state directory, outside every workspace.
pub fn cut(dirs: &Dirs, base: &Path, run_id: &str) -> Res<PathBuf> {
    let failed = |what: &str| Fail::new(Exit::RunFailed, format!("cannot cut a worktree: {what}"));
    let (toplevel, _) =
        spawn::git_roots(base).ok_or_else(|| failed("the base is no longer a repository"))?;

    if toplevel.join("daft.yml").is_file()
        && let Ok(daft) = spawn::system_tool("daft", &[])
    {
        let base_arg = base.to_string_lossy();
        let output = spawn::run_helper(
            &daft,
            &["-C", base_arg.as_ref(), "start", "--fork", "--no-cd"],
            None,
            Duration::from_secs(900),
        )?;
        let path = output
            .stdout
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty());
        return match (output.status, path) {
            (Some(0), Some(path)) if Path::new(path.trim()).is_dir() => {
                Ok(PathBuf::from(path.trim()))
            }
            _ => Err(failed("`daft start --fork` did not produce one")),
        };
    }

    let root = dirs.state.join("worktrees");
    ensure_private_dir(&root)?;
    let path = root.join(run_id);
    let git = spawn::system_tool("git", &[])?;
    let (base_arg, path_arg) = (base.to_string_lossy(), path.to_string_lossy());
    let output = spawn::run_helper(
        &git,
        &[
            "-C",
            base_arg.as_ref(),
            "worktree",
            "add",
            "--detach",
            path_arg.as_ref(),
            "HEAD",
        ],
        None,
        Duration::from_secs(300),
    )?;
    if output.status == Some(0) && path.is_dir() {
        Ok(path)
    } else {
        Err(failed(
            "`git worktree add` refused (a repository with no commit has no HEAD to cut from)",
        ))
    }
}

/// `git status --short` in a writer's worktree: what it changed, for the
/// caller to review. Capped; this is a summary, not the diff.
pub fn changes(worktree: &Path) -> Vec<String> {
    let Ok(git) = spawn::system_tool("git", &[]) else {
        return Vec::new();
    };
    spawn::run_helper(
        &git,
        &["status", "--short", "--untracked-files=all"],
        Some(worktree),
        Duration::from_secs(30),
    )
    .map(|output| {
        output
            .stdout
            .lines()
            .take(200)
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// Removes a worktree cahoots cut itself (never one of daft's, never the
/// caller's tree) once its run has aged out.
pub fn discard(dirs: &Dirs, base: &Path, worktree: &Path) {
    if !worktree.starts_with(dirs.state.join("worktrees")) {
        return;
    }
    if let Ok(git) = spawn::system_tool("git", &[]) {
        let (base_arg, path_arg) = (base.to_string_lossy(), worktree.to_string_lossy());
        let _ = spawn::run_helper(
            &git,
            &[
                "-C",
                base_arg.as_ref(),
                "worktree",
                "remove",
                "--force",
                path_arg.as_ref(),
            ],
            None,
            Duration::from_secs(60),
        );
    }
    let _ = fs::remove_dir_all(worktree);
}
