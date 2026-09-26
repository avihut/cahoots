//! Every setting a person can change, as data: where it lives in config.toml,
//! what it may be, what it is now and why. `cahoots settings` shows them on a
//! page and changes them one at a time; `settings set` and `settings reset` do
//! the same without one. Both write through `config::edit`, so config.toml
//! stays the person's own file. There are no words for a person here (hard
//! rule 11): `src/cli/settings.rs` has those.
//!
//! The defaults are the registry's own — an empty config's registry — so a
//! default is written down once. Reading the settings never runs anything:
//! programs are found on PATH and in `meter.json`, not asked for a version.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use toml_edit::{Array, InlineTable};

use crate::config::edit::{self, Change, KeyPath};
use crate::config::{self, UserConfig};
use crate::exit::{Exit, Fail, Res};
use crate::meter::{Exe, MeterFile, MeterId, Selection};
use crate::model::{Candidate, HarnessId, ModelName, Role};
use crate::registry::{self, Registry};
use crate::spawn;

/// A setting, by what it sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Enabled(HarnessId),
    Cap(HarnessId),
    AbortAt(HarnessId),
    MaxConcurrent(HarnessId),
    Billing(HarnessId),
    Binary(HarnessId),
    /// `meter.use`: which usage meter judges the gate.
    Meter,
    MeterBinary(MeterId),
    MaxDataAge,
    ClaudeBlockTokens,
    CodexDayTokens,
    RunsPerHour,
    TokensPerDay,
    MaxActiveRuns,
    MaxDepth,
    Timeout,
    Wait,
    IntGrace,
    TermGrace,
    Watchdog,
    AllowInPlace,
    Review,
    SampleRate,
    ApplyRouting,
    Candidates(Role),
    Calibrate(Role),
}

/// Where a setting is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Harness(HarnessId),
    Meter,
    Runs,
    Review,
    Roles,
}

impl Key {
    /// Every setting, in the order the page shows them.
    pub fn all() -> Vec<Key> {
        use Key::*;
        let mut keys = Vec::new();
        for id in HarnessId::ALL {
            keys.extend([
                Enabled(id),
                Cap(id),
                AbortAt(id),
                MaxConcurrent(id),
                Billing(id),
                Binary(id),
            ]);
        }
        keys.extend([
            Meter,
            MeterBinary(MeterId::AgentUsage),
            MaxDataAge,
            MeterBinary(MeterId::Ccusage),
            ClaudeBlockTokens,
            CodexDayTokens,
            RunsPerHour,
            TokensPerDay,
            MaxActiveRuns,
            MaxDepth,
            Timeout,
            Wait,
            IntGrace,
            TermGrace,
            Watchdog,
            AllowInPlace,
            Review,
            SampleRate,
            ApplyRouting,
        ]);
        for role in Role::ALL {
            keys.extend([Candidates(role), Calibrate(role)]);
        }
        keys
    }

    /// Its dotted path in config.toml: `harness.codex.cap`.
    pub fn name(self) -> String {
        use Key::*;
        match self {
            Enabled(id) => format!("harness.{id}.enabled"),
            Cap(id) => format!("harness.{id}.cap"),
            AbortAt(id) => format!("harness.{id}.abort_at"),
            MaxConcurrent(id) => format!("harness.{id}.max_concurrent"),
            Billing(id) => format!("harness.{id}.billing"),
            Binary(id) => format!("harness.{id}.binary"),
            Meter => "meter.use".to_string(),
            MeterBinary(id) => format!("meter.{id}.binary"),
            MaxDataAge => "meter.agent-usage.max_data_age_secs".to_string(),
            ClaudeBlockTokens => "meter.ccusage.claude_block_tokens".to_string(),
            CodexDayTokens => "meter.ccusage.codex_day_tokens".to_string(),
            RunsPerHour => "meter.ledger.max_runs_per_hour".to_string(),
            TokensPerDay => "meter.ledger.max_tokens_per_day".to_string(),
            MaxActiveRuns => "limits.max_active_runs".to_string(),
            MaxDepth => "limits.max_depth".to_string(),
            Timeout => "limits.timeout_secs".to_string(),
            Wait => "limits.wait_secs".to_string(),
            IntGrace => "limits.int_grace_secs".to_string(),
            TermGrace => "limits.term_grace_secs".to_string(),
            Watchdog => "limits.watchdog_secs".to_string(),
            AllowInPlace => "limits.allow_in_place".to_string(),
            Review => "review.enabled".to_string(),
            SampleRate => "review.sample_rate".to_string(),
            ApplyRouting => "review.apply_routing".to_string(),
            Candidates(role) => format!("roles.{role}.candidates"),
            Calibrate(role) => format!("roles.{role}.calibrate"),
        }
    }

    pub fn parse(name: &str) -> Option<Key> {
        Key::all().into_iter().find(|key| key.name() == name)
    }

    pub fn section(self) -> Section {
        use Key::*;
        match self {
            Enabled(id) | Cap(id) | AbortAt(id) | MaxConcurrent(id) | Billing(id) | Binary(id) => {
                Section::Harness(id)
            }
            Meter | MeterBinary(_) | MaxDataAge | ClaudeBlockTokens | CodexDayTokens
            | RunsPerHour | TokensPerDay => Section::Meter,
            MaxActiveRuns | MaxDepth | Timeout | Wait | IntGrace | TermGrace | Watchdog
            | AllowInPlace => Section::Runs,
            Review | SampleRate | ApplyRouting => Section::Review,
            Candidates(_) | Calibrate(_) => Section::Roles,
        }
    }

    /// What a reset takes out of the file: the key, or for a role's list the
    /// role's whole table (its `calibrate` belongs to the list).
    fn reset_path(self) -> KeyPath {
        match self {
            Key::Candidates(role) => KeyPath::of(&format!("roles.{role}")),
            _ => KeyPath::of(&self.name()),
        }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// What a setting may be.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Toggle,
    /// One of these names.
    Choice(Vec<&'static str>),
    /// A whole number from `min` to `max`, in `unit`.
    Number {
        min: i64,
        max: i64,
        unit: Unit,
    },
    /// A share, from 0.0 to 1.0.
    Share,
    /// A program: one of these, or any other that passes the binary policy.
    Program(Vec<Program>),
    /// A role's candidates, first choice first. Their order is what changes.
    Order,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Percent,
    Seconds,
    Tokens,
    Count,
}

/// A copy of a program that could be the one used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub path: PathBuf,
    pub from: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// On this PATH.
    Path,
    /// Where `install` found it.
    Install,
    /// config.toml names it.
    Config,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Number(i64),
    Share(f64),
    /// One of a `Choice`'s names.
    Name(String),
    Program(PathBuf),
    Candidates(Vec<Candidate>),
}

/// Where a value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Nothing sets it.
    Default,
    /// config.toml.
    Config,
    /// What `install` found: a meter's program, or the only meter found.
    Found,
}

impl Origin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Origin::Default => "default",
            Origin::Config => "config",
            Origin::Found => "found",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub key: Key,
    pub kind: Kind,
    /// What it is now: config.toml's value, else what it would be without
    /// one. `None` when there is nothing: a limit not set, a program not
    /// found.
    pub value: Option<Value>,
    pub default: Option<Value>,
    pub origin: Origin,
    /// It cannot be set as things are: where runs stop must be above the
    /// cap, and the cap is 100.
    pub locked: bool,
}

/// Every setting as it is now. `found` is what `install` found
/// (`meter.json`); `path` is the PATH the harnesses' programs are looked for
/// on. A role's `calibrate` is there only once its list is the person's own:
/// the defaults always calibrate.
pub fn current(
    config: &UserConfig,
    found: Option<&MeterFile>,
    path: Option<&OsStr>,
) -> Vec<Setting> {
    let now = Registry::effective(config);
    let base = Registry::effective(&UserConfig::default());
    let origin = |set: bool| if set { Origin::Config } else { Origin::Default };
    let mut all = Vec::new();

    for id in HarnessId::ALL {
        let user = config.harness.get(&id);
        let set = |field: fn(&config::HarnessConfig) -> bool| origin(user.is_some_and(field));
        let entry = now.harness(id);
        let bool_of = |on: bool| Some(Value::Bool(on));
        let number = |n: u64| Some(Value::Number(n as i64));
        all.push(Setting::new(
            Key::Enabled(id),
            Kind::Toggle,
            bool_of(entry.enabled),
            bool_of(base.harness(id).enabled),
            set(|u| u.enabled.is_some()),
        ));
        all.push(Setting::new(
            Key::Cap(id),
            number_kind(&config::CAP, Unit::Percent),
            number(entry.cap.into()),
            number(base.harness(id).cap.into()),
            set(|u| u.cap.is_some()),
        ));
        all.push(Setting {
            // Where runs stop is above the cap: at a cap of 100, nowhere.
            locked: entry.cap >= 100,
            ..Setting::new(
                Key::AbortAt(id),
                Kind::Number {
                    min: i64::from(entry.cap) + 1,
                    max: 100,
                    unit: Unit::Percent,
                },
                number(entry.abort_at.into()),
                number(registry::default_abort_at(entry.cap).into()),
                set(|u| u.abort_at.is_some()),
            )
        });
        all.push(Setting::new(
            Key::MaxConcurrent(id),
            number_kind(&config::MAX_CONCURRENT, Unit::Count),
            number(entry.max_concurrent.into()),
            number(base.harness(id).max_concurrent.into()),
            set(|u| u.max_concurrent.is_some()),
        ));
        all.push(Setting::new(
            Key::Billing(id),
            Kind::Choice(vec!["subscription", "api"]),
            Some(Value::Name(billing(entry.billing))),
            Some(Value::Name(billing(base.harness(id).billing))),
            set(|u| u.billing.is_some()),
        ));
        let name = crate::harness::harness(id).binary_name();
        let on_path: Vec<PathBuf> = spawn::find_all_on_path(name, path)
            .into_iter()
            .filter(|binary| spawn::resolve_binary(name, Some(binary), &[]).is_ok())
            .collect();
        let pinned = user.and_then(|u| u.binary.clone());
        let programs = programs(pinned.as_deref(), on_path.iter().map(|p| (p, Source::Path)));
        let first = on_path.first().cloned().map(Value::Program);
        all.push(Setting::new(
            Key::Binary(id),
            Kind::Program(programs),
            pinned.clone().map(Value::Program).or(first.clone()),
            first,
            origin(pinned.is_some()),
        ));
    }

    let meter = &config.meter;
    let only = found.and_then(MeterFile::only);
    let (value, chosen_by) = match (meter.use_, only) {
        (Some(chosen), _) => (chosen, Origin::Config),
        (None, Some(only)) => (Selection::Meter(only), Origin::Found),
        (None, None) => (Selection::NoMeter, Origin::Default),
    };
    let name = |selection: Selection| Some(Value::Name(selection.as_str().to_string()));
    all.push(Setting {
        key: Key::Meter,
        kind: Kind::Choice(vec!["agent-usage", "ccusage", "none"]),
        value: name(value),
        default: name(only.map_or(Selection::NoMeter, Selection::Meter)),
        origin: chosen_by,
        locked: false,
    });
    for id in MeterId::ALL {
        let at = found.and_then(|file| file.found.get(&id)).cloned();
        let pinned = meter.binary(id).map(Path::to_path_buf);
        let programs = programs(pinned.as_deref(), at.iter().map(|p| (p, Source::Install)));
        let (value, from) = match (&pinned, &at) {
            (Some(pinned), _) => (Some(pinned.clone()), Origin::Config),
            (None, Some(at)) => (Some(at.clone()), Origin::Found),
            (None, None) => (None, Origin::Default),
        };
        all.push(Setting {
            key: Key::MeterBinary(id),
            kind: Kind::Program(programs),
            value: value.map(Value::Program),
            default: at.map(Value::Program),
            origin: from,
            locked: false,
        });
        match id {
            MeterId::AgentUsage => {
                let age = meter.agent_usage.as_ref().and_then(|t| t.max_data_age_secs);
                all.push(Setting {
                    key: Key::MaxDataAge,
                    kind: number_kind(&config::MAX_DATA_AGE_SECS, Unit::Seconds),
                    value: Some(Value::Number(
                        age.unwrap_or(registry::DEFAULT_MAX_DATA_AGE_SECS) as i64,
                    )),
                    default: Some(Value::Number(registry::DEFAULT_MAX_DATA_AGE_SECS as i64)),
                    origin: origin(age.is_some()),
                    locked: false,
                });
            }
            MeterId::Ccusage => {
                let table = meter.ccusage.as_ref();
                for (key, limit) in [
                    (
                        Key::ClaudeBlockTokens,
                        table.and_then(|t| t.claude_block_tokens),
                    ),
                    (Key::CodexDayTokens, table.and_then(|t| t.codex_day_tokens)),
                ] {
                    all.push(tokens(key, limit));
                }
            }
        }
    }
    let ledger = meter.ledger.as_ref();
    let runs = ledger.and_then(|t| t.max_runs_per_hour);
    all.push(Setting {
        key: Key::RunsPerHour,
        kind: number_kind(&config::MAX_RUNS_PER_HOUR, Unit::Count),
        value: Some(Value::Number(now.meters.ledger_max_runs_per_hour.into())),
        default: Some(Value::Number(base.meters.ledger_max_runs_per_hour.into())),
        origin: origin(runs.is_some()),
        locked: false,
    });
    all.push(tokens(
        Key::TokensPerDay,
        ledger.and_then(|t| t.max_tokens_per_day),
    ));

    let limits = &config.limits;
    let (now_limits, base_limits) = (&now.limits, &base.limits);
    for (key, range, unit, value, default, set) in [
        (
            Key::MaxActiveRuns,
            widen(&config::MAX_ACTIVE_RUNS),
            Unit::Count,
            now_limits.max_active_runs.into(),
            base_limits.max_active_runs.into(),
            limits.max_active_runs.is_some(),
        ),
        (
            Key::MaxDepth,
            widen(&config::MAX_DEPTH),
            Unit::Count,
            now_limits.max_depth.into(),
            base_limits.max_depth.into(),
            limits.max_depth.is_some(),
        ),
        (
            Key::Timeout,
            config::TIMEOUT_SECS,
            Unit::Seconds,
            now_limits.timeout_secs,
            base_limits.timeout_secs,
            limits.timeout_secs.is_some(),
        ),
        (
            Key::Wait,
            config::WAIT_SECS,
            Unit::Seconds,
            now_limits.wait_secs,
            base_limits.wait_secs,
            limits.wait_secs.is_some(),
        ),
        (
            Key::IntGrace,
            config::GRACE_SECS,
            Unit::Seconds,
            now_limits.int_grace_secs,
            base_limits.int_grace_secs,
            limits.int_grace_secs.is_some(),
        ),
        (
            Key::TermGrace,
            config::GRACE_SECS,
            Unit::Seconds,
            now_limits.term_grace_secs,
            base_limits.term_grace_secs,
            limits.term_grace_secs.is_some(),
        ),
        (
            Key::Watchdog,
            config::WATCHDOG_SECS,
            Unit::Seconds,
            now_limits.watchdog_secs,
            base_limits.watchdog_secs,
            limits.watchdog_secs.is_some(),
        ),
    ] {
        all.push(Setting {
            key,
            kind: number_kind(&range, unit),
            value: Some(Value::Number(value as i64)),
            default: Some(Value::Number(default as i64)),
            origin: origin(set),
            locked: false,
        });
    }
    let review = &config.review;
    for (key, value, default, set) in [
        (
            Key::AllowInPlace,
            now_limits.allow_in_place,
            base_limits.allow_in_place,
            limits.allow_in_place.is_some(),
        ),
        (
            Key::Review,
            now.review.enabled,
            base.review.enabled,
            review.enabled.is_some(),
        ),
    ] {
        all.push(toggle(key, value, default, origin(set)));
    }
    all.push(Setting {
        key: Key::SampleRate,
        kind: Kind::Share,
        value: Some(Value::Share(now.review.sample_rate)),
        default: Some(Value::Share(base.review.sample_rate)),
        origin: origin(review.sample_rate.is_some()),
        locked: false,
    });
    all.push(toggle(
        Key::ApplyRouting,
        now.review.apply_routing,
        base.review.apply_routing,
        origin(review.apply_routing.is_some()),
    ));

    for role in Role::ALL {
        let user = config.roles.get(&role);
        all.push(Setting {
            key: Key::Candidates(role),
            kind: Kind::Order,
            value: Some(Value::Candidates(now.roles[&role].candidates.clone())),
            default: Some(Value::Candidates(base.roles[&role].candidates.clone())),
            origin: origin(user.is_some()),
            locked: false,
        });
        if let Some(user) = user {
            all.push(toggle(
                Key::Calibrate(role),
                now.roles[&role].calibrate,
                false,
                origin(user.calibrate.is_some()),
            ));
        }
    }
    all
}

fn number_kind<T: Copy>(range: &std::ops::RangeInclusive<T>, unit: Unit) -> Kind
where
    i64: TryFrom<T>,
{
    let whole = |n: T| i64::try_from(n).unwrap_or(i64::MAX);
    Kind::Number {
        min: whole(*range.start()),
        max: whole(*range.end()),
        unit,
    }
}

fn widen(range: &std::ops::RangeInclusive<u32>) -> std::ops::RangeInclusive<u64> {
    u64::from(*range.start())..=u64::from(*range.end())
}

impl From<u64> for Value {
    fn from(n: u64) -> Value {
        Value::Number(n as i64)
    }
}

fn toggle(key: Key, value: bool, default: bool, origin: Origin) -> Setting {
    Setting::new(
        key,
        Kind::Toggle,
        Some(Value::Bool(value)),
        Some(Value::Bool(default)),
        origin,
    )
}

impl Setting {
    fn new(
        key: Key,
        kind: Kind,
        value: Option<Value>,
        default: Option<Value>,
        origin: Origin,
    ) -> Setting {
        Setting {
            key,
            kind,
            value,
            default,
            origin,
            locked: false,
        }
    }
}

/// A token limit: none unless one is declared.
fn tokens(key: Key, limit: Option<u64>) -> Setting {
    Setting {
        key,
        kind: Kind::Number {
            min: config::MIN_TOKEN_LIMIT as i64,
            max: i64::MAX,
            unit: Unit::Tokens,
        },
        value: limit.map(Value::from),
        default: None,
        origin: if limit.is_some() {
            Origin::Config
        } else {
            Origin::Default
        },
        locked: false,
    }
}

fn billing(billing: config::Billing) -> String {
    match billing {
        config::Billing::Subscription => "subscription",
        config::Billing::Api => "api",
    }
    .to_string()
}

/// The copies to choose from: the one config.toml names first, then the
/// others, each once.
fn programs<'a>(
    pinned: Option<&Path>,
    others: impl Iterator<Item = (&'a PathBuf, Source)>,
) -> Vec<Program> {
    let mut programs: Vec<Program> = pinned
        .map(|path| Program {
            path: path.to_path_buf(),
            from: Source::Config,
        })
        .into_iter()
        .collect();
    for (path, from) in others {
        if !programs.iter().any(|program| program.path == *path) {
            programs.push(Program {
                path: path.clone(),
                from,
            });
        }
    }
    programs
}

/// The setting called `name` among `settings`.
pub fn find<'a>(settings: &'a [Setting], name: &str) -> Res<&'a Setting> {
    if let Some(setting) = settings.iter().find(|s| s.key.name() == name) {
        return Ok(setting);
    }
    Err(Fail::new(
        Exit::Usage,
        match Key::parse(name) {
            Some(Key::Calibrate(role)) => format!(
                "{name} belongs to a list of your own — set roles.{role}.candidates first (the \
                 default lists always calibrate)"
            ),
            _ => format!("no setting is called {name:?}"),
        },
    ))
}

impl Setting {
    /// `text` as a value for this setting, the way a command line writes it:
    /// `on`/`off` (or `true`/`false`), a number in config.toml's units, a
    /// share as a fraction, an absolute path, or a role's candidates as
    /// `harness:model:effort,…`.
    pub fn parse(&self, text: &str) -> Res<Value> {
        let key = self.key;
        let refuse = |why: String| Fail::new(Exit::Usage, format!("{key} = {text}: {why}"));
        if self.locked {
            return Err(refuse(
                "it cannot be set while the cap is 100: nothing is above it".to_string(),
            ));
        }
        Ok(match &self.kind {
            Kind::Toggle => match text {
                "on" | "true" => Value::Bool(true),
                "off" | "false" => Value::Bool(false),
                _ => return Err(refuse("on or off".to_string())),
            },
            Kind::Choice(names) => match names.iter().find(|name| **name == text) {
                Some(name) => Value::Name(name.to_string()),
                None => return Err(refuse(format!("one of {}", names.join(", ")))),
            },
            Kind::Number { min, max, .. } => {
                let n: i64 = text
                    .replace('_', "")
                    .parse()
                    .map_err(|_| refuse("a whole number".to_string()))?;
                if n < *min || n > *max {
                    return Err(refuse(if *max == i64::MAX {
                        format!("at least {min}")
                    } else {
                        format!("{min}–{max}")
                    }));
                }
                Value::Number(n)
            }
            Kind::Share => match text.parse::<f64>() {
                Ok(share) if config::SAMPLE_RATE.contains(&share) => Value::Share(share),
                _ => return Err(refuse("a share from 0.0 to 1.0".to_string())),
            },
            Kind::Program(_) => {
                let path = PathBuf::from(text);
                if !path.is_absolute() {
                    return Err(refuse("an absolute path".to_string()));
                }
                let held = match key {
                    Key::Binary(id) => spawn::resolve_binary(
                        crate::harness::harness(id).binary_name(),
                        Some(&path),
                        &[],
                    )
                    .map(drop),
                    Key::MeterBinary(id) => Exe::pin(id, &path).map(drop),
                    _ => Ok(()),
                };
                held.map_err(|fail| refuse(fail.message))?;
                Value::Program(path)
            }
            Kind::Order => {
                let candidates = text
                    .split(',')
                    .map(|one| candidate(one.trim()))
                    .collect::<Result<Vec<_>, String>>()
                    .map_err(refuse)?;
                if candidates.is_empty() {
                    return Err(refuse("at least one candidate".to_string()));
                }
                Value::Candidates(candidates)
            }
        })
    }
}

/// `codex:gpt-5.6-sol:high`.
fn candidate(text: &str) -> Result<Candidate, String> {
    let [harness, model, effort] = text.split(':').collect::<Vec<_>>()[..] else {
        return Err(format!("{text:?} is not harness:model:effort"));
    };
    Ok(Candidate {
        harness: harness.parse().map_err(|fail: Fail| fail.message)?,
        model: ModelName::try_from(model.to_string())?,
        effort: effort.parse().map_err(|fail: Fail| fail.message)?,
    })
}

impl Value {
    /// As config.toml writes it. A role's candidates go one to a line.
    pub fn to_toml(&self) -> toml_edit::Value {
        match self {
            Value::Bool(on) => (*on).into(),
            Value::Number(n) => (*n).into(),
            Value::Share(share) => (*share).into(),
            Value::Name(name) => name.as_str().into(),
            Value::Program(path) => path.to_string_lossy().as_ref().into(),
            Value::Candidates(candidates) => {
                let mut list = Array::new();
                for candidate in candidates {
                    let mut entry = InlineTable::new();
                    entry.insert("harness", candidate.harness.as_str().into());
                    entry.insert("model", candidate.model.as_str().into());
                    entry.insert("effort", candidate.effort.as_str().into());
                    list.push(entry);
                }
                for entry in list.iter_mut() {
                    entry.decor_mut().set_prefix("\n  ");
                }
                list.set_trailing("\n");
                list.set_trailing_comma(true);
                list.into()
            }
        }
    }

    /// As the JSON envelope says it.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Bool(on) => (*on).into(),
            Value::Number(n) => (*n).into(),
            Value::Share(share) => (*share).into(),
            Value::Name(_) | Value::Program(_) | Value::Candidates(_) => self.to_string().into(),
        }
    }
}

/// As a command line writes it (`Setting::parse` reads it back).
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bool(on) => f.write_str(if *on { "on" } else { "off" }),
            Value::Number(n) => write!(f, "{n}"),
            Value::Share(share) => write!(f, "{share}"),
            Value::Name(name) => f.write_str(name),
            Value::Program(path) => write!(f, "{}", path.display()),
            Value::Candidates(candidates) => {
                let each: Vec<String> = candidates
                    .iter()
                    .map(|c| format!("{}:{}:{}", c.harness, c.model.as_str(), c.effort))
                    .collect();
                f.write_str(&each.join(","))
            }
        }
    }
}

/// Sets one setting in config.toml, and returns the config it now holds. A
/// change the config refuses (a cap above where runs stop) is `Usage`, and
/// the file is left as it was.
pub fn set(file: &Path, key: Key, value: &Value) -> Res<UserConfig> {
    edit::apply(
        file,
        &[Change::Set {
            path: KeyPath::of(&key.name()),
            value: value.to_toml(),
        }],
    )
}

/// Takes one setting out of config.toml, so its default applies again.
pub fn reset(file: &Path, key: Key) -> Res<UserConfig> {
    edit::apply(
        file,
        &[Change::Remove {
            path: key.reset_path(),
        }],
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn config(text: &str) -> UserConfig {
        UserConfig::parse(text).unwrap()
    }

    fn setting(settings: &[Setting], name: &str) -> Setting {
        find(settings, name).unwrap().clone()
    }

    #[test]
    fn every_key_has_a_name_that_leads_back_to_it_and_a_section() {
        let all = Key::all();
        assert_eq!(all.len(), 2 * 6 + 8 + 8 + 3 + 4 * 2);
        for key in &all {
            assert_eq!(Key::parse(&key.name()), Some(*key), "{key}");
        }
        assert_eq!(
            Key::parse("harness.codex.cap"),
            Some(Key::Cap(HarnessId::Codex))
        );
        assert_eq!(Key::parse("harness.gemini.cap"), None);
        assert_eq!(Key::Meter.section(), Section::Meter);
    }

    #[test]
    fn with_nothing_set_every_value_is_its_default() {
        let settings = current(&UserConfig::default(), None, None);
        let calibrates = settings
            .iter()
            .filter(|s| matches!(s.key, Key::Calibrate(_)))
            .count();
        assert_eq!(calibrates, 0, "the default lists always calibrate");
        assert_eq!(settings.len(), Key::all().len() - 4);
        for setting in &settings {
            assert_eq!(setting.value, setting.default, "{}", setting.key);
            assert_eq!(setting.origin, Origin::Default, "{}", setting.key);
        }
        let cap = setting(&settings, "harness.codex.cap");
        assert_eq!(cap.value, Some(Value::Number(75)));
        assert_eq!(
            cap.kind,
            Kind::Number {
                min: 1,
                max: 100,
                unit: Unit::Percent
            }
        );
        assert_eq!(
            setting(&settings, "meter.use").value,
            Some(Value::Name("none".to_string()))
        );
        assert_eq!(
            setting(&settings, "meter.ccusage.claude_block_tokens").value,
            None
        );
        assert_eq!(
            setting(&settings, "review.sample_rate").value,
            Some(Value::Share(0.2))
        );
    }

    #[test]
    fn a_value_in_config_toml_says_so_and_where_runs_stop_follows_the_cap() {
        let settings = current(
            &config("schema = 1\n[harness.codex]\ncap = 60\nenabled = true"),
            None,
            None,
        );
        let cap = setting(&settings, "harness.codex.cap");
        assert_eq!(
            (cap.value, cap.origin),
            (Some(Value::Number(60)), Origin::Config)
        );
        assert_eq!(cap.default, Some(Value::Number(75)));
        let stop = setting(&settings, "harness.codex.abort_at");
        assert_eq!(stop.value, Some(Value::Number(70)));
        assert_eq!(stop.default, Some(Value::Number(70)), "ten above the cap");
        assert_eq!(stop.origin, Origin::Default);
        assert!(matches!(
            stop.kind,
            Kind::Number {
                min: 61,
                max: 100,
                ..
            }
        ));
        assert_eq!(
            setting(&settings, "harness.codex.enabled").value,
            Some(Value::Bool(true))
        );
        // At a cap of 100 there is nowhere above it to stop runs.
        let full = current(
            &config("schema = 1\n[harness.codex]\ncap = 100"),
            None,
            None,
        );
        let stop = setting(&full, "harness.codex.abort_at");
        assert!(stop.locked);
        assert!(stop.parse("100").is_err());
    }

    #[test]
    fn the_meter_is_a_choice_else_the_only_one_found() {
        let found = MeterFile {
            v: MeterFile::V,
            found: [(MeterId::Ccusage, PathBuf::from("/opt/homebrew/bin/ccusage"))].into(),
        };
        let settings = current(&UserConfig::default(), Some(&found), None);
        let meter = setting(&settings, "meter.use");
        assert_eq!(meter.value, Some(Value::Name("ccusage".to_string())));
        assert_eq!(meter.origin, Origin::Found);
        let binary = setting(&settings, "meter.ccusage.binary");
        assert_eq!(
            (binary.value, binary.origin),
            (
                Some(Value::Program(PathBuf::from("/opt/homebrew/bin/ccusage"))),
                Origin::Found
            )
        );
        assert_eq!(setting(&settings, "meter.agent-usage.binary").value, None);

        let chosen = current(
            &config("schema = 1\nmeter.use = \"none\""),
            Some(&found),
            None,
        );
        let meter = setting(&chosen, "meter.use");
        assert_eq!(meter.value, Some(Value::Name("none".to_string())));
        assert_eq!(meter.origin, Origin::Config);
        assert_eq!(meter.default, Some(Value::Name("ccusage".to_string())));
    }

    #[test]
    fn a_harness_program_is_chosen_from_the_copies_on_path_that_pass_the_policy() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let mut dirs = Vec::new();
        for (dir, mode) in [("a", 0o755), ("b", 0o777), ("c", 0o755)] {
            let dir = root.join(dir);
            fs::create_dir(&dir).unwrap();
            let codex = dir.join("codex");
            fs::write(&codex, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&codex, fs::Permissions::from_mode(mode)).unwrap();
            dirs.push(dir);
        }
        let path = std::env::join_paths(&dirs).unwrap();
        let settings = current(&UserConfig::default(), None, Some(&path));
        let binary = setting(&settings, "harness.codex.binary");
        let Kind::Program(programs) = &binary.kind else {
            panic!("{:?}", binary.kind);
        };
        assert_eq!(
            programs.iter().map(|p| p.path.clone()).collect::<Vec<_>>(),
            [dirs[0].join("codex"), dirs[2].join("codex")],
            "the one anyone could write is not offered"
        );
        assert_eq!(binary.value, Some(Value::Program(dirs[0].join("codex"))));
        assert_eq!(binary.origin, Origin::Default);
    }

    #[test]
    fn a_command_line_value_is_read_in_config_toml_s_units() {
        let settings = current(
            &config(
                "schema = 1\n[roles.advise]\ncandidates = [{ harness = \"claude\", model = \"opus\", effort = \"high\" }]",
            ),
            None,
            None,
        );
        let read = |name: &str, text: &str| setting(&settings, name).parse(text);
        assert_eq!(
            read("harness.codex.enabled", "on").unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            read("harness.codex.enabled", "false").unwrap(),
            Value::Bool(false)
        );
        assert_eq!(read("harness.codex.cap", "60").unwrap(), Value::Number(60));
        assert!(read("harness.codex.cap", "101").is_err());
        assert!(read("harness.codex.cap", "sixty").is_err());
        assert_eq!(
            read("meter.ccusage.claude_block_tokens", "300_000_000").unwrap(),
            Value::Number(300_000_000)
        );
        let small = read("meter.ccusage.claude_block_tokens", "300").unwrap_err();
        assert!(
            small.message.contains("at least 100000"),
            "{}",
            small.message
        );
        assert_eq!(
            read("review.sample_rate", "0.25").unwrap(),
            Value::Share(0.25)
        );
        assert!(read("review.sample_rate", "25").is_err());
        assert_eq!(
            read("meter.use", "ccusage").unwrap(),
            Value::Name("ccusage".to_string())
        );
        assert!(read("meter.use", "codexbar").is_err());
        assert!(read("harness.codex.binary", "codex").is_err(), "relative");
        let list = read(
            "roles.advise.candidates",
            "codex:gpt-5.6-sol:high, claude:opus:max",
        )
        .unwrap();
        assert_eq!(list.to_string(), "codex:gpt-5.6-sol:high,claude:opus:max");
        for bad in ["", "codex:--oss:high", "gemini:x:high", "codex:gpt"] {
            let fail = read("roles.advise.candidates", bad).unwrap_err();
            assert_eq!(fail.exit, Exit::Usage, "{bad}");
        }
        assert!(read("roles.advise.calibrate", "on").is_ok());
        let not_yet = find(&settings, "roles.review.calibrate").unwrap_err();
        assert!(
            not_yet.message.contains("roles.review.candidates"),
            "{}",
            not_yet.message
        );
        assert!(find(&settings, "harness.codex.nope").is_err());
    }

    #[test]
    fn set_writes_the_key_and_reset_takes_it_out() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("config.toml");
        fs::write(&file, "# mine\nschema = 1\n").unwrap();
        set(&file, Key::Cap(HarnessId::Codex), &Value::Number(60)).unwrap();
        set(&file, Key::SampleRate, &Value::Share(0.5)).unwrap();
        let list = Value::Candidates(vec![
            candidate("claude:opus:high").unwrap(),
            candidate("codex:gpt-5.6-sol:high").unwrap(),
        ]);
        set(&file, Key::Candidates(Role::Advise), &list).unwrap();
        let config = set(&file, Key::Calibrate(Role::Advise), &Value::Bool(true)).unwrap();
        assert_eq!(config.roles[&Role::Advise].calibrate, Some(true));
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "# mine\nschema = 1\n\n[harness.codex]\ncap = 60\n\n[review]\nsample_rate = 0.5\n\n\
             [roles.advise]\ncandidates = [\n  { harness = \"claude\", model = \"opus\", effort = \"high\" },\n  \
             { harness = \"codex\", model = \"gpt-5.6-sol\", effort = \"high\" },\n]\ncalibrate = true\n"
        );
        // Where runs stop can't be set at or below the cap: refused, and the
        // file is as it was.
        let before = fs::read_to_string(&file).unwrap();
        let refused = set(&file, Key::AbortAt(HarnessId::Codex), &Value::Number(60)).unwrap_err();
        assert_eq!(refused.exit, Exit::Usage);
        assert_eq!(fs::read_to_string(&file).unwrap(), before);

        reset(&file, Key::Cap(HarnessId::Codex)).unwrap();
        reset(&file, Key::SampleRate).unwrap();
        let config = reset(&file, Key::Candidates(Role::Advise)).unwrap();
        assert!(config.roles.is_empty(), "calibrate goes with its list");
        assert_eq!(fs::read_to_string(&file).unwrap(), "# mine\nschema = 1\n");
    }
}
