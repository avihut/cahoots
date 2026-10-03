//! Where a run works. A reader works where the caller is. A WRITER never
//! does, unless a person's config says it may: it gets a worktree of its own,
//! cut from the caller's HEAD, so that what it writes is a proposal the
//! caller reviews — not an edit that lands in the middle of the caller's own
//! work.
//!
//! The client decides and checks (`decide`); the detached supervisor does the
//! cutting (`cut`), because a checkout of a large repository can take longer
//! than a caller's tool call may. The cut runs no command the repository
//! defines: no git hook, no `daft.yml` job, no fsmonitor (docs/THREAT-MODEL.md,
//! Writers).

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::dirs::{Dirs, ensure_private_dir};
use crate::exit::{Exit, Fail, Res};
use crate::model::Role;
use crate::patch::Commit;
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

/// How long `daft start --fork` and `git worktree add` may take, before a
/// run's own `--timeout` shortens it.
const DAFT_DEADLINE: Duration = Duration::from_secs(900);
const GIT_DEADLINE: Duration = Duration::from_secs(300);

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

/// Cuts the worktree a writer will work in, and returns its path and its git
/// directory.
///
/// In a repository that opted into daft (a `daft.yml` at its top) the fork is
/// daft's — `daft start --fork`, with every hook skipped — and the path daft
/// prints is used only if it is fit for a writer (`unfit`). Anywhere else it
/// is a plain detached `git worktree` under cahoots' state directory, outside
/// every workspace. No repository hook runs either way. `roots` are the
/// directories no tool started here may come from; `deadline` is the run's
/// own timeout, which shortens the cut's. `at` is the commit to cut at — the
/// one the run recorded — and with none, the base's HEAD.
pub fn cut(
    dirs: &Dirs,
    base: &Path,
    run_id: &str,
    roots: &[&Path],
    deadline: Duration,
    at: Option<&Commit>,
) -> Res<(PathBuf, PathBuf)> {
    let (top, common) = spawn::git_roots(base, roots)?
        .ok_or_else(|| cut_failed("the base is no longer a repository"))?;
    let mut roots = roots.to_vec();
    roots.extend([base, top.as_path()]);

    if top.join("daft.yml").is_file() {
        match spawn::system_tool("daft", &roots) {
            Ok(daft) => {
                return cut_with_daft(dirs, &daft, base, (&top, &common), &roots, deadline, at);
            }
            Err(fail) if fail.exit == Exit::Policy => {
                return Err(Fail::policy(format!(
                    "cannot cut a worktree: {}",
                    fail.message
                )));
            }
            // No daft to run: a plain worktree, as anywhere else.
            Err(_) => {}
        }
    }

    let worktrees = dirs.state.join("worktrees");
    ensure_private_dir(&worktrees)?;
    let path = worktrees.join(run_id);
    let git = spawn::system_tool("git", &roots)?;
    let deadline = deadline.min(GIT_DEADLINE);
    let before = linked_worktrees(&common)?;
    let mut args = quiet_git_args(dirs)?;
    args.extend([
        OsString::from("-C"),
        base.into(),
        "worktree".into(),
        "add".into(),
        "--detach".into(),
        path.clone().into(),
        at.map_or("HEAD", Commit::as_str).into(),
    ]);
    let started = Instant::now();
    let output =
        spawn::run_helper_with_env(&git, &args, None, deadline, spawn::helper_path(&roots), &[])
            .map_err(|fail| did_not_finish("`git worktree add`", started, deadline, fail))?;
    if output.status != Some(0) || !path.is_dir() {
        return Err(cut_failed(
            "`git worktree add` refused (a repository with no commit has no HEAD to cut from)",
        ));
    }
    // A refused pin leaves the worktree where it is: no run on record points
    // at it, so the refusal names it, for a person to remove.
    let gitdir = pin(dirs, &path, &common, &roots, &before).map_err(|why| {
        Fail::policy(format!(
            "cannot cut a worktree: {}, {why} — refusing to run a writer there, and the \
             worktree is left there for a person to remove",
            path.display()
        ))
    })?;
    Ok((path, gitdir))
}

/// `daft start --fork`, with no hook of the repository's: `--skip-hooks all`
/// for daft's own, and for the `git` it runs, the same empty hooks directory
/// cahoots' own git gets. `--no-carry`, so that a person's carry setting
/// never brings their uncommitted work into the writer's tree (and from
/// there into its patch), and the commit to fork from, when there is one.
/// `repository` is the base's: its toplevel and its common directory.
fn cut_with_daft(
    dirs: &Dirs,
    daft: &Path,
    base: &Path,
    (top, common): (&Path, &Path),
    roots: &[&Path],
    deadline: Duration,
    at: Option<&Commit>,
) -> Res<(PathBuf, PathBuf)> {
    let deadline = deadline.min(DAFT_DEADLINE);
    let before = linked_worktrees(common)?;
    let mut args = vec![
        OsStr::new("-C"),
        base.as_os_str(),
        OsStr::new("start"),
        OsStr::new("--fork"),
        OsStr::new("--no-cd"),
        OsStr::new("--skip-hooks"),
        OsStr::new("all"),
        OsStr::new("--no-carry"),
    ];
    args.extend(at.map(|commit| OsStr::new(commit.as_str())));
    let started = Instant::now();
    let output = spawn::run_helper_with_env(
        daft,
        &args,
        None,
        deadline,
        spawn::helper_path(roots),
        &quiet_git_env(dirs)?,
    )
    .map_err(|fail| did_not_finish("`daft start --fork`", started, deadline, fail))?;
    match output.status {
        Some(0) => {}
        Some(code) => {
            return Err(cut_failed(&format!(
                "`daft start --fork` exited with status {code}"
            )));
        }
        None => return Err(cut_failed("`daft start --fork` was killed by a signal")),
    }
    let Some(printed) = output
        .stdout
        .lines()
        .map(str::trim)
        .rev()
        .find(|line| !line.is_empty())
    else {
        return Err(cut_failed("`daft start --fork` printed no path"));
    };
    let refuse = |why: &str| {
        Fail::policy(format!(
            "cannot cut a worktree: `daft start --fork` printed {printed}, which {why} — \
             refusing to run a writer there"
        ))
    };
    if let Some(why) = unfit(dirs, printed, top, common, roots) {
        return Err(refuse(why));
    }
    let worktree = fs::canonicalize(printed).map_err(|_| refuse("is not a directory"))?;
    let gitdir = pin(dirs, &worktree, common, roots, &before).map_err(|why| {
        Fail::policy(format!(
            "cannot cut a worktree: `daft start --fork` printed {printed}, {why} — refusing to \
             run a writer there"
        ))
    })?;
    Ok((worktree, gitdir))
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
fn git_dir_of(dirs: &Dirs, worktree: &Path, roots: &[&Path]) -> Option<PathBuf> {
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
fn is_linked_gitdir(gitdir: &Path, common: &Path) -> bool {
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

/// Removes a worktree cahoots cut itself (never one of daft's, never the
/// caller's tree) once no run on record works in it any more.
pub fn discard(dirs: &Dirs, base: &Path, worktree: &Path, roots: &[&Path]) {
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

/// The quiet settings as `GIT_CONFIG_*` variables: for `daft`, so that the
/// git it starts is told too.
fn quiet_git_env(dirs: &Dirs) -> Res<Vec<(OsString, OsString)>> {
    let settings = quiet_settings(dirs)?;
    let mut vars = vec![(
        OsString::from("GIT_CONFIG_COUNT"),
        OsString::from(settings.len().to_string()),
    )];
    for (n, (key, value)) in settings.into_iter().enumerate() {
        vars.push((format!("GIT_CONFIG_KEY_{n}").into(), key.into()));
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
            overridden: true,
        };
        let (top, common) = (root.join("work"), root.join("work/.git"));
        for dir in [
            root.join("state/elsewhere"),
            root.join("config/x"),
            root.join("state/worktrees/f"),
            top.clone(),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        let unfit_here = |path: &Path| unfit(&dirs, path.to_str().unwrap(), &top, &common, &[]);

        // A plain directory, so a git check would say "not a worktree": the
        // order makes it cahoots' directories that are named.
        for inside in [root.join("state/elsewhere"), root.join("config/x")] {
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
