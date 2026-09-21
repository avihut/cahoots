//! The user's file: `<config>/config.toml`. Typed knobs only — there is no
//! command template and no free-form flag list, so nothing written here can
//! widen what cahoots may do (hard rule 6). Unknown keys are errors: a typo
//! in a cap must not silently mean "no cap".
//!
//! Never read from the working directory. A repository cannot configure the
//! tool that is about to run on it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::exit::{Fail, Res};
use crate::model::{Candidate, HarnessId, Role};

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
    pub meter: MeterConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub review: ReviewConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessConfig {
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

/// `[meter.<id>]` turns that usage meter on — one at a time — and holds its
/// knobs. Without one, the meter `install` chose is on (`meter.rs`).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeterConfig {
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

/// A declared limit below this is a unit slip (millions meant), not a plan:
/// one delegated run can take more than that.
pub const MIN_TOKEN_LIMIT: u64 = 100_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerConfig {
    pub max_runs_per_hour: Option<u32>,
    pub max_tokens_per_day: Option<u64>,
}

/// Review and local learning. OFF unless a person turns it on: it spends the
/// reviewing harness's own plan, and it changes how cahoots behaves over time.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewConfig {
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
            if let Some(cap) = harness.cap
                && !(1..=100).contains(&cap)
            {
                return Err(Fail::config(format!(
                    "harness.{id}.cap = {cap}: must be 1–100"
                )));
            }
            if let Some(abort_at) = harness.abort_at {
                let cap = harness.cap.unwrap_or(crate::registry::DEFAULT_CAP);
                if abort_at <= cap || abort_at > 100 {
                    return Err(Fail::config(format!(
                        "harness.{id}.abort_at = {abort_at}: must be above the cap ({cap}) and at most 100"
                    )));
                }
            }
            if let Some(n) = harness.max_concurrent
                && !(1..=8).contains(&n)
            {
                return Err(Fail::config(format!(
                    "harness.{id}.max_concurrent = {n}: must be 1–8"
                )));
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
        let meter = &self.meter;
        if meter.agent_usage.is_some() && meter.ccusage.is_some() {
            return Err(Fail::config(
                "[meter.agent-usage] and [meter.ccusage]: one usage meter at a time — keep the one you want",
            ));
        }
        let binaries = [
            (
                "agent-usage",
                meter.agent_usage.as_ref().and_then(|m| m.binary.as_ref()),
            ),
            (
                "ccusage",
                meter.ccusage.as_ref().and_then(|m| m.binary.as_ref()),
            ),
        ];
        for (id, binary) in binaries {
            if binary.is_some_and(|binary| !binary.is_absolute()) {
                return Err(Fail::config(format!(
                    "meter.{id}.binary must be an absolute path"
                )));
            }
        }
        if let Some(ccusage) = &meter.ccusage {
            for (name, limit) in [
                ("claude_block_tokens", ccusage.claude_block_tokens),
                ("codex_day_tokens", ccusage.codex_day_tokens),
            ] {
                if let Some(limit) = limit
                    && limit < MIN_TOKEN_LIMIT
                {
                    return Err(Fail::config(format!(
                        "meter.ccusage.{name} = {limit}: a limit is at least {MIN_TOKEN_LIMIT} tokens — did you mean {}?",
                        limit.saturating_mul(1_000_000)
                    )));
                }
            }
        }
        if let Some(rate) = self.review.sample_rate
            && !(0.0..=1.0).contains(&rate)
        {
            return Err(Fail::config(format!(
                "review.sample_rate = {rate}: must be 0.0–1.0"
            )));
        }
        let limits = &self.limits;
        let in_range =
            |name: &str, value: Option<u64>, range: std::ops::RangeInclusive<u64>| match value {
                Some(v) if !range.contains(&v) => Err(Fail::config(format!(
                    "limits.{name} = {v}: must be {}–{}",
                    range.start(),
                    range.end()
                ))),
                _ => Ok(()),
            };
        in_range(
            "max_active_runs",
            limits.max_active_runs.map(u64::from),
            1..=16,
        )?;
        in_range("max_depth", limits.max_depth.map(u64::from), 0..=3)?;
        in_range("timeout_secs", limits.timeout_secs, 30..=14_400)?;
        in_range("wait_secs", limits.wait_secs, 0..=540)?;
        in_range("int_grace_secs", limits.int_grace_secs, 1..=60)?;
        in_range("term_grace_secs", limits.term_grace_secs, 1..=60)?;
        in_range("watchdog_secs", limits.watchdog_secs, 1..=3600)?;
        Ok(())
    }
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
    fn one_usage_meter_at_a_time() {
        let config = UserConfig::parse(
            "schema = 1\n[meter.ccusage]\nclaude_block_tokens = 300_000_000\ncodex_day_tokens = 60_000_000",
        )
        .unwrap();
        let ccusage = config.meter.ccusage.unwrap();
        assert_eq!(ccusage.claude_block_tokens, Some(300_000_000));
        assert_eq!(ccusage.binary, None, "install's choice, or PATH");
        assert!(UserConfig::parse("schema = 1\n[meter.agent-usage]").is_ok());
        let both = UserConfig::parse(
            "schema = 1\n[meter.agent-usage]\n[meter.ccusage]\nclaude_block_tokens = 300_000_000",
        )
        .unwrap_err();
        assert!(
            both.message.contains("one usage meter at a time"),
            "{}",
            both.message
        );
    }
}
