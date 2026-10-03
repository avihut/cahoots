//! The effective registry: every harness and every role, defined once.
//! Defaults ship in this file; the user's config adjusts typed knobs on top.
//! Precedence, per knob: CLI flag > user config > learned (M5) > default.
//!
//! The caller is never removed from the registry — `pick` leaves it out at
//! run time, which is why one definition serves every direction.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::config::edit::{self, Change, KeyPath};
use crate::config::{Billing, UserConfig};
use crate::dirs::Dirs;
use crate::exit::{Exit, Fail, Res};
use crate::meter::{self, Chosen, MeterFile, UsageMeter};
use crate::model::{Candidate, Effort, HarnessId, ModelName, Role, TaskKindName};
use crate::placement::provider::ProviderId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Default,
    User,
}

#[derive(Debug, Clone, Serialize)]
pub struct HarnessEntry {
    /// Off until a person turns it on (`enabled = true` in its table, which
    /// `cahoots enable <harness>` writes): a run sends repository content to
    /// that vendor.
    pub enabled: bool,
    /// The pinned program: the only one that runs. Unpinned, the harness is
    /// not a candidate.
    pub binary: Option<PathBuf>,
    /// Its settings folder, when a person set one (`harness.<id>.home`).
    pub home: Option<PathBuf>,
    pub cap: u8,
    /// Where a run that is already going gets stopped. Always above `cap`.
    pub abort_at: u8,
    pub max_concurrent: u32,
    pub billing: Billing,
    pub origin: BTreeMap<&'static str, Origin>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleEntry {
    pub candidates: Vec<Candidate>,
    pub origin: Origin,
    /// Whether learning may reorder this list: the defaults, yes; a list a
    /// person wrote, only if they said `calibrate = true`.
    pub calibrate: bool,
    /// The one swap learning made here, if any — so `registry` can show it.
    pub learned_swap: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KindEntry {
    pub description: String,
    pub role: Role,
    pub candidates: Vec<Candidate>,
    pub explore: KindExplore,
}

/// A kind's own exploration share; none inherits its role's.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct KindExplore {
    pub share: Option<f64>,
}

/// Every role's share of new runs that try the next candidate first.
#[derive(Debug, Clone, Serialize)]
pub struct Explore {
    pub share: BTreeMap<Role, f64>,
}

pub struct Routing<'a> {
    pub role: Role,
    pub kind: Option<&'a TaskKindName>,
    pub candidates: &'a [Candidate],
    /// The share in effect for this list: a kind's own, else its role's.
    pub exploration_share: f64,
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
    pub watchdog_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Meters {
    /// The usage meter in effect, if any (`meter.rs`).
    pub usage: Option<UsageMeter>,
    pub usage_chosen_by: Option<Chosen>,
    pub ledger_max_runs_per_hour: u32,
    pub ledger_max_tokens_per_day: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Review {
    pub blind: bool,
    pub enabled: bool,
    pub sample_rate: f64,
    /// False is shadow mode: the adjustment is computed and shown, not used.
    pub apply_routing: bool,
}

/// What cuts a writer's worktree (`[fork]` in config.toml).
#[derive(Debug, Clone, Serialize)]
pub struct Fork {
    /// The provider a person chose: git, or daft where the repository has a
    /// `daft.yml`.
    pub provider: ProviderId,
    /// The daft a person chose. Unchosen, daft never runs.
    pub daft_binary: Option<PathBuf>,
    /// Whether daft runs the repository's hooks in a new worktree.
    pub daft_hooks: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Registry {
    pub harnesses: BTreeMap<HarnessId, HarnessEntry>,
    pub roles: BTreeMap<Role, RoleEntry>,
    pub kinds: BTreeMap<TaskKindName, KindEntry>,
    pub limits: Limits,
    pub meters: Meters,
    pub review: Review,
    pub explore: Explore,
    pub fork: Fork,
    /// The pinned `git` and `ps`, and the recorded PATH (`[tools]`).
    pub tools: crate::tools::Pins,
}

pub const DEFAULT_CAP: u8 = 75;
pub const DEFAULT_MAX_DATA_AGE_SECS: u64 = 900;

/// Ten points above the cap, never at 100 if it can be helped (a plan that is
/// fully used has already cut the user off), and always above the cap.
pub fn default_abort_at(cap: u8) -> u8 {
    cap.saturating_add(10)
        .min(99)
        .max(cap.saturating_add(1))
        .min(100)
}

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

/// What `enable` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enabled {
    /// The targets now on.
    pub enabled: Vec<HarnessId>,
    /// The program pinned for the target, the one a run uses.
    pub binary: Option<PathBuf>,
    /// Whether this call pinned it.
    pub pinned: bool,
    /// Whether this call recorded the person's PATH (`tools.path`).
    pub path_recorded: bool,
    /// Whether this call recorded the target's settings folder.
    pub home_recorded: bool,
}

/// Turns a target on or off, in config.toml: `enabled = true`, or no
/// `enabled` at all, which is off. Only human verbs call this, and those only
/// run from a terminal: sending a repository's content to another vendor is
/// a person's decision. Turning one on pins its program if none is pinned:
/// the first copy on the person's PATH that a run would take (`terminal`),
/// in the same write as `enabled` — or, with none, nothing is written at all
/// (34). With it go the person's PATH, if none is recorded, and the
/// target's settings folder their terminal names, if config.toml names
/// none. Off keeps the pin.
pub fn set_enabled(
    dirs: &Dirs,
    harness: HarnessId,
    on: bool,
    terminal: &crate::tools::Terminal,
) -> Res<Enabled> {
    let path = KeyPath::of(&format!("harness.{harness}.enabled"));
    let file = dirs.config_file();
    let now = Registry::effective(&UserConfig::load(&file)?);
    let entry = now.harness(harness);
    let set = |key: String, value: String| Change::Set {
        path: KeyPath::of(&key),
        value: value.into(),
    };
    let mut done = Enabled {
        enabled: Vec::new(),
        binary: entry.binary.clone(),
        pinned: false,
        path_recorded: false,
        home_recorded: false,
    };
    let mut changes = Vec::new();
    if on {
        let mut home = entry.home.clone();
        if home.is_none()
            && let Some(theirs) = terminal.home(harness)
            && crate::tools::check_home(theirs).is_ok()
        {
            changes.push(set(
                format!("harness.{harness}.home"),
                theirs.display().to_string(),
            ));
            home = Some(theirs.to_path_buf());
            done.home_recorded = true;
        }
        let mut pins = now.tools.clone();
        if pins.path.is_none()
            && let Some(recorded) =
                crate::tools::decide_path(terminal.path.as_deref(), None, &terminal.workspace).path
        {
            changes.push(set("tools.path".to_string(), recorded.clone()));
            pins.path = Some(recorded);
            done.path_recorded = true;
        }
        if entry.binary.is_none() {
            let name = crate::harness::harness(harness).binary_name();
            let (found, _) = crate::tools::discover_harness(
                harness,
                terminal.path.as_deref(),
                home.as_deref(),
                &pins,
                &terminal.workspace,
            )
            .map_err(|why| {
                Fail::config(format!(
                    "no `{name}` on your PATH that cahoots can run ({}) — choose its program \
                     with `cahoots settings` (harness.{harness}.binary), then enable it",
                    why.unwrap_or_else(|| "none found".to_string())
                ))
            })?;
            changes.push(set(
                format!("harness.{harness}.binary"),
                found.display().to_string(),
            ));
            done.binary = Some(found);
            done.pinned = true;
        }
        changes.push(Change::Set {
            path,
            value: true.into(),
        });
    } else {
        changes.push(Change::Remove { path });
    }
    let config = edit::apply(&file, &changes)?;
    done.enabled = HarnessId::ALL
        .into_iter()
        .filter(|id| config.harness.get(id).and_then(|h| h.enabled) == Some(true))
        .collect();
    Ok(done)
}

impl Registry {
    /// Resolve once, so admission and placement share the selected list's role.
    pub fn routing(&self, role: Option<Role>, kind: Option<&str>) -> Res<Routing<'_>> {
        if let Some(raw) = kind {
            let name = TaskKindName::try_from(raw.to_string())
                .map_err(|reason| Fail::new(Exit::Usage, reason))?;
            let (name, entry) = self.kinds.get_key_value(&name).ok_or_else(|| {
                Fail::new(
                    Exit::Usage,
                    format!("unknown task kind {raw:?} — a person defines kinds in config.toml"),
                )
            })?;
            if let Some(requested) = role
                && requested != entry.role
            {
                return Err(Fail::new(
                    Exit::Usage,
                    format!(
                        "task kind {:?} has role {}, which does not match --role {requested}",
                        name.as_str(),
                        entry.role
                    ),
                ));
            }
            Ok(Routing {
                role: entry.role,
                kind: Some(name),
                candidates: &entry.candidates,
                exploration_share: entry
                    .explore
                    .share
                    .unwrap_or(self.explore.share[&entry.role]),
            })
        } else {
            let role =
                role.ok_or_else(|| Fail::new(Exit::Usage, "one of --role or --kind is required"))?;
            Ok(Routing {
                role,
                kind: None,
                candidates: &self.roles[&role].candidates,
                exploration_share: self.explore.share[&role],
            })
        }
    }

    pub fn load(dirs: &Dirs) -> Res<Registry> {
        let config = UserConfig::load(&dirs.config_file())?;
        let mut registry = Registry::effective(&config);
        // What `install` found: the meter on when config.toml chooses none,
        // and where each meter is when its table names no binary.
        let found = MeterFile::load(dirs)?;
        let usage = meter::effective(&config.meter, found.as_ref());
        registry.meters.usage_chosen_by = usage.as_ref().map(|(_, by)| *by);
        registry.meters.usage = usage.map(|(meter, _)| meter);
        if registry.review.enabled && registry.review.apply_routing {
            let learned = registry.learned(dirs);
            for (role, entry) in &mut registry.roles {
                if learned.apply(*role, &mut entry.candidates) {
                    entry.learned_swap = learned.swaps.get(role).copied();
                }
            }
        }
        Ok(registry)
    }

    /// What the history supports changing, for the roles learning may touch.
    /// Computed against THIS registry's lists, so it can never refer to a
    /// candidate that is not there.
    pub fn learned(&self, dirs: &Dirs) -> crate::calibrate::LearnedAdjustments {
        use crate::calibrate::{WINDOW_DAYS, suggest};
        let events = crate::history::read(dirs);
        let forgotten_at = events
            .iter()
            .filter_map(|event| match event {
                crate::history::Event::Forget { t } => Some(*t),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let since = crate::run::record::now()
            .saturating_sub(WINDOW_DAYS * 24 * 3600)
            .max(forgotten_at);
        let stories = crate::history::stories(&events);
        let swaps = self
            .roles
            .iter()
            .filter(|(_, entry)| entry.calibrate && entry.learned_swap.is_none())
            .filter_map(|(role, entry)| {
                suggest(&stories, *role, &entry.candidates, since).map(|at| (*role, at))
            })
            .collect();
        crate::calibrate::LearnedAdjustments { swaps }
    }

    pub fn effective(config: &UserConfig) -> Registry {
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
            pick("enabled", user.is_some_and(|u| u.enabled.is_some()));
            pick("cap", user.is_some_and(|u| u.cap.is_some()));
            pick("binary", user.is_some_and(|u| u.binary.is_some()));
            pick("home", user.is_some_and(|u| u.home.is_some()));
            pick(
                "max_concurrent",
                user.is_some_and(|u| u.max_concurrent.is_some()),
            );
            pick("billing", user.is_some_and(|u| u.billing.is_some()));
            pick("abort_at", user.is_some_and(|u| u.abort_at.is_some()));
            let cap = user.and_then(|u| u.cap).unwrap_or(DEFAULT_CAP);
            harnesses.insert(
                id,
                HarnessEntry {
                    enabled: user.and_then(|u| u.enabled).unwrap_or(false),
                    binary: user.and_then(|u| u.binary.clone()),
                    home: user.and_then(|u| u.home.clone()),
                    cap,
                    abort_at: user
                        .and_then(|u| u.abort_at)
                        .unwrap_or_else(|| default_abort_at(cap)),
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
                        calibrate: user.calibrate.unwrap_or(false),
                        learned_swap: None,
                    },
                    None => RoleEntry {
                        candidates: default_candidates(role),
                        origin: Origin::Default,
                        calibrate: true,
                        learned_swap: None,
                    },
                };
                (role, entry)
            })
            .collect();
        let ledger = config.meter.ledger.clone().unwrap_or_default();
        let usage = meter::effective(&config.meter, None);
        Registry {
            harnesses,
            roles,
            kinds: config
                .kinds
                .iter()
                .map(|(name, entry)| {
                    (
                        name.clone(),
                        KindEntry {
                            description: entry.description.clone(),
                            role: entry.role,
                            candidates: entry.candidates.clone(),
                            explore: KindExplore {
                                share: entry.explore.share,
                            },
                        },
                    )
                })
                .collect(),
            limits: Limits {
                max_active_runs: config.limits.max_active_runs.unwrap_or(3),
                max_depth: config.limits.max_depth.unwrap_or(1),
                timeout_secs: config.limits.timeout_secs.unwrap_or(1800),
                wait_secs: config.limits.wait_secs.unwrap_or(90),
                int_grace_secs: config.limits.int_grace_secs.unwrap_or(10),
                term_grace_secs: config.limits.term_grace_secs.unwrap_or(5),
                allow_in_place: config.limits.allow_in_place.unwrap_or(false),
                watchdog_secs: config.limits.watchdog_secs.unwrap_or(120),
            },
            review: Review {
                blind: config.review.blind.unwrap_or(false),
                enabled: config.review.enabled.unwrap_or(false),
                sample_rate: config.review.sample_rate.unwrap_or(0.2),
                apply_routing: config.review.apply_routing.unwrap_or(false),
            },
            explore: Explore {
                share: Role::ALL
                    .into_iter()
                    .map(|role| {
                        (
                            role,
                            config.explore.share.get(&role).copied().unwrap_or(0.0),
                        )
                    })
                    .collect(),
            },
            meters: Meters {
                usage_chosen_by: usage.as_ref().map(|(_, by)| *by),
                usage: usage.map(|(meter, _)| meter),
                ledger_max_runs_per_hour: ledger.max_runs_per_hour.unwrap_or(12),
                ledger_max_tokens_per_day: ledger.max_tokens_per_day,
            },
            tools: crate::tools::Pins {
                git: config.tools.git.as_ref().and_then(|t| t.binary.clone()),
                ps: config.tools.ps.as_ref().and_then(|t| t.binary.clone()),
                path: config.tools.path.clone(),
            },
            fork: Fork {
                provider: config.fork.provider.unwrap_or(ProviderId::Git),
                daft_binary: config.fork.daft.as_ref().and_then(|d| d.binary.clone()),
                daft_hooks: config
                    .fork
                    .daft
                    .as_ref()
                    .and_then(|d| d.hooks)
                    .unwrap_or(false),
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
    fn enabling_pins_what_it_finds_and_is_idempotent_and_reversible() {
        use std::os::unix::fs::PermissionsExt;
        // Not under a temp directory, where no PATH is recorded from.
        let tmp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let dirs = Dirs {
            home: root.clone(),
            config: root.join("config"),
            state: root.join("state"),
            data: root.join("data"),
            overridden: true,
        };
        // A codex on the person's PATH, and nothing else there.
        let bin = root.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let codex = bin.join("codex");
        std::fs::write(&codex, "#!/bin/sh\necho 'codex-cli 0.155.1'\n").unwrap();
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
        let terminal = crate::tools::Terminal {
            path: Some(bin.clone().into_os_string()),
            homes: Vec::new(),
            workspace: Vec::new(),
        };
        let enabled = |on| set_enabled(&dirs, HarnessId::Codex, on, &terminal).unwrap();

        let first = enabled(true);
        assert_eq!(first.enabled, [HarnessId::Codex]);
        assert_eq!(
            (first.binary.as_deref(), first.pinned),
            (Some(&*codex), true)
        );
        assert!(first.path_recorded, "no PATH was recorded, so this one is");
        let written = std::fs::read_to_string(dirs.config_file()).unwrap();
        assert!(
            written.contains(&format!("binary = \"{}\"", codex.display()))
                && written.contains("enabled = true")
                && written.contains(&format!("path = \"{}\"", bin.display())),
            "{written}"
        );
        // Again: the pin is kept, and nothing is pinned or recorded now.
        let again = enabled(true);
        assert_eq!(again.enabled, [HarnessId::Codex]);
        assert_eq!(
            (again.binary.as_deref(), again.pinned),
            (Some(&*codex), false)
        );
        assert!(!again.path_recorded);
        assert_eq!(
            std::fs::read_to_string(dirs.config_file()).unwrap(),
            written
        );
        assert!(
            Registry::load(&dirs)
                .unwrap()
                .harness(HarnessId::Codex)
                .enabled
        );
        // Off keeps the pin.
        assert!(enabled(false).enabled.is_empty());
        let off = Registry::load(&dirs).unwrap();
        assert!(!off.harness(HarnessId::Codex).enabled);
        assert_eq!(
            off.harness(HarnessId::Codex).binary.as_deref(),
            Some(&*codex)
        );

        // Nothing on the PATH to pin: refused, and nothing written.
        let before = std::fs::read_to_string(dirs.config_file()).unwrap();
        let empty = crate::tools::Terminal {
            path: Some(root.join("nothing").into_os_string()),
            homes: Vec::new(),
            workspace: Vec::new(),
        };
        let fail = set_enabled(&dirs, HarnessId::Claude, true, &empty).unwrap_err();
        assert_eq!(fail.exit, Exit::Config);
        assert!(fail.message.contains("none found"), "{}", fail.message);
        assert!(
            fail.message.contains("cahoots settings"),
            "{}",
            fail.message
        );
        assert_eq!(std::fs::read_to_string(dirs.config_file()).unwrap(), before);
    }

    #[test]
    fn the_default_abort_threshold_sits_above_the_cap() {
        for (cap, abort_at) in [
            (50, 60),
            (75, 85),
            (89, 99),
            (95, 99),
            (99, 100),
            (100, 100),
        ] {
            assert_eq!(default_abort_at(cap), abort_at, "cap {cap}");
        }
    }

    #[test]
    fn git_cuts_until_a_person_chooses_daft() {
        let fork = Registry::effective(&UserConfig::default()).fork;
        assert_eq!(
            (fork.provider, fork.daft_binary, fork.daft_hooks),
            (ProviderId::Git, None, false)
        );
        let config = UserConfig::parse(
            "schema = 1\nfork.provider = \"daft\"\nfork.daft.binary = \"/opt/bin/daft\"\nfork.daft.hooks = true",
        )
        .unwrap();
        let fork = Registry::effective(&config).fork;
        assert_eq!(fork.provider, ProviderId::Daft);
        assert_eq!(fork.daft_binary, Some(PathBuf::from("/opt/bin/daft")));
        assert!(fork.daft_hooks);
    }

    #[test]
    fn nothing_is_enabled_until_a_human_says_so() {
        let registry = Registry::effective(&UserConfig::default());
        assert!(registry.harnesses.values().all(|h| !h.enabled));
        let config = UserConfig::parse("schema = 1\nharness.codex.enabled = true").unwrap();
        let registry = Registry::effective(&config);
        assert!(registry.harness(HarnessId::Codex).enabled);
        assert!(!registry.harness(HarnessId::Claude).enabled);
    }

    #[test]
    fn every_role_has_a_candidate_on_every_harness_by_default() {
        let registry = Registry::effective(&UserConfig::default());
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
        let registry = Registry::effective(&config);
        let codex = registry.harness(HarnessId::Codex);
        assert_eq!((codex.cap, codex.origin["cap"]), (80, Origin::User));
        assert_eq!(codex.origin["billing"], Origin::Default);
        assert_eq!(registry.harness(HarnessId::Claude).cap, DEFAULT_CAP);
        // Stopping a run in flight is always a higher bar than refusing to start one.
        assert_eq!(codex.abort_at, 90);
        for entry in registry.harnesses.values() {
            assert!(entry.abort_at > entry.cap && entry.abort_at <= 100);
        }
        assert_eq!(registry.roles[&Role::Explore].origin, Origin::User);
        assert_eq!(registry.roles[&Role::Explore].candidates.len(), 1);
        assert_eq!(registry.roles[&Role::Advise].origin, Origin::Default);
    }

    fn kind_registry() -> Registry {
        Registry::effective(&UserConfig::parse("schema = 1\n[kinds.rust-review]\ndescription = \"Review Rust.\"\nrole = \"review\"\ncandidates = [{ harness = \"claude\", model = \"custom\", effort = \"low\" }]").unwrap())
    }

    #[test]
    fn a_kind_resolves_its_own_role_and_exact_list() {
        assert!(Registry::effective(&UserConfig::default()).kinds.is_empty());
        let registry = kind_registry();
        let routing = registry.routing(None, Some("rust-review")).unwrap();
        assert_eq!(routing.role, Role::Review);
        assert_eq!(routing.kind.unwrap().as_str(), "rust-review");
        assert_eq!(
            routing.candidates,
            registry
                .routing(Some(Role::Review), Some("rust-review"))
                .unwrap()
                .candidates
        );
        assert_eq!(routing.candidates.len(), 1);
        assert_eq!(routing.candidates[0].model.as_str(), "custom");
        let role = registry.routing(Some(Role::Review), None).unwrap();
        assert_eq!(role.candidates, registry.roles[&Role::Review].candidates);
        assert!(role.kind.is_none());
    }

    #[test]
    fn unknown_kinds_and_disagreeing_roles_are_usage_errors() {
        let mut registry = kind_registry();
        for raw in ["missing", "a.b", "A", "", "--flag"] {
            let fail = registry
                .routing(Some(Role::Review), Some(raw))
                .err()
                .unwrap();
            assert_eq!(fail.exit, Exit::Usage);
            assert!(fail.message.starts_with(if raw == "missing" {
                "unknown task kind"
            } else {
                "task kind name"
            }));
        }
        for configured in Role::ALL {
            registry.kinds.values_mut().next().unwrap().role = configured;
            for requested in Role::ALL {
                if configured == requested {
                    continue;
                }
                let fail = registry
                    .routing(Some(requested), Some("rust-review"))
                    .err()
                    .unwrap();
                assert_eq!(fail.exit, Exit::Usage);
                assert_eq!(
                    fail.message,
                    format!(
                        "task kind \"rust-review\" has role {configured}, which does not match --role {requested}"
                    )
                );
            }
        }
        assert_eq!(
            registry.routing(None, None).err().unwrap().exit,
            Exit::Usage
        );
    }

    #[test]
    fn exploration_shares_default_to_zero_and_a_kind_inherits_its_role() {
        let registry = Registry::effective(&UserConfig::default());
        for role in Role::ALL {
            assert_eq!(registry.explore.share[&role], 0.0, "{role}");
            assert_eq!(
                registry
                    .routing(Some(role), None)
                    .unwrap()
                    .exploration_share,
                0.0
            );
        }
        let kind = |name: &str, role: &str, share: &str| {
            format!(
                "[kinds.{name}]\ndescription = \"x\"\nrole = \"{role}\"\ncandidates = [{{ harness = \"codex\", model = \"m\", effort = \"high\" }}]\n{share}"
            )
        };
        let config = UserConfig::parse(&format!(
            "schema = 1\n[explore.share]\nreview = 0.4\n{}{}{}",
            kind("inherits", "review", ""),
            kind("zero", "review", "[kinds.zero.explore]\nshare = 0.0\n"),
            kind("own", "advise", "[kinds.own.explore]\nshare = 0.9\n"),
        ))
        .unwrap();
        let registry = Registry::effective(&config);
        let share = |kind: &str| {
            registry
                .routing(None, Some(kind))
                .unwrap()
                .exploration_share
        };
        assert_eq!(share("inherits"), 0.4);
        assert_eq!(share("zero"), 0.0, "an explicit zero beats a nonzero role");
        assert_eq!(share("own"), 0.9, "over a role at zero");
        assert_eq!(
            registry
                .routing(Some(Role::Review), None)
                .unwrap()
                .exploration_share,
            0.4
        );
        // The registry shows the approved paths; an absent override is null.
        let json = serde_json::to_value(&registry).unwrap();
        assert_eq!(json["explore"]["share"]["review"], 0.4);
        assert_eq!(json["explore"]["share"]["implement"], 0.0);
        assert!(json["kinds"]["inherits"]["explore"]["share"].is_null());
        assert_eq!(json["kinds"]["zero"]["explore"]["share"], 0.0);
        // Learned adjustments reorder a list; they have nowhere to put a share.
        assert_eq!(registry.explore.share.len(), Role::ALL.len());
    }
}
