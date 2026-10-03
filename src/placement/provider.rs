//! What cuts a writer's worktree: a closed set of providers, chosen by a
//! person in config.toml (`fork.provider`), never by a flag and never by the
//! repository. Each is a variant here, its command line built in code from
//! typed values (hard rule 3) and checked before it runs, as a harness's is.
//!
//! - **git** — `git worktree add --detach`, at a path cahoots chose under its
//!   state directory. cahoots owns it, and removes it once no run on record
//!   works in it.
//! - **daft** — `daft start --fork`, where daft's layout puts it, and only
//!   where the repository has a `daft.yml`. It is daft's, and stays until a
//!   person removes it.
//!
//! Adding one is a module and a variant.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::daft::Daft;
use super::git::Git;
use crate::exit::{Exit, Fail, Res};
use crate::harness::Version;
use crate::patch::Commit;
use crate::registry;
use crate::spawn;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderId {
    Git,
    Daft,
}

impl ProviderId {
    pub const ALL: [ProviderId; 2] = [ProviderId::Git, ProviderId::Daft];

    pub const fn as_str(self) -> &'static str {
        match self {
            ProviderId::Git => "git",
            ProviderId::Daft => "daft",
        }
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl FromStr for ProviderId {
    type Err = Fail;
    fn from_str(s: &str) -> Result<Self, Fail> {
        ProviderId::ALL
            .into_iter()
            .find(|id| id.as_str() == s)
            .ok_or_else(|| {
                Fail::new(
                    Exit::Usage,
                    format!("unknown worktree provider: {s:?} (git or daft)"),
                )
            })
    }
}

/// Where the worktree goes: a path cahoots chose, or the one the tool prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Chosen,
    Printed,
}

/// Who checks a new worktree out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkout {
    /// The tool, as it cuts it. cahoots reads the configuration before it,
    /// in the tree the worktree is cut from.
    Tool,
    /// cahoots, once the tool cut the worktree with no checkout: it reads the
    /// configuration as git reads it in the new worktree first.
    Cahoots,
}

/// Who removes the worktree once it is done with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// cahoots, once no run on record works in it.
    Cahoots,
    /// daft, which is to say a person (`daft remove`).
    Daft,
}

impl Owner {
    pub const fn as_str(self) -> &'static str {
        match self {
            Owner::Cahoots => "cahoots",
            Owner::Daft => "daft",
        }
    }
}

/// Everything a provider needs to build one cut's command line, as typed
/// values.
#[derive(Debug, Clone)]
pub struct CutSpec<'a> {
    /// The tree the worktree is cut from.
    pub base: &'a Path,
    /// The commit to cut at; with none, the base's HEAD.
    pub at: Option<&'a Commit>,
    /// Where it goes, for a provider whose place cahoots chooses.
    pub path: Option<&'a Path>,
    /// Whether the provider runs the repository's hooks (`fork.daft.hooks`).
    pub hooks: bool,
    /// The quiet settings as `-c` arguments (`placement::quiet_git_args`).
    pub quiet: &'a [OsString],
}

pub trait Provider: Sync {
    fn id(&self) -> ProviderId;
    /// The executable's name.
    fn binary_name(&self) -> &'static str;
    /// Recognises this tool in its `--version` output: a binary with the
    /// right name and the wrong fingerprint is something else.
    fn fingerprint(&self, version_output: &str) -> Option<Version>;
    /// The oldest version the command line was written against, and the
    /// first it has NOT been checked against.
    fn tested(&self) -> (Version, Version);
    /// How long a cut may take, before a run's own `--timeout` shortens it.
    fn deadline(&self) -> Duration;
    /// The command, as a message names it: "`git worktree add`".
    fn what(&self) -> &'static str;
    fn place(&self) -> Place;
    fn checkout(&self) -> Checkout;
    fn owner(&self) -> Owner;
    /// Why a cut whose tool exited with `status` (not 0), or left nothing
    /// where cahoots chose, did not happen, in this tool's words.
    fn failure(&self, status: Option<i32>) -> String;
    fn build_argv(&self, spec: &CutSpec) -> Vec<OsString>;
    /// This provider's check of a command line: `Err` names what is wrong.
    fn check_argv(&self, spec: &CutSpec, argv: &[OsString]) -> Result<(), String>;
}

pub fn provider(id: ProviderId) -> &'static dyn Provider {
    match id {
        ProviderId::Git => &Git,
        ProviderId::Daft => &Daft,
    }
}

/// The provider that cuts here: daft only where a person chose it and the
/// repository has a `daft.yml`, git everywhere else. A `daft.yml` never
/// turns daft on by itself.
pub fn provider_for(chosen: ProviderId, has_daft_yml: bool) -> ProviderId {
    match (chosen, has_daft_yml) {
        (ProviderId::Daft, true) => ProviderId::Daft,
        _ => ProviderId::Git,
    }
}

/// The one way to obtain a cut's command line: build, then check. A command
/// line that fails its check is a bug in cahoots, and never runs.
pub fn command_line(provider: &dyn Provider, spec: &CutSpec) -> Res<Vec<OsString>> {
    let argv = provider.build_argv(spec);
    provider.check_argv(spec, &argv).map_err(|why| {
        Fail::new(
            Exit::Internal,
            format!(
                "refusing to cut with {}: {why} (this is a bug in cahoots, not in your setup)",
                provider.id()
            ),
        )
    })?;
    Ok(argv)
}

/// The provider's binary, held to the binary policy and to its fingerprint.
/// git is found on PATH; daft runs only from the path a person chose
/// (`fork.daft.binary`), never from a PATH lookup. Missing, unfingerprinted
/// or too old is the person's setup to fix (34); a binary the policy refuses
/// is a refusal (33).
pub fn locate(id: ProviderId, fork: &registry::Fork, roots: &[&Path]) -> Res<(PathBuf, Version)> {
    match id {
        ProviderId::Git => fingerprinted(&Git, spawn::system_tool("git", roots)?, roots),
        ProviderId::Daft => {
            let pinned = fork.daft_binary.as_deref().ok_or_else(|| {
                Fail::config(
                    "cahoots needs `daft`: none is chosen — fork.provider is \"daft\" and this \
                     repository has a daft.yml; choose one with fork.daft.binary (`cahoots \
                     settings`)",
                )
            })?;
            locate_pinned_daft(pinned, roots)
        }
    }
}

/// The daft at `path`, held to the binary policy and to its fingerprint: what
/// a cut runs, and what a person is held to when they choose one.
pub fn locate_pinned_daft(path: &Path, roots: &[&Path]) -> Res<(PathBuf, Version)> {
    fingerprinted(
        &Daft,
        spawn::pinned_system_tool("daft", path, roots)?,
        roots,
    )
}

/// Asks `binary` its version, from `/` and on a PATH without the workspace's
/// directories, and holds it to the provider's fingerprint and tested floor.
fn fingerprinted(
    provider: &dyn Provider,
    binary: PathBuf,
    roots: &[&Path],
) -> Res<(PathBuf, Version)> {
    let name = provider.binary_name();
    let version = spawn::run_helper_with_env(
        &binary,
        &["--version"],
        Some(Path::new("/")),
        Duration::from_secs(10),
        spawn::helper_path(roots),
        &[],
    )
    .ok()
    .and_then(|output| provider.fingerprint(&output.stdout))
    .ok_or_else(|| {
        Fail::config(format!(
            "cahoots needs `{name}`: {} does not identify itself as {name}",
            binary.display()
        ))
    })?;
    let (oldest, _) = provider.tested();
    if version < oldest {
        return Err(Fail::config(format!(
            "cahoots needs `{name}`: {name} {version} is older than the oldest version cahoots \
             supports ({oldest})"
        )));
    }
    Ok((binary, version))
}

/// The entries of `git config --list --show-origin --show-scope -z`: each
/// its scope and its key, as git lists them (the section and the name
/// lowercased, a subsection as written).
fn entries(listing: &[u8]) -> Vec<(&[u8], &[u8])> {
    let fields: Vec<&[u8]> = listing.split(|byte| *byte == 0).collect();
    fields
        .chunks_exact(3)
        .map(|entry| {
            let key = entry[2].split(|byte| *byte == b'\n').next().unwrap_or(&[]);
            (entry[0], key)
        })
        .collect()
}

/// Every filter driver the configuration defines, in any scope: the `<name>`
/// of each key `filter.<name>.<var>`, byte for byte as git lists it — case
/// and dots as written, and bytes that are not UTF-8 too: a name made into
/// text would turn off a driver of another name, and leave this one on. A
/// key `filter.<var>` names no driver.
///
/// Read off every field, not only each entry's key: a driver taken for one
/// by mistake is only turned off, and one missed would run.
pub fn filter_drivers(listing: &[u8]) -> BTreeSet<Vec<u8>> {
    listing
        .split(|byte| *byte == 0)
        .filter_map(|field| field.split(|byte| *byte == b'\n').next())
        .filter_map(|key| key.strip_prefix(b"filter."))
        .filter_map(|rest| {
            let at = rest.iter().rposition(|byte| *byte == b'.')?;
            Some(rest[..at].to_vec())
        })
        .collect()
}

/// Each include in the repository's own configuration that git reads afresh
/// for every worktree: a conditional one (`includeIf.<condition>.path`) in
/// the `local` or `worktree` scope, and any include in the `worktree` scope,
/// whose relative path is the worktree's own. What it names for a worktree
/// being cut — a filter, a `daft.hooks` key — is read only once the
/// worktree exists, too late to turn off, so the cut does not happen.
pub fn includes_read_per_worktree(listing: &[u8]) -> Vec<String> {
    entries(listing)
        .into_iter()
        .filter_map(|(scope, key)| {
            let key = String::from_utf8_lossy(key).into_owned();
            let lower = key.to_lowercase();
            let conditional = lower.starts_with("includeif.");
            let per_worktree = match scope {
                b"local" => conditional,
                b"worktree" => conditional || lower.starts_with("include."),
                _ => false,
            };
            per_worktree.then_some(key)
        })
        .collect()
}

/// Each key that would steer daft's hooks from the repository's own git
/// configuration: a `daft.hooks.*` key in the `local` or `worktree` scope —
/// includes followed, as git reports them in the including file's scope.
/// The person's own scopes are theirs, and do not count.
pub fn steering_daft_hooks(listing: &[u8]) -> Vec<String> {
    entries(listing)
        .into_iter()
        .filter(|(scope, _)| matches!(*scope, b"local" | b"worktree"))
        .map(|(_, key)| String::from_utf8_lossy(key).into_owned())
        .filter(|key| key.to_lowercase().starts_with("daft.hooks."))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(entries: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (scope, key, value) in entries {
            bytes.extend_from_slice(scope.as_bytes());
            bytes.push(0);
            bytes.extend_from_slice(b"file:.git/config");
            bytes.push(0);
            bytes.extend_from_slice(key.as_bytes());
            bytes.push(b'\n');
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
        bytes
    }

    fn commit() -> Commit {
        Commit::parse(&"a".repeat(40)).unwrap()
    }

    fn quiet() -> Vec<OsString> {
        [
            "-c",
            "core.hooksPath=/state/no-hooks",
            "-c",
            "core.fsmonitor=false",
        ]
        .map(OsString::from)
        .to_vec()
    }

    #[test]
    fn provider_for_picks_daft_only_where_chosen_and_opted_in() {
        use ProviderId::{Daft, Git};
        for (chosen, daft_yml, cuts) in [
            (Git, false, Git),
            (Git, true, Git),
            (Daft, false, Git),
            (Daft, true, Daft),
        ] {
            assert_eq!(provider_for(chosen, daft_yml), cuts, "{chosen} {daft_yml}");
        }
    }

    #[test]
    fn provider_ids_parse_from_their_names() {
        for id in ProviderId::ALL {
            assert_eq!(id.as_str().parse::<ProviderId>().unwrap(), id);
            assert_eq!(
                serde_json::to_value(id).unwrap(),
                serde_json::Value::String(id.as_str().into())
            );
            assert_eq!(
                serde_json::from_value::<ProviderId>(id.as_str().into()).unwrap(),
                id
            );
            assert_eq!(provider(id).id(), id);
        }
        let fail = "worktrunk".parse::<ProviderId>().unwrap_err();
        assert_eq!(fail.exit, Exit::Usage);
        assert!(serde_json::from_value::<ProviderId>("worktrunk".into()).is_err());
    }

    #[test]
    fn the_owner_follows_the_provider() {
        assert_eq!(provider(ProviderId::Git).owner().as_str(), "cahoots");
        assert_eq!(provider(ProviderId::Daft).owner().as_str(), "daft");
        assert_eq!(provider(ProviderId::Git).place(), Place::Chosen);
        assert_eq!(provider(ProviderId::Daft).place(), Place::Printed);
        assert_eq!(provider(ProviderId::Git).checkout(), Checkout::Cahoots);
        assert_eq!(provider(ProviderId::Daft).checkout(), Checkout::Tool);
    }

    #[test]
    fn fingerprints_read_real_version_strings() {
        let (git, daft) = (provider(ProviderId::Git), provider(ProviderId::Daft));
        assert_eq!(
            git.fingerprint("git version 2.50.1 (Apple Git-155)\n"),
            Some(Version(2, 50, 1))
        );
        assert_eq!(
            git.fingerprint("git version 2.43.0\n"),
            Some(Version(2, 43, 0))
        );
        assert_eq!(daft.fingerprint("daft 1.27.9\n"), Some(Version(1, 27, 9)));
        for not in ["codex-cli 0.155.1", "1.27.9", "daft", "daft\n", ""] {
            assert_eq!(daft.fingerprint(not), None, "{not:?}");
            assert_eq!(git.fingerprint(not), None, "{not:?}");
        }
        assert_eq!(git.fingerprint("daft 1.27.9"), None);
        assert_eq!(daft.fingerprint("git version 2.50.1"), None);
        for id in ProviderId::ALL {
            let (oldest, next) = provider(id).tested();
            assert!(oldest < next, "{id}");
        }
    }

    fn spec<'a>(
        base: &'a Path,
        at: Option<&'a Commit>,
        path: Option<&'a Path>,
        hooks: bool,
        quiet: &'a [OsString],
    ) -> CutSpec<'a> {
        CutSpec {
            base,
            at,
            path,
            hooks,
            quiet,
        }
    }

    fn strings(argv: &[OsString]) -> Vec<String> {
        argv.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn each_provider_builds_a_valid_command() {
        let (base, path, sha, quiet) = (
            Path::new("/r/work"),
            Path::new("/s/worktrees/run"),
            commit(),
            quiet(),
        );
        for at in [None, Some(&sha)] {
            let rev = at.map_or("HEAD", Commit::as_str);
            let git = command_line(
                provider(ProviderId::Git),
                &spec(base, at, Some(path), false, &quiet),
            )
            .unwrap();
            let mut expected = strings(&quiet);
            expected.extend(
                [
                    "-C",
                    "/r/work",
                    "worktree",
                    "add",
                    "--no-checkout",
                    "--detach",
                    "/s/worktrees/run",
                    rev,
                ]
                .map(str::to_string),
            );
            assert_eq!(strings(&git), expected);
            for hooks in [false, true] {
                let daft = command_line(
                    provider(ProviderId::Daft),
                    &spec(base, at, None, hooks, &quiet),
                )
                .unwrap();
                let mut expected: Vec<&str> = vec!["-C", "/r/work", "start", "--fork", "--no-cd"];
                expected.extend(if hooks {
                    ["--hooks", "foreground"]
                } else {
                    ["--skip-hooks", "all"]
                });
                expected.push("--no-carry");
                expected.extend(at.map(Commit::as_str));
                assert_eq!(strings(&daft), expected, "hooks {hooks}");
            }
        }
    }

    #[test]
    fn a_cut_command_gone_wrong_is_refused() {
        let (base, path, sha, quiet) = (
            Path::new("/r/work"),
            Path::new("/s/worktrees/run"),
            commit(),
            quiet(),
        );
        let daft = provider(ProviderId::Daft);
        for hooks in [false, true] {
            let spec = spec(base, Some(&sha), None, hooks, &quiet);
            let good = daft.build_argv(&spec);
            assert!(daft.check_argv(&spec, &good).is_ok());
            let mut dropped: Vec<&str> = vec!["--no-carry", "--no-cd", "--fork", "start"];
            if !hooks {
                dropped.extend(["--skip-hooks", "all"]);
            } else {
                dropped.extend(["--hooks", "foreground"]);
            }
            for gone in dropped {
                let argv: Vec<OsString> = good.iter().filter(|arg| *arg != gone).cloned().collect();
                assert!(
                    daft.check_argv(&spec, &argv).is_err(),
                    "hooks {hooks}: dropping {gone} went unnoticed"
                );
            }
            for extra in [
                &["-x", "make"][..],
                &["--exec", "make"],
                &["-c"],
                &["--carry"],
                &["--with-related"],
                &["--at", "/elsewhere"],
                &["-@", "/elsewhere"],
                &["--repo", "other"],
                &["-n", "2"],
                &["--count", "2"],
                &["--local"],
                &["--hooks", "auto"],
                &["--hooks", "background"],
                &["--skip-hooks", "all"],
                &["--hooks", "foreground"],
            ] {
                if (hooks && extra == ["--hooks", "foreground"])
                    || (!hooks && extra == ["--skip-hooks", "all"])
                {
                    // A second copy of what is already there.
                    continue;
                }
                let mut argv = good.clone();
                argv.extend(extra.iter().map(OsString::from));
                assert!(
                    daft.check_argv(&spec, &argv).is_err(),
                    "hooks {hooks}: {extra:?} was let through"
                );
            }
            // The commit swapped for another.
            let mut argv = good.clone();
            *argv.last_mut().unwrap() = OsString::from("b".repeat(40));
            assert!(daft.check_argv(&spec, &argv).is_err());
        }
        // The hooks mode of the other setting.
        let off = spec(base, None, None, false, &quiet);
        let on = spec(base, None, None, true, &quiet);
        assert!(daft.check_argv(&off, &daft.build_argv(&on)).is_err());
        assert!(daft.check_argv(&on, &daft.build_argv(&off)).is_err());

        let git = provider(ProviderId::Git);
        let spec = spec(base, Some(&sha), Some(path), false, &quiet);
        let good = git.build_argv(&spec);
        assert!(git.check_argv(&spec, &good).is_ok());
        // A checkout in the cut itself: before cahoots has read the
        // configuration as git reads it in the new worktree.
        let checks_out: Vec<OsString> = good
            .iter()
            .filter(|arg| *arg != "--no-checkout")
            .cloned()
            .collect();
        assert!(
            git.check_argv(&spec, &checks_out).is_err(),
            "dropping --no-checkout went unnoticed"
        );
        for drop in [0..2, 2..4, 0..4] {
            let mut argv = good.clone();
            argv.drain(drop.clone());
            assert!(
                git.check_argv(&spec, &argv).is_err(),
                "dropping {drop:?} went unnoticed"
            );
        }
        let elsewhere: Vec<OsString> = good
            .iter()
            .map(|arg| {
                if arg == "/s/worktrees/run" {
                    OsString::from("/elsewhere")
                } else {
                    arg.clone()
                }
            })
            .collect();
        assert!(git.check_argv(&spec, &elsewhere).is_err());
        let mut head = good.clone();
        *head.last_mut().unwrap() = OsString::from("HEAD");
        assert!(git.check_argv(&spec, &head).is_err(), "not at the commit");
        let mut extra = good.clone();
        extra.push(OsString::from("--force"));
        assert!(git.check_argv(&spec, &extra).is_err());
        let nowhere = CutSpec { path: None, ..spec };
        assert!(command_line(git, &nowhere).is_err());
        assert_eq!(
            command_line(git, &nowhere).unwrap_err().exit,
            Exit::Internal
        );
    }

    /// Drift: every flag cahoots gives `daft start` exists in the real
    /// `daft start --help`, captured at the tested version
    /// (tests/fixtures/help). `-C` is daft's own, before the subcommand.
    #[test]
    fn every_emitted_daft_flag_exists_in_the_captured_help() {
        let help = include_str!("../../tests/fixtures/help/daft-start-1.27.9.txt");
        let (sha, quiet) = (commit(), quiet());
        for hooks in [false, true] {
            let argv = strings(&provider(ProviderId::Daft).build_argv(&spec(
                Path::new("/r/work"),
                Some(&sha),
                None,
                hooks,
                &quiet,
            )));
            let start = argv.iter().position(|arg| arg == "start").unwrap();
            for arg in &argv[start + 1..] {
                if arg.starts_with("--") || (arg.starts_with('-') && arg.len() == 2) {
                    assert!(help.contains(arg.as_str()), "daft's help has no {arg}");
                }
            }
            // The values too: `--hooks foreground`, `--skip-hooks all`.
            let value = if hooks { "- foreground:" } else { "(all |" };
            assert!(help.contains(value), "daft's help has no {value}");
        }
    }

    #[test]
    fn repository_keys_that_steer_daft_hooks_are_found() {
        let found = steering_daft_hooks(&listing(&[
            ("global", "daft.hooks.userdirectory", "/mine"),
            ("system", "daft.hooks.timeout", "5"),
            ("command", "daft.hooks.userdirectory", "/cmd"),
            ("local", "daft.hooks.userdirectory", "/theirs"),
            ("worktree", "daft.Hooks.trust", "x"),
            ("local", "DAFT.HOOKS.x", "y"),
            ("local", "daft.checkout.push", "false"),
            ("local", "daft.hooksx.y", "z"),
            ("local", "core.hookspath", "/h"),
        ]));
        assert_eq!(
            found,
            [
                "daft.hooks.userdirectory",
                "daft.Hooks.trust",
                "DAFT.HOOKS.x"
            ]
        );
        assert!(steering_daft_hooks(b"").is_empty());
    }

    #[test]
    fn filter_drivers_are_read_from_every_scope() {
        let found = filter_drivers(&listing(&[
            ("local", "filter.evil.smudge", "x"),
            ("local", "filter.evil.clean", "x"),
            ("worktree", "filter.wt.process", "x"),
            ("global", "filter.lfs.required", "true"),
            ("system", "filter.sys.smudge", "x"),
            ("local", "filter.Inc.Name.smudge", "x"),
            ("local", "filter.a=b.smudge", "x"),
            ("local", "filter.x", "no subsection"),
            ("local", "core.filter", "x"),
            ("local", "interactive.difffilter", "x"),
        ]));
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            [
                b"Inc.Name".to_vec(),
                b"a=b".to_vec(),
                b"evil".to_vec(),
                b"lfs".to_vec(),
                b"sys".to_vec(),
                b"wt".to_vec()
            ]
        );
        // A key with no value (`[filter "bare"] smudge`) is still a key.
        assert_eq!(
            filter_drivers(b"local\0file:.git/config\0filter.bare.smudge\0")
                .into_iter()
                .collect::<Vec<_>>(),
            [b"bare".to_vec()]
        );
        // A name that is not UTF-8 is kept as it is, byte for byte.
        assert_eq!(
            filter_drivers(b"local\0file:.git/config\0filter.ev\xffil.smudge\nx\0")
                .into_iter()
                .collect::<Vec<_>>(),
            [b"ev\xffil".to_vec()]
        );
        assert!(filter_drivers(b"").is_empty());
    }

    #[test]
    fn includes_git_reads_for_each_worktree_are_found() {
        let found = includes_read_per_worktree(&listing(&[
            ("local", "includeif.gitdir:**/worktrees/**.path", "/x"),
            ("local", "includeIf.onbranch:main.path", "/x"),
            ("local", "include.path", "/x"),
            ("worktree", "include.path", "evil.cfg"),
            ("worktree", "includeif.gitdir/i:x.path", "/x"),
            ("global", "includeif.gitdir:~/work/.path", "/mine"),
            ("system", "includeif.gitdir:/srv/.path", "/x"),
            ("command", "includeif.gitdir:x.path", "/x"),
            ("local", "core.hookspath", "/h"),
        ]));
        assert_eq!(
            found,
            [
                "includeif.gitdir:**/worktrees/**.path",
                "includeIf.onbranch:main.path",
                "include.path",
                "includeif.gitdir/i:x.path"
            ]
        );
        assert!(includes_read_per_worktree(b"").is_empty());
    }
}
