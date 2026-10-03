//! The user's file: `<config>/config.toml`. Typed knobs only — there is no
//! command template and no free-form flag list, so nothing written here can
//! widen what cahoots may do (hard rule 6). Unknown keys are errors: a typo
//! in a cap must not silently mean "no cap".
//!
//! Never read from the working directory. A repository cannot configure the
//! tool that is about to run on it.

pub mod edit;

use std::collections::BTreeMap;
use std::fs;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::exit::{Fail, Res};
use crate::meter::{MeterId, Selection};
use crate::model::{Candidate, HarnessId, Role, TaskKindName};
use crate::placement::provider::ProviderId;

pub const SCHEMA: u32 = 1;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    pub schema: Option<u32>,
    #[serde(default)]
    pub harness: BTreeMap<HarnessId, HarnessConfig>,
    #[serde(default)]
    pub roles: BTreeMap<Role, RoleConfig>,
    #[serde(default)]
    pub kinds: BTreeMap<TaskKindName, KindConfig>,
    #[serde(default)]
    pub meter: MeterConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub review: ReviewConfig,
    #[serde(default)]
    pub explore: ExploreConfig,
    #[serde(default)]
    pub fork: ForkConfig,
}

/// `[fork]`: what cuts a writer's worktree. A person's choice, here and only
/// here — never a flag on `run`, and never the repository's: a repository's
/// content must not choose a tool that runs the repository's own commands.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkConfig {
    /// `git` (the default) or `daft`, which cuts only where the repository
    /// has a `daft.yml`, and git everywhere else.
    pub provider: Option<ProviderId>,
    pub daft: Option<DaftConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaftConfig {
    /// Absolute path to `daft`. daft runs only from here, never from PATH.
    pub binary: Option<PathBuf>,
    /// Let daft run the repository's hooks in a new worktree. Off unless a
    /// person turns it on.
    pub hooks: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessConfig {
    /// Whether cahoots may delegate to it. Off unless a person turns it on
    /// (`cahoots enable`): a run sends repository content to that vendor.
    pub enabled: Option<bool>,
    /// Absolute path to the harness CLI. Default: found on PATH.
    pub binary: Option<PathBuf>,
    /// Percent of the plan cahoots may see used and still delegate.
    pub cap: Option<u8>,
    pub max_concurrent: Option<u32>,
    pub billing: Option<Billing>,
    /// Percent of the plan at which a run that is ALREADY going is stopped.
    /// Above `cap`, which only decides whether a run may start. Default:
    /// cap + 10.
    pub abort_at: Option<u8>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Billing {
    /// The user's signed-in plan. Vendor API keys are stripped from the callee.
    #[default]
    Subscription,
    /// Per-token API billing, on purpose: the key is passed through.
    Api,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleConfig {
    /// Replaces the default candidate order for the role (arrays replace,
    /// they do not merge).
    pub candidates: Vec<Candidate>,
    /// A list a person wrote is theirs: learning leaves its order alone
    /// unless they say otherwise here.
    pub calibrate: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindConfig {
    pub description: String,
    pub role: Role,
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub explore: KindExploreConfig,
}

/// `[explore]`: the share of new runs that try the next listed candidate
/// first, per role. A role left out explores nothing.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExploreConfig {
    #[serde(default)]
    pub share: BTreeMap<Role, f64>,
}

/// `[kinds.<name>.explore]`: this kind's own share, replacing its role's —
/// zero included.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindExploreConfig {
    pub share: Option<f64>,
}

pub const MAX_KIND_DESCRIPTION_CHARS: usize = 1024;

/// Shared by stored config and settings edits; preserves the person's text.
pub fn validate_kind_description(name: &TaskKindName, text: &str) -> Res<()> {
    if text.trim().is_empty()
        || text.chars().count() > MAX_KIND_DESCRIPTION_CHARS
        || text
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err(Fail::config(format!(
            "kinds.{name}.description must be nonblank, one line, at most 1024 characters, and contain no control characters"
        )));
    }
    Ok(())
}

/// `[meter]`: which usage meter judges the gate (`use`), and each meter's
/// knobs in a table of its own. A table only configures its meter; `use` is
/// what turns one on. Without `use`, the meter is the only one `install`
/// found, if it found just one (`meter.rs`).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeterConfig {
    /// `agent-usage`, `ccusage`, or `none` for none at all.
    #[serde(rename = "use")]
    pub use_: Option<Selection>,
    #[serde(rename = "agent-usage")]
    pub agent_usage: Option<AgentUsageConfig>,
    pub ccusage: Option<CcusageConfig>,
    pub ledger: Option<LedgerConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentUsageConfig {
    /// Absolute path to `usage-cli` (it is not on PATH). Default: where
    /// `install` found it.
    pub binary: Option<PathBuf>,
    /// How old a measurement may be before it counts as stale. Default 900.
    pub max_data_age_secs: Option<u64>,
}

/// ccusage counts tokens and cannot see a plan's limit, so a percentage —
/// what `cap` and `abort_at` are — exists only against one declared here.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CcusageConfig {
    /// Absolute path to `ccusage`. Default: where `install` found it.
    pub binary: Option<PathBuf>,
    /// Tokens in one 5-hour block that count as Claude Code's whole limit.
    pub claude_block_tokens: Option<u64>,
    /// Tokens in one day that count as Codex's whole limit.
    pub codex_day_tokens: Option<u64>,
}

impl MeterConfig {
    /// The binary its table names for a meter, if it names one.
    pub fn binary(&self, id: MeterId) -> Option<&Path> {
        match id {
            MeterId::AgentUsage => self.agent_usage.as_ref()?.binary.as_deref(),
            MeterId::Ccusage => self.ccusage.as_ref()?.binary.as_deref(),
        }
    }
}

/// A declared limit below this is a unit slip (millions meant), not a plan:
/// one delegated run can take more than that.
pub const MIN_TOKEN_LIMIT: u64 = 100_000;

// The values each knob may take. `validate` holds a file to them, and the
// settings page offers nothing outside them.
pub const CAP: RangeInclusive<u8> = 1..=100;
pub const MAX_CONCURRENT: RangeInclusive<u32> = 1..=8;
pub const MAX_ACTIVE_RUNS: RangeInclusive<u32> = 1..=16;
pub const MAX_DEPTH: RangeInclusive<u32> = 0..=3;
pub const TIMEOUT_SECS: RangeInclusive<u64> = 30..=14_400;
pub const WAIT_SECS: RangeInclusive<u64> = 0..=540;
pub const GRACE_SECS: RangeInclusive<u64> = 1..=60;
pub const WATCHDOG_SECS: RangeInclusive<u64> = 1..=3600;
pub const MAX_DATA_AGE_SECS: RangeInclusive<u64> = 60..=86_400;
/// 0 is a real setting: no runs at all.
pub const MAX_RUNS_PER_HOUR: RangeInclusive<u32> = 0..=600;
pub const SAMPLE_RATE: RangeInclusive<f64> = 0.0..=1.0;
/// A share of new runs, as a fraction. Containment refuses NaN and infinities.
pub const EXPLORE_SHARE: RangeInclusive<f64> = 0.0..=1.0;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerConfig {
    pub max_runs_per_hour: Option<u32>,
    pub max_tokens_per_day: Option<u64>,
}

/// Review and local learning are off by default. Reviewing spends the caller's
/// plan; blindness independently controls the presentation of run identity.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewConfig {
    pub blind: Option<bool>,
    pub enabled: Option<bool>,
    /// The share of finished runs offered for review. 0.0–1.0, default 0.2.
    pub sample_rate: Option<f64>,
    /// Let outcome statistics reorder a role's candidates — by one position,
    /// never more. Off by default: until it is on, cahoots only SAYS what it
    /// would do (`report --suggest`).
    pub apply_routing: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    pub max_active_runs: Option<u32>,
    pub max_depth: Option<u32>,
    pub timeout_secs: Option<u64>,
    pub wait_secs: Option<u64>,
    /// How long a callee gets to stop after SIGINT, then after SIGTERM.
    pub int_grace_secs: Option<u64>,
    pub term_grace_secs: Option<u64>,
    /// Let a writer work in the caller's own working tree (`--in-place`)
    /// instead of a worktree of its own. Off unless a person turns it on.
    pub allow_in_place: Option<bool>,
    /// How often a running job's target is re-checked against `abort_at`.
    pub watchdog_secs: Option<u64>,
}

impl UserConfig {
    /// A missing file is an empty config. A present one must parse, name its
    /// schema, and hold every value in range.
    pub fn load(path: &Path) -> Res<UserConfig> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(UserConfig::default());
            }
            Err(error) => {
                return Err(Fail::config(format!(
                    "cannot read {}: {error}",
                    path.display()
                )));
            }
        };
        Self::parse(&text)
            .map_err(|fail| Fail::new(fail.exit, format!("{}: {}", path.display(), fail.message)))
    }

    pub fn parse(text: &str) -> Res<UserConfig> {
        let config: UserConfig =
            toml::from_str(text).map_err(|error| Fail::config(error.message().to_string()))?;
        match config.schema {
            Some(SCHEMA) => {}
            Some(other) => {
                return Err(Fail::config(format!(
                    "schema = {other} is not understood by this version (it reads schema = {SCHEMA})"
                )));
            }
            None => return Err(Fail::config(format!("missing `schema = {SCHEMA}`"))),
        }
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Res<()> {
        for (id, harness) in &self.harness {
            if let Some(cap) = harness.cap {
                in_range(&format!("harness.{id}.cap"), cap, &CAP)?;
            }
            if let Some(abort_at) = harness.abort_at {
                let cap = harness.cap.unwrap_or(crate::registry::DEFAULT_CAP);
                if abort_at <= cap || abort_at > 100 {
                    return Err(Fail::config(format!(
                        "harness.{id}.abort_at = {abort_at}: must be above the cap ({cap}) and at most 100"
                    )));
                }
            }
            if let Some(n) = harness.max_concurrent {
                in_range(&format!("harness.{id}.max_concurrent"), n, &MAX_CONCURRENT)?;
            }
            if let Some(binary) = &harness.binary
                && !binary.is_absolute()
            {
                return Err(Fail::config(format!(
                    "harness.{id}.binary must be an absolute path"
                )));
            }
        }
        for (role, entry) in &self.roles {
            if entry.candidates.is_empty() {
                return Err(Fail::config(format!(
                    "roles.{role}.candidates is empty — remove the table to get the defaults"
                )));
            }
        }
        for (role, share) in &self.explore.share {
            in_range(&format!("explore.share.{role}"), *share, &EXPLORE_SHARE)?;
        }
        for (name, entry) in &self.kinds {
            validate_kind_description(name, &entry.description)?;
            if let Some(share) = entry.explore.share {
                in_range(
                    &format!("kinds.{name}.explore.share"),
                    share,
                    &EXPLORE_SHARE,
                )?;
            }
            if entry.candidates.is_empty() {
                return Err(Fail::config(format!(
                    "kinds.{name}.candidates must contain at least one candidate"
                )));
            }
            for (at, candidate) in entry.candidates.iter().enumerate() {
                if entry.candidates[..at].contains(candidate) {
                    return Err(Fail::config(format!(
                        "kinds.{name}.candidates repeats {}:{}:{}",
                        candidate.harness,
                        candidate.model.as_str(),
                        candidate.effort
                    )));
                }
            }
        }
        let meter = &self.meter;
        for id in MeterId::ALL {
            if meter.binary(id).is_some_and(|binary| !binary.is_absolute()) {
                return Err(Fail::config(format!(
                    "meter.{id}.binary must be an absolute path"
                )));
            }
        }
        if let Some(age) = meter.agent_usage.as_ref().and_then(|t| t.max_data_age_secs) {
            in_range(
                "meter.agent-usage.max_data_age_secs",
                age,
                &MAX_DATA_AGE_SECS,
            )?;
        }
        let token_limits = [
            (
                "meter.ccusage.claude_block_tokens",
                meter.ccusage.as_ref().and_then(|t| t.claude_block_tokens),
            ),
            (
                "meter.ccusage.codex_day_tokens",
                meter.ccusage.as_ref().and_then(|t| t.codex_day_tokens),
            ),
            (
                "meter.ledger.max_tokens_per_day",
                meter.ledger.as_ref().and_then(|t| t.max_tokens_per_day),
            ),
        ];
        for (name, limit) in token_limits {
            if let Some(limit) = limit
                && limit < MIN_TOKEN_LIMIT
            {
                return Err(Fail::config(format!(
                    "{name} = {limit}: a limit is at least {MIN_TOKEN_LIMIT} tokens — did you mean {}?",
                    limit.saturating_mul(1_000_000)
                )));
            }
        }
        if let Some(runs) = meter.ledger.as_ref().and_then(|t| t.max_runs_per_hour) {
            in_range("meter.ledger.max_runs_per_hour", runs, &MAX_RUNS_PER_HOUR)?;
        }
        if let Some(rate) = self.review.sample_rate {
            in_range("review.sample_rate", rate, &SAMPLE_RATE)?;
        }
        if let Some(binary) = self
            .fork
            .daft
            .as_ref()
            .and_then(|daft| daft.binary.as_ref())
            && !binary.is_absolute()
        {
            return Err(Fail::config("fork.daft.binary must be an absolute path"));
        }
        let limits = &self.limits;
        let checks = [
            (
                "max_active_runs",
                limits.max_active_runs.map(u64::from),
                widen(&MAX_ACTIVE_RUNS),
            ),
            (
                "max_depth",
                limits.max_depth.map(u64::from),
                widen(&MAX_DEPTH),
            ),
            ("timeout_secs", limits.timeout_secs, TIMEOUT_SECS),
            ("wait_secs", limits.wait_secs, WAIT_SECS),
            ("int_grace_secs", limits.int_grace_secs, GRACE_SECS),
            ("term_grace_secs", limits.term_grace_secs, GRACE_SECS),
            ("watchdog_secs", limits.watchdog_secs, WATCHDOG_SECS),
        ];
        for (name, value, range) in checks {
            if let Some(value) = value {
                in_range(&format!("limits.{name}"), value, &range)?;
            }
        }
        Ok(())
    }
}

/// `name = value: must be low–high`, when it is not.
fn in_range<T: PartialOrd + std::fmt::Display>(
    name: &str,
    value: T,
    range: &RangeInclusive<T>,
) -> Res<()> {
    if range.contains(&value) {
        return Ok(());
    }
    Err(Fail::config(format!(
        "{name} = {value}: must be {}–{}",
        range.start(),
        range.end()
    )))
}

fn widen(range: &RangeInclusive<u32>) -> RangeInclusive<u64> {
    u64::from(*range.start())..=u64::from(*range.end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exit::Exit;

    #[test]
    fn a_full_config_parses() {
        let config = UserConfig::parse(
            r#"
            schema = 1
            [harness.codex]
            cap = 80
            binary = "/opt/homebrew/bin/codex"
            [harness.claude]
            cap = 50
            billing = "api"
            [roles.advise]
            candidates = [{ harness = "codex", model = "gpt-6-astra", effort = "high" }]
            [meter.agent-usage]
            binary = "/Applications/AgentUsage.app/Contents/MacOS/usage-cli"
            [meter.ledger]
            max_runs_per_hour = 6
            [limits]
            timeout_secs = 600
            "#,
        )
        .unwrap();
        assert_eq!(config.harness[&HarnessId::Codex].cap, Some(80));
        assert_eq!(
            config.harness[&HarnessId::Claude].billing,
            Some(Billing::Api)
        );
        assert_eq!(config.roles[&Role::Advise].candidates.len(), 1);
    }

    #[test]
    fn what_could_widen_authority_is_not_even_a_key() {
        for text in [
            "schema = 1\n[harness.codex]\nargs = [\"--dangerously-bypass-approvals-and-sandbox\"]",
            "schema = 1\n[harness.codex]\ncommand = \"sh -c evil\"",
            "schema = 1\n[harness.claude]\nsandbox = \"off\"",
            "schema = 1\nungated = true",
            "schema = 1\n[harness.gemini]\ncap = 50",
            "schema = 1\n[roles.deploy]\ncandidates = []",
        ] {
            let fail = UserConfig::parse(text).unwrap_err();
            assert_eq!(fail.exit, Exit::Config, "{text}");
        }
    }

    #[test]
    fn values_are_held_in_range_and_the_schema_is_required() {
        for text in [
            "",
            "schema = 2",
            "schema = 1\n[harness.codex]\ncap = 0",
            "schema = 1\n[harness.codex]\ncap = 101",
            "schema = 1\n[harness.codex]\nbinary = \"codex\"",
            "schema = 1\n[roles.advise]\ncandidates = []",
            "schema = 1\n[roles.advise]\ncandidates = [{ harness = \"codex\", model = \"--oss\", effort = \"high\" }]",
            "schema = 1\n[limits]\ntimeout_secs = 5",
            "schema = 1\n[review]\nsample_rate = 1.5",
            "schema = 1\n[review]\nauto_apply_everything = true",
            "schema = 1\n[harness.codex]\ncap = 80\nabort_at = 80",
            "schema = 1\n[harness.codex]\nabort_at = 60",
            "schema = 1\n[meter.ccusage]\nbinary = \"ccusage\"",
            "schema = 1\n[meter.agent-usage]\nbinary = \"usage-cli\"",
            "schema = 1\n[meter.ccusage]\nclaude_block_tokens = 300",
            "schema = 1\n[meter.ccusage]\ncodex_tokens = 50_000_000",
            "schema = 1\n[meter.codexbar]\nbinary = \"/usr/local/bin/codexbar\"",
        ] {
            assert!(UserConfig::parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn use_chooses_the_meter_and_a_table_only_configures_one() {
        let config = UserConfig::parse(
            "schema = 1\n[meter.ccusage]\nclaude_block_tokens = 300_000_000\ncodex_day_tokens = 60_000_000",
        )
        .unwrap();
        assert_eq!(config.meter.use_, None, "limits alone choose nothing");
        let ccusage = config.meter.ccusage.unwrap();
        assert_eq!(ccusage.claude_block_tokens, Some(300_000_000));
        assert_eq!(ccusage.binary, None, "where install found it");
        // Both tables at once is fine now: `use` says which is on.
        let both = UserConfig::parse(
            "schema = 1\n[meter]\nuse = \"agent-usage\"\n[meter.agent-usage]\n[meter.ccusage]\nclaude_block_tokens = 300_000_000",
        )
        .unwrap();
        assert_eq!(both.meter.use_, Some(Selection::Meter(MeterId::AgentUsage)));
        // `use` as a dotted key, with the meters' tables after it.
        let dotted = UserConfig::parse(
            "schema = 1\nmeter.use = \"none\"\n[meter.ccusage]\nbinary = \"/opt/homebrew/bin/ccusage\"",
        )
        .unwrap();
        assert_eq!(dotted.meter.use_, Some(Selection::NoMeter));
        assert_eq!(
            dotted.meter.binary(MeterId::Ccusage),
            Some(Path::new("/opt/homebrew/bin/ccusage"))
        );
        let unknown = UserConfig::parse("schema = 1\n[meter]\nuse = \"codexbar\"").unwrap_err();
        assert!(unknown.message.contains("codexbar"), "{}", unknown.message);
    }

    #[test]
    fn the_provider_is_a_name_and_daft_a_path() {
        let config = UserConfig::parse(
            "schema = 1\nfork.provider = \"daft\"\nfork.daft.binary = \"/opt/homebrew/bin/daft\"\nfork.daft.hooks = true",
        )
        .unwrap();
        assert_eq!(config.fork.provider, Some(ProviderId::Daft));
        let daft = config.fork.daft.unwrap();
        assert_eq!(
            daft.binary.as_deref(),
            Some(Path::new("/opt/homebrew/bin/daft"))
        );
        assert_eq!(daft.hooks, Some(true));
        let unknown =
            UserConfig::parse("schema = 1\n[fork]\nprovider = \"worktrunk\"").unwrap_err();
        assert_eq!(unknown.exit, Exit::Config);
        assert!(unknown.message.contains("worktrunk"), "{}", unknown.message);
        let relative = UserConfig::parse("schema = 1\n[fork.daft]\nbinary = \"daft\"").unwrap_err();
        assert_eq!(relative.exit, Exit::Config);
        assert_eq!(
            relative.message,
            "fork.daft.binary must be an absolute path"
        );
        for text in [
            "schema = 1\n[fork]\ncommand = \"daft start\"",
            "schema = 1\n[fork.daft]\nargs = [\"-x\"]",
            "schema = 1\n[fork.git]\nbinary = \"/usr/bin/git\"",
            "schema = 1\n[fork.daft]\nhooks = \"yes\"",
        ] {
            assert_eq!(
                UserConfig::parse(text).unwrap_err().exit,
                Exit::Config,
                "{text}"
            );
        }
    }

    #[test]
    fn a_harness_is_enabled_in_its_table() {
        let config = UserConfig::parse(
            "schema = 1\nharness.codex.enabled = true\n[harness.claude]\ncap = 60",
        )
        .unwrap();
        assert_eq!(config.harness[&HarnessId::Codex].enabled, Some(true));
        assert_eq!(config.harness[&HarnessId::Claude].enabled, None);
    }

    #[test]
    fn the_newer_knobs_are_held_in_range_too() {
        for text in [
            "schema = 1\n[meter.agent-usage]\nmax_data_age_secs = 30",
            "schema = 1\n[meter.agent-usage]\nmax_data_age_secs = 90_000",
            "schema = 1\n[meter.ledger]\nmax_runs_per_hour = 601",
            "schema = 1\n[meter.ledger]\nmax_tokens_per_day = 50",
        ] {
            assert!(UserConfig::parse(text).is_err(), "{text:?}");
        }
        for text in [
            "schema = 1\n[meter.agent-usage]\nmax_data_age_secs = 60",
            "schema = 1\n[meter.ledger]\nmax_runs_per_hour = 0",
            "schema = 1\n[meter.ledger]\nmax_tokens_per_day = 2_000_000",
        ] {
            assert!(UserConfig::parse(text).is_ok(), "{text:?}");
        }
    }

    const KIND: &str = "[kinds.rust-review]\ndescription = \"Review Rust.\"\nrole = \"review\"\ncandidates = [{ harness = \"codex\", model = \"m\", effort = \"high\" }, { harness = \"codex\", model = \"m\", effort = \"medium\" }]\n";

    #[test]
    fn kinds_require_complete_definitions_and_preserve_distinct_efforts() {
        assert!(UserConfig::parse("schema = 1").unwrap().kinds.is_empty());
        let config = UserConfig::parse(&format!("schema = 1\n{KIND}")).unwrap();
        let entry = config.kinds.values().next().unwrap();
        assert_eq!(entry.candidates.len(), 2);
        assert_eq!(entry.candidates[0].effort, crate::model::Effort::High);
        assert_eq!(entry.candidates[1].effort, crate::model::Effort::Medium);
        for field in ["description", "role", "candidates"] {
            let incomplete = KIND
                .lines()
                .filter(|line| !line.starts_with(field))
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(
                UserConfig::parse(&format!("schema = 1\n{incomplete}"))
                    .unwrap_err()
                    .exit,
                Exit::Config
            );
        }
        assert!(
            UserConfig::parse(&format!(
                "schema = 1\n{}",
                KIND.replace("kinds.rust-review", "kinds.\"rust-review\"")
            ))
            .is_ok()
        );
    }

    #[test]
    fn kind_validation_refuses_bad_text_duplicates_and_authority_fields() {
        for bad in [
            "",
            "  ",
            "a\nb",
            "a\rb",
            "a\tb",
            "a\u{1b}b",
            "a\u{2028}b",
            "a\u{2029}b",
            &"é".repeat(1025),
        ] {
            let text = KIND.replace("\"Review Rust.\"", &serde_json::to_string(bad).unwrap());
            let fail = UserConfig::parse(&format!("schema = 1\n{text}")).unwrap_err();
            assert_eq!(fail.exit, Exit::Config, "{bad:?}");
            assert_eq!(
                fail.message,
                "kinds.rust-review.description must be nonblank, one line, at most 1024 characters, and contain no control characters",
                "{bad:?}"
            );
        }
        let name = TaskKindName::try_from("rust-review".to_string()).unwrap();
        assert!(validate_kind_description(&name, &"é".repeat(1024)).is_ok());
        for (text, message) in [
            (
                KIND.replace("effort = \"medium\"", "effort = \"high\""),
                "kinds.rust-review.candidates repeats codex:m:high",
            ),
            (
                KIND.replace("candidates = [", "candidates = []\n# ["),
                "kinds.rust-review.candidates must contain at least one candidate",
            ),
        ] {
            let fail = UserConfig::parse(&format!("schema = 1\n{text}")).unwrap_err();
            assert_eq!(fail.exit, Exit::Config);
            assert_eq!(fail.message, message);
        }
        for text in [
            KIND.replace("role = \"review\"", "role = \"deploy\""),
            KIND.replace("harness = \"codex\"", "harness = \"other\""),
            KIND.replace("model = \"m\"", "model = \"--flag\""),
            KIND.replace("effort = \"high\"", "effort = \"ultra\""),
        ] {
            assert_eq!(
                UserConfig::parse(&format!("schema = 1\n{text}"))
                    .unwrap_err()
                    .exit,
                Exit::Config
            );
        }
        for field in [
            "calibrate = true",
            "cap = 99",
            "reserve = 0",
            "sandbox = \"write\"",
            "exploration = 1",
            "flags = []",
        ] {
            assert!(UserConfig::parse(&format!("schema = 1\n{KIND}{field}")).is_err());
        }
    }

    #[test]
    fn exploration_config_defaults_inherits_and_refuses_bad_data() {
        let none = UserConfig::parse("schema = 1").unwrap();
        assert!(none.explore.share.is_empty(), "every omitted share is zero");
        let config = UserConfig::parse(&format!(
            "schema = 1\n[explore]\nshare = {{ advise = 0.1, review = 0.05, implement = 1 }}\n{KIND}[kinds.rust-review.explore]\nshare = 0.0\n"
        ))
        .unwrap();
        assert_eq!(config.explore.share[&Role::Advise], 0.1);
        assert_eq!(config.explore.share[&Role::Implement], 1.0);
        assert!(!config.explore.share.contains_key(&Role::Explore));
        assert_eq!(
            config.kinds.values().next().unwrap().explore.share,
            Some(0.0),
            "an explicit zero is a value, not an absence"
        );
        assert_eq!(
            UserConfig::parse(&format!("schema = 1\n{KIND}"))
                .unwrap()
                .kinds
                .values()
                .next()
                .unwrap()
                .explore
                .share,
            None
        );
        // The endpoints are fine.
        for share in ["0", "0.0", "1", "1.0"] {
            assert!(
                UserConfig::parse(&format!("schema = 1\n[explore.share]\nadvise = {share}"))
                    .is_ok()
            );
        }
        // Out of range, not a number, NaN and infinities: all refused, by key.
        for (share, shown) in [
            ("1.5", "1.5"),
            ("-0.1", "-0.1"),
            ("nan", "NaN"),
            ("inf", "inf"),
            ("-inf", "-inf"),
        ] {
            let fail = UserConfig::parse(&format!("schema = 1\n[explore.share]\nadvise = {share}"))
                .unwrap_err();
            assert_eq!(fail.exit, Exit::Config, "{share}");
            assert_eq!(
                fail.message,
                format!("explore.share.advise = {shown}: must be 0–1"),
                "{share}"
            );
            let fail = UserConfig::parse(&format!(
                "schema = 1\n{KIND}[kinds.rust-review.explore]\nshare = {share}"
            ))
            .unwrap_err();
            assert_eq!(fail.exit, Exit::Config, "{share}");
            assert_eq!(
                fail.message,
                format!("kinds.rust-review.explore.share = {shown}: must be 0–1"),
                "{share}"
            );
        }
        for text in [
            "schema = 1\n[explore.share]\nadvise = \"high\"",
            "schema = 1\n[explore.share]\nadvise = true",
            "schema = 1\n[explore.share]\ndeploy = 0.5",
            "schema = 1\n[explore]\nrate = 0.5",
            "schema = 1\n[explore]\nflags = []",
            "schema = 1\n[explore]\nshare = 0.5",
        ] {
            assert_eq!(
                UserConfig::parse(text).unwrap_err().exit,
                Exit::Config,
                "{text}"
            );
        }
        for extra in [
            "[kinds.rust-review.explore]\nrate = 0.5",
            "[kinds.rust-review.explore]\ncandidates = []",
            "[kinds.rust-review.explore]\nshare = \"1\"",
        ] {
            assert_eq!(
                UserConfig::parse(&format!("schema = 1\n{KIND}{extra}"))
                    .unwrap_err()
                    .exit,
                Exit::Config,
                "{extra}"
            );
        }
        // A nested exploration table cannot make a kind exist.
        assert_eq!(
            UserConfig::parse("schema = 1\n[kinds.rust-review.explore]\nshare = 0.2")
                .unwrap_err()
                .exit,
            Exit::Config
        );
    }
}
