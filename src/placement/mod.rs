//! Where a run works. A reader works where the caller is. A WRITER never
//! does, unless a person's config says it may: it gets a worktree of its own,
//! cut from the caller's HEAD, so that what it writes is a proposal the
//! caller reviews — not an edit that lands in the middle of the caller's own
//! work.
//!
//! The client decides and checks (`decide`); the detached supervisor does the
//! cutting (`cut`), because a checkout of a large repository can take longer
//! than a caller's tool call may. What cuts it is a provider a person chose
//! (`provider`: git, or daft where the repository has a `daft.yml`). The cut
//! runs no command the repository defines: no git hook, no `daft.yml` job
//! unless a person turned daft's hooks on, no fsmonitor, no filter
//! (docs/THREAT-MODEL.md, Writers).

mod daft;
mod git;
pub mod provider;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::dirs::{Dirs, ensure_private_dir};
use crate::exit::{Exit, Fail, Res};
use crate::model::Role;
use crate::patch::Commit;
use crate::paths::{self, Workspace};
use crate::registry::{self, Registry};
use crate::spawn;
use provider::{CutSpec, Owner, Place, ProviderId};

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
            let mut roots = workspace.roots();
            roots.push(&base);
            if spawn::git_roots(&base, &roots)?.is_none() {
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

/// A worktree cut for a writer: where it is, its git directory as read at
/// the cut, and the provider that cut it.
#[derive(Debug, Clone)]
pub struct Cut {
    pub worktree: PathBuf,
    pub gitdir: PathBuf,
    pub provider: ProviderId,
}

/// Cuts the worktree a writer will work in, with the provider `fork` chooses
/// for this repository (`provider::provider_for`): git, or daft where a
/// person chose it and the repository has a `daft.yml` at its top. A chosen
/// daft that is missing or unfit fails the cut; it never falls back to git.
///
/// No repository hook runs, unless a person turned daft's on, and no filter
/// runs during the checkout: every filter driver the configuration defines
/// is turned off for the cut, and a driver that appears while it runs fails
/// it. The tool runs in a process group of its own, killed when it returns.
/// git's worktree goes where cahoots chooses, under its state directory;
/// the path daft prints is used only if it is fit for a writer (`unfit`).
/// `roots` are the directories no tool started here may come from;
/// `deadline` is the run's own timeout, which shortens the cut's. `at` is
/// the commit to cut at — the one the run recorded — and with none, the
/// base's HEAD.
pub fn cut(
    dirs: &Dirs,
    fork: &registry::Fork,
    base: &Path,
    run_id: &str,
    roots: &[&Path],
    deadline: Duration,
    at: Option<&Commit>,
) -> Res<Cut> {
    let (top, common) = spawn::git_roots(base, roots)?
        .ok_or_else(|| cut_failed("the base is no longer a repository"))?;
    let mut roots = roots.to_vec();
    roots.extend([base, top.as_path()]);

    let id = provider::provider_for(fork.provider, top.join("daft.yml").is_file());
    let tool = provider::provider(id);
    let (binary, _) = provider::locate(id, fork, &roots)?;
    // A tool that runs git of its own finds it on the PATH it is given: that
    // git is held to the binary policy and the version floor before the tool
    // starts (`spawn::path_for_git_users`). git itself was, by `locate`.
    let path = if tool.binary_name() == "git" {
        spawn::helper_path(&roots)
    } else {
        spawn::path_for_git_users(&roots)?
    };
    let hooks = id == ProviderId::Daft && fork.daft_hooks;

    // Checked before the configuration is listed: the git that lists it is
    // given these settings too, and a hooks directory that is not empty is
    // refused here, as it always was, before anything cuts.
    let quiet = quiet_git_args(dirs)?;
    // The filters to turn off are the ones the configuration names now.
    let listing = config_listing(dirs, base, &roots).map_err(|why| {
        cut_failed(&format!(
            "the git configuration could not be read ({why}), so its filters cannot be turned off"
        ))
    })?;
    if let Some(key) = provider::includes_read_per_worktree(&listing).first() {
        return Err(Fail::policy(format!(
            "cannot cut a worktree: the repository's own git configuration sets {key}, an \
             include git reads for each worktree apart — what it names in a new worktree cannot \
             be read before the cut, so not every filter could be turned off"
        )));
    }
    let filters = provider::filter_drivers(&listing);
    if hooks && let Some(key) = provider::steering_daft_hooks(&listing).first() {
        return Err(Fail::policy(format!(
            "cannot cut a worktree: the repository's own git configuration sets {key} — with \
             fork.daft.hooks on, it would choose what daft runs"
        )));
    }
    let before = linked_worktrees(&common)?;

    let chosen = match tool.place() {
        Place::Chosen => {
            let worktrees = dirs.state.join("worktrees");
            ensure_private_dir(&worktrees)?;
            Some(worktrees.join(run_id))
        }
        Place::Printed => None,
    };
    let argv = provider::command_line(
        tool,
        &CutSpec {
            base,
            at,
            path: chosen.as_deref(),
            hooks,
            quiet: &quiet,
        },
    )?;
    let vars = cut_git_env(dirs, &filters)?;
    let deadline = deadline.min(tool.deadline());
    let started = Instant::now();
    let ran = spawn::run_helper_grouped(&binary, &argv, None, deadline, path, &vars);
    let printed = match &ran {
        Ok(spawn::Grouped::Exited(output)) => last_line(&output.stdout),
        _ => None,
    };

    // A filter named while the tool ran was not turned off, and may have
    // run: the writer does not start there, whatever the tool made of it.
    let now = config_listing(dirs, base, &roots).map_err(|why| {
        cut_failed(&format!(
            "the git configuration could not be read again after the cut ({why}), so a filter \
             it gained cannot be ruled out"
        ))
    })?;
    let gained: Vec<String> = provider::filter_drivers(&now)
        .difference(&filters)
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect();
    if !gained.is_empty() {
        let made = chosen
            .as_deref()
            .or(printed.as_deref().map(Path::new))
            .filter(|made| made.is_dir());
        return Err(Fail::policy(format!(
            "cannot cut a worktree: the git configuration gained a filter ({}) while the \
             worktree was cut — refusing to run a writer there{}",
            gained.join(", "),
            made.map_or(String::new(), |made| format!(
                ", and {} is left for a person to remove",
                made.display()
            ))
        )));
    }

    let output = match ran.map_err(|fail| did_not_finish(tool.what(), started, deadline, fail))? {
        spawn::Grouped::Exited(output) => output,
        spawn::Grouped::OutputHeld => {
            return Err(cut_failed(&format!(
                "{} left a process holding its output",
                tool.what()
            )));
        }
    };
    if output.status != Some(0) {
        return Err(cut_failed(&tool.failure(output.status)));
    }

    match chosen {
        Some(path) => {
            if !path.is_dir() {
                return Err(cut_failed(&tool.failure(output.status)));
            }
            // A refused pin leaves the worktree where it is: no run on record
            // points at it, so the refusal names it, for a person to remove.
            let gitdir = pin(dirs, &path, &common, &roots, &before).map_err(|why| {
                Fail::policy(format!(
                    "cannot cut a worktree: {}, {why} — refusing to run a writer there, and the \
                     worktree is left there for a person to remove",
                    path.display()
                ))
            })?;
            Ok(Cut {
                worktree: path,
                gitdir,
                provider: id,
            })
        }
        None => {
            let Some(printed) = printed else {
                return Err(cut_failed(&format!("{} printed no path", tool.what())));
            };
            let refuse = |why: &str| {
                Fail::policy(format!(
                    "cannot cut a worktree: {} printed {printed}, which {why} — refusing to run a \
                     writer there",
                    tool.what()
                ))
            };
            if let Some(why) = unfit(dirs, &printed, &top, &common, &roots) {
                return Err(refuse(why));
            }
            let worktree = fs::canonicalize(&printed).map_err(|_| refuse("is not a directory"))?;
            let gitdir = pin(dirs, &worktree, &common, &roots, &before).map_err(|why| {
                Fail::policy(format!(
                    "cannot cut a worktree: {} printed {printed}, {why} — refusing to run a \
                     writer there",
                    tool.what()
                ))
            })?;
            Ok(Cut {
                worktree,
                gitdir,
                provider: id,
            })
        }
    }
}

/// The last line with something on it, trimmed: the path a tool printed.
fn last_line(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .map(str::trim)
        .rev()
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn cut_failed(what: &str) -> Fail {
    Fail::new(Exit::RunFailed, format!("cannot cut a worktree: {what}"))
}

/// A tool the cut ran did not finish: its deadline passed (told apart by the
/// clock, so that the spawner's own error stays what it is everywhere else),
/// or it never started.
fn did_not_finish(what: &str, started: Instant, deadline: Duration, fail: Fail) -> Fail {
    if started.elapsed() >= deadline {
        cut_failed(&format!(
            "{what} did not finish within {}s",
            deadline.as_secs()
        ))
    } else {
        cut_failed(&fail.message)
    }
}

/// Why a path `daft` printed may not be a writer's worktree, or `None` if it
/// may. In this order, and the first that fails is the reason.
fn unfit(
    dirs: &Dirs,
    printed: &str,
    top: &Path,
    common: &Path,
    roots: &[&Path],
) -> Option<&'static str> {
    let path = Path::new(printed);
    if !path.is_absolute() {
        return Some("is not an absolute path");
    }
    let Ok(canonical) = fs::canonicalize(path) else {
        return Some("is not a directory");
    };
    if !canonical.is_dir() {
        return Some("is not a directory");
    }
    if canonical.starts_with(top) || top.starts_with(&canonical) {
        return Some("is the tree it was cut from, or inside it");
    }
    // Compared here, not with `Dirs::refuse_inside`, which lets a dev
    // build's overridden directories through.
    let state = canonical_of(&dirs.state);
    if canonical.starts_with(canonical_of(&dirs.config))
        || canonical.starts_with(canonical_of(&dirs.data))
        || (canonical.starts_with(&state)
            && !canonical.starts_with(canonical_of(&dirs.state.join("worktrees"))))
    {
        return Some("is inside cahoots' own directories");
    }
    match spawn::git_roots(&canonical, roots) {
        Ok(Some((toplevel, theirs))) if toplevel == canonical && theirs == common => None,
        _ => Some("is not a worktree of the repository it was cut from"),
    }
}

/// A new worktree's git directory, read before the writer has run, and held
/// to git's layout for a linked worktree: `<common>/worktrees/<name>`, and a
/// name that was not there before the cut (`before`) — so never a worktree
/// that was already someone's, the caller's own among them. What the
/// worktree's `.git` names later is the writer's to change; this is not.
fn pin(
    dirs: &Dirs,
    worktree: &Path,
    common: &Path,
    roots: &[&Path],
    before: &[OsString],
) -> Result<PathBuf, &'static str> {
    let gitdir = git_dir_of(dirs, worktree, roots)
        .filter(|gitdir| is_linked_gitdir(gitdir, common))
        .ok_or("whose git directory is not the repository's")?;
    if gitdir
        .file_name()
        .is_some_and(|name| before.iter().any(|known| known == name))
    {
        return Err("which was a worktree already, not one cut for this run");
    }
    Ok(gitdir)
}

/// The names of the repository's linked worktrees, as git keeps them under
/// `<common>/worktrees` — none before the first. A list that cannot be read
/// fails the cut: without it a fresh worktree cannot be told from one that
/// was already there.
fn linked_worktrees(common: &Path) -> Res<Vec<OsString>> {
    let listed = common.join("worktrees");
    let unreadable = |error: std::io::Error| {
        cut_failed(&format!(
            "cannot list {} ({error}), so a fresh worktree cannot be told from one that was \
             already there",
            listed.display()
        ))
    };
    let entries = match fs::read_dir(&listed) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(unreadable(error)),
    };
    entries
        .map(|entry| entry.map(|entry| entry.file_name()).map_err(unreadable))
        .collect()
}

/// The git directory git takes `worktree` to have now, canonical.
pub(crate) fn git_dir_of(dirs: &Dirs, worktree: &Path, roots: &[&Path]) -> Option<PathBuf> {
    let mut roots = roots.to_vec();
    roots.push(worktree);
    let git = spawn::system_tool("git", &roots).ok()?;
    let mut args = quiet_git_args(dirs).ok()?;
    args.extend([
        OsString::from("-C"),
        worktree.into(),
        "rev-parse".into(),
        "--absolute-git-dir".into(),
    ]);
    let output = spawn::run_helper_with_env(
        &git,
        &args,
        None,
        Duration::from_secs(10),
        spawn::helper_path(&roots),
        &[],
    )
    .ok()?;
    if output.status != Some(0) {
        return None;
    }
    fs::canonicalize(output.stdout.trim()).ok()
}

/// Whether `gitdir` is where git keeps a linked worktree of the repository
/// whose common directory is `common`.
pub(crate) fn is_linked_gitdir(gitdir: &Path, common: &Path) -> bool {
    gitdir.parent() == Some(canonical_of(common).join("worktrees").as_path())
}

/// `git status --short` in a writer's worktree: what it changed, for the
/// caller to review. Capped; this is a summary, not the diff. `Ok` with no
/// lines means clean, and only that: a status that cannot be read says why.
///
/// A fork's is read against its git directory (`fork_git_dir`), never by
/// following the worktree's `.git`, which the writer could have rewritten.
pub fn changes(
    dirs: &Dirs,
    worktree: &Path,
    base: &Path,
    gitdir: Option<&Path>,
    roots: &[&Path],
) -> Result<Vec<String>, String> {
    if !worktree.is_dir() {
        return Err(format!("{} is not there any more", worktree.display()));
    }
    let mut roots = roots.to_vec();
    roots.push(worktree);
    let gitdir = fork_git_dir(dirs, worktree, base, gitdir, &roots)?;
    status(dirs, worktree, Some(&gitdir), &roots)
}

/// [`changes`] for a writer that worked in the caller's own tree, which a
/// person let it into: there is no cut to hold it to, and the tree's `.git`
/// may have been within its reach. So `git status` runs only while the git
/// configuration it would read is what it was before the writer started
/// (`before`, from [`config_listing`]); otherwise this says why it did not.
pub fn changes_in_place(
    dirs: &Dirs,
    tree: &Path,
    before: Option<&[u8]>,
    roots: &[&Path],
) -> Result<Vec<String>, String> {
    if !tree.is_dir() {
        return Err(format!("{} is not there any more", tree.display()));
    }
    let mut roots = roots.to_vec();
    roots.push(tree);
    let not_run = |what: &str| {
        format!(
            "the git configuration {what}, so git status was not run — read the tree yourself \
             before trusting it"
        )
    };
    let Some(before) = before else {
        return Err(not_run("was not recorded before the run"));
    };
    match config_listing(dirs, tree, &roots) {
        Ok(now) if now == before => status(dirs, tree, None, &roots),
        Ok(_) => Err(not_run("changed during the run")),
        Err(_) => Err(not_run("could not be read after the run")),
    }
}

/// The git configuration git would read in `dir`, as git itself resolves it:
/// every scope, includes and `includeIf` followed, each value with the file
/// it came from (`git config --list --show-origin --show-scope -z`). It runs
/// nothing. The snapshot taken before an in-place run and the reading
/// compared with it after are both this, so that they can only differ where
/// the configuration did.
pub fn config_listing(dirs: &Dirs, dir: &Path, roots: &[&Path]) -> Result<Vec<u8>, String> {
    let mut roots = roots.to_vec();
    roots.push(dir);
    let git = spawn::system_tool("git", &roots).map_err(|fail| fail.message)?;
    let mut args = quiet_git_args(dirs).map_err(|fail| fail.message)?;
    args.extend(["config", "--list", "--show-origin", "--show-scope", "-z"].map(OsString::from));
    let output = spawn::run_helper_with_env(
        &git,
        &args,
        Some(dir),
        Duration::from_secs(10),
        spawn::helper_path(&roots),
        &[],
    )
    .map_err(|fail| fail.message)?;
    match output.status {
        Some(0) => Ok(output.bytes),
        _ => Err(format!("`git config --list` failed in {}", dir.display())),
    }
}

/// `git status --short` in `tree`, against `gitdir` when there is one, with
/// the quiet settings.
fn status(
    dirs: &Dirs,
    tree: &Path,
    gitdir: Option<&Path>,
    roots: &[&Path],
) -> Result<Vec<String>, String> {
    let mut args = quiet_git_args(dirs).map_err(|fail| fail.message)?;
    if let Some(gitdir) = gitdir {
        let (mut git_dir, mut work_tree) =
            (OsString::from("--git-dir="), OsString::from("--work-tree="));
        git_dir.push(gitdir);
        work_tree.push(tree);
        args.extend([git_dir, work_tree]);
    }
    // `--ignore-submodules=all`: a `git status` recurses into each populated
    // submodule with a child `git status` there, which reads the submodule's
    // own gitdir and config — outside this run's pin and the in-place config
    // snapshot both. A writer can drop a `.git` file in a submodule directory
    // naming a gitdir of its own, with a filter; the flag overrides every
    // `submodule.*.ignore` and never recurses, so no such child runs. The
    // cost is that a modified submodule no longer shows in `changes`.
    args.extend(
        [
            "status",
            "--short",
            "--untracked-files=all",
            "--ignore-submodules=all",
        ]
        .map(OsString::from),
    );
    let git = spawn::system_tool("git", roots).map_err(|fail| fail.message)?;
    let output = spawn::run_helper_with_env(
        &git,
        &args,
        Some(tree),
        Duration::from_secs(30),
        spawn::helper_path(roots),
        &[],
    )
    .map_err(|fail| fail.message)?;
    match output.status {
        Some(0) => Ok(output
            .stdout
            .lines()
            .take(200)
            .map(str::to_string)
            .collect()),
        Some(code) => Err(format!(
            "`git status` failed in {} (exit {code})",
            tree.display()
        )),
        None => Err(format!(
            "`git status` was killed by a signal in {}",
            tree.display()
        )),
    }
}

/// The git directory a fork's changes are read against: the one pinned when
/// it was cut, if it is still where git keeps one of the repository's
/// worktrees. A record from before the pin has none, so the worktree must
/// still belong to the repository it was cut from, and the git directory its
/// `.git` names now must be one git keeps for that repository — never one
/// the writer made, whose config would be the writer's.
pub(crate) fn fork_git_dir(
    dirs: &Dirs,
    worktree: &Path,
    base: &Path,
    pinned: Option<&Path>,
    roots: &[&Path],
) -> Result<PathBuf, String> {
    let common = spawn::git_roots(base, roots)
        .map_err(|fail| fail.message)?
        .map(|(_, common)| common);
    if let Some(pinned) = pinned {
        return fs::canonicalize(pinned)
            .ok()
            .filter(|gitdir| {
                common
                    .as_deref()
                    .is_some_and(|common| is_linked_gitdir(gitdir, common))
            })
            .ok_or_else(|| "the worktree's git directory has moved".to_string());
    }
    let gone = || {
        format!(
            "{} is no longer a worktree of the repository it was cut from — read it yourself \
             before trusting it",
            worktree.display()
        )
    };
    let Some(common) = common else {
        return Err(gone());
    };
    match spawn::git_roots(worktree, roots).map_err(|fail| fail.message)? {
        Some((toplevel, theirs)) if theirs == common && toplevel == canonical_of(worktree) => {}
        _ => return Err(gone()),
    }
    git_dir_of(dirs, worktree, roots)
        .filter(|gitdir| is_linked_gitdir(gitdir, &common))
        .ok_or_else(gone)
}

/// Removes a worktree cahoots cut itself once no run on record works in it
/// any more: one its provider leaves to cahoots (git's), under cahoots'
/// `worktrees` — never one of daft's, wherever daft put it, and never the
/// caller's tree. A record from before the provider was recorded has none,
/// and is held to the path alone.
pub fn discard(
    dirs: &Dirs,
    base: &Path,
    worktree: &Path,
    cut_by: Option<ProviderId>,
    roots: &[&Path],
) {
    if cut_by.is_some_and(|id| provider::provider(id).owner() != Owner::Cahoots) {
        return;
    }
    if worktree == base || !worktree.starts_with(dirs.state.join("worktrees")) {
        return;
    }
    if let (Ok(git), Ok(mut args)) = (spawn::system_tool("git", roots), quiet_git_args(dirs)) {
        args.extend([
            OsString::from("-C"),
            base.into(),
            "worktree".into(),
            "remove".into(),
            "--force".into(),
            worktree.into(),
        ]);
        let _ = spawn::run_helper_with_env(
            &git,
            &args,
            None,
            Duration::from_secs(60),
            spawn::helper_path(roots),
            &[],
        );
    }
    let _ = fs::remove_dir_all(worktree);
}

/// What every `git` cahoots starts is told, above every config file: its
/// hooks are in an empty directory of cahoots' own, so none of the
/// repository's runs, and there is no fsmonitor, which is a command too.
fn quiet_settings(dirs: &Dirs) -> Res<[(&'static str, OsString); 2]> {
    let hooks = dirs.no_hooks();
    ensure_private_dir(&hooks)?;
    let empty = fs::read_dir(&hooks).is_ok_and(|mut entries| entries.next().is_none());
    if !empty {
        return Err(Fail::new(
            Exit::RunFailed,
            format!(
                "{} is not empty, and cahoots points git's hooks there",
                hooks.display()
            ),
        ));
    }
    Ok([
        ("core.hooksPath", hooks.into_os_string()),
        ("core.fsmonitor", OsString::from("false")),
    ])
}

/// The quiet settings as `-c` arguments, first in argv: for the git cahoots
/// runs itself, where a setting given on the command line is never ignored.
pub(crate) fn quiet_git_args(dirs: &Dirs) -> Res<Vec<OsString>> {
    Ok(quiet_settings(dirs)?
        .into_iter()
        .flat_map(|(key, value)| {
            let mut setting = OsString::from(format!("{key}="));
            setting.push(value);
            [OsString::from("-c"), setting]
        })
        .collect())
}

/// What the tool that cuts a worktree is told, as `GIT_CONFIG_*` variables,
/// so that every `git` it starts is told too: the quiet settings, then, for
/// each filter driver in `filters`, no smudge, clean or process command, and
/// not required — an emptied filter that is still required fails the
/// checkout. Each file a filter would have converted comes up as git stores
/// it. Variables, not `-c`, which splits at the first `=`, and a driver's
/// name may hold one.
pub(crate) fn cut_git_env(
    dirs: &Dirs,
    filters: &BTreeSet<Vec<u8>>,
) -> Res<Vec<(OsString, OsString)>> {
    let mut settings: Vec<(OsString, OsString)> = quiet_settings(dirs)?
        .into_iter()
        .map(|(key, value)| (OsString::from(key), value))
        .collect();
    for name in filters {
        for (var, value) in [
            ("smudge", ""),
            ("clean", ""),
            ("process", ""),
            ("required", "false"),
        ] {
            // Byte for byte: the driver's own name, whatever its bytes.
            let mut key = b"filter.".to_vec();
            key.extend_from_slice(name);
            key.push(b'.');
            key.extend_from_slice(var.as_bytes());
            settings.push((OsString::from_vec(key), value.into()));
        }
    }
    let mut vars = vec![(
        OsString::from("GIT_CONFIG_COUNT"),
        OsString::from(settings.len().to_string()),
    )];
    for (n, (key, value)) in settings.into_iter().enumerate() {
        vars.push((format!("GIT_CONFIG_KEY_{n}").into(), key));
        vars.push((format!("GIT_CONFIG_VALUE_{n}").into(), value));
    }
    Ok(vars)
}

fn canonical_of(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_in_cahoots_own_directories_is_refused_before_git_is_asked() {
        let root = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(root.path()).unwrap();
        let dirs = Dirs {
            home: root.join("home"),
            config: root.join("config"),
            state: root.join("state"),
            data: root.join("data"),
            overridden: true,
        };
        let (top, common) = (root.join("work"), root.join("work/.git"));
        for dir in [
            root.join("state/elsewhere"),
            root.join("config/x"),
            root.join("data/evals/tasks/x"),
            root.join("state/worktrees/f"),
            top.clone(),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        let unfit_here = |path: &Path| unfit(&dirs, path.to_str().unwrap(), &top, &common, &[]);

        // A plain directory, so a git check would say "not a worktree": the
        // order makes it cahoots' directories that are named.
        // A link from outside that resolves into one is inside it too.
        std::os::unix::fs::symlink(root.join("data/evals"), root.join("to-data")).unwrap();
        for inside in [
            root.join("state/elsewhere"),
            root.join("config/x"),
            root.join("data/evals/tasks/x"),
            root.join("data"),
            root.join("to-data/tasks/x"),
        ] {
            assert_eq!(
                unfit_here(&inside),
                Some("is inside cahoots' own directories"),
                "{}",
                inside.display()
            );
        }
        // Under `worktrees` is cahoots' own place for one: only git judges it.
        assert_eq!(
            unfit_here(&root.join("state/worktrees/f")),
            Some("is not a worktree of the repository it was cut from")
        );
        assert_eq!(
            unfit_here(&top.join("..").join("work")),
            Some("is the tree it was cut from, or inside it")
        );
        assert_eq!(
            unfit(&dirs, "work", &top, &common, &[]),
            Some("is not an absolute path")
        );
        assert_eq!(
            unfit_here(&root.join("missing")),
            Some("is not a directory")
        );
    }

    #[test]
    fn the_cut_env_turns_every_filter_off() {
        let root = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(root.path()).unwrap();
        let dirs = Dirs {
            home: root.join("home"),
            config: root.join("config"),
            state: root.join("state"),
            data: root.join("data"),
            overridden: true,
        };
        let filters: BTreeSet<Vec<u8>> =
            [b"evil".to_vec(), b"a=b".to_vec(), b"ev\xffil".to_vec()].into();
        let vars = cut_git_env(&dirs, &filters).unwrap();
        let get = |name: &str| {
            vars.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone().into_vec())
                .unwrap_or_else(|| panic!("no {name}"))
        };
        assert_eq!(get("GIT_CONFIG_COUNT"), b"14");
        assert_eq!(vars.len(), 1 + 2 * 14);
        let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..14)
            .map(|n| {
                (
                    get(&format!("GIT_CONFIG_KEY_{n}")),
                    get(&format!("GIT_CONFIG_VALUE_{n}")),
                )
            })
            .collect();
        assert_eq!(pairs[0].0, b"core.hooksPath");
        assert_eq!(
            pairs[0].1,
            dirs.no_hooks().into_os_string().into_vec(),
            "the empty hooks directory"
        );
        assert_eq!(pairs[1], (b"core.fsmonitor".to_vec(), b"false".to_vec()));
        // In the set's order; a name holding `=`, or bytes that are not
        // UTF-8, a key as it is.
        let mut expected = Vec::new();
        for name in [&b"a=b"[..], b"evil", b"ev\xffil"] {
            for (var, value) in [
                ("smudge", ""),
                ("clean", ""),
                ("process", ""),
                ("required", "false"),
            ] {
                let mut key = b"filter.".to_vec();
                key.extend_from_slice(name);
                key.push(b'.');
                key.extend_from_slice(var.as_bytes());
                expected.push((key, value.as_bytes().to_vec()));
            }
        }
        assert_eq!(pairs[2..], expected);
        // Nothing to turn off: the quiet settings alone.
        let quiet = cut_git_env(&dirs, &BTreeSet::new()).unwrap();
        assert_eq!(quiet[0], ("GIT_CONFIG_COUNT".into(), "2".into()));
    }

    #[test]
    fn a_linked_gitdir_is_one_under_the_common_dirs_worktrees() {
        let common = Path::new("/r/.git");
        assert!(is_linked_gitdir(Path::new("/r/.git/worktrees/f1"), common));
        assert!(!is_linked_gitdir(Path::new("/r/.git"), common));
        assert!(!is_linked_gitdir(
            Path::new("/r/.git/worktrees/f1/x"),
            common
        ));
        assert!(!is_linked_gitdir(Path::new("/evil/worktrees/f1"), common));
    }
}
