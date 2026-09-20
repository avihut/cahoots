//! `cahoots install` / `uninstall`: the skill and the agent definitions, per
//! harness. Install IS update. Every file cahoots writes carries a
//! `cahoots_version` stamp, and that stamp is the only thing that makes a file
//! cahoots' to overwrite or to remove — a file of the same name that a person
//! wrote is left alone, and said so.
//!
//! What this module never does: touch a harness's settings or permission
//! files. The rules are printed (`rules.rs`) for a person to add.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::dirs::Dirs;
use crate::exit::{Fail, Res};
use crate::model::HarnessId;
use crate::run::record::write_private;

const SKILL: &str = include_str!("assets/SKILL.md");
const REVIEW_SKILL: &str = include_str!("assets/REVIEW-SKILL.md");
const CLAUDE_AGENT: &str = include_str!("assets/claude-agent.md");
const CODEX_AGENT: &str = include_str!("assets/codex-agent.toml");
const STAMP: &str = "cahoots_version";

fn render(template: &str) -> String {
    template.replace("{{version}}", env!("CARGO_PKG_VERSION"))
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

/// Everything cahoots installs. The skill goes to the shared skills location
/// and to each harness's own; the agent definitions are per harness.
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

fn stamp_of(text: &str) -> Option<String> {
    let line = text.lines().find(|line| line.contains(STAMP))?;
    let mut quoted = line.split('"');
    quoted.next()?;
    quoted.next().map(str::to_string)
}

/// A file is only ever written inside the user's home — judged by where the
/// path RESOLVES, and judged BEFORE anything is created: an agent home that is
/// a symlink to somewhere else must not even get a directory made through it.
/// So the nearest ancestor that already exists is resolved first.
fn confined(home: &Path, dir: &Path) -> Res<()> {
    let home = fs::canonicalize(home)
        .map_err(|error| Fail::internal(format!("cannot resolve {}: {error}", home.display())))?;
    let existing = dir
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .ok_or_else(|| Fail::internal(format!("{} has no existing ancestor", dir.display())))?;
    let resolved = fs::canonicalize(existing).map_err(|error| {
        Fail::internal(format!("cannot resolve {}: {error}", existing.display()))
    })?;
    if resolved.starts_with(&home) {
        Ok(())
    } else {
        Err(Fail::policy(format!(
            "{} resolves to {}, outside your home — refusing to write there",
            existing.display(),
            resolved.display()
        )))
    }
}

pub fn install(dirs: &Dirs, only: Option<HarnessId>, dry_run: bool) -> Res<Vec<Report>> {
    let mut manifest = load_manifest(dirs);
    let mut reports = Vec::new();
    for item in &ITEMS {
        if only.is_some() && item.harness.is_some() && item.harness != only {
            continue;
        }
        let path = dirs.home.join(item.path);
        let outcome = place(dirs, item, &path, dry_run)?;
        let ours = !matches!(outcome, Outcome::Skipped { .. });
        if ours && !manifest.files.contains(&path) {
            manifest.files.push(path.clone());
        }
        reports.push(Report { path, outcome });
    }
    if !dry_run {
        save_manifest(dirs, &mut manifest)?;
    }
    Ok(reports)
}

fn place(dirs: &Dirs, item: &Item, path: &Path, dry_run: bool) -> Res<Outcome> {
    if !dirs.home.join(item.needs).is_dir() {
        return Ok(Outcome::Skipped {
            why: format!(
                "~/{} does not exist — that harness is not set up here",
                item.needs
            ),
        });
    }
    let wanted = render(item.template);
    let outcome = match fs::symlink_metadata(path) {
        Err(_) => Outcome::Installed,
        Ok(meta) if !meta.is_file() => {
            return Ok(Outcome::Skipped {
                why: "exists and is not a regular file — left alone".to_string(),
            });
        }
        Ok(_) => {
            let existing = fs::read_to_string(path).unwrap_or_default();
            match stamp_of(&existing) {
                None => {
                    return Ok(Outcome::Skipped {
                        why: "exists and is not cahoots' (no cahoots_version stamp) — left alone"
                            .to_string(),
                    });
                }
                Some(_) if existing == wanted => return Ok(Outcome::UpToDate),
                Some(from) if from == env!("CARGO_PKG_VERSION") => Outcome::Refreshed,
                Some(from) => Outcome::Updated { from },
            }
        }
    };
    if !dry_run {
        let parent = path.parent().expect("an item path has a parent");
        // BEFORE anything is created: see `confined`.
        confined(&dirs.home, parent)?;
        fs::create_dir_all(parent).map_err(|error| {
            Fail::internal(format!("cannot create {}: {error}", parent.display()))
        })?;
        // And again now that it exists: nothing may have swapped it meanwhile.
        confined(&dirs.home, parent)?;
        fs::write(path, wanted)
            .map_err(|error| Fail::internal(format!("cannot write {}: {error}", path.display())))?;
    }
    Ok(outcome)
}

/// Removes what `install` wrote: only files the manifest lists, and only if
/// they still carry the stamp. Then the `cahoots` skill directories, if empty.
pub fn uninstall(dirs: &Dirs, dry_run: bool) -> Res<Vec<Report>> {
    let mut manifest = load_manifest(dirs);
    let mut reports = Vec::new();
    let mut kept = Vec::new();
    for path in std::mem::take(&mut manifest.files) {
        let outcome = match fs::read_to_string(&path) {
            Err(_) => Outcome::Skipped {
                why: "already gone".to_string(),
            },
            Ok(text) if stamp_of(&text).is_none() => Outcome::Skipped {
                why: "no longer carries the cahoots_version stamp — someone made it theirs; left alone"
                    .to_string(),
            },
            Ok(_) => {
                if !dry_run {
                    fs::remove_file(&path).map_err(|error| {
                        Fail::internal(format!("cannot remove {}: {error}", path.display()))
                    })?;
                    if let Some(parent) = path.parent()
                        && parent
                            .file_name()
                            .is_some_and(|name| name == "cahoots" || name == "cahoots-review")
                    {
                        let _ = fs::remove_dir(parent); // only succeeds when empty
                    }
                }
                Outcome::Removed
            }
        };
        if dry_run || matches!(&outcome, Outcome::Skipped { why } if why != "already gone") {
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

fn save_manifest(dirs: &Dirs, manifest: &mut Manifest) -> Res<()> {
    manifest.v = 1;
    manifest.files.sort();
    crate::dirs::ensure_private_dir(&dirs.state)?;
    let json = serde_json::to_vec_pretty(manifest)
        .map_err(|error| Fail::internal(format!("cannot encode the install manifest: {error}")))?;
    write_private(&manifest_path(dirs), &json)
}

/// Installed copies that are not what this version would write — for `doctor`.
pub fn stale(dirs: &Dirs) -> Vec<PathBuf> {
    ITEMS
        .iter()
        .map(|item| (dirs.home.join(item.path), render(item.template)))
        .filter(|(path, wanted)| {
            fs::read_to_string(path).is_ok_and(|text| stamp_of(&text).is_some() && text != *wanted)
        })
        .map(|(path, _)| path)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Tier, tier_of};

    /// Drift: every `cahoots <verb>` the skill and the agent definitions
    /// mention is a real verb, and none of the human ones is ever shown as a
    /// command to run.
    #[test]
    fn the_embedded_texts_only_teach_agent_verbs() {
        for text in [SKILL, REVIEW_SKILL, CLAUDE_AGENT, CODEX_AGENT] {
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
}
