//! What a run started from, and what a writer produced: the commit every run
//! records (`base_commit`), the patch a fork writer leaves in its run
//! directory (`patch.diff`), and the compact summary of it that the history
//! keeps after the patch itself has aged out.
//!
//! Every `git` here is started the way `placement`'s are: resolved under the
//! binary policy, on a PATH with no workspace directory in it, with the quiet
//! settings first in argv; and a fork's tree is read against the git
//! directory pinned when it was cut, never by following its `.git`. Content
//! comes only from git's output, byte for byte: nothing here reads a file in
//! the worktree (a writer can plant a link to anywhere). A link is read as a
//! link, with `read_link`.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::dirs::Dirs;
use crate::exit::{Exit, Fail, Res};
use crate::placement;
use crate::spawn;

/// How long keeping a patch may take, all of its steps together.
pub const CAPTURE_DEADLINE: Duration = Duration::from_secs(8);
/// The most a kept patch may weigh. Past it, no patch is kept at all.
pub const PATCH_CAP: usize = 32 * 1024 * 1024;
/// The most `[hash, lines]` pairs, and the most files, a summary lists.
pub const LISTED: usize = 1000;
const HEAD_DEADLINE: Duration = Duration::from_secs(10);

/// Every setting and option that shapes how a patch is written, so that
/// neither the person's git config nor the repository's has a say in it: the
/// same change is the same bytes, and the same hashes, on every machine.
/// What git converts on the way in — the repository's attributes and clean
/// filters, and `core.autocrlf`, which checkout applied to the tree already —
/// is honoured as git honours it. It follows the quiet settings and the
/// `--git-dir`/`--work-tree` pair in argv. #37 hashes later commits with
/// exactly this.
pub const DIFF: &[&str] = &[
    "-c",
    "core.quotePath=true",
    "-c",
    "core.bigFileThreshold=512m",
    "-c",
    "diff.noprefix=false",
    "-c",
    "diff.mnemonicPrefix=false",
    "-c",
    "diff.relative=false",
    "-c",
    "diff.suppressBlankEmpty=false",
    "diff",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--binary",
    "--full-index",
    "--no-renames",
    "--diff-algorithm=myers",
    "--indent-heuristic",
    "--unified=3",
    "--inter-hunk-context=0",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "-O/dev/null",
    "--ignore-submodules=all",
];

/// A commit id: exactly 40 or 64 lowercase hex digits, and nothing else — so
/// a hand-edited `run.json` cannot put anything else into a git argv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Commit(String);

impl Commit {
    pub fn parse(text: &str) -> Option<Commit> {
        let hex = text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        (matches!(text.len(), 40 | 64) && hex).then(|| Commit(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Commit {
    type Error = String;
    fn try_from(text: String) -> Result<Commit, String> {
        Commit::parse(&text).ok_or_else(|| format!("{text:?} is not a commit id"))
    }
}

impl From<Commit> for String {
    fn from(commit: Commit) -> String {
        commit.0
    }
}

impl std::fmt::Display for Commit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a writer's patch touched, without what it says: per file, the path,
/// the lines added and removed, and a hash and a line count per changed
/// block (`summarize`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchSummary {
    pub files: Vec<FileSummary>,
    /// Over every file, listed or not.
    pub added: u64,
    pub removed: u64,
    /// The lists were cut at `LISTED`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSummary {
    /// As git prints it after `a/`, C-quoted (quotes kept) when git quotes it.
    pub path: String,
    pub added: u64,
    pub removed: u64,
    /// `[hash, lines]` per block.
    pub hunks: Vec<(String, u32)>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The commit checked out in `dir` — against `gitdir` when there is one —
/// or `None` outside a repository, in one with no commit, or when git
/// cannot say.
pub fn head(dirs: &Dirs, dir: &Path, gitdir: Option<&Path>, roots: &[&Path]) -> Option<Commit> {
    let mut roots = roots.to_vec();
    roots.push(dir);
    let git = spawn::system_tool("git", &roots).ok()?;
    let mut args = placement::quiet_git_args(dirs).ok()?;
    if let Some(gitdir) = gitdir {
        let mut git_dir = OsString::from("--git-dir=");
        git_dir.push(gitdir);
        args.push(git_dir);
    }
    args.extend(
        [
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            "HEAD^{commit}",
        ]
        .map(OsString::from),
    );
    let output = spawn::run_helper_with_env(
        &git,
        &args,
        Some(dir),
        HEAD_DEADLINE,
        spawn::helper_path(&roots),
        &[],
    )
    .ok()?;
    if output.status != Some(0) {
        return None;
    }
    Commit::parse(output.stdout.trim())
}

/// A fork writer's whole change since `base`: what it committed, what it
/// left uncommitted, and the files it added, as one patch that applies with
/// `git apply` at `base`. All of it or nothing: a step that fails, output
/// past `PATCH_CAP` in all — the list of untracked files and the links
/// written here included — or a capture past `CAPTURE_DEADLINE` is an `Err`.
pub fn capture(
    dirs: &Dirs,
    worktree: &Path,
    gitdir: &Path,
    base: &Commit,
    roots: &[&Path],
) -> Res<Vec<u8>> {
    let mut budget = Budget::new(CAPTURE_DEADLINE, PATCH_CAP);
    let mut roots = roots.to_vec();
    roots.push(worktree);
    let git = spawn::system_tool("git", &roots)?;
    let path = spawn::helper_path(&roots);
    let mut prefix = placement::quiet_git_args(dirs)?;
    let (mut git_dir, mut work_tree) =
        (OsString::from("--git-dir="), OsString::from("--work-tree="));
    git_dir.push(gitdir);
    work_tree.push(worktree);
    prefix.extend([git_dir, work_tree]);

    // One step: its argv after the prefix, and the status that is success.
    // It gets the time and the bytes that are left, and spends them.
    let step =
        |budget: &mut Budget, what: &str, rest: &[&OsStr], ok: i32| -> Result<Vec<u8>, String> {
            let left = budget.time_left()?;
            let mut args = prefix.clone();
            args.extend(rest.iter().map(|arg| arg.to_os_string()));
            let (output, overflowed) = spawn::run_helper_capped(
                &git,
                &args,
                Some(worktree),
                left,
                path.clone(),
                &[],
                budget.bytes,
            )
            .map_err(|fail| format!("{what}: {}", fail.message))?;
            if overflowed {
                return Err(format!("{what}: the patch is past {PATCH_CAP} bytes"));
            }
            // An answer that comes after the deadline is not taken.
            budget.time_left()?;
            if output.status != Some(ok) {
                return Err(format!(
                    "{what} exited with {}",
                    output
                        .status
                        .map_or("a signal".to_string(), |code| code.to_string())
                ));
            }
            budget.spend(output.bytes.len())?;
            Ok(output.bytes)
        };
    let diff: Vec<&OsStr> = DIFF.iter().map(OsStr::new).collect();

    let run = |budget: &mut Budget| -> Result<Vec<u8>, String> {
        // What the tracked diff may read — every path in the index, listed
        // from the index alone, without a worktree file opened — looked at
        // before any git reads one: none of it a hard link, and all of it
        // the same files once git has read them.
        let indexed = step(
            budget,
            "git ls-files --cached",
            &["ls-files", "-z", "--cached"].map(OsStr::new),
            0,
        )?;
        let watched = watch(&indexed, worktree, budget)?;
        let mut tracked = diff.clone();
        tracked.extend(["--end-of-options", base.as_str(), "--"].map(OsStr::new));
        let mut patch = step(budget, "git diff", &tracked, 0)?;
        unchanged(&watched, worktree, budget)?;
        let listing = step(
            budget,
            "git ls-files",
            &[
                "-c",
                "core.quotePath=true",
                "ls-files",
                "-z",
                "--others",
                "--exclude-standard",
            ]
            .map(OsStr::new),
            0,
        )?;
        patch.extend(untracked(&listing, worktree, budget, |budget, name| {
            let mut untracked = diff.clone();
            untracked.extend(["--no-index", "--", "/dev/null"].map(OsStr::new));
            untracked.push(name.as_os_str());
            // `--no-index` implies `--exit-code`: 1 is "they differ".
            step(budget, "git diff --no-index", &untracked, 1)
        })?);
        unchanged(&watched, worktree, budget)?;
        budget.time_left()?;
        Ok(patch)
    };
    run(&mut budget).map_err(|why| Fail::new(Exit::Internal, why))
}

/// The time and the bytes a capture has left, from one deadline and one cap
/// for all of its steps.
struct Budget {
    started: Instant,
    time: Duration,
    bytes: usize,
}

impl Budget {
    fn new(time: Duration, bytes: usize) -> Budget {
        Budget {
            started: Instant::now(),
            time,
            bytes,
        }
    }

    /// What is left of the time, or why there is none.
    fn time_left(&self) -> Result<Duration, String> {
        self.time
            .checked_sub(self.started.elapsed())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| format!("the capture took longer than {}s", self.time.as_secs()))
    }

    fn spend(&mut self, bytes: usize) -> Result<(), String> {
        self.bytes = self
            .bytes
            .checked_sub(bytes)
            .ok_or_else(|| format!("the patch is past {PATCH_CAP} bytes"))?;
        Ok(())
    }
}

/// The creation of every untracked file in `listing` (`ls-files -z`), in
/// its order. A file is git's (`diff_file`), read only if it is not a hard
/// link and is the same file after git has read it; a link is written here,
/// as a link, from `read_link`: `git diff --no-index` takes a link to a
/// directory for the directory, and diffs what is in it. One ending in `/`
/// is a repository of its own, nested in the tree, and is left out.
fn untracked(
    listing: &[u8],
    worktree: &Path,
    budget: &mut Budget,
    mut diff_file: impl FnMut(&mut Budget, &Path) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    let mut patch = Vec::new();
    for entry in listing
        .split(|&byte| byte == 0)
        .filter(|entry| !entry.is_empty() && !entry.ends_with(b"/"))
    {
        budget.time_left()?;
        let name = Path::new(OsStr::from_bytes(entry));
        let path = worktree.join(name);
        let before = identity(&path)?;
        if before.is_some_and(|before| before.kind == KIND_LINK) {
            let target = std::fs::read_link(&path)
                .map_err(|error| format!("cannot read an untracked link: {error}"))?;
            let link = new_link(entry, target.as_os_str().as_bytes());
            budget.spend(link.len())?;
            patch.extend(link);
        } else {
            let bytes = diff_file(budget, name)?;
            unchanged(&[(entry, before)], worktree, budget)?;
            patch.extend(bytes);
        }
    }
    Ok(patch)
}

/// What a path in the worktree is, by `lstat`, never by what a link names:
/// its device, inode, type, link count and size, and when its inode and its
/// content last changed (`ctime`, `mtime`, to the nanosecond). A file is the
/// same, unchanged file while all of these are. Device and inode alone are
/// not enough: Linux hands a freed inode number straight to the next file
/// made, so a replacement can carry the one it replaced. Any replacement or
/// write sets the new inode's `ctime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    kind: u32,
    nlink: u64,
    size: u64,
    ctime: (i64, i64),
    mtime: (i64, i64),
}

const KIND_LINK: u32 = 0o120000;

/// `path`'s identity, `None` when there is nothing there — or why it may not
/// be read. A regular file with more than one hard link may be another
/// file's, from outside the worktree, linked in: git would read its content
/// into the patch, outside the writer's sandbox. `git worktree add` never
/// makes one.
fn identity(path: &Path) -> Result<Option<Identity>, String> {
    let meta = match path.symlink_metadata() {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot look at {}: {error}", path.display())),
    };
    if meta.file_type().is_file() && meta.nlink() > 1 {
        return Err(format!(
            "{} has more than one hard link, so it is not read",
            path.display()
        ));
    }
    Ok(Some(Identity {
        dev: meta.dev(),
        ino: meta.ino(),
        kind: meta.mode() & 0o170000,
        nlink: meta.nlink(),
        size: meta.size(),
        ctime: (meta.ctime(), meta.ctime_nsec()),
        mtime: (meta.mtime(), meta.mtime_nsec()),
    }))
}

/// A file in the worktree by its name there, and what it was.
type Watched<'a> = (&'a [u8], Option<Identity>);

/// The identity of every path in `listing` (NUL-separated, as git lists
/// them), within the capture's time: a large tree is looked at against the
/// same deadline as everything else.
fn watch<'a>(
    listing: &'a [u8],
    worktree: &Path,
    budget: &Budget,
) -> Result<Vec<Watched<'a>>, String> {
    listing
        .split(|&byte| byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| {
            scanning(budget)?;
            identity(&worktree.join(OsStr::from_bytes(name))).map(|before| (name, before))
        })
        .collect()
}

/// Every one of `watched` is still what it was before git read it — looked
/// at within the capture's time.
fn unchanged(watched: &[Watched], worktree: &Path, budget: &Budget) -> Result<(), String> {
    for (name, before) in watched {
        scanning(budget)?;
        let path = worktree.join(OsStr::from_bytes(name));
        if identity(&path)? != *before {
            return Err(format!(
                "{} changed while its patch was read",
                path.display()
            ));
        }
    }
    Ok(())
}

/// The time check of a scan of the tree, which says where it ran out.
fn scanning(budget: &Budget) -> Result<(), String> {
    budget
        .time_left()
        .map(|_| ())
        .map_err(|why| format!("{why}, looking at the tree's files"))
}

/// The creation of a link `name` → `target`, in git's form for one — less
/// the `index` line, whose blob id only git could compute, and which `git
/// apply` does not need.
fn new_link(name: &[u8], target: &[u8]) -> Vec<u8> {
    let (a, b) = (side(b"a/", name), side(b"b/", name));
    let (body, ends_in_newline) = match target.strip_suffix(b"\n") {
        Some(body) => (body, true),
        None => (target, false),
    };
    let lines: Vec<&[u8]> = body.split(|&byte| byte == b'\n').collect();
    let mut out = Vec::new();
    for part in [b"diff --git ".as_slice(), &a, b" ", &b, b"\n"] {
        out.extend_from_slice(part);
    }
    out.extend_from_slice(b"new file mode 120000\n--- /dev/null\n+++ ");
    out.extend_from_slice(&b);
    out.extend_from_slice(
        match lines.len() {
            1 => b"\n@@ -0,0 +1 @@\n".to_vec(),
            n => format!("\n@@ -0,0 +1,{n} @@\n").into_bytes(),
        }
        .as_slice(),
    );
    for line in &lines {
        out.push(b'+');
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    if !ends_in_newline {
        out.extend_from_slice(b"\\ No newline at end of file\n");
    }
    out
}

/// `prefix` + `name`, C-quoted as git quotes a path with `core.quotePath`.
fn side(prefix: &[u8], name: &[u8]) -> Vec<u8> {
    let plain = |byte: u8| (0x20..0x7f).contains(&byte) && byte != b'"' && byte != b'\\';
    let mut path = prefix.to_vec();
    path.extend_from_slice(name);
    if path.iter().all(|&byte| plain(byte)) {
        return path;
    }
    let mut out = vec![b'"'];
    for byte in path {
        match byte {
            0x07 => out.extend_from_slice(b"\\a"),
            0x08 => out.extend_from_slice(b"\\b"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            0x0b => out.extend_from_slice(b"\\v"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            byte if plain(byte) => out.push(byte),
            byte => out.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
        }
    }
    out.push(b'"');
    out
}

/// A changed block's hash: FNV-1a 64 of its lines, each as it is in the
/// patch, marker included, followed by `\n`; 16 lowercase hex digits.
pub fn block_hash(block: &[u8]) -> String {
    format!("{:016x}", crate::history::fnv1a64(block))
}

/// The summary of a patch. Pure. The golden vectors in `the_hash_is_frozen`
/// and the block tests below pin it, and #37 depends on it: a change to what
/// it yields is a change to the history.
///
/// A block is a maximal run of `-`/`+` lines inside one hunk — what `-U0`
/// would call a hunk — so it does not depend on the context width. Lines
/// before the first `diff --git ` belong to no file and are not read.
pub fn summarize(patch: &[u8]) -> PatchSummary {
    walk(patch).summary
}

/// One changed block of a patch: the file it is in, as `summarize` lists
/// it, and its hash and line count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub path: String,
    pub hash: String,
    pub lines: u32,
}

/// Every block of a patch, in its order, none left out: what `summarize`
/// lists, before its lists are cut at `LISTED`. Pure.
pub fn blocks(patch: &[u8]) -> Vec<Block> {
    walk(patch).blocks
}

/// The one reading of a patch that `summarize` and `blocks` share.
fn walk(patch: &[u8]) -> Summing {
    let mut sum = Summing::default();
    let body = patch.strip_suffix(b"\n").unwrap_or(patch);
    for line in body
        .split(|&byte| byte == b'\n')
        .filter(|_| !patch.is_empty())
    {
        if let Some(rest) = line.strip_prefix(b"diff --git ") {
            sum.end_file();
            sum.file = Some(FileSummary {
                path: header_path(rest),
                added: 0,
                removed: 0,
                hunks: Vec::new(),
            });
            sum.in_hunk = false;
            sum.binary = false;
            continue;
        }
        if sum.binary || sum.file.is_none() {
            continue;
        }
        if line.starts_with(b"GIT binary patch") {
            sum.close_block();
            sum.binary = true;
            sum.in_hunk = false;
            continue;
        }
        if line.starts_with(b"@@") {
            sum.close_block();
            sum.in_hunk = true;
            continue;
        }
        if !sum.in_hunk {
            continue;
        }
        match line.first() {
            Some(b'+') => sum.change(line, true),
            Some(b'-') => sum.change(line, false),
            Some(b'\\') => {}
            _ => sum.close_block(),
        }
    }
    sum.end_file();
    sum
}

/// What `summarize` carries from line to line: the summary so far, and the
/// file and block it is in.
#[derive(Default)]
struct Summing {
    summary: PatchSummary,
    file: Option<FileSummary>,
    in_hunk: bool,
    binary: bool,
    block: Vec<u8>,
    block_lines: u32,
    listed_hunks: usize,
    /// Every block, listed or not.
    blocks: Vec<Block>,
}

impl Summing {
    fn change(&mut self, line: &[u8], added: bool) {
        let Some(file) = &mut self.file else {
            return;
        };
        let (in_file, in_all) = match added {
            true => (&mut file.added, &mut self.summary.added),
            false => (&mut file.removed, &mut self.summary.removed),
        };
        *in_file += 1;
        *in_all += 1;
        self.block.extend_from_slice(line);
        self.block.push(b'\n');
        self.block_lines += 1;
    }

    /// Ends the open block, if there is one, and lists it if there is room.
    fn close_block(&mut self) {
        if self.block_lines == 0 {
            return;
        }
        let pair = (block_hash(&self.block), self.block_lines);
        self.block.clear();
        self.block_lines = 0;
        if let Some(file) = &self.file {
            self.blocks.push(Block {
                path: file.path.clone(),
                hash: pair.0.clone(),
                lines: pair.1,
            });
        }
        match &mut self.file {
            Some(file) if self.listed_hunks < LISTED => {
                file.hunks.push(pair);
                self.listed_hunks += 1;
            }
            _ => self.summary.truncated = true,
        }
    }

    /// Ends the current file, and lists it if there is room. Its lines are
    /// in the totals either way.
    fn end_file(&mut self) {
        self.close_block();
        if let Some(file) = self.file.take() {
            if self.summary.files.len() < LISTED {
                self.summary.files.push(file);
            } else {
                self.summary.truncated = true;
            }
        }
    }
}

/// The path in a `diff --git ` header, as git printed it, without `a/`.
/// Quoted: the first C-quoted token, the `a/` taken from inside it, quotes
/// and escapes kept. Plain: `a/P b/P` — the same `P` twice (no renames), so
/// its length gives it away even when `P` has spaces.
fn header_path(rest: &[u8]) -> String {
    if rest.first() == Some(&b'"') {
        let mut end = None;
        let mut at = 1;
        while at < rest.len() {
            match rest[at] {
                b'\\' => at += 2,
                b'"' => {
                    end = Some(at);
                    break;
                }
                _ => at += 1,
            }
        }
        let token = &rest[..=end.unwrap_or(rest.len() - 1)];
        let inner = token.strip_prefix(b"\"a/").unwrap_or(&token[1..]);
        let mut path = b"\"".to_vec();
        path.extend_from_slice(inner);
        return String::from_utf8_lossy(&path).into_owned();
    }
    let plain = if rest.len() >= 5 {
        &rest[2..2 + (rest.len() - 5) / 2]
    } else {
        rest
    };
    String::from_utf8_lossy(plain).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunks(summary: &PatchSummary) -> Vec<(String, u32)> {
        summary
            .files
            .iter()
            .flat_map(|file| file.hunks.clone())
            .collect()
    }

    #[test]
    fn the_hash_is_frozen() {
        for (block, hash) in [
            (b"".as_slice(), "cbf29ce484222325"),
            (b"-old line\n+new line\n", "c45902d4cdcd0514"),
            (b"+}\n", "11d27d17fb00834d"),
            (b"+caf\xe9\n", "a74982ef942c363b"),
        ] {
            assert_eq!(block_hash(block), hash, "{block:?}");
        }
    }

    #[test]
    fn a_block_is_a_run_of_changes_and_context_splits_it() {
        let patch = b"diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n-a\n+b\n c\n-d\n";
        let summary = summarize(patch);
        assert_eq!(
            hunks(&summary),
            vec![(block_hash(b"-a\n+b\n"), 2), (block_hash(b"-d\n"), 1)]
        );
        assert_eq!((summary.files[0].added, summary.files[0].removed), (1, 2));

        // `\ No newline` neither splits a block nor enters its hash.
        let patch = b"diff --git a/f b/f\n@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n\\ No newline at end of file\n";
        assert_eq!(hunks(&summarize(patch)), vec![(block_hash(b"-a\n+b\n"), 2)]);

        // An empty line in a hunk is context, as a blank context line is
        // printed with `diff.suppressBlankEmpty`.
        let patch = b"diff --git a/f b/f\n@@ -1,3 +1,3 @@\n-a\n\n+b\n";
        assert_eq!(
            hunks(&summarize(patch)),
            vec![(block_hash(b"-a\n"), 1), (block_hash(b"+b\n"), 1)]
        );

        // A CRLF line keeps its `\r`.
        let patch = b"diff --git a/f b/f\n@@ -0,0 +1 @@\n+x\r\n";
        assert_eq!(hunks(&summarize(patch)), vec![(block_hash(b"+x\r\n"), 1)]);

        // In a hunk, a removed `-- x` is `--- x`: a removal, not a header.
        let patch = b"diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n--- x\n+++ y\n";
        let summary = summarize(patch);
        assert_eq!(hunks(&summary), vec![(block_hash(b"--- x\n+++ y\n"), 2)]);
        assert_eq!((summary.added, summary.removed), (1, 1));
    }

    #[test]
    fn the_context_width_does_not_change_the_blocks() {
        let u0 =
            b"diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -2 +2 @@\n-b\n+B\n@@ -6 +6 @@\n-f\n+F\n";
        let u3 = b"diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,9 +1,9 @@\n a\n-b\n+B\n c\n d\n e\n-f\n+F\n g\n h\n i\n";
        assert_eq!(hunks(&summarize(u0)), hunks(&summarize(u3)));
        assert_eq!(hunks(&summarize(u0)).len(), 2);
    }

    #[test]
    fn paths_are_read_from_the_header() {
        let patch = concat!(
            "diff --git a/src/a.rs b/src/a.rs\n@@ -0,0 +1 @@\n+x\n",
            "diff --git a/with space/a b.txt b/with space/a b.txt\n@@ -0,0 +1 @@\n+x\n",
            "diff --git \"a/caf\\303\\251.txt\" \"b/caf\\303\\251.txt\"\n@@ -0,0 +1 @@\n+x\n",
            "diff --git \"a/q\\\"uote.txt\" \"b/q\\\"uote.txt\"\n@@ -0,0 +1 @@\n+x\n",
            "diff --git a/logo.png b/logo.png\nnew file mode 100644\nindex 0000000..1111111\nGIT binary patch\nliteral 3\n@@KcmZ>\n\nliteral 0\n",
            "diff --git a/b b/b\n@@ -0,0 +1 @@\n+y\n",
        );
        let summary = summarize(patch.as_bytes());
        let paths: Vec<&str> = summary.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "src/a.rs",
                "with space/a b.txt",
                "\"caf\\303\\251.txt\"",
                "\"q\\\"uote.txt\"",
                "logo.png",
                "b"
            ]
        );
        let binary = &summary.files[4];
        assert_eq!((binary.added, binary.removed), (0, 0));
        assert!(binary.hunks.is_empty(), "{binary:?}");
        assert_eq!(summary.files[5].hunks, vec![(block_hash(b"+y\n"), 1)]);
        assert_eq!(summary.added, 5);
    }

    #[test]
    fn a_long_patch_lists_at_most_1000_hunks() {
        let mut patch = String::new();
        for (file, blocks) in [("one", 600), ("two", 401)] {
            patch.push_str(&format!("diff --git a/{file} b/{file}\n"));
            for n in 0..blocks {
                patch.push_str(&format!("@@ -{n} +{n} @@\n-{file} {n}\n+{file} {n}!\n"));
            }
        }
        let summary = summarize(patch.as_bytes());
        assert_eq!(hunks(&summary).len(), 1000);
        assert!(summary.truncated);
        assert_eq!((summary.added, summary.removed), (1001, 1001));
        assert_eq!(summary.files[1].hunks.len(), 400);
        assert_eq!(
            (summary.files[1].added, summary.files[1].removed),
            (401, 401)
        );
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["truncated"], true);
        let short = serde_json::to_value(summarize(b"")).unwrap();
        assert_eq!(
            short,
            serde_json::json!({"files": [], "added": 0, "removed": 0})
        );
    }

    #[test]
    fn blocks_are_what_the_summary_lists_and_none_is_left_out() {
        let patch = concat!(
            "diff --git a/f b/f\n@@ -1,3 +1,3 @@\n-a\n+b\n c\n-d\n",
            "diff --git a/logo.png b/logo.png\nGIT binary patch\nliteral 3\n",
            "diff --git \"a/caf\\303\\251\" \"b/caf\\303\\251\"\n@@ -0,0 +1 @@\n+x\n",
        );
        let summary = summarize(patch.as_bytes());
        let listed: Vec<(String, String, u32)> = summary
            .files
            .iter()
            .flat_map(|file| {
                file.hunks
                    .iter()
                    .map(|(hash, lines)| (file.path.clone(), hash.clone(), *lines))
            })
            .collect();
        let all: Vec<(String, String, u32)> = blocks(patch.as_bytes())
            .into_iter()
            .map(|block| (block.path, block.hash, block.lines))
            .collect();
        assert_eq!(all, listed);
        assert_eq!(all.len(), 3);

        // Past `LISTED`, the summary stops listing; `blocks` does not.
        let mut long = String::new();
        for n in 0..1001 {
            long.push_str(&format!("diff --git a/f{n} b/f{n}\n@@ -0,0 +1 @@\n+x\n"));
        }
        assert_eq!(blocks(long.as_bytes()).len(), 1001);
        assert!(blocks(b"").is_empty());
    }

    #[test]
    fn a_long_patch_lists_at_most_1000_files() {
        let mut patch = String::new();
        for n in 0..1002 {
            patch.push_str(&format!("diff --git a/f{n} b/f{n}\n@@ -0,0 +1 @@\n+x\n"));
        }
        let summary = summarize(patch.as_bytes());
        assert_eq!(summary.files.len(), 1000);
        assert!(summary.truncated);
        assert_eq!(summary.added, 1002);
    }

    #[test]
    fn a_link_is_written_as_git_writes_one() {
        let link = new_link(b"leak", b"/home/x/secret");
        assert_eq!(
            String::from_utf8(link.clone()).unwrap(),
            "diff --git a/leak b/leak\nnew file mode 120000\n--- /dev/null\n+++ b/leak\n\
             @@ -0,0 +1 @@\n+/home/x/secret\n\\ No newline at end of file\n"
        );
        let summary = summarize(&link);
        assert_eq!(summary.files[0].path, "leak");
        assert_eq!((summary.added, summary.removed), (1, 0));
        let quoted = new_link("caf\u{e9}".as_bytes(), b"t");
        assert!(
            quoted.starts_with(b"diff --git \"a/caf\\303\\251\" \"b/caf\\303\\251\"\n"),
            "{}",
            String::from_utf8_lossy(&quoted)
        );
        assert_eq!(summarize(&quoted).files[0].path, "\"caf\\303\\251\"");
    }

    #[test]
    fn lines_outside_a_file_are_not_read() {
        // Not git's output, but `summarize` is for anyone's patch: a hunk
        // with no `diff --git` before it belongs to no file.
        assert_eq!(
            summarize(b"@@ -0,0 +1 @@\n+x\n-y\n"),
            PatchSummary::default()
        );
        assert_eq!(summarize(b"\n"), PatchSummary::default());
    }

    #[test]
    fn one_budget_covers_every_step() {
        // Steps that each fit can be past the cap together.
        let mut budget = Budget::new(Duration::from_secs(8), 10);
        assert!(budget.spend(6).is_ok());
        assert!(budget.spend(5).is_err());
        // Exactly the cap is within it.
        let mut budget = Budget::new(Duration::from_secs(8), 10);
        assert!(budget.spend(4).is_ok() && budget.spend(6).is_ok());
        assert!(budget.spend(1).is_err());
        assert!(Budget::new(Duration::ZERO, 10).time_left().is_err());
    }

    /// A directory with a regular file `f` and links `a` and `b`.
    fn links() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), "f\n").unwrap();
        std::os::unix::fs::symlink("/somewhere/a", dir.path().join("a")).unwrap();
        std::os::unix::fs::symlink("/somewhere", dir.path().join("b")).unwrap();
        dir
    }

    #[test]
    fn links_spend_the_bytes_that_are_left() {
        let dir = links();
        let listing = b"a\0b\0nested/\0";
        let both = [
            new_link(b"a", b"/somewhere/a"),
            new_link(b"b", b"/somewhere"),
        ]
        .concat();
        let no_file =
            |_: &mut Budget, _: &Path| -> Result<Vec<u8>, String> { panic!("a link went to git") };
        let mut exact = Budget::new(Duration::from_secs(8), both.len());
        assert_eq!(
            untracked(listing, dir.path(), &mut exact, no_file).unwrap(),
            both
        );
        assert_eq!(exact.bytes, 0);
        let mut short = Budget::new(Duration::from_secs(8), both.len() - 1);
        assert!(untracked(listing, dir.path(), &mut short, no_file).is_err());
    }

    #[test]
    fn links_are_not_written_once_the_time_is_up() {
        let dir = links();
        // The file before the links takes the rest of the time: no link is
        // written after it.
        let mut budget = Budget::new(Duration::from_millis(50), PATCH_CAP);
        let slow_file = |_: &mut Budget, _: &Path| -> Result<Vec<u8>, String> {
            std::thread::sleep(Duration::from_millis(100));
            Ok(Vec::new())
        };
        let late = untracked(b"f\0a\0b\0", dir.path(), &mut budget, slow_file).unwrap_err();
        assert!(late.contains("took longer"), "{late}");
        let mut none_left = Budget::new(Duration::ZERO, PATCH_CAP);
        assert!(untracked(b"a\0", dir.path(), &mut none_left, slow_file).is_err());
    }

    #[test]
    fn a_hard_link_is_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        std::fs::write(dir.path().join("secret"), "TOP-SECRET\n").unwrap();
        std::fs::hard_link(dir.path().join("secret"), tree.join("leak")).unwrap();
        let mut budget = Budget::new(Duration::from_secs(8), PATCH_CAP);
        let refused = untracked(b"leak\0", &tree, &mut budget, |_, _| {
            panic!("a hard link went to git")
        })
        .unwrap_err();
        assert!(refused.contains("more than one hard link"), "{refused}");
    }

    #[test]
    fn a_file_swapped_while_git_reads_it_fails_the_capture() {
        // Whatever takes the file's place while git reads it — a link, a
        // new file of another size or of the same size, or the same file
        // rewritten — is told apart, whether or not it gets the old inode
        // number back. The pause first puts the change past the coarsest
        // clock a file system stamps a file with.
        type Swap = fn(&Path);
        let swaps: [(&str, Swap); 4] = [
            ("a link", |path| {
                std::fs::remove_file(path).unwrap();
                std::os::unix::fs::symlink("/somewhere", path).unwrap();
            }),
            ("a larger file", |path| {
                std::fs::remove_file(path).unwrap();
                std::fs::write(path, "another file\n").unwrap();
            }),
            ("a file of the same size", |path| {
                std::fs::remove_file(path).unwrap();
                std::fs::write(path, "g\n").unwrap();
            }),
            ("the same file, rewritten", |path| {
                std::fs::write(path, "g\n").unwrap();
            }),
        ];
        for (what, swap) in swaps {
            let dir = links();
            let path = dir.path().join("f");
            let mut budget = Budget::new(Duration::from_secs(8), PATCH_CAP);
            let swapped = untracked(b"f\0", dir.path(), &mut budget, |_, _| {
                std::thread::sleep(Duration::from_millis(50));
                swap(&path);
                Ok(b"what git read".to_vec())
            })
            .map(|_| ())
            .expect_err(what);
            assert!(swapped.contains("changed while"), "{what}: {swapped}");
        }
    }

    #[test]
    fn looking_at_the_tree_stops_when_the_time_is_up() {
        let dir = links();
        let none_left = Budget::new(Duration::ZERO, PATCH_CAP);
        let late = watch(b"f\0a\0", dir.path(), &none_left).unwrap_err();
        assert!(late.contains("looking at the tree's files"), "{late}");
        let budget = Budget::new(Duration::from_secs(8), PATCH_CAP);
        let watched = watch(b"f\0a\0missing\0", dir.path(), &budget).unwrap();
        assert_eq!(watched.len(), 3);
        assert!(unchanged(&watched, dir.path(), &budget).is_ok());
        let late = unchanged(&watched, dir.path(), &none_left).unwrap_err();
        assert!(late.contains("looking at the tree's files"), "{late}");
        // Nothing to look at takes no time.
        assert!(watch(b"", dir.path(), &none_left).unwrap().is_empty());
    }

    #[test]
    fn a_commit_is_hex_of_the_right_length() {
        let sha1 = "0123456789abcdef0123456789abcdef01234567";
        let sha256 = "0123456789abcdef".repeat(4);
        assert!(Commit::parse(sha1).is_some());
        assert!(Commit::parse(&sha256).is_some());
        for bad in [
            "",
            "HEAD",
            "-x",
            &sha1[..39],
            &sha1.to_uppercase(),
            &format!("{sha1}\n"),
        ] {
            assert!(Commit::parse(bad).is_none(), "{bad:?}");
        }
        assert_eq!(
            serde_json::from_str::<Commit>(&format!("\"{sha1}\"")).unwrap(),
            Commit::parse(sha1).unwrap()
        );
        assert!(serde_json::from_str::<Commit>("\"--exec=x\"").is_err());
    }
}
