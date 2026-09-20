//! Where cahoots keeps things. Fixed XDG-style paths under a home directory
//! that comes from the passwd database — NOT `$HOME`, which the calling agent
//! controls. A dev build may be pointed elsewhere (`env::dev_dir_override`);
//! nothing anyone installs can.

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nix::unistd::{Uid, User};

use crate::env::{self, DirKind};
use crate::exit::{Fail, Res};

#[derive(Debug, Clone)]
pub struct Dirs {
    pub home: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    /// True when a dev-build override placed either directory.
    pub overridden: bool,
}

/// The current user's home, from the passwd database.
pub fn passwd_home() -> Res<PathBuf> {
    match User::from_uid(Uid::current()) {
        Ok(Some(user)) if user.dir.is_absolute() => Ok(user.dir),
        Ok(_) => Err(Fail::config(
            "the passwd database has no usable home directory for this user",
        )),
        Err(error) => Err(Fail::internal(format!("passwd lookup failed: {error}"))),
    }
}

impl Dirs {
    pub fn resolve() -> Res<Dirs> {
        let home = passwd_home()?;
        let config_override = env::dev_dir_override(DirKind::Config);
        let state_override = env::dev_dir_override(DirKind::State);
        let overridden = config_override.is_some() || state_override.is_some();
        Ok(Dirs {
            config: config_override.unwrap_or_else(|| home.join(".config/cahoots")),
            state: state_override.unwrap_or_else(|| home.join(".local/state/cahoots")),
            home,
            overridden,
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    /// Which targets a human has enabled. Its own file, written only by the
    /// `enable` verb, so the hand-written config is never rewritten.
    pub fn enabled_file(&self) -> PathBuf {
        self.config.join("enabled.json")
    }

    pub fn runs(&self) -> PathBuf {
        self.state.join("runs")
    }

    pub fn slots(&self) -> PathBuf {
        self.state.join("slots")
    }

    /// A workspace must never contain cahoots' own directories: a repository
    /// could then ship a config, or a run record, to the tool about to run on
    /// it. Skipped for a dev build's overrides, which are temp dirs by design.
    pub fn refuse_inside(&self, roots: &[&Path]) -> Res<()> {
        if self.overridden {
            return Ok(());
        }
        for dir in [&self.config, &self.state] {
            let resolved = fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
            for root in roots {
                if resolved.starts_with(root) {
                    return Err(Fail::policy(format!(
                        "{} is inside the workspace {} — refusing to use it",
                        resolved.display(),
                        root.display()
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Creates a directory (and parents) private to the user, and tightens it if
/// it already existed looser.
pub fn ensure_private_dir(path: &Path) -> Res<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|error| Fail::internal(format!("cannot create {}: {error}", path.display())))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| Fail::internal(format!("cannot secure {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_comes_from_passwd_and_is_absolute() {
        assert!(passwd_home().unwrap().is_absolute());
    }

    #[test]
    fn a_workspace_that_contains_the_state_dir_is_refused() {
        let dirs = Dirs {
            home: PathBuf::from("/home/u"),
            config: PathBuf::from("/home/u/.config/cahoots"),
            state: PathBuf::from("/home/u/.local/state/cahoots"),
            overridden: false,
        };
        assert!(dirs.refuse_inside(&[Path::new("/home/u")]).is_err());
        assert!(dirs.refuse_inside(&[Path::new("/home/u/project")]).is_ok());
    }
}
