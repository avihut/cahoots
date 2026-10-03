//! git: `git worktree add --no-checkout --detach`, at a path under cahoots'
//! state directory, outside every workspace; cahoots checks it out once it
//! has read the configuration as git reads it there (`placement::cut`).
//! cahoots owns the worktree, and removes it once no run on record works in
//! it.

use std::ffi::OsString;
use std::time::Duration;

use super::provider::{Checkout, CutSpec, Owner, Place, Provider, ProviderId};
use crate::harness::Version;
use crate::patch::Commit;

pub struct Git;

impl Provider for Git {
    fn id(&self) -> ProviderId {
        ProviderId::Git
    }

    fn binary_name(&self) -> &'static str {
        "git"
    }

    /// `git version 2.50.1 (Apple Git-155)`.
    fn fingerprint(&self, version_output: &str) -> Option<Version> {
        let line = version_output.lines().next()?;
        line.strip_prefix("git version ").and_then(Version::find_in)
    }

    /// 2.31 is where `rev-parse --path-format=absolute` came, which finding
    /// a repository's top needs (`spawn::git_roots`).
    fn tested(&self) -> (Version, Version) {
        (Version(2, 31, 0), Version(3, 0, 0))
    }

    fn deadline(&self) -> Duration {
        Duration::from_secs(300)
    }

    fn what(&self) -> &'static str {
        "`git worktree add`"
    }

    fn place(&self) -> Place {
        Place::Chosen
    }

    fn checkout(&self) -> Checkout {
        Checkout::Cahoots
    }

    fn owner(&self) -> Owner {
        Owner::Cahoots
    }

    fn failure(&self, _status: Option<i32>) -> String {
        "`git worktree add` refused (a repository with no commit has no HEAD to cut from)"
            .to_string()
    }

    /// `<quiet> -C <base> worktree add --no-checkout --detach <path> <commit |
    /// HEAD>`.
    fn build_argv(&self, spec: &CutSpec) -> Vec<OsString> {
        let mut argv = spec.quiet.to_vec();
        argv.extend([
            OsString::from("-C"),
            spec.base.into(),
            "worktree".into(),
            "add".into(),
            "--no-checkout".into(),
            "--detach".into(),
            spec.path.map(OsString::from).unwrap_or_default(),
            spec.at.map_or("HEAD", Commit::as_str).into(),
        ]);
        argv
    }

    fn check_argv(&self, spec: &CutSpec, argv: &[OsString]) -> Result<(), String> {
        // Told first, above every config file, that its hooks are in an
        // empty directory of cahoots' own and that there is no fsmonitor.
        let told = |at: usize, setting: &str| {
            argv.get(at).is_some_and(|arg| arg == "-c")
                && argv
                    .get(at + 1)
                    .and_then(|arg| arg.to_str())
                    .is_some_and(|arg| arg.starts_with(setting))
        };
        if !told(0, "core.hooksPath=")
            || !told(2, "core.fsmonitor=false")
            || argv.get(..4) != Some(spec.quiet)
        {
            return Err(
                "git is not told first that its hooks are cahoots' empty directory and that \
                 there is no fsmonitor"
                    .to_string(),
            );
        }
        let Some(path) = spec.path else {
            return Err("git cuts at a path cahoots chose, and none was chosen".to_string());
        };
        let rev = spec.at.map_or("HEAD", Commit::as_str);
        let expected: [OsString; 8] = [
            "-C".into(),
            spec.base.into(),
            "worktree".into(),
            "add".into(),
            "--no-checkout".into(),
            "--detach".into(),
            path.into(),
            rev.into(),
        ];
        if argv[4..] != expected {
            return Err(format!(
                "the command is not `git -C {} worktree add --no-checkout --detach {} {rev}`",
                spec.base.display(),
                path.display()
            ));
        }
        Ok(())
    }
}
