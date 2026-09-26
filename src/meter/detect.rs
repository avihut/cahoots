//! `install`'s part: which usage meters this machine has, and which one to
//! use. A person's choice (`[meter] use` in config.toml) stands until they
//! change it, and `install --meter` is one way to. With no choice made, one
//! usable meter is used and several bring a question; the only one found is
//! only ever found, never chosen, so a second one brings the question too.
//!
//! This is logic, so it decides and never asks (hard rule 11): the question
//! goes out as `Decision::Ask`, and the answer comes back in through `picked`.
//! The command layer puts it to the person (`src/cli/questions.rs`).
//!
//! Detection runs a candidate only after it passes the binary policy, and
//! only with arguments that read: ccusage's `--version`, and the tracker's
//! `headroom`, which answers from its digest (never the network, never a
//! credential).

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{Exe, MeterId, Selection, agent_usage, ccusage};
use crate::config::MeterConfig;
use crate::config::edit::{Change, KeyPath};
use crate::exit::{Exit, Fail, Res};
use crate::spawn;

/// A usage CLI found on this machine.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Found {
    pub meter: MeterId,
    pub binary: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Worth knowing, but it can be used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Why it cannot be used as it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unusable: Option<String>,
}

impl Found {
    pub fn usable(&self) -> bool {
        self.unusable.is_none()
    }
}

/// Whether `binary` is a `meter` cahoots can use.
pub fn probe(meter: MeterId, binary: &Path) -> Found {
    let mut found = Found {
        meter,
        binary: binary.to_path_buf(),
        version: None,
        note: None,
        unusable: None,
    };
    let exe = match Exe::pin(meter, binary) {
        Ok(exe) => exe,
        Err(fail) => {
            found.unusable = Some(fail.message);
            return found;
        }
    };
    match meter {
        MeterId::AgentUsage => match agent_usage::probe(&exe) {
            Ok(note) => found.note = note,
            Err(why) => found.unusable = Some(why),
        },
        MeterId::Ccusage => match ccusage::probe(&exe) {
            Ok((version, note)) => {
                found.version = Some(version.to_string());
                found.note = note;
            }
            Err(why) => found.unusable = Some(why),
        },
    }
    found
}

/// Where `install` looks for meters.
pub struct Places<'a> {
    pub home: &'a Path,
    /// A PATH value.
    pub path: Option<&'a OsStr>,
    /// The folders apps are installed in.
    pub applications: Vec<PathBuf>,
}

impl<'a> Places<'a> {
    /// This machine's: the PATH given, and both Applications folders.
    pub fn of(home: &'a Path, path: Option<&'a OsStr>) -> Places<'a> {
        Places {
            home,
            path,
            applications: vec![PathBuf::from("/Applications"), home.join("Applications")],
        }
    }
}

/// Every meter this machine has: per meter, the first copy that can be used
/// — or, when none can, the first one found, to say why not. `first` is
/// tried before the usual places: where config.toml says a meter is, and
/// where it was found last time.
pub fn detect(at: &Places<'_>, first: &[(MeterId, &Path)]) -> Vec<Found> {
    let mut found = Vec::new();
    for meter in MeterId::ALL {
        let on_path = spawn::find_on_path(meter.binary_name(), at.path);
        let mut places: Vec<PathBuf> = first
            .iter()
            .filter(|(id, _)| *id == meter)
            .map(|(_, binary)| binary.to_path_buf())
            .collect();
        match meter {
            MeterId::AgentUsage => {
                places.extend(agent_usage::candidates(at.home, on_path, &at.applications));
            }
            MeterId::Ccusage => places.extend(on_path),
        }
        let mut seen = Vec::new();
        let mut unusable = None;
        let mut usable = None;
        for place in places {
            // Not there at all is not worth a line.
            let Ok(canonical) = fs::canonicalize(&place) else {
                continue;
            };
            if seen.contains(&canonical) {
                continue;
            }
            seen.push(canonical);
            let candidate = probe(meter, &place);
            if candidate.usable() {
                usable = Some(candidate);
                break;
            }
            unusable.get_or_insert(candidate);
        }
        found.extend(usable.or(unusable));
    }
    found
}

/// What `install` does about the meter.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// config.toml chose, and the choice stands (`None`: no meter).
    Keep { meter: Option<MeterId> },
    /// This one: the only one found, the one named, or the one picked.
    Use {
        meter: MeterId,
        binary: PathBuf,
        because: Because,
    },
    /// None, because a person said so (`--meter none`, or the question).
    NoneChosen,
    /// None could be used; only the ledger gates runs.
    NoneFound,
    /// More than one could be: a person picks.
    Ask { options: Vec<Found> },
}

/// Why `install` uses the meter it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Because {
    /// Nothing else could be used — no one chose it.
    OnlyOneFound,
    /// `--meter` named it.
    YouNamedIt,
    /// The answer to the question.
    YouChoseIt,
}

impl std::fmt::Display for Because {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Because::OnlyOneFound => "the only one found",
            Because::YouNamedIt => "you named it",
            Because::YouChoseIt => "you chose it",
        })
    }
}

impl Decision {
    /// The meter on once this decision is carried out.
    pub fn in_effect(&self) -> Option<MeterId> {
        match self {
            Decision::Use { meter, .. } => Some(*meter),
            Decision::Keep { meter } => *meter,
            Decision::NoneChosen | Decision::NoneFound | Decision::Ask { .. } => None,
        }
    }

    /// What `install` writes to config.toml for this decision: a person's
    /// choice as `[meter] use`, and a binary they named (`--meter-binary`) as
    /// that meter's `binary`. What was only found goes to `meter.json`, not
    /// here: nobody chose it.
    pub fn config_changes(&self, binary_named: bool) -> Vec<Change> {
        let chose = |selection: Selection| Change::Set {
            path: KeyPath::of("meter.use"),
            value: selection.as_str().into(),
        };
        match self {
            Decision::Use {
                meter,
                binary,
                because: because @ (Because::YouNamedIt | Because::YouChoseIt),
            } => {
                let mut changes = vec![chose(Selection::Meter(*meter))];
                if binary_named && *because == Because::YouNamedIt {
                    changes.push(Change::Set {
                        path: KeyPath::of(&format!("meter.{meter}.binary")),
                        value: binary.to_string_lossy().as_ref().into(),
                    });
                }
                changes
            }
            Decision::NoneChosen => vec![chose(Selection::NoMeter)],
            _ => Vec::new(),
        }
    }

    /// One sentence for the person who ran `install`.
    pub fn sentence(&self) -> String {
        match self {
            Decision::Keep { meter: Some(meter) } => {
                format!("Usage meter: {meter}, as chosen in config.toml.")
            }
            Decision::Keep { meter: None } => {
                "No usage meter, as chosen in config.toml: only the ledger gates runs.".to_string()
            }
            Decision::Use { meter, because, .. } => format!("Usage meter: {meter} ({because})."),
            Decision::NoneChosen => "No usage meter, as you said: only the ledger gates runs.".to_string(),
            Decision::NoneFound => {
                "No usage meter here can be used: only the ledger gates runs. Agent Usage or \
                 ccusage adds plan limits — install or update one, then run `cahoots install` again."
                    .to_string()
            }
            Decision::Ask { .. } => {
                "Usage meter: more than one was found, and a real install asks which to use."
                    .to_string()
            }
        }
    }
}

/// The decision, from what config.toml chose (`chosen`), what the command
/// line says (`selection`, which wins: it is the person's word now) and what
/// was found. PURE — `install` asks and writes; this only decides, so it is
/// tested without a terminal.
pub fn decide(
    chosen: Option<Selection>,
    selection: Option<Selection>,
    found: &[Found],
) -> Res<Decision> {
    match selection {
        Some(Selection::NoMeter) => return Ok(Decision::NoneChosen),
        Some(Selection::Meter(meter)) => {
            return match found.iter().find(|found| found.meter == meter) {
                Some(found) if found.usable() => Ok(Decision::Use {
                    meter,
                    binary: found.binary.clone(),
                    because: Because::YouNamedIt,
                }),
                Some(found) => Err(Fail::config(format!(
                    "{meter} at {}: {}",
                    found.binary.display(),
                    found.unusable.as_deref().unwrap_or_default()
                ))),
                None => Err(Fail::config(format!(
                    "no {} was found — pass its path with --meter-binary",
                    meter.binary_name()
                ))),
            };
        }
        None => {}
    }
    if let Some(chosen) = chosen {
        return Ok(Decision::Keep {
            meter: chosen.meter(),
        });
    }
    let usable: Vec<&Found> = found.iter().filter(|found| found.usable()).collect();
    Ok(match usable.as_slice() {
        [] => Decision::NoneFound,
        [only] => Decision::Use {
            meter: only.meter,
            binary: only.binary.clone(),
            because: Because::OnlyOneFound,
        },
        _ => Decision::Ask {
            options: usable.into_iter().cloned().collect(),
        },
    })
}

/// What a person still has to do before the meter in effect caps anything:
/// a meter chosen in config.toml that is not here refuses every run, and
/// ccusage has no percentage until a limit is declared.
pub fn still_to_do(
    in_effect: Option<MeterId>,
    config: &MeterConfig,
    found: &[Found],
    config_file: &Path,
) -> Option<String> {
    let meter = in_effect?;
    let mut to_do = Vec::new();
    // Its table's binary is the one used, when it names one.
    let pinned = config.binary(meter);
    let here = found.iter().find(|found| found.meter == meter);
    let works =
        here.is_some_and(|found| found.usable() && pinned.is_none_or(|p| p == found.binary));
    if !works {
        let why = match (pinned, here) {
            (Some(pinned), Some(found)) if found.binary == pinned => format!(
                "{} cannot be used: {}",
                pinned.display(),
                found.unusable.as_deref().unwrap_or("it did not answer")
            ),
            (Some(pinned), _) => format!("{} cannot be used", pinned.display()),
            (None, Some(found)) => format!(
                "the {} at {} cannot be used: {}",
                meter.binary_name(),
                found.binary.display(),
                found.unusable.as_deref().unwrap_or("it did not answer")
            ),
            (None, None) => format!("no {} was found", meter.binary_name()),
        };
        to_do.push(format!(
            "config.toml chooses the {meter} meter, but {why}, so every run is refused until it \
             can be: install or update it, set its path as `binary` under [meter.{meter}], or \
             choose another meter with `use` under [meter] — in {}.",
            config_file.display()
        ));
    }
    if meter == MeterId::Ccusage {
        let table = config.ccusage.as_ref();
        let claude = table.and_then(|t| t.claude_block_tokens).is_none();
        let codex = table.and_then(|t| t.codex_day_tokens).is_none();
        let missing = match (claude, codex) {
            (false, false) => None,
            (true, true) => Some((
                "claude_block_tokens and codex_day_tokens",
                "Claude Code is held only by its own limit notice, and Codex only by the ledger",
            )),
            (true, false) => Some((
                "claude_block_tokens",
                "Claude Code is held only by its own limit notice",
            )),
            (false, true) => Some(("codex_day_tokens", "Codex is held only by the ledger")),
        };
        if let Some((missing, until)) = missing {
            to_do.push(format!(
                "ccusage counts tokens and cannot see a plan's limit: set {missing} under \
                 [meter.ccusage] in {} — the tokens that count as a whole plan, in a 5-hour block \
                 for Claude Code and in a day for Codex. Until then {until}. `cahoots doctor` \
                 shows the counts to size them by.",
                config_file.display()
            ));
        }
    }
    (!to_do.is_empty()).then(|| to_do.join(" "))
}

/// What a person picked from the question, as a decision.
pub fn picked(selection: Selection, options: &[Found]) -> Res<Decision> {
    match selection {
        Selection::NoMeter => Ok(Decision::NoneChosen),
        Selection::Meter(meter) => options
            .iter()
            .find(|found| found.meter == meter)
            .map(|found| Decision::Use {
                meter,
                binary: found.binary.clone(),
                because: Because::YouChoseIt,
            })
            .ok_or_else(|| Fail::new(Exit::Usage, format!("{meter} was not one of the choices"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UserConfig;

    fn found(meter: MeterId, binary: &str, unusable: Option<&str>) -> Found {
        Found {
            meter,
            binary: PathBuf::from(binary),
            version: None,
            note: None,
            unusable: unusable.map(str::to_string),
        }
    }

    fn both() -> Vec<Found> {
        vec![
            found(
                MeterId::AgentUsage,
                "/Applications/AgentUsage.app/Contents/MacOS/usage-cli",
                None,
            ),
            found(MeterId::Ccusage, "/opt/homebrew/bin/ccusage", None),
        ]
    }

    fn only_ccusage() -> Vec<Found> {
        vec![
            found(
                MeterId::AgentUsage,
                "/x/usage-cli",
                Some("it has no `headroom` noun yet"),
            ),
            found(MeterId::Ccusage, "/opt/homebrew/bin/ccusage", None),
        ]
    }

    const CCUSAGE: Option<Selection> = Some(Selection::Meter(MeterId::Ccusage));

    #[test]
    fn with_no_choice_one_usable_meter_is_used_and_several_are_asked_about() {
        assert_eq!(
            decide(None, None, &only_ccusage()).unwrap(),
            Decision::Use {
                meter: MeterId::Ccusage,
                binary: PathBuf::from("/opt/homebrew/bin/ccusage"),
                because: Because::OnlyOneFound,
            }
        );
        match decide(None, None, &both()).unwrap() {
            Decision::Ask { options } => assert_eq!(options.len(), 2),
            other => panic!("expected a question, got {other:?}"),
        }
        assert_eq!(decide(None, None, &[]).unwrap(), Decision::NoneFound);
        assert_eq!(
            decide(None, None, &only_ccusage()[..1]).unwrap(),
            Decision::NoneFound,
            "found but unusable is not a meter"
        );
    }

    #[test]
    fn a_choice_in_config_toml_stands_and_nothing_is_asked() {
        assert_eq!(
            decide(CCUSAGE, None, &both()).unwrap(),
            Decision::Keep {
                meter: Some(MeterId::Ccusage)
            }
        );
        assert_eq!(
            decide(Some(Selection::NoMeter), None, &both()).unwrap(),
            Decision::Keep { meter: None },
            "no meter is a choice too"
        );
        // Even when what it chose is gone: install says so (`still_to_do`)
        // rather than choose for the person.
        assert_eq!(
            decide(CCUSAGE, None, &[]).unwrap(),
            Decision::Keep {
                meter: Some(MeterId::Ccusage)
            }
        );
    }

    #[test]
    fn the_command_line_outranks_config_toml_and_is_written_there() {
        let named = decide(
            CCUSAGE,
            Some(Selection::Meter(MeterId::AgentUsage)),
            &both(),
        )
        .unwrap();
        assert!(
            matches!(
                named,
                Decision::Use {
                    meter: MeterId::AgentUsage,
                    because: Because::YouNamedIt,
                    ..
                }
            ),
            "{named:?}"
        );
        let text = |changes: Vec<Change>| format!("{changes:?}");
        assert!(text(named.config_changes(false)).contains("\"agent-usage\""));
        assert_eq!(named.config_changes(false).len(), 1);
        assert_eq!(
            named.config_changes(true).len(),
            2,
            "a binary named on the command line is written too"
        );
        let none = decide(CCUSAGE, Some(Selection::NoMeter), &both()).unwrap();
        assert_eq!(none, Decision::NoneChosen);
        assert!(text(none.config_changes(false)).contains("\"none\""));
        let picked = picked(Selection::Meter(MeterId::Ccusage), &both()).unwrap();
        assert_eq!(
            picked.config_changes(false).len(),
            1,
            "an answer is a choice"
        );
    }

    #[test]
    fn what_was_only_found_is_never_written_as_a_choice() {
        for decision in [
            decide(None, None, &only_ccusage()).unwrap(),
            decide(CCUSAGE, None, &both()).unwrap(),
            Decision::NoneFound,
            decide(None, None, &both()).unwrap(),
        ] {
            assert!(decision.config_changes(true).is_empty(), "{decision:?}");
        }
    }

    #[test]
    fn a_named_meter_must_be_there_and_usable() {
        let unusable = vec![found(MeterId::AgentUsage, "/x/usage-cli", Some("too old"))];
        let fail =
            decide(None, Some(Selection::Meter(MeterId::AgentUsage)), &unusable).unwrap_err();
        assert!(fail.message.contains("too old"), "{}", fail.message);
        let fail = decide(None, Some(Selection::Meter(MeterId::Ccusage)), &unusable).unwrap_err();
        assert!(fail.message.contains("--meter-binary"), "{}", fail.message);
    }

    #[test]
    fn a_chosen_meter_that_is_not_here_is_what_is_still_to_do() {
        let config = |text: &str| UserConfig::parse(text).unwrap().meter;
        let file = Path::new("/c/config.toml");
        let chose = config("schema = 1\nmeter.use = \"agent-usage\"");
        let to_do = still_to_do(Some(MeterId::AgentUsage), &chose, &[], file).unwrap();
        assert!(
            to_do.contains("no usage-cli was found")
                && to_do.contains("every run is refused")
                && to_do.contains("[meter.agent-usage]"),
            "{to_do}"
        );
        let unusable = [found(MeterId::AgentUsage, "/x/usage-cli", Some("too old"))];
        let to_do = still_to_do(Some(MeterId::AgentUsage), &chose, &unusable, file).unwrap();
        assert!(
            to_do.contains("/x/usage-cli cannot be used: too old"),
            "{to_do}"
        );
        assert_eq!(
            still_to_do(Some(MeterId::AgentUsage), &chose, &both(), file),
            None
        );
        // A path of its own that does not work is not saved by another copy.
        let pinned = config(
            "schema = 1\nmeter.use = \"agent-usage\"\n[meter.agent-usage]\nbinary = \"/gone/usage-cli\"",
        );
        let to_do = still_to_do(Some(MeterId::AgentUsage), &pinned, &both(), file).unwrap();
        assert!(to_do.contains("/gone/usage-cli cannot be used"), "{to_do}");
    }

    #[test]
    fn ccusage_is_still_to_do_until_its_limits_are_declared() {
        let config = |text: &str| UserConfig::parse(text).unwrap().meter;
        let file = Path::new("/c/config.toml");
        let found = only_ccusage();
        let to_do =
            still_to_do(Some(MeterId::Ccusage), &config("schema = 1"), &found, file).unwrap();
        assert!(
            to_do.contains("claude_block_tokens and codex_day_tokens"),
            "{to_do}"
        );
        let declared = config(
            "schema = 1\n[meter.ccusage]\nclaude_block_tokens = 300_000_000\ncodex_day_tokens = 50_000_000",
        );
        assert_eq!(
            still_to_do(Some(MeterId::Ccusage), &declared, &found, file),
            None
        );
        assert_eq!(still_to_do(None, &declared, &[], file), None);
    }
}
