//! The effective registry: every harness and every role, defined once.
//! Defaults ship in this file; the user's config adjusts typed knobs on top.
//! Precedence, per knob: CLI flag > user config > learned (M5) > default.
//!
//! The caller is never removed from the registry — `pick` leaves it out at
//! run time, which is why one definition serves every direction.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{AgentUsageConfig, Billing, UserConfig};
use crate::dirs::Dirs;
use crate::exit::{Fail, Res};
use crate::model::{Candidate, Effort, HarnessId, ModelName, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Default,
    User,
}

#[derive(Debug, Clone, Serialize)]
pub struct HarnessEntry {
    /// Off until a human runs `cahoots enable <harness>`: a run sends
    /// repository content to that vendor.
    pub enabled: bool,
    pub binary: Option<PathBuf>,
    pub cap: u8,
    pub max_concurrent: u32,
    pub billing: Billing,
    pub origin: BTreeMap<&'static str, Origin>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleEntry {
    pub candidates: Vec<Candidate>,
    pub origin: Origin,
}

#[derive(Debug, Clone, Serialize)]
pub struct Limits {
    pub max_active_runs: u32,
    pub max_depth: u32,
    pub timeout_secs: u64,
    pub wait_secs: u64,
    pub int_grace_secs: u64,
    pub term_grace_secs: u64,
    pub allow_in_place: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Meters {
    pub agent_usage: Option<AgentUsageConfig>,
    pub ledger_max_runs_per_hour: u32,
    pub ledger_max_tokens_per_day: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Registry {
    pub harnesses: BTreeMap<HarnessId, HarnessEntry>,
    pub roles: BTreeMap<Role, RoleEntry>,
    pub limits: Limits,
    pub meters: Meters,
}

pub const DEFAULT_CAP: u8 = 75;
pub const DEFAULT_MAX_DATA_AGE_SECS: u64 = 900;

fn candidate(harness: HarnessId, model: &str, effort: Effort) -> Candidate {
    Candidate {
        harness,
        model: ModelName::try_from(model.to_string()).expect("a shipped default model name"),
        effort,
    }
}

/// The shipped routing. Opinionated, and only a starting point: ideation goes
/// to the strongest reasoner, review to the strongest coder, exploration to
/// the balanced tier. Override per role in the config; M5 tunes it locally.
fn default_candidates(role: Role) -> Vec<Candidate> {
    use Effort::{High, Medium};
    use HarnessId::{Claude, Codex};
    match role {
        Role::Advise => vec![
            candidate(Codex, "gpt-6-astra", High),
            candidate(Claude, "opus", High),
        ],
        Role::Review => vec![
            candidate(Codex, "gpt-5.6-sol", High),
            candidate(Claude, "opus", High),
        ],
        Role::Explore => vec![
            candidate(Codex, "gpt-5.6-terra", Medium),
            candidate(Claude, "sonnet", Medium),
        ],
        Role::Implement => vec![
            candidate(Codex, "gpt-5.6-sol", High),
            candidate(Claude, "opus", High),
        ],
    }
}

/// What `enabled.json` holds. Written only by the `enable` verb.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnabledFile {
    pub v: u32,
    pub enabled: Vec<HarnessId>,
}

impl EnabledFile {
    pub fn load(dirs: &Dirs) -> Res<EnabledFile> {
        let path = dirs.enabled_file();
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| Fail::config(format!("{}: {error}", path.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(EnabledFile {
                v: 1,
                enabled: Vec::new(),
            }),
            Err(error) => Err(Fail::config(format!(
                "cannot read {}: {error}",
                path.display()
            ))),
        }
    }
}

/// Turns a target on or off. Only the `enable` verb calls this, and that verb
/// only runs from a terminal: sending a repository's content to another
/// vendor is a person's decision.
pub fn set_enabled(dirs: &Dirs, harness: HarnessId, on: bool) -> Res<Vec<HarnessId>> {
    let mut file = EnabledFile::load(dirs)?;
    file.v = 1;
    file.enabled.retain(|id| *id != harness);
    if on {
        file.enabled.push(harness);
    }
    file.enabled.sort();
    crate::dirs::ensure_private_dir(&dirs.config)?;
    let json = serde_json::to_vec_pretty(&file)
        .map_err(|error| Fail::internal(format!("cannot encode the enabled list: {error}")))?;
    crate::run::record::write_private(&dirs.enabled_file(), &json)?;
    Ok(file.enabled)
}

impl Registry {
    pub fn load(dirs: &Dirs) -> Res<Registry> {
        let config = UserConfig::load(&dirs.config_file())?;
        let enabled = EnabledFile::load(dirs)?;
        Ok(Registry::effective(&config, &enabled.enabled))
    }

    pub fn effective(config: &UserConfig, enabled: &[HarnessId]) -> Registry {
        let mut harnesses = BTreeMap::new();
        for id in HarnessId::ALL {
            let user = config.harness.get(&id);
            let mut origin = BTreeMap::new();
            let mut pick = |name: &'static str, from_user: bool| {
                origin.insert(
                    name,
                    if from_user {
                        Origin::User
                    } else {
                        Origin::Default
                    },
                );
            };
            pick("cap", user.is_some_and(|u| u.cap.is_some()));
            pick("binary", user.is_some_and(|u| u.binary.is_some()));
            pick(
                "max_concurrent",
                user.is_some_and(|u| u.max_concurrent.is_some()),
            );
            pick("billing", user.is_some_and(|u| u.billing.is_some()));
            harnesses.insert(
                id,
                HarnessEntry {
                    enabled: enabled.contains(&id),
                    binary: user.and_then(|u| u.binary.clone()),
                    cap: user.and_then(|u| u.cap).unwrap_or(DEFAULT_CAP),
                    max_concurrent: user.and_then(|u| u.max_concurrent).unwrap_or(1),
                    billing: user.and_then(|u| u.billing).unwrap_or_default(),
                    origin,
                },
            );
        }
        let roles = Role::ALL
            .into_iter()
            .map(|role| {
                let entry = match config.roles.get(&role) {
                    Some(user) => RoleEntry {
                        candidates: user.candidates.clone(),
                        origin: Origin::User,
                    },
                    None => RoleEntry {
                        candidates: default_candidates(role),
                        origin: Origin::Default,
                    },
                };
                (role, entry)
            })
            .collect();
        let ledger = config.meter.ledger.clone().unwrap_or_default();
        Registry {
            harnesses,
            roles,
            limits: Limits {
                max_active_runs: config.limits.max_active_runs.unwrap_or(3),
                max_depth: config.limits.max_depth.unwrap_or(1),
                timeout_secs: config.limits.timeout_secs.unwrap_or(1800),
                wait_secs: config.limits.wait_secs.unwrap_or(90),
                int_grace_secs: config.limits.int_grace_secs.unwrap_or(10),
                term_grace_secs: config.limits.term_grace_secs.unwrap_or(5),
                allow_in_place: config.limits.allow_in_place.unwrap_or(false),
            },
            meters: Meters {
                agent_usage: config.meter.agent_usage.clone(),
                ledger_max_runs_per_hour: ledger.max_runs_per_hour.unwrap_or(12),
                ledger_max_tokens_per_day: ledger.max_tokens_per_day,
            },
        }
    }

    pub fn harness(&self, id: HarnessId) -> &HarnessEntry {
        &self.harnesses[&id]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabling_is_idempotent_and_reversible() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = Dirs {
            home: tmp.path().to_path_buf(),
            config: tmp.path().join("config"),
            state: tmp.path().join("state"),
            overridden: true,
        };
        assert_eq!(
            set_enabled(&dirs, HarnessId::Codex, true).unwrap(),
            [HarnessId::Codex]
        );
        assert_eq!(
            set_enabled(&dirs, HarnessId::Codex, true).unwrap(),
            [HarnessId::Codex]
        );
        assert!(
            Registry::load(&dirs)
                .unwrap()
                .harness(HarnessId::Codex)
                .enabled
        );
        assert!(
            set_enabled(&dirs, HarnessId::Codex, false)
                .unwrap()
                .is_empty()
        );
        assert!(
            !Registry::load(&dirs)
                .unwrap()
                .harness(HarnessId::Codex)
                .enabled
        );
    }

    #[test]
    fn nothing_is_enabled_until_a_human_says_so() {
        let registry = Registry::effective(&UserConfig::default(), &[]);
        assert!(registry.harnesses.values().all(|h| !h.enabled));
        let registry = Registry::effective(&UserConfig::default(), &[HarnessId::Codex]);
        assert!(registry.harness(HarnessId::Codex).enabled);
        assert!(!registry.harness(HarnessId::Claude).enabled);
    }

    #[test]
    fn every_role_has_a_candidate_on_every_harness_by_default() {
        let registry = Registry::effective(&UserConfig::default(), &[]);
        for role in Role::ALL {
            for harness in HarnessId::ALL {
                assert!(
                    registry.roles[&role]
                        .candidates
                        .iter()
                        .any(|c| c.harness == harness),
                    "{role} has no default candidate on {harness}: a caller on the other \
                     harness would have nobody to ask"
                );
            }
        }
    }

    #[test]
    fn user_knobs_override_defaults_and_say_where_they_came_from() {
        let config = UserConfig::parse(
            "schema = 1\n[harness.codex]\ncap = 80\n[roles.explore]\ncandidates = [{ harness = \"claude\", model = \"haiku\", effort = \"low\" }]",
        )
        .unwrap();
        let registry = Registry::effective(&config, &[]);
        let codex = registry.harness(HarnessId::Codex);
        assert_eq!((codex.cap, codex.origin["cap"]), (80, Origin::User));
        assert_eq!(codex.origin["billing"], Origin::Default);
        assert_eq!(registry.harness(HarnessId::Claude).cap, DEFAULT_CAP);
        assert_eq!(registry.roles[&Role::Explore].origin, Origin::User);
        assert_eq!(registry.roles[&Role::Explore].candidates.len(), 1);
        assert_eq!(registry.roles[&Role::Advise].origin, Origin::Default);
    }
}
