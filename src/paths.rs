//! Where a run may happen and what it may be handed. Once `cahoots run` is
//! allowed outside a harness's sandbox, its path arguments are that harness's
//! way to read files — so `--brief ~/.ssh/id_ed25519` must not be a way to
//! ship a key to another vendor, and `--dir /` must not be a way to point an
//! agent at the whole machine.

use std::fs;
use std::path::{Path, PathBuf};

use crate::env;
use crate::exit::{Exit, Fail, Res};
use crate::spawn;

pub const BRIEF_MAX_BYTES: u64 = 256 * 1024;

/// The caller's working directory, and the repository around it if any.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub cwd: PathBuf,
    pub toplevel: Option<PathBuf>,
    pub common_dir: Option<PathBuf>,
}

impl Workspace {
    pub fn around(cwd: &Path) -> Res<Workspace> {
        let cwd = fs::canonicalize(cwd).map_err(|error| {
            Fail::internal(format!("cannot resolve the working directory: {error}"))
        })?;
        let roots = spawn::git_roots(&cwd);
        Ok(Workspace {
            toplevel: roots.as_ref().map(|(top, _)| canonical(top)),
            common_dir: roots.as_ref().map(|(_, common)| canonical(common)),
            cwd,
        })
    }

    /// The directories a workspace-supplied file could live in.
    pub fn roots(&self) -> Vec<&Path> {
        let mut roots = vec![self.cwd.as_path()];
        roots.extend(self.toplevel.as_deref());
        roots
    }
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn temp_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        canonical(Path::new("/tmp")),
        canonical(Path::new("/var/tmp")),
    ];
    roots.extend(env::tmpdir().map(|dir| canonical(&dir)));
    roots
}

/// Reads the brief: a regular UTF-8 file, size-capped, that lives in the
/// workspace or in a temp directory — the places an agent legitimately
/// writes one. Judged by where the path RESOLVES, so a symlink out is caught.
pub fn read_brief(path: &Path, workspace: &Workspace) -> Res<String> {
    let refuse = |why: String| Fail::policy(format!("--brief {}: {why}", path.display()));
    let resolved = fs::canonicalize(path)
        .map_err(|error| Fail::new(Exit::Usage, format!("--brief {}: {error}", path.display())))?;
    let meta = fs::metadata(&resolved).map_err(|error| refuse(error.to_string()))?;
    if !meta.is_file() {
        return Err(refuse("not a regular file".to_string()));
    }
    if meta.len() > BRIEF_MAX_BYTES {
        return Err(refuse(format!(
            "larger than {} KiB",
            BRIEF_MAX_BYTES / 1024
        )));
    }
    let allowed = workspace
        .roots()
        .into_iter()
        .map(Path::to_path_buf)
        .chain(temp_roots())
        .any(|root| resolved.starts_with(root));
    if !allowed {
        return Err(refuse(
            "a brief must be inside the working directory, its repository, or a temp directory"
                .to_string(),
        ));
    }
    let text = fs::read_to_string(&resolved).map_err(|_| refuse("not UTF-8 text".to_string()))?;
    if text.trim().is_empty() {
        return Err(Fail::new(Exit::Usage, "the brief is empty"));
    }
    Ok(text)
}

/// Where the callee works. Default: the caller's working directory. `--dir`
/// may only name another worktree of the SAME repository.
pub fn run_dir(requested: Option<&Path>, workspace: &Workspace) -> Res<PathBuf> {
    let Some(requested) = requested else {
        return Ok(workspace.cwd.clone());
    };
    let resolved = fs::canonicalize(requested).map_err(|error| {
        Fail::new(
            Exit::Usage,
            format!("--dir {}: {error}", requested.display()),
        )
    })?;
    if resolved.starts_with(&workspace.cwd) {
        return Ok(resolved);
    }
    let theirs = spawn::git_roots(&resolved).map(|(_, common)| canonical(&common));
    match (&workspace.common_dir, theirs) {
        (Some(ours), Some(theirs)) if *ours == theirs => Ok(resolved),
        _ => Err(Fail::policy(format!(
            "--dir {}: not the working directory, nor a worktree of its repository",
            resolved.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(dir: &Path) -> Workspace {
        Workspace {
            cwd: canonical(dir),
            toplevel: None,
            common_dir: None,
        }
    }

    #[test]
    fn a_brief_outside_the_workspace_is_refused() {
        let inside = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let elsewhere = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let ws = workspace(inside.path());
        fs::write(inside.path().join("brief.md"), "review this").unwrap();
        fs::write(elsewhere.path().join("secret"), "a private key").unwrap();

        assert_eq!(
            read_brief(&inside.path().join("brief.md"), &ws).unwrap(),
            "review this"
        );
        let fail = read_brief(&elsewhere.path().join("secret"), &ws).unwrap_err();
        assert_eq!(fail.exit, Exit::Policy);

        // A symlink inside the workspace that points out of it is still out.
        let link = inside.path().join("innocent.md");
        std::os::unix::fs::symlink(elsewhere.path().join("secret"), &link).unwrap();
        assert_eq!(read_brief(&link, &ws).unwrap_err().exit, Exit::Policy);
    }

    #[test]
    fn a_brief_must_be_a_small_text_file() {
        let inside = tempfile::tempdir().unwrap();
        let ws = workspace(inside.path());
        assert_eq!(
            read_brief(inside.path(), &ws).unwrap_err().exit,
            Exit::Policy
        );
        fs::write(inside.path().join("binary"), [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(
            read_brief(&inside.path().join("binary"), &ws)
                .unwrap_err()
                .exit,
            Exit::Policy
        );
        fs::write(
            inside.path().join("big"),
            "x".repeat(BRIEF_MAX_BYTES as usize + 1),
        )
        .unwrap();
        assert_eq!(
            read_brief(&inside.path().join("big"), &ws)
                .unwrap_err()
                .exit,
            Exit::Policy
        );
        fs::write(inside.path().join("empty"), "  \n").unwrap();
        assert_eq!(
            read_brief(&inside.path().join("empty"), &ws)
                .unwrap_err()
                .exit,
            Exit::Usage
        );
    }

    #[test]
    fn dir_may_not_leave_the_repository() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let ws = workspace(here.path());
        fs::create_dir(here.path().join("sub")).unwrap();
        assert_eq!(run_dir(None, &ws).unwrap(), ws.cwd);
        assert!(run_dir(Some(&here.path().join("sub")), &ws).is_ok());
        assert_eq!(
            run_dir(Some(there.path()), &ws).unwrap_err().exit,
            Exit::Policy
        );
        assert_eq!(
            run_dir(Some(Path::new("/")), &ws).unwrap_err().exit,
            Exit::Policy
        );
    }
}
