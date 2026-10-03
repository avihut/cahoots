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
    /// What a person keeps, which nothing ages out: the eval suite.
    pub data: PathBuf,
    /// True when a dev-build override placed any directory.
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
        let overrides = [
            DirKind::Config,
            DirKind::State,
            DirKind::Data,
            DirKind::Home,
        ]
        .map(env::dev_dir_override);
        let overridden = overrides.iter().any(Option::is_some);
        let all_overridden = overrides.iter().all(Option::is_some);

        // Real state is for real builds. A unit test never gets the real
        // directories, and a dev build only gets them when its developer asks
        // — otherwise ALL of config, state, data and home must point somewhere
        // throwaway. (2026-09-20: a unit test reached `install` through
        // `dispatch` and wrote into a real ~/.claude and ~/.codex.)
        let real_dirs_refused = if cfg!(test) {
            Some("a unit test reached for the real directories — build a `Dirs` by hand instead")
        } else if env::dev_overrides_honoured() && !all_overridden && !env::dev_real_dirs_allowed()
        {
            Some(
                "this is a DEV build, which only works on throwaway directories: set \
                 CAHOOTS_CONFIG_DIR, CAHOOTS_STATE_DIR, CAHOOTS_DATA_DIR and CAHOOTS_HOME_DIR — or \
                 CAHOOTS_DEV_REAL_DIRS=1 to use your real ones on purpose",
            )
        } else {
            None
        };
        if let Some(why) = real_dirs_refused {
            return Err(Fail::config(why));
        }

        let [config, state, data, home_override] = overrides;
        let home = match home_override {
            Some(home) => home,
            None => passwd_home()?,
        };
        Ok(Dirs {
            config: config.unwrap_or_else(|| home.join(".config/cahoots")),
            state: state.unwrap_or_else(|| home.join(".local/state/cahoots")),
            data: data.unwrap_or_else(|| home.join(".local/share/cahoots")),
            home,
            overridden,
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    /// Where `install` found each usage meter: cahoots' record of this
    /// machine, beside the person's own config.
    pub fn meter_file(&self) -> PathBuf {
        self.config.join("meter.json")
    }

    pub fn runs(&self) -> PathBuf {
        self.state.join("runs")
    }

    pub fn slots(&self) -> PathBuf {
        self.state.join("slots")
    }

    /// The eval suite's tasks, one directory each.
    pub fn evals_tasks(&self) -> PathBuf {
        self.data.join("evals/tasks")
    }

    /// The empty directory every `git` cahoots starts is given as its hooks
    /// directory, so that none of a repository's hooks runs.
    pub fn no_hooks(&self) -> PathBuf {
        self.state.join("no-hooks")
    }

    /// A workspace must never contain cahoots' own directories: a repository
    /// could then ship a config, or a run record, to the tool about to run on
    /// it. Skipped for a dev build's overrides, which are temp dirs by design.
    pub fn refuse_inside(&self, roots: &[&Path]) -> Res<()> {
        if self.overridden {
            return Ok(());
        }
        for dir in [&self.config, &self.state, &self.data] {
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
    fn a_unit_test_cannot_reach_the_real_directories() {
        assert_eq!(Dirs::resolve().unwrap_err().exit, crate::exit::Exit::Config);
    }

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
            data: PathBuf::from("/home/u/.local/share/cahoots"),
            overridden: false,
        };
        assert!(dirs.refuse_inside(&[Path::new("/home/u")]).is_err());
        assert!(dirs.refuse_inside(&[Path::new("/home/u/project")]).is_ok());
    }

    #[test]
    fn a_workspace_that_contains_the_data_dir_is_refused() {
        let dirs = Dirs {
            home: PathBuf::from("/home/u"),
            config: PathBuf::from("/elsewhere/config"),
            state: PathBuf::from("/elsewhere/state"),
            data: PathBuf::from("/home/u/project/data"),
            overridden: false,
        };
        assert!(dirs.refuse_inside(&[Path::new("/home/u/project")]).is_err());
        assert!(dirs.refuse_inside(&[Path::new("/home/u/other")]).is_ok());
    }
}
