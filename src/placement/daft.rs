//! daft: `daft start --fork`, where daft's layout puts the worktree, in a
//! repository with a `daft.yml`. The worktree is daft's, and stays until a
//! person removes it (`daft remove`).
//!
//! With no hook of the repository's unless a person turned them on
//! (`fork.daft.hooks`): `--skip-hooks all` for daft's own, and for the `git`
//! it runs, the same empty hooks directory cahoots' own git gets. With them
//! on, `--hooks foreground`, so every job is done when daft exits and none
//! runs on beside the writer. Always `--no-carry`, so that a person's carry
//! setting never brings their uncommitted work into the writer's tree (and
//! from there into its patch), and the commit to fork from, when there is
//! one.

use std::ffi::OsString;
use std::time::Duration;

use super::provider::{Checkout, CutSpec, Owner, Place, Provider, ProviderId};
use crate::harness::Version;

pub struct Daft;

/// Flags `daft start` takes that cahoots never gives it: each runs a
/// command, carries the caller's work along, reaches another repository or
/// place, or makes more than one worktree. (`--hooks` is held to its one
/// mode apart.)
const NEVER: [&str; 11] = [
    "-x",
    "--exec",
    "-c",
    "--carry",
    "--with-related",
    "--repo",
    "-@",
    "--at",
    "-n",
    "--count",
    "--local",
];

impl Provider for Daft {
    fn id(&self) -> ProviderId {
        ProviderId::Daft
    }

    fn binary_name(&self) -> &'static str {
        "daft"
    }

    /// `daft 1.27.9`.
    fn fingerprint(&self, version_output: &str) -> Option<Version> {
        let line = version_output.lines().next()?;
        line.strip_prefix("daft ").and_then(Version::find_in)
    }

    /// `--fork` came in 1.24.0 and `--hooks <MODE>` in 1.27.0 (daft's
    /// CHANGELOG): one tested range for both settings.
    fn tested(&self) -> (Version, Version) {
        (Version(1, 27, 0), Version(1, 28, 0))
    }

    fn deadline(&self) -> Duration {
        Duration::from_secs(900)
    }

    fn what(&self) -> &'static str {
        "`daft start --fork`"
    }

    fn place(&self) -> Place {
        Place::Printed
    }

    /// `daft start --fork` checks out as it cuts: it has no way to cut with
    /// no checkout (daft 1.27.9).
    fn checkout(&self) -> Checkout {
        Checkout::Tool
    }

    fn owner(&self) -> Owner {
        Owner::Daft
    }

    fn failure(&self, status: Option<i32>) -> String {
        match status {
            Some(code) => format!("`daft start --fork` exited with status {code}"),
            None => "`daft start --fork` was killed by a signal".to_string(),
        }
    }

    /// `-C <base> start --fork --no-cd <hooks> --no-carry [<commit>]`, where
    /// `<hooks>` is `--skip-hooks all`, or `--hooks foreground` with
    /// `fork.daft.hooks` on.
    fn build_argv(&self, spec: &CutSpec) -> Vec<OsString> {
        let mut argv = vec![
            OsString::from("-C"),
            spec.base.into(),
            "start".into(),
            "--fork".into(),
            "--no-cd".into(),
        ];
        argv.extend(
            if spec.hooks {
                ["--hooks", "foreground"]
            } else {
                ["--skip-hooks", "all"]
            }
            .map(OsString::from),
        );
        argv.push("--no-carry".into());
        argv.extend(spec.at.map(|commit| OsString::from(commit.as_str())));
        argv
    }

    fn check_argv(&self, spec: &CutSpec, argv: &[OsString]) -> Result<(), String> {
        let has = |flag: &str| argv.iter().any(|arg| arg == flag);
        let after = |flag: &str| {
            argv.iter()
                .position(|arg| arg == flag)
                .and_then(|at| argv.get(at + 1))
        };
        for arg in argv {
            let flag = arg
                .to_str()
                .map_or("", |arg| arg.split('=').next().unwrap_or(arg));
            if NEVER.contains(&flag) {
                return Err(format!(
                    "{flag} runs a command, carries work along, or reaches past one fork"
                ));
            }
        }
        for required in ["start", "--fork", "--no-cd", "--no-carry"] {
            if !has(required) {
                return Err(format!("`daft start --fork` without {required}"));
            }
        }
        let hooks_flags = argv
            .iter()
            .filter(|arg| matches!(arg.to_str(), Some(flag) if flag.starts_with("--hooks") || flag.starts_with("--skip-hooks")))
            .count();
        let (mode, value) = if spec.hooks {
            ("--hooks", "foreground")
        } else {
            ("--skip-hooks", "all")
        };
        if hooks_flags != 1 || after(mode).is_none_or(|arg| arg != value) {
            return Err(format!(
                "daft's hooks must be given exactly as `{mode} {value}`, as fork.daft.hooks is {}",
                if spec.hooks { "on" } else { "off" }
            ));
        }
        let mut expected: Vec<OsString> = vec![
            "-C".into(),
            spec.base.into(),
            "start".into(),
            "--fork".into(),
            "--no-cd".into(),
            mode.into(),
            value.into(),
            "--no-carry".into(),
        ];
        expected.extend(spec.at.map(|commit| OsString::from(commit.as_str())));
        if argv != expected {
            return Err(format!(
                "the command is not `daft -C {} start --fork --no-cd {mode} {value} --no-carry{}`",
                spec.base.display(),
                spec.at
                    .map_or(String::new(), |commit| format!(" {}", commit.as_str()))
            ));
        }
        Ok(())
    }
}
