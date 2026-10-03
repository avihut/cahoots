//! `cahoots install` / `uninstall`: the skill and the agent definitions, per
//! harness. Install IS update. Every file cahoots writes carries a
//! `cahoots_version` stamp, and that stamp is the only thing that makes a file
//! cahoots' to overwrite or to remove — a file of the same name that a person
//! wrote is left alone, and said so.
//!
//! Besides the fixed files, each kind of task a person defined gets a
//! subagent of its own in each harness that could delegate it
//! (`cahoots-kind-<name>`). The kinds come from config.toml, so the set of
//! files changes with them: install writes the new ones, refreshes the
//! changed ones, and removes the ones no kind wants any more (`prune`).
//!
//! What this module never does: touch a harness's settings or permission
//! files. The rules are printed (`rules.rs`) for a person to add.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::dir::{Dir, Entry, entry_by_path, outside_home};
use crate::dirs::Dirs;
use crate::exit::{Fail, Res};
use crate::model::{HarnessId, Role, TaskKindName};
use crate::registry::KindEntry;
use crate::run::record::write_private;

const SKILL: &str = include_str!("assets/SKILL.md");
const REVIEW_SKILL: &str = include_str!("assets/REVIEW-SKILL.md");
const CLAUDE_AGENT: &str = include_str!("assets/claude-agent.md");
const CODEX_AGENT: &str = include_str!("assets/codex-agent.toml");
const CLAUDE_KIND_AGENT: &str = include_str!("assets/claude-kind-agent.md");
const CODEX_KIND_AGENT: &str = include_str!("assets/codex-kind-agent.toml");
/// A kind's subagent description: the person's words, then what it does.
const KIND_DESCRIPTION: &str = include_str!("assets/kind-description.txt");
const STAMP: &str = "cahoots_version";

fn render(template: &str) -> String {
    template.replace("{{version}}", env!("CARGO_PKG_VERSION"))
}

/// The agent home a harness's files go in.
const fn home_of(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::Claude => ".claude",
        HarnessId::Codex => ".codex",
    }
}

fn kind_agent_path(harness: HarnessId, name: &TaskKindName) -> String {
    match harness {
        HarnessId::Claude => format!(".claude/agents/cahoots-kind-{name}.md"),
        HarnessId::Codex => format!(".codex/agents/cahoots-kind-{name}.toml"),
    }
}

/// A kind's subagent for `harness`. The person's description is put in
/// LAST, quoted for the file's own syntax, so nothing in it is ever read as
/// a placeholder — and it goes in the description field and nowhere else.
fn render_kind(harness: HarnessId, name: &TaskKindName, entry: &KindEntry) -> String {
    let fork = if entry.role == Role::Implement {
        " --fork"
    } else {
        ""
    };
    let description = KIND_DESCRIPTION
        .trim_end()
        .replace("{{kind}}", name.as_str())
        .replace("{{description}}", &entry.description);
    let (template, quoted) = match harness {
        // A JSON string is a YAML double-quoted scalar.
        HarnessId::Claude => (
            CLAUDE_KIND_AGENT,
            serde_json::Value::String(description).to_string(),
        ),
        HarnessId::Codex => (
            CODEX_KIND_AGENT,
            toml::Value::String(description).to_string(),
        ),
    };
    // The role is written down even where the instructions don't show it,
    // so that changing it always changes the file, and doctor sees it.
    render(template)
        .replace("{{kind}}", name.as_str())
        .replace("{{role}}", entry.role.as_str())
        .replace("{{fork}}", fork)
        .replace("{{description}}", &quoted)
}

/// The skill, as installed by this version.
pub fn skill_text() -> String {
    render(SKILL)
}

struct Item {
    /// Relative to the user's home.
    path: &'static str,
    /// The agent home that must already exist for this file to be wanted.
    needs: &'static str,
    harness: Option<HarnessId>,
    template: &'static str,
}

/// The fixed files cahoots installs. The skill goes to the shared skills
/// location and to each harness's own; the agent definitions are per harness.
const ITEMS: [Item; 8] = [
    Item {
        path: ".agents/skills/cahoots/SKILL.md",
        needs: ".agents/skills",
        harness: None,
        template: SKILL,
    },
    Item {
        path: ".agents/skills/cahoots-review/SKILL.md",
        needs: ".agents/skills",
        harness: None,
        template: REVIEW_SKILL,
    },
    Item {
        path: ".claude/skills/cahoots-review/SKILL.md",
        needs: ".claude",
        harness: Some(HarnessId::Claude),
        template: REVIEW_SKILL,
    },
    Item {
        path: ".codex/skills/cahoots-review/SKILL.md",
        needs: ".codex",
        harness: Some(HarnessId::Codex),
        template: REVIEW_SKILL,
    },
    Item {
        path: ".claude/skills/cahoots/SKILL.md",
        needs: ".claude",
        harness: Some(HarnessId::Claude),
        template: SKILL,
    },
    Item {
        path: ".claude/agents/cahoots-delegate.md",
        needs: ".claude",
        harness: Some(HarnessId::Claude),
        template: CLAUDE_AGENT,
    },
    Item {
        path: ".codex/skills/cahoots/SKILL.md",
        needs: ".codex",
        harness: Some(HarnessId::Codex),
        template: SKILL,
    },
    Item {
        path: ".codex/agents/cahoots-delegate.toml",
        needs: ".codex",
        harness: Some(HarnessId::Codex),
        template: CODEX_AGENT,
    },
];

/// One file install wants with the current kinds — or a path it knows of and
/// does not write, and why. `install` and `stale` both read this one list,
/// so what doctor calls out of date is exactly what install would change.
struct Want {
    path: PathBuf,
    /// The agent home that must already exist for this file to be wanted.
    needs: &'static str,
    harness: Option<HarnessId>,
    /// A kind's subagent, not one of the fixed files.
    kind: bool,
    text: Result<String, String>,
}

fn wanted(dirs: &Dirs, kinds: &BTreeMap<TaskKindName, KindEntry>) -> Vec<Want> {
    let fixed = ITEMS.iter().map(|item| Want {
        path: dirs.home.join(item.path),
        needs: item.needs,
        harness: item.harness,
        kind: false,
        text: Ok(render(item.template)),
    });
    let per_kind = kinds.iter().flat_map(|(name, entry)| {
        HarnessId::ALL.into_iter().map(move |harness| Want {
            path: dirs.home.join(kind_agent_path(harness, name)),
            needs: home_of(harness),
            harness: Some(harness),
            kind: true,
            // `pick` always leaves the caller out, so a kind with no
            // candidate elsewhere could never be delegated from here.
            text: if entry.candidates.iter().all(|c| c.harness == harness) {
                Err(format!(
                    "every candidate of kind `{name}` is on {harness}, and a harness does not \
                     delegate to itself"
                ))
            } else {
                Ok(render_kind(harness, name, entry))
            },
        })
    });
    fixed.chain(per_kind).collect()
}

/// Whether a run limited to `only` touches a file of `harness` (`None`: a
/// shared file, which every run covers).
fn covers(only: Option<HarnessId>, harness: Option<HarnessId>) -> bool {
    only.is_none() || harness.is_none() || harness == only
}

/// Which harness's home a path is in, if any.
fn harness_of(dirs: &Dirs, path: &Path) -> Option<HarnessId> {
    HarnessId::ALL
        .into_iter()
        .find(|harness| path.starts_with(dirs.home.join(home_of(*harness))))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Outcome {
    Installed,
    Updated {
        from: String,
    },
    /// Same version, different content (edited by hand, or a dev build).
    Refreshed,
    UpToDate,
    Removed,
    /// Left alone, and why.
    Skipped {
        why: String,
    },
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub path: PathBuf,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    v: u32,
    files: Vec<PathBuf>,
}

fn manifest_path(dirs: &Dirs) -> PathBuf {
    dirs.state.join("install-manifest.json")
}

fn load_manifest(dirs: &Dirs) -> Manifest {
    fs::read_to_string(manifest_path(dirs))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The `cahoots_version` stamp, read only from a line that BEGINS with it —
/// the frontmatter key or the TOML comment. Anywhere else (a kind's
/// description, say) it is a person's text, and a file whose stamp line was
/// deleted is theirs even if its words still mention the stamp.
fn stamp_of(text: &str) -> Option<String> {
    let line = text.lines().find(|line| {
        line.starts_with(&format!("{STAMP}:")) || line.starts_with(&format!("# {STAMP} ="))
    })?;
    let mut quoted = line.split('"');
    quoted.next()?;
    quoted.next().map(str::to_string)
}

/// A file is only ever written inside the user's home — judged by where the
/// path RESOLVES, and judged BEFORE anything is created: an agent home that is
/// a symlink to somewhere else must not even get a directory made through it.
/// So the nearest ancestor that already exists is resolved first. The write
/// itself then happens in the directory held open (`Dir`), judged again.
fn confined(home: &Path, dir: &Path) -> Res<()> {
    match outside_home(home, dir)? {
        None => Ok(()),
        Some((existing, resolved)) => Err(Fail::policy(format!(
            "{} resolves to {}, outside your home — refusing to write there",
            existing.display(),
            resolved.display()
        ))),
    }
}

/// Writes what the current kinds want, then removes what they no longer do.
pub fn install(
    dirs: &Dirs,
    kinds: &BTreeMap<TaskKindName, KindEntry>,
    only: Option<HarnessId>,
    dry_run: bool,
) -> Res<Vec<Report>> {
    let mut manifest = load_manifest(dirs);
    let wants = wanted(dirs, kinds);
    let mut reports = Vec::new();
    for want in &wants {
        if !covers(only, want.harness) {
            continue;
        }
        if want.text.is_err() && manifest.files.contains(&want.path) {
            continue; // one it wrote before: `prune` says what becomes of it
        }
        let outcome = place(dirs, want, dry_run)?;
        let ours = !matches!(outcome, Outcome::Skipped { .. });
        if ours && !manifest.files.contains(&want.path) {
            manifest.files.push(want.path.clone());
        }
        reports.push(Report {
            path: want.path.clone(),
            outcome,
        });
    }
    reports.extend(prune(dirs, &mut manifest, &wants, only, dry_run)?);
    if !dry_run {
        save_manifest(dirs, &mut manifest)?;
    }
    Ok(reports)
}

fn place(dirs: &Dirs, want: &Want, dry_run: bool) -> Res<Outcome> {
    if !dirs.home.join(want.needs).is_dir() {
        return Ok(Outcome::Skipped {
            why: format!(
                "~/{} does not exist — that harness is not set up here",
                want.needs
            ),
        });
    }
    let wanted = match &want.text {
        Ok(text) => text,
        Err(why) => return Ok(Outcome::Skipped { why: why.clone() }),
    };
    let (parent, name) = split(&want.path);
    // The file is judged in the directory it will be written in, held open.
    // One that resolves outside the home is judged through its path — never
    // written: `confined` refuses that below.
    let mut held = None;
    let existing = if parent.is_dir() {
        match Dir::open_confined(&dirs.home, parent)? {
            Ok(dir) => {
                let entry = dir.entry(name)?;
                held = Some(dir);
                entry
            }
            Err(_) => entry_by_path(&want.path),
        }
    } else {
        Entry::Absent
    };
    let outcome = match existing {
        Entry::Absent => Outcome::Installed,
        Entry::NotRegular => {
            return Ok(Outcome::Skipped {
                why: NOT_REGULAR.to_string(),
            });
        }
        // Install's rule since it began: a file it cannot read is not one it
        // can tell is its own, so it is left alone as a person's.
        Entry::Unreadable(_) => {
            return Ok(Outcome::Skipped {
                why: NOT_STAMPED.to_string(),
            });
        }
        Entry::Regular(existing) => match judge(&existing, wanted) {
            Ok(outcome) => outcome,
            Err(done) => return Ok(done),
        },
    };
    if !dry_run {
        let dir = match held {
            Some(dir) => dir,
            None => {
                // BEFORE anything is created: see `confined`.
                confined(&dirs.home, parent)?;
                fs::create_dir_all(parent).map_err(|error| {
                    Fail::internal(format!("cannot create {}: {error}", parent.display()))
                })?;
                // And again now that it exists: nothing may have swapped it
                // meanwhile.
                Dir::open_confined(&dirs.home, parent)?
                    .map_err(|why| Fail::policy(format!("{why} — refusing to write there")))?
            }
        };
        if outcome != Outcome::Installed {
            dir.replace(name, wanted)?;
        } else if !dir.publish(name, wanted)? {
            return Ok(Outcome::Skipped {
                why: APPEARED.to_string(),
            });
        }
    }
    Ok(outcome)
}

/// A file that was not there when cahoots looked, and was by the time it
/// wrote: someone else's, and never overwritten.
const APPEARED: &str = "appeared while cahoots was writing it — left alone";

const NOT_REGULAR: &str = "exists and is not a regular file — left alone";
const NOT_STAMPED: &str = "exists and is not cahoots' (no cahoots_version stamp) — left alone";

/// What writing `wanted` over a regular file holding `existing` would be —
/// or, `Err`, what leaving it alone is.
fn judge(existing: &str, wanted: &str) -> Result<Outcome, Outcome> {
    match stamp_of(existing) {
        None => Err(Outcome::Skipped {
            why: NOT_STAMPED.to_string(),
        }),
        Some(_) if existing == wanted => Err(Outcome::UpToDate),
        Some(from) if from == env!("CARGO_PKG_VERSION") => Ok(Outcome::Refreshed),
        Some(from) => Ok(Outcome::Updated { from }),
    }
}

/// A file's directory and its name. Every path cahoots writes has both.
fn split(path: &Path) -> (&Path, &str) {
    let parent = path.parent().expect("an item path has a parent");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("an item path ends in a UTF-8 name");
    (parent, name)
}

/// The one place `install` removes anything: files it wrote that the current
/// kinds no longer want (a kind removed or renamed, or left with no candidate
/// on another harness). By uninstall's own rule — listed in the manifest,
/// still stamped — and only in the homes this run covers.
fn prune(
    dirs: &Dirs,
    manifest: &mut Manifest,
    wants: &[Want],
    only: Option<HarnessId>,
    dry_run: bool,
) -> Res<Vec<Report>> {
    let mut reports = Vec::new();
    let mut kept = Vec::new();
    for path in std::mem::take(&mut manifest.files) {
        if is_wanted(wants, &path) || !covers(only, harness_of(dirs, &path)) {
            kept.push(path);
            continue;
        }
        let outcome = remove_ours(&dirs.home, &path, dry_run)?;
        if still_listed(&outcome, dry_run) {
            kept.push(path.clone());
        }
        reports.push(Report { path, outcome });
    }
    manifest.files = kept;
    Ok(reports)
}

fn is_wanted(wants: &[Want], path: &Path) -> bool {
    wants
        .iter()
        .any(|want| want.text.is_ok() && want.path == path)
}

/// Removes one file the manifest lists, if it still carries the stamp and
/// its directory resolves inside the user's home — the same confinement a
/// write gets. Then its `cahoots` skill directory, if that is left empty.
/// Only a file that is not there any more is "already gone"; one that cannot
/// be read is left, and stays listed.
fn remove_ours(home: &Path, path: &Path, dry_run: bool) -> Res<Outcome> {
    if fs::symlink_metadata(path).is_err_and(|error| error.kind() == ErrorKind::NotFound) {
        return Ok(Outcome::Skipped {
            why: GONE.to_string(),
        });
    }
    let parent = path.parent().expect("an installed path has a parent");
    if let Some((existing, resolved)) = outside_home(home, parent)? {
        return Ok(Outcome::Skipped {
            why: format!(
                "{} resolves to {}, outside your home — left alone",
                existing.display(),
                resolved.display()
            ),
        });
    }
    Ok(match fs::read_to_string(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Outcome::Skipped {
            why: GONE.to_string(),
        },
        Err(error) => Outcome::Skipped {
            why: format!("{CANNOT_BE_READ} ({error}) — left alone"),
        },
        Ok(text) if stamp_of(&text).is_none() => Outcome::Skipped {
            why: "no longer carries the cahoots_version stamp — someone made it theirs; left alone"
                .to_string(),
        },
        Ok(_) => {
            if !dry_run {
                fs::remove_file(path).map_err(|error| {
                    Fail::internal(format!("cannot remove {}: {error}", path.display()))
                })?;
                if parent
                    .file_name()
                    .is_some_and(|name| name == "cahoots" || name == "cahoots-review")
                {
                    let _ = fs::remove_dir(parent); // only succeeds when empty
                }
            }
            Outcome::Removed
        }
    })
}

const GONE: &str = "already gone";
const CANNOT_BE_READ: &str = "cannot be read";

/// Whether the manifest still lists a file after `remove_ours` said this.
fn still_listed(outcome: &Outcome, dry_run: bool) -> bool {
    dry_run || matches!(outcome, Outcome::Skipped { why } if why != GONE)
}

/// Removes what `install` wrote: only files the manifest lists, and only if
/// they still carry the stamp. Then the `cahoots` skill directories, if empty.
pub fn uninstall(dirs: &Dirs, dry_run: bool) -> Res<Vec<Report>> {
    let mut manifest = load_manifest(dirs);
    let mut reports = Vec::new();
    let mut kept = Vec::new();
    for path in std::mem::take(&mut manifest.files) {
        let outcome = remove_ours(&dirs.home, &path, dry_run)?;
        if still_listed(&outcome, dry_run) {
            kept.push(path.clone());
        }
        reports.push(Report { path, outcome });
    }
    if !dry_run {
        manifest.files = kept;
        save_manifest(dirs, &mut manifest)?;
    }
    Ok(reports)
}

/// Whether `install` has written into `harness`'s home: the manifest lists a
/// file there.
fn installed_in(dirs: &Dirs, manifest: &Manifest, harness: HarnessId) -> bool {
    manifest
        .files
        .iter()
        .any(|path| harness_of(dirs, path) == Some(harness))
}

const NOTHING_INSTALLED: &str =
    "nothing has been installed here yet: run `cahoots install` from a terminal first";

/// The manifest, for `refresh`, which never guesses what `install` wrote:
/// none, an empty one, or one it cannot read is a person's to fix.
fn load_manifest_strict(dirs: &Dirs) -> Res<Manifest> {
    let path = manifest_path(dirs);
    let unreadable = |error: &dyn std::fmt::Display| {
        Fail::config(format!(
            "cannot read {}: {error} — run `cahoots install` from a terminal to write it again",
            path.display()
        ))
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(Fail::config(NOTHING_INSTALLED));
        }
        Err(error) => return Err(unreadable(&error)),
    };
    let manifest: Manifest = serde_json::from_str(&text).map_err(|error| unreadable(&error))?;
    if manifest.files.is_empty() {
        return Err(Fail::config(NOTHING_INSTALLED));
    }
    Ok(manifest)
}

/// The kind whose subagent `path` is — only when it is exactly the path
/// cahoots builds for that kind, one file directly in a harness's `agents`.
fn kind_of(dirs: &Dirs, path: &Path) -> Option<(HarnessId, TaskKindName)> {
    HarnessId::ALL.into_iter().find_map(|harness| {
        let rest = path
            .strip_prefix(dirs.home.join(home_of(harness)).join("agents"))
            .ok()?;
        let mut components = rest.components();
        let only = match (components.next(), components.next()) {
            (Some(std::path::Component::Normal(only)), None) => only.to_str()?,
            _ => return None,
        };
        let suffix = match harness {
            HarnessId::Claude => ".md",
            HarnessId::Codex => ".toml",
        };
        let name = only.strip_prefix("cahoots-kind-")?.strip_suffix(suffix)?;
        let name = TaskKindName::try_from(name.to_string()).ok()?;
        (dirs.home.join(kind_agent_path(harness, &name)) == path).then_some((harness, name))
    })
}

/// What `refresh` did: a report per file, and how many of them it could not
/// change.
#[derive(Debug)]
pub struct Refreshed {
    pub files: Vec<Report>,
    pub failed: usize,
}

const NOT_WRITTEN_BY_INSTALL: &str =
    "`cahoots install` did not write it — run it from a terminal to add it";
const GONE_NOT_WRITTEN_BACK: &str = "gone — `cahoots install`, from a terminal, writes it again";
const NOT_OURS_TO_ADD: &str = "exists, and `cahoots install` did not write it — left alone";
const NOT_WRITTEN_HERE: &str =
    "this version does not write it — `cahoots install`, from a terminal, removes it";

/// `cahoots refresh`: what `install`, from a terminal, would do to the files
/// it already wrote — and nothing more. It rewrites a file the manifest
/// lists, still stamped, from this binary's text. It adds a kind's subagent
/// only in a harness home the manifest already covers, and only where no
/// file is; it removes one only by install's own rule (`remove_ours`). It
/// never writes a file the manifest does not list otherwise, never one
/// without the stamp, never through a link, and never anywhere outside the
/// home. A file it cannot change is reported, and the rest still done.
pub fn refresh(dirs: &Dirs, kinds: &BTreeMap<TaskKindName, KindEntry>) -> Res<Refreshed> {
    let mut manifest = load_manifest_strict(dirs)?;
    let before = {
        let mut files = manifest.files.clone();
        files.sort();
        files
    };
    let wants = wanted(dirs, kinds);
    let mut done = Refreshed {
        files: Vec::new(),
        failed: 0,
    };
    let mut added = Vec::new();
    for want in &wants {
        let listed = manifest.files.contains(&want.path);
        if listed && want.text.is_err() {
            continue; // `prune`'s, below
        }
        let outcome = refresh_one(dirs, &manifest, want, listed).unwrap_or_else(|failed| {
            done.failed += 1;
            let (doing, fail) = match failed {
                Failed::Read(fail) => ("read", fail),
                Failed::Write(fail) => ("write", fail),
            };
            Outcome::Skipped {
                why: format!("cannot {doing}: {}", fail.message),
            }
        });
        if !listed && outcome == Outcome::Installed {
            added.push(want.path.clone());
        }
        done.files.push(Report {
            path: want.path.clone(),
            outcome,
        });
    }
    let mut kept = Vec::new();
    for path in std::mem::take(&mut manifest.files) {
        if is_wanted(&wants, &path) {
            kept.push(path);
            continue;
        }
        let outcome = match kind_of(dirs, &path) {
            Some(_) => match remove_ours(&dirs.home, &path, false) {
                Ok(outcome) => {
                    // Uninstall's words for a file it cannot read; a
                    // failure all the same.
                    if matches!(&outcome, Outcome::Skipped { why } if why.starts_with(CANNOT_BE_READ))
                    {
                        done.failed += 1;
                    }
                    outcome
                }
                Err(fail) => {
                    done.failed += 1;
                    Outcome::Skipped {
                        why: format!("cannot remove: {}", fail.message),
                    }
                }
            },
            None => Outcome::Skipped {
                why: NOT_WRITTEN_HERE.to_string(),
            },
        };
        if still_listed(&outcome, false) {
            kept.push(path.clone());
        }
        done.files.push(Report { path, outcome });
    }
    manifest.files = kept;
    manifest.files.extend(added);
    manifest.files.sort();
    if manifest.files != before {
        save_manifest(dirs, &mut manifest)?;
    }
    Ok(done)
}

/// What `refresh` could not do to one file: read it, or write it.
enum Failed {
    Read(Fail),
    Write(Fail),
}

impl From<Fail> for Failed {
    fn from(fail: Fail) -> Self {
        Failed::Write(fail)
    }
}

/// One wanted file, for `refresh` (see its rules there).
fn refresh_one(
    dirs: &Dirs,
    manifest: &Manifest,
    want: &Want,
    listed: bool,
) -> Result<Outcome, Failed> {
    let skipped = |why: &str| {
        Ok(Outcome::Skipped {
            why: why.to_string(),
        })
    };
    if !dirs.home.join(want.needs).is_dir() {
        return skipped(&format!(
            "~/{} does not exist — that harness is not set up here",
            want.needs
        ));
    }
    let (parent, name) = split(&want.path);
    if listed {
        let Ok(wanted) = &want.text else {
            unreachable!("a listed file with no text is prune's")
        };
        if fs::symlink_metadata(parent).is_err() {
            return skipped(GONE_NOT_WRITTEN_BACK);
        }
        let dir = match Dir::open_confined(&dirs.home, parent)? {
            Ok(dir) => dir,
            Err(why) => return skipped(&format!("{why} — left alone")),
        };
        return Ok(match dir.entry(name).map_err(Failed::Read)? {
            Entry::Unreadable(why) => return Err(Failed::Read(Fail::internal(why))),
            Entry::Absent => Outcome::Skipped {
                why: GONE_NOT_WRITTEN_BACK.to_string(),
            },
            Entry::NotRegular => Outcome::Skipped {
                why: NOT_REGULAR.to_string(),
            },
            Entry::Regular(existing) => match judge(&existing, wanted) {
                Err(done) => done,
                Ok(outcome) => {
                    dir.replace(name, wanted)?;
                    outcome
                }
            },
        });
    }
    let Some(harness) = want.harness.filter(|_| want.kind) else {
        return skipped(NOT_WRITTEN_BY_INSTALL);
    };
    if !installed_in(dirs, manifest, harness) {
        return skipped(&format!(
            "`cahoots install` has not written into ~/{} — run it there from a terminal first",
            home_of(harness)
        ));
    }
    let wanted = match &want.text {
        Ok(text) => text,
        Err(why) => return skipped(why),
    };
    if let Some((existing, resolved)) = outside_home(&dirs.home, parent)? {
        return skipped(&format!(
            "{} resolves to {}, outside your home — left alone",
            existing.display(),
            resolved.display()
        ));
    }
    fs::create_dir_all(parent)
        .map_err(|error| Fail::internal(format!("cannot create {}: {error}", parent.display())))?;
    let dir = match Dir::open_confined(&dirs.home, parent)? {
        Ok(dir) => dir,
        Err(why) => return skipped(&format!("{why} — left alone")),
    };
    // Something there — or something that appears before the file is
    // published, which `publish` will not replace — is not cahoots' to add.
    if dir.entry(name).map_err(Failed::Read)? != Entry::Absent || !dir.publish(name, wanted)? {
        return skipped(NOT_OURS_TO_ADD);
    }
    Ok(Outcome::Installed)
}

fn save_manifest(dirs: &Dirs, manifest: &mut Manifest) -> Res<()> {
    manifest.v = 1;
    manifest.files.sort();
    crate::dirs::ensure_private_dir(&dirs.state)?;
    let json = serde_json::to_vec_pretty(manifest)
        .map_err(|error| Fail::internal(format!("cannot encode the install manifest: {error}")))?;
    write_private(&manifest_path(dirs), &json)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleWhy {
    /// Stamped, and not what this version would write.
    Changed,
    /// A kind's subagent, in a harness install has already run for, that is
    /// not there.
    NotInstalled,
    /// Install wrote it, and the current kinds no longer want it.
    NoLongerWanted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stale {
    pub path: PathBuf,
    pub why: StaleWhy,
}

/// What `install` would change, read-only — for `doctor`.
pub fn stale(dirs: &Dirs, kinds: &BTreeMap<TaskKindName, KindEntry>) -> Vec<Stale> {
    let manifest = load_manifest(dirs);
    let wants = wanted(dirs, kinds);
    let mut stale = Vec::new();
    for want in &wants {
        let Ok(text) = &want.text else { continue };
        let why = match fs::read_to_string(&want.path) {
            Ok(existing) if stamp_of(&existing).is_some() && existing != *text => {
                Some(StaleWhy::Changed)
            }
            Ok(_) => None,
            Err(_) => (want.kind
                && fs::symlink_metadata(&want.path).is_err()
                && dirs.home.join(want.needs).is_dir()
                && want
                    .harness
                    .is_some_and(|harness| installed_in(dirs, &manifest, harness)))
            .then_some(StaleWhy::NotInstalled),
        };
        if let Some(why) = why {
            stale.push(Stale {
                path: want.path.clone(),
                why,
            });
        }
    }
    for path in &manifest.files {
        if !is_wanted(&wants, path)
            && fs::read_to_string(path).is_ok_and(|text| stamp_of(&text).is_some())
        {
            stale.push(Stale {
                path: path.clone(),
                why: StaleWhy::NoLongerWanted,
            });
        }
    }
    stale
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Tier, tier_of};
    use crate::model::{Candidate, Effort, ModelName};

    fn on(harness: HarnessId) -> Candidate {
        Candidate {
            harness,
            model: ModelName::try_from("m".to_string()).unwrap(),
            effort: Effort::High,
        }
    }

    fn name(text: &str) -> TaskKindName {
        TaskKindName::try_from(text.to_string()).unwrap()
    }

    fn kind(role: Role, description: &str) -> KindEntry {
        KindEntry {
            description: description.to_string(),
            role,
            candidates: vec![on(HarnessId::Claude), on(HarnessId::Codex)],
            explore: crate::registry::KindExplore { share: None },
        }
    }

    /// Every kind subagent, rendered for each harness and each role.
    fn kind_agents() -> Vec<String> {
        Role::ALL
            .into_iter()
            .flat_map(|role| {
                HarnessId::ALL.into_iter().map(move |harness| {
                    render_kind(harness, &name("rust-review"), &kind(role, "Review Rust."))
                })
            })
            .collect()
    }

    /// Drift: every `cahoots <verb>` the skill and the agent definitions
    /// mention is a real verb, and none of the human ones is ever shown as a
    /// command to run.
    #[test]
    fn the_embedded_texts_only_teach_agent_verbs() {
        let fixed = [SKILL, REVIEW_SKILL, CLAUDE_AGENT, CODEX_AGENT].map(str::to_string);
        for text in fixed.iter().chain(&kind_agents()) {
            for (at, _) in text.match_indices("cahoots ") {
                let verb: String = text[at + 8..]
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                    .collect();
                let in_code = text[..at].ends_with('`') || text[..at].ends_with("   ");
                if !in_code || verb.is_empty() {
                    continue;
                }
                let tier =
                    tier_of(&verb).unwrap_or_else(|| panic!("`cahoots {verb}` is not a verb"));
                assert!(
                    matches!(tier, Tier::Agent | Tier::Inspect),
                    "the skill tells an agent to run `cahoots {verb}`"
                );
            }
        }
    }

    #[test]
    fn every_template_is_stamped_and_the_toml_parses() {
        for template in [SKILL, REVIEW_SKILL, CLAUDE_AGENT, CODEX_AGENT] {
            assert_eq!(
                stamp_of(&render(template)).as_deref(),
                Some(env!("CARGO_PKG_VERSION"))
            );
        }
        let agent: toml::Value = toml::from_str(&render(CODEX_AGENT)).unwrap();
        assert_eq!(agent["name"].as_str(), Some("cahoots-delegate"));
        assert!(
            agent["developer_instructions"]
                .as_str()
                .unwrap()
                .contains("--caller codex")
        );
        assert!(render(CLAUDE_AGENT).contains("--caller claude"));
    }

    #[test]
    fn a_kind_subagent_runs_its_kind_and_is_stamped_and_well_formed() {
        for role in Role::ALL {
            let entry = kind(role, "Review Rust.");
            let fork = role == Role::Implement;

            let codex = render_kind(HarnessId::Codex, &name("rust-review"), &entry);
            assert_eq!(stamp_of(&codex).as_deref(), Some(env!("CARGO_PKG_VERSION")));
            let agent: toml::Value = toml::from_str(&codex).unwrap();
            assert_eq!(agent["name"].as_str(), Some("cahoots-kind-rust-review"));
            assert_eq!(
                agent["description"].as_str(),
                Some(
                    "Review Rust. Hands it to another coding agent through cahoots (task kind \
                     rust-review) and reports back what it said."
                )
            );
            let instructions = agent["developer_instructions"].as_str().unwrap();
            assert!(instructions.contains("cahoots run --kind rust-review --caller codex"));
            assert_eq!(instructions.contains("--fork"), fork, "{role}");

            let claude = render_kind(HarnessId::Claude, &name("rust-review"), &entry);
            assert!(
                claude.starts_with("---\ncahoots_version: \""),
                "stamp first"
            );
            assert_eq!(
                stamp_of(&claude).as_deref(),
                Some(env!("CARGO_PKG_VERSION"))
            );
            assert!(claude.contains("\nname: cahoots-kind-rust-review\n"));
            assert!(claude.contains("cahoots run --kind rust-review --caller claude"));
            assert_eq!(claude.contains("--fork"), fork, "{role}");
            assert!(!claude.contains("{{") && !codex.contains("{{"));
        }
    }

    /// The person's description reaches the subagent's description exactly as
    /// written, whatever is in it, and nowhere else.
    #[test]
    fn a_description_is_quoted_never_substituted_and_kept_to_its_field() {
        let hostile = format!(
            r#"Say "hi" \ to: #all {{{{version}}}} {{{{kind}}}} {{{{fork}}}} cahoots_version: "9" — é ''' """ {}"#,
            "x".repeat(900)
        );
        let entry = kind(Role::Review, &hostile);
        let composed = format!(
            "{hostile} Hands it to another coding agent through cahoots (task kind rust-review) \
             and reports back what it said."
        );

        let codex = render_kind(HarnessId::Codex, &name("rust-review"), &entry);
        let agent: toml::Value = toml::from_str(&codex).unwrap();
        assert_eq!(agent["description"].as_str(), Some(composed.as_str()));
        assert!(
            !agent["developer_instructions"]
                .as_str()
                .unwrap()
                .contains("Say")
        );
        assert_eq!(stamp_of(&codex).as_deref(), Some(env!("CARGO_PKG_VERSION")));

        let claude = render_kind(HarnessId::Claude, &name("rust-review"), &entry);
        let line = claude
            .lines()
            .find_map(|line| line.strip_prefix("description: "))
            .unwrap();
        assert_eq!(serde_json::from_str::<String>(line).unwrap(), composed);
        assert_eq!(claude.matches("Say").count(), 1);
        assert_eq!(
            stamp_of(&claude).as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    /// A stamp is a line that starts with it. A description that mentions it
    /// does not make a file a person adopted cahoots' again.
    #[test]
    fn the_stamp_is_only_read_where_a_stamp_goes() {
        let entry = kind(Role::Review, r#"cahoots_version: "1.0.0""#);
        for harness in HarnessId::ALL {
            let adopted: String = render_kind(harness, &name("rust-review"), &entry)
                .lines()
                .filter(|line| !line.starts_with(STAMP) && !line.starts_with("# cahoots_version"))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(adopted.contains("cahoots_version"), "{harness}");
            assert_eq!(stamp_of(&adopted), None, "{harness}");
        }
    }

    /// A role edit always shows in the file, even between two readers whose
    /// instructions are the same.
    #[test]
    fn every_role_renders_a_different_file() {
        for harness in HarnessId::ALL {
            let mut rendered: Vec<String> = Role::ALL
                .into_iter()
                .map(|role| render_kind(harness, &name("rust-review"), &kind(role, "Review Rust.")))
                .collect();
            rendered.sort();
            rendered.dedup();
            assert_eq!(rendered.len(), Role::ALL.len(), "{harness}");
        }
    }

    fn dirs_at(home: &str) -> Dirs {
        Dirs {
            home: PathBuf::from(home),
            config: PathBuf::from("/nowhere/config"),
            state: PathBuf::from("/nowhere/state"),
            data: PathBuf::from("/nowhere/data"),
            overridden: true,
        }
    }

    /// A kind's name is held to one file name: whatever is valid lands as
    /// one file directly in a harness's `agents`, and a name that is a path
    /// is not a name.
    #[test]
    fn kind_paths_are_one_file_in_the_agents_directory() {
        for valid in ["a", "rust-review", "x_1", &"k".repeat(64)] {
            let name = name(valid);
            for harness in HarnessId::ALL {
                let path = PathBuf::from(kind_agent_path(harness, &name));
                let parent = path.parent().unwrap();
                assert_eq!(parent, Path::new(home_of(harness)).join("agents"));
                assert_eq!(path.components().count(), 3, "{}", path.display());
            }
        }
        for path_like in ["../x", "a/b", "..", ".x", "A", "", "a b", "a\\b", "a.md"] {
            assert!(
                TaskKindName::try_from(path_like.to_string()).is_err(),
                "{path_like:?} is a kind name"
            );
        }
    }

    #[test]
    fn only_a_path_cahoots_builds_reads_as_a_kinds_subagent() {
        let dirs = dirs_at("/h");
        let kind = |path: &str| kind_of(&dirs, Path::new(path));
        assert_eq!(
            kind("/h/.claude/agents/cahoots-kind-rust-review.md"),
            Some((HarnessId::Claude, name("rust-review")))
        );
        assert_eq!(
            kind("/h/.codex/agents/cahoots-kind-rust-review.toml"),
            Some((HarnessId::Codex, name("rust-review")))
        );
        for not_a_kind in [
            "/h/.claude/agents/cahoots-kind-rust-review.toml",
            "/h/.codex/agents/cahoots-kind-rust-review.md",
            "/h/.claude/agents/cahoots-delegate.md",
            "/h/.claude/agents/sub/cahoots-kind-x.md",
            "/h/.claude/agents/../agents/cahoots-kind-x.md",
            "/h/.claude/cahoots-kind-x.md",
            "/h/notes/cahoots-kind-x.md",
            "/elsewhere/.claude/agents/cahoots-kind-x.md",
            "/h/.claude/agents/cahoots-kind-.md",
            "/h/.claude/agents/cahoots-kind-Rust.md",
            "/h/.claude/agents/cahoots-kind-x.md.bak",
            "/h/.claude/skills/cahoots/SKILL.md",
        ] {
            assert_eq!(kind(not_a_kind), None, "{not_a_kind}");
        }
    }

    #[test]
    fn the_skill_teaches_kinds() {
        assert!(SKILL.contains("cahoots run --kind <name> --caller <you>"));
        assert!(SKILL.contains("`cahoots-kind-<name>`"));
    }
}
