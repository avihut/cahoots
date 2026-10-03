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
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::unix::ffi::OsStrExt;
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

/// One entry of `git config --list --show-origin --show-scope -z`: its
/// scope, where it came from (`file:<path>`, `command line:`), and its key
/// as git lists it (the section and the name lowercased, a subsection as
/// written) and value.
struct Entry<'a> {
    scope: &'a [u8],
    origin: &'a [u8],
    key: &'a [u8],
    value: Option<&'a [u8]>,
}

fn entries(listing: &[u8]) -> Vec<Entry<'_>> {
    let fields: Vec<&[u8]> = listing.split(|byte| *byte == 0).collect();
    fields
        .chunks_exact(3)
        .map(|entry| {
            let (key, value) = key_and_value(entry[2]);
            Entry {
                scope: entry[0],
                origin: entry[1],
                key,
                value,
            }
        })
        .collect()
}

/// `key\nvalue`, or a key with no value at all (`[section] name`).
fn key_and_value(field: &[u8]) -> (&[u8], Option<&[u8]>) {
    match field.iter().position(|byte| *byte == b'\n') {
        Some(at) => (&field[..at], Some(&field[at + 1..])),
        None => (field, None),
    }
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

/// An include directive: the path it names, as written, the file it is
/// written in (`None` when it is not in a file), and whether it is the
/// repository's own — in its `local` or `worktree` configuration, or in a
/// file one of those includes reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Include {
    pub path: Vec<u8>,
    pub from: Option<PathBuf>,
    pub repository: bool,
}

/// `include.path`, or `includeIf.<condition>.path`, whatever the condition.
fn is_include(key: &[u8]) -> bool {
    let key = String::from_utf8_lossy(key).to_lowercase();
    key == "include.path" || (key.starts_with("includeif.") && key.ends_with(".path"))
}

/// Every include directive in a listing of every scope (`--show-origin
/// --show-scope -z`), whatever its condition, and whether git followed it
/// here or not. A relative origin is relative to `cwd`, where git ran.
pub fn includes(listing: &[u8], cwd: &Path) -> Vec<Include> {
    entries(listing)
        .into_iter()
        .filter(|entry| is_include(entry.key))
        .filter_map(|entry| {
            Some(Include {
                path: entry.value?.to_vec(),
                from: entry
                    .origin
                    .strip_prefix(b"file:")
                    .map(|file| cwd.join(OsStr::from_bytes(file))),
                repository: matches!(entry.scope, b"local" | b"worktree"),
            })
        })
        .collect()
}

/// Every include directive in the listing of one file (`config --file
/// <file> --list -z`), written in `file`; the repository's own if the file
/// was reached from the repository's own configuration.
pub fn includes_in_file(listing: &[u8], file: &Path, repository: bool) -> Vec<Include> {
    listing
        .split(|byte| *byte == 0)
        .map(key_and_value)
        .filter(|(key, _)| is_include(key))
        .filter_map(|(_, value)| {
            Some(Include {
                path: value?.to_vec(),
                from: Some(file.to_path_buf()),
                repository,
            })
        })
        .collect()
}

/// Where git looks for the file an include names: `~/` against `home` (the
/// HOME cahoots gives git), a relative path against the directory of the
/// file it is written in, an absolute path as it is. `None` where cahoots
/// cannot say as git would: `~user/`, `%(prefix)/`, an empty path, or a
/// relative one not written in a file.
pub fn include_target(include: &Include, home: &Path) -> Option<PathBuf> {
    let path = &include.path[..];
    if path.is_empty() || path.starts_with(b"%(") {
        return None;
    }
    if let Some(rest) = path.strip_prefix(b"~/") {
        return Some(home.join(OsStr::from_bytes(rest)));
    }
    if path.starts_with(b"~") {
        return None;
    }
    let path = Path::new(OsStr::from_bytes(path));
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    Some(include.from.as_ref()?.parent()?.join(path))
}

/// The `daft.hooks` keys in the listing of one file (`config --file <file>
/// --list -z`), as git lists them.
pub fn daft_hook_keys_in_file(listing: &[u8]) -> Vec<String> {
    listing
        .split(|byte| *byte == 0)
        .map(|field| String::from_utf8_lossy(key_and_value(field).0).into_owned())
        .filter(|key| key.to_lowercase().starts_with("daft.hooks."))
        .collect()
}

/// Each key that would steer daft's hooks from the repository's own git
/// configuration: a `daft.hooks.*` key in the `local` or `worktree` scope —
/// includes followed, as git reports them in the including file's scope.
/// The person's own scopes are theirs, and do not count.
pub fn steering_daft_hooks(listing: &[u8]) -> Vec<String> {
    entries(listing)
        .into_iter()
        .filter(|entry| matches!(entry.scope, b"local" | b"worktree"))
        .map(|entry| String::from_utf8_lossy(entry.key).into_owned())
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
    fn include_directives_are_found_in_every_scope_whatever_the_condition() {
        let mut listing = listing(&[
            (
                "local",
                "includeif.gitdir:**/worktrees/**.path",
                "per-worktree.cfg",
            ),
            (
                "global",
                "includeIf.gitdir:~/Work/.path",
                "~/.gitconfig-work",
            ),
            ("worktree", "include.path", "/abs.cfg"),
            ("system", "includeif.onbranch:main.path", "/sys.cfg"),
            ("local", "core.hookspath", "/h"),
            ("local", "includeif.gitdir:x.notpath", "/x"),
        ]);
        // A key with no value names no file.
        listing.extend_from_slice(b"local\0file:.git/config\0include.path\0");
        let found = includes(&listing, Path::new("/repo"));
        assert_eq!(
            found
                .iter()
                .map(|include| (
                    String::from_utf8_lossy(&include.path).into_owned(),
                    include.from.clone().unwrap(),
                    include.repository
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "per-worktree.cfg".into(),
                    PathBuf::from("/repo/.git/config"),
                    true
                ),
                (
                    "~/.gitconfig-work".into(),
                    PathBuf::from("/repo/.git/config"),
                    false
                ),
                ("/abs.cfg".into(), PathBuf::from("/repo/.git/config"), true),
                ("/sys.cfg".into(), PathBuf::from("/repo/.git/config"), false),
            ]
        );
        let in_file = includes_in_file(
            b"filter.x.smudge\ncat\0include.path\nnested.cfg\0includeIf.gitdir:y.path\n/y.cfg\0",
            Path::new("/conf/a.cfg"),
            true,
        );
        assert_eq!(
            in_file,
            [
                Include {
                    path: b"nested.cfg".to_vec(),
                    from: Some(PathBuf::from("/conf/a.cfg")),
                    repository: true
                },
                Include {
                    path: b"/y.cfg".to_vec(),
                    from: Some(PathBuf::from("/conf/a.cfg")),
                    repository: true
                },
            ]
        );
        assert_eq!(
            daft_hook_keys_in_file(b"daft.hooks.defaulttrust\nallow\0daft.checkout.push\nfalse\0"),
            ["daft.hooks.defaulttrust"]
        );
    }

    #[test]
    fn include_paths_resolve_as_git_resolves_them() {
        let home = Path::new("/home/u");
        let include = |path: &str, from: Option<&str>| Include {
            path: path.as_bytes().to_vec(),
            from: from.map(PathBuf::from),
            repository: false,
        };
        let target = |path: &str, from: Option<&str>| include_target(&include(path, from), home);
        assert_eq!(
            target("~/.gitconfig-work", Some("/etc/gitconfig")),
            Some(PathBuf::from("/home/u/.gitconfig-work"))
        );
        assert_eq!(
            target("nested/x.cfg", Some("/repo/.git/config")),
            Some(PathBuf::from("/repo/.git/nested/x.cfg")),
            "relative to the file it is written in"
        );
        assert_eq!(target("/abs.cfg", None), Some(PathBuf::from("/abs.cfg")));
        for unresolvable in [
            ("~other/x", Some("/repo/.git/config")),
            ("~", Some("/repo/.git/config")),
            ("%(prefix)/etc/x", Some("/repo/.git/config")),
            ("", Some("/repo/.git/config")),
            ("relative.cfg", None),
        ] {
            assert_eq!(
                target(unresolvable.0, unresolvable.1),
                None,
                "{unresolvable:?}"
            );
        }
    }
}
