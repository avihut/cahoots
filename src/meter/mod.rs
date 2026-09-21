//! Usage meters: how much of a harness's plan is used, asked of a usage CLI
//! the person already has. The gate (`gate.rs`) turns a meter's answer into
//! admit or refuse; this module only knows how to ask each meter, and what
//! its answer means.
//!
//! - **agent-usage** — the Agent Usage tracker's `usage-cli headroom`: the
//!   plan's own percentages, as the vendor reports them.
//! - **ccusage** — token counts, read from the harnesses' own logs. ccusage
//!   cannot see a plan's limit, so the person declares one, in tokens.
//!
//! One meter is on at a time, beside the built-in ledger (`gate.rs`), which
//! is always on. A `[meter.<id>]` table in the config file turns a meter on;
//! without one, the meter `install` chose (`meter.json`) is on.
//!
//! The set is closed. A meter is a variant here, its command lines built in
//! code from typed values (hard rule 3) — never taken from a config file.
//! Adding one is a module and a variant.

mod agent_usage;
mod ccusage;
pub mod detect;

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use agent_usage::AgentUsage;
pub use ccusage::Ccusage;

use crate::config::MeterConfig;
use crate::dirs::Dirs;
use crate::exit::{Exit, Fail, Res};
use crate::model::HarnessId;
use crate::spawn;

/// How long a meter may take to answer.
const DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeterId {
    AgentUsage,
    Ccusage,
}

impl MeterId {
    /// In the order `install` offers them: the plan's own percentages first,
    /// then token counts measured against a limit the person declares.
    pub const ALL: [MeterId; 2] = [MeterId::AgentUsage, MeterId::Ccusage];

    pub const fn as_str(self) -> &'static str {
        match self {
            MeterId::AgentUsage => "agent-usage",
            MeterId::Ccusage => "ccusage",
        }
    }

    /// The executable's name on PATH.
    pub const fn binary_name(self) -> &'static str {
        match self {
            MeterId::AgentUsage => "usage-cli",
            MeterId::Ccusage => "ccusage",
        }
    }

    /// One line for a person choosing between meters.
    pub const fn summary(self) -> &'static str {
        match self {
            MeterId::AgentUsage => "each plan's own percentages, as the vendors report them",
            MeterId::Ccusage => "token counts from the agents' logs, against limits you declare",
        }
    }
}

impl fmt::Display for MeterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `pad`, so a width in a format string lines a column up.
        f.pad(self.as_str())
    }
}

impl FromStr for MeterId {
    type Err = Fail;
    fn from_str(s: &str) -> Result<Self, Fail> {
        MeterId::ALL
            .into_iter()
            .find(|id| id.as_str() == s)
            .ok_or_else(|| Fail::new(Exit::Usage, format!("unknown meter: {s:?}")))
    }
}

/// What `install --meter` names: a meter, or none at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Meter(MeterId),
    NoMeter,
}

impl FromStr for Selection {
    type Err = Fail;
    fn from_str(s: &str) -> Result<Self, Fail> {
        if s == "none" {
            return Ok(Selection::NoMeter);
        }
        s.parse().map(Selection::Meter).map_err(|_| {
            Fail::new(
                Exit::Usage,
                format!("unknown meter: {s:?} (agent-usage, ccusage or none)"),
            )
        })
    }
}

/// The meter in effect, with its settings.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "meter", rename_all = "kebab-case")]
pub enum UsageMeter {
    AgentUsage(AgentUsage),
    Ccusage(Ccusage),
}

/// Who turned the meter on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Chosen {
    /// A `[meter.<id>]` table in the config file.
    Config,
    /// `cahoots install`, which found it (and asked, if it found several).
    Install,
}

/// One question to a meter: is `harness` under `cap` percent?
#[derive(Debug, Clone, Copy)]
pub struct Ask {
    pub harness: HarnessId,
    pub cap: u8,
    /// Refuse a reading older than the meter allows. (A meter that reads the
    /// harness's own logs has nothing that can go stale.)
    pub fresh: bool,
    /// Also refuse when usage is on course to pass the limit before it resets.
    /// A meter with no forecast worth refusing on (ccusage) ignores it.
    pub forecast: bool,
}

/// A meter's answer, in the gate's terms.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// `Exit::Ok`, or one of the gate's five refusals (13, 21, 24, 25, 26) —
    /// or `Config` (34) when no binary is pinned for the meter at all.
    pub verdict: Exit,
    /// Percent of the limit used, when the meter knows it.
    pub percent: Option<f64>,
    /// What the meter said, kept on the run record.
    pub reading: Option<Value>,
    /// Why, in the meter's own terms, when that says more than the verdict.
    pub why: Option<String>,
}

impl Answer {
    fn no_answer(why: impl Into<String>) -> Answer {
        Answer {
            verdict: Exit::NoDigest,
            percent: None,
            reading: None,
            why: Some(why.into()),
        }
    }
}

impl UsageMeter {
    pub fn id(&self) -> MeterId {
        match self {
            UsageMeter::AgentUsage(_) => MeterId::AgentUsage,
            UsageMeter::Ccusage(_) => MeterId::Ccusage,
        }
    }

    /// The binary the config or `install` named.
    pub fn binary(&self) -> Option<&Path> {
        match self {
            UsageMeter::AgentUsage(meter) => meter.binary.as_deref(),
            UsageMeter::Ccusage(meter) => meter.binary.as_deref(),
        }
    }

    /// The binary, held to the same policy as a harness's (`spawn`) — and
    /// only a binary a person pinned. Never a PATH lookup: the calling agent
    /// sets PATH, and a meter it could shadow would say whatever it liked.
    pub fn resolve(&self) -> Res<Exe> {
        let id = self.id();
        let pinned = self.binary().ok_or_else(|| {
            Fail::config(format!(
                "no {} is recorded for the {id} meter — `cahoots install` finds it, or set \
                 meter.{id}.binary",
                id.binary_name()
            ))
        })?;
        Exe::pin(id, pinned)
    }

    /// Whether this meter says anything about `harness` at all. When it says
    /// nothing, the gate does not ask, and only the ledger holds that target.
    pub fn measures(&self, harness: HarnessId) -> bool {
        match self {
            UsageMeter::AgentUsage(_) => true,
            UsageMeter::Ccusage(meter) => meter.measures(harness),
        }
    }

    /// Whether it can tell a running job's target crossing `abort_at` — which
    /// takes a percentage, and one that moves while the job runs.
    pub fn watches(&self, harness: HarnessId) -> bool {
        match self {
            UsageMeter::AgentUsage(_) => true,
            UsageMeter::Ccusage(meter) => meter.limit(harness).is_some(),
        }
    }

    /// Readings that only refresh when the harness itself runs. For these a
    /// strict freshness guard would refuse forever: nothing but a run would
    /// ever make the data fresh again (docs/ARCHITECTURE.md → The gate).
    pub fn refreshes_on_use(&self, harness: HarnessId) -> bool {
        match self {
            UsageMeter::AgentUsage(_) => harness == HarnessId::Codex,
            UsageMeter::Ccusage(_) => false,
        }
    }

    /// Asks. A meter that cannot be run, or that answers in a way cahoots
    /// does not understand, is a `NoDigest` answer — never a yes.
    pub fn ask(&self, ask: &Ask) -> Answer {
        let exe = match self.resolve() {
            Ok(exe) => exe,
            Err(fail) => {
                // No binary pinned at all is the person's setup to fix, not a
                // reason to try another target: `fix_config`, not `other_target`.
                let verdict = if fail.exit == Exit::Config {
                    Exit::Config
                } else {
                    Exit::NoDigest
                };
                return Answer {
                    verdict,
                    ..Answer::no_answer(format!("the usage meter: {}", fail.message))
                };
            }
        };
        match self {
            UsageMeter::AgentUsage(meter) => meter.ask(&exe, ask),
            UsageMeter::Ccusage(meter) => meter.ask(&exe, ask),
        }
    }

    /// The refusal a caller reads, for an answer that is not `Ok`.
    pub fn refusal(&self, ask: &Ask, answer: &Answer) -> String {
        match self {
            UsageMeter::AgentUsage(meter) => meter.refusal(ask, answer),
            UsageMeter::Ccusage(meter) => meter.refusal(ask, answer),
        }
    }
}

/// A meter's executable: the path a person pinned, and what it resolves to
/// once held to the binary policy.
#[derive(Debug, Clone)]
pub struct Exe {
    pub pinned: PathBuf,
    pub resolved: PathBuf,
}

impl Exe {
    pub fn pin(id: MeterId, pinned: &Path) -> Res<Exe> {
        Ok(Exe {
            resolved: spawn::resolve_binary(id.binary_name(), Some(pinned), &[])?,
            pinned: pinned.to_path_buf(),
        })
    }

    /// The meter's PATH: its own directories — where a package manager puts
    /// the interpreter a script meter runs on — then the system's. Never the
    /// caller's, which would choose that interpreter for it.
    fn path(&self) -> Option<std::ffi::OsString> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        for dir in [self.pinned.parent(), self.resolved.parent()]
            .into_iter()
            .flatten()
            .map(Path::to_path_buf)
            .chain(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(PathBuf::from))
        {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        std::env::join_paths(dirs).ok()
    }
}

/// Runs a meter: from `/`, so that no file in the caller's working directory
/// can configure it (a repository cannot tune the gate about to judge it),
/// with a PATH of its own, and HOME from the passwd database.
fn run(exe: &Exe, args: &[String]) -> Result<spawn::Output, String> {
    spawn::run_helper_with_path(
        &exe.resolved,
        args,
        Some(Path::new("/")),
        DEADLINE,
        exe.path(),
    )
    .map_err(|fail| fail.message)
}

/// What `meter.json` holds: the meter `install` chose. Its own file, written
/// only by `install`, so the hand-written config is never rewritten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeterFile {
    pub v: u32,
    /// `None`: a person chose to have no usage meter (`install --meter none`).
    pub meter: Option<MeterId>,
    pub binary: Option<PathBuf>,
}

impl MeterFile {
    /// `None` when `install` has never chosen.
    pub fn load(dirs: &Dirs) -> Res<Option<MeterFile>> {
        let path = dirs.meter_file();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(Fail::config(format!(
                    "cannot read {}: {error}",
                    path.display()
                )));
            }
        };
        let file: MeterFile = serde_json::from_str(&text)
            .map_err(|error| Fail::config(format!("{}: {error}", path.display())))?;
        if file
            .binary
            .as_deref()
            .is_some_and(|binary| !binary.is_absolute())
        {
            return Err(Fail::config(format!(
                "{}: the meter's binary must be an absolute path",
                path.display()
            )));
        }
        Ok(Some(file))
    }

    /// Only the `install` verb calls this, and that verb only runs from a
    /// terminal: which meter judges the gate is a person's decision.
    pub fn save(&self, dirs: &Dirs) -> Res<()> {
        crate::dirs::ensure_private_dir(&dirs.config)?;
        let json = serde_json::to_vec_pretty(self)
            .map_err(|error| Fail::internal(format!("cannot encode the meter choice: {error}")))?;
        crate::run::record::write_private(&dirs.meter_file(), &json)
    }
}

/// The meter in effect: the one the config file turns on, else the one
/// `install` chose, else none. A `[meter.<id>]` table with no `binary` takes
/// the path `install` recorded for that same meter — and with neither, the
/// meter refuses until one is pinned (`UsageMeter::resolve`).
pub fn effective(config: &MeterConfig, chosen: Option<&MeterFile>) -> Option<(UsageMeter, Chosen)> {
    let recorded = |id: MeterId| {
        chosen
            .filter(|file| file.meter == Some(id))
            .and_then(|file| file.binary.clone())
    };
    if let Some(table) = &config.agent_usage {
        return Some((
            UsageMeter::AgentUsage(AgentUsage::from_config(
                table,
                recorded(MeterId::AgentUsage),
            )),
            Chosen::Config,
        ));
    }
    if let Some(table) = &config.ccusage {
        return Some((
            UsageMeter::Ccusage(Ccusage::from_config(table, recorded(MeterId::Ccusage))),
            Chosen::Config,
        ));
    }
    let file = chosen?;
    let meter = match file.meter? {
        MeterId::AgentUsage => UsageMeter::AgentUsage(AgentUsage::from_config(
            &Default::default(),
            file.binary.clone(),
        )),
        MeterId::Ccusage => UsageMeter::Ccusage(Ccusage::from_config(
            &Default::default(),
            file.binary.clone(),
        )),
    };
    Some((meter, Chosen::Install))
}

/// The meter a config file turns on, if it turns one on.
pub fn configured(config: &MeterConfig) -> Option<MeterId> {
    effective(config, None).map(|(meter, _)| meter.id())
}

/// `161_441_521` → `161M`: how token counts read in a message.
pub fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{}k", n / 1_000),
        1_000_000..10_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{}M", n / 1_000_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UserConfig;

    #[test]
    fn meters_and_selections_parse_from_their_names() {
        for id in MeterId::ALL {
            assert_eq!(id.as_str().parse::<MeterId>().unwrap(), id);
            assert_eq!(
                serde_json::to_value(id).unwrap(),
                Value::String(id.as_str().into())
            );
        }
        assert_eq!("none".parse::<Selection>().unwrap(), Selection::NoMeter);
        assert_eq!(
            "ccusage".parse::<Selection>().unwrap(),
            Selection::Meter(MeterId::Ccusage)
        );
        assert!("codexbar".parse::<Selection>().is_err());
    }

    fn file(meter: Option<MeterId>, binary: &str) -> MeterFile {
        MeterFile {
            v: 1,
            meter,
            binary: Some(PathBuf::from(binary)),
        }
    }

    #[test]
    fn the_config_file_outranks_what_install_chose() {
        let chosen = file(Some(MeterId::AgentUsage), "/opt/usage-cli");
        let none = UserConfig::parse("schema = 1").unwrap();
        let (meter, by) = effective(&none.meter, Some(&chosen)).unwrap();
        assert_eq!((meter.id(), by), (MeterId::AgentUsage, Chosen::Install));
        assert_eq!(meter.binary(), Some(Path::new("/opt/usage-cli")));

        let ccusage =
            UserConfig::parse("schema = 1\n[meter.ccusage]\nclaude_block_tokens = 300_000_000")
                .unwrap();
        let (meter, by) = effective(&ccusage.meter, Some(&chosen)).unwrap();
        assert_eq!((meter.id(), by), (MeterId::Ccusage, Chosen::Config));
        // The path install recorded belongs to another meter: not borrowed.
        assert_eq!(meter.binary(), None);

        // The same meter: its table's knobs, install's path.
        let chosen = file(Some(MeterId::Ccusage), "/opt/homebrew/bin/ccusage");
        let (meter, _) = effective(&ccusage.meter, Some(&chosen)).unwrap();
        assert_eq!(meter.binary(), Some(Path::new("/opt/homebrew/bin/ccusage")));
        assert!(meter.watches(HarnessId::Claude) && !meter.watches(HarnessId::Codex));
    }

    #[test]
    fn no_table_and_no_choice_is_no_meter() {
        let none = UserConfig::parse("schema = 1").unwrap();
        assert!(effective(&none.meter, None).is_none());
        let said_none = MeterFile {
            v: 1,
            meter: None,
            binary: None,
        };
        assert!(effective(&none.meter, Some(&said_none)).is_none());
    }

    #[test]
    fn token_counts_read_short() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(42_000), "42k");
        assert_eq!(tokens(2_450_000), "2.5M");
        assert_eq!(tokens(161_441_521), "161M");
    }
}
