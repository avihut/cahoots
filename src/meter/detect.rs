//! `install`'s part: which usage meters this machine has, and which one to
//! use. One usable meter is used; several, and a person picks. A person's
//! choice stands until `install --meter` changes it; the only one found is
//! chosen again each time, so a second one brings the question. A
//! `[meter.<id>]` table in the config file outranks all of it.
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

use super::{Exe, MeterFile, MeterId, Selection, agent_usage, ccusage};
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
/// tried before the usual places: the copy chosen last time.
pub fn detect(at: &Places<'_>, first: Option<(MeterId, &Path)>) -> Vec<Found> {
    let mut found = Vec::new();
    for meter in MeterId::ALL {
        let on_path = spawn::find_on_path(meter.binary_name(), at.path);
        let mut places: Vec<PathBuf> = first
            .filter(|(id, _)| *id == meter)
            .map(|(_, binary)| binary.to_path_buf())
            .into_iter()
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
    /// The config file turns a meter on; the choice is the config file's.
    Config { meter: MeterId },
    /// What was chosen before still stands (`None`: no meter, as a person said).
    Keep { meter: Option<MeterId> },
    /// This one: the only one found, the one named, or the one picked.
    Use {
        meter: MeterId,
        binary: PathBuf,
        because: Because,
    },
    /// None, because a person said so (`--meter none`).
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

/// What becomes of `meter.json` once a decision is carried out.
#[derive(Debug, PartialEq)]
pub enum Record {
    Leave,
    Write(MeterFile),
    /// Nothing can be used: an earlier choice would only refuse every run
    /// while `install` says the ledger alone gates them.
    Remove,
}

impl Decision {
    /// The meter on once this decision is carried out.
    pub fn in_effect(&self) -> Option<MeterId> {
        match self {
            Decision::Config { meter } | Decision::Use { meter, .. } => Some(*meter),
            Decision::Keep { meter } => *meter,
            Decision::NoneChosen | Decision::NoneFound | Decision::Ask { .. } => None,
        }
    }

    pub fn record(&self) -> Record {
        match self {
            Decision::Use {
                meter,
                binary,
                because,
            } => Record::Write(MeterFile {
                v: 1,
                meter: Some(*meter),
                binary: Some(binary.clone()),
                only_one_found: *because == Because::OnlyOneFound,
            }),
            Decision::NoneChosen => Record::Write(MeterFile {
                v: 1,
                meter: None,
                binary: None,
                only_one_found: false,
            }),
            Decision::NoneFound => Record::Remove,
            Decision::Config { .. } | Decision::Keep { .. } | Decision::Ask { .. } => Record::Leave,
        }
    }

    /// One sentence for the person who ran `install`.
    pub fn sentence(&self) -> String {
        match self {
            Decision::Config { meter } => format!("Usage meter: {meter}, as the config file says."),
            Decision::Keep { meter: Some(meter) } => {
                format!("Usage meter: {meter}, as chosen before.")
            }
            Decision::Keep { meter: None } => {
                "No usage meter, as chosen before: only the ledger gates runs.".to_string()
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

/// The decision, from what was found and what was said. PURE — `install`
/// asks and writes; this only decides, so it is tested without a terminal.
pub fn decide(
    configured: Option<MeterId>,
    previous: Option<&MeterFile>,
    selection: Option<Selection>,
    found: &[Found],
) -> Res<Decision> {
    if let Some(meter) = configured {
        if selection.is_some() {
            return Err(Fail::new(
                Exit::Usage,
                format!(
                    "the config file turns on [meter.{meter}], and that outranks what install chooses — remove the table first, or leave out --meter"
                ),
            ));
        }
        return Ok(Decision::Config { meter });
    }
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
    if let Some(previous) = previous {
        let still_there = |meter: MeterId| {
            found.iter().any(|found| {
                found.meter == meter
                    && found.usable()
                    && Some(&found.binary) == previous.binary.as_ref()
            })
        };
        match previous.meter {
            None => return Ok(Decision::Keep { meter: None }),
            Some(meter) if !previous.only_one_found && still_there(meter) => {
                return Ok(Decision::Keep { meter: Some(meter) });
            }
            // Gone, moved or broken — or no one's choice: choose again, as if
            // for the first time.
            Some(_) => {}
        }
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
/// ccusage has no percentage until a limit is declared.
pub fn still_to_do(
    in_effect: Option<MeterId>,
    config: &crate::config::MeterConfig,
    config_file: &Path,
) -> Option<String> {
    if in_effect != Some(MeterId::Ccusage) {
        return None;
    }
    let table = config.ccusage.as_ref();
    let claude = table.and_then(|t| t.claude_block_tokens).is_none();
    let codex = table.and_then(|t| t.codex_day_tokens).is_none();
    let (missing, until) = match (claude, codex) {
        (false, false) => return None,
        (true, true) => (
            "claude_block_tokens and codex_day_tokens",
            "Claude Code is held only by its own limit notice, and Codex only by the ledger",
        ),
        (true, false) => (
            "claude_block_tokens",
            "Claude Code is held only by its own limit notice",
        ),
        (false, true) => ("codex_day_tokens", "Codex is held only by the ledger"),
    };
    Some(format!(
        "ccusage counts tokens and cannot see a plan's limit: set {missing} under [meter.ccusage] \
         in {} — the tokens that count as a whole plan, in a 5-hour block for Claude Code and in a \
         day for Codex. Until then {until}. `cahoots doctor` shows the counts to size them by.",
        config_file.display()
    ))
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

    #[test]
    fn one_usable_meter_is_used_and_several_are_asked_about() {
        let only_ccusage = vec![
            found(
                MeterId::AgentUsage,
                "/x/usage-cli",
                Some("it has no `headroom` noun yet"),
            ),
            found(MeterId::Ccusage, "/opt/homebrew/bin/ccusage", None),
        ];
        assert_eq!(
            decide(None, None, None, &only_ccusage).unwrap(),
            Decision::Use {
                meter: MeterId::Ccusage,
                binary: PathBuf::from("/opt/homebrew/bin/ccusage"),
                because: Because::OnlyOneFound,
            }
        );
        match decide(None, None, None, &both()).unwrap() {
            Decision::Ask { options } => assert_eq!(options.len(), 2),
            other => panic!("expected a question, got {other:?}"),
        }
        assert_eq!(decide(None, None, None, &[]).unwrap(), Decision::NoneFound);
        assert_eq!(
            decide(None, None, None, &only_ccusage[..1]).unwrap(),
            Decision::NoneFound,
            "found but unusable is not a meter"
        );
    }

    #[test]
    fn a_choice_stands_until_it_is_gone_or_changed() {
        let chosen = MeterFile {
            v: 1,
            meter: Some(MeterId::Ccusage),
            binary: Some(PathBuf::from("/opt/homebrew/bin/ccusage")),
            only_one_found: false,
        };
        assert_eq!(
            decide(None, Some(&chosen), None, &both()).unwrap(),
            Decision::Keep {
                meter: Some(MeterId::Ccusage)
            }
        );
        // Its binary is gone: choose again, and with two found, ask.
        let moved = MeterFile {
            binary: Some(PathBuf::from("/usr/local/bin/ccusage")),
            ..chosen.clone()
        };
        assert!(matches!(
            decide(None, Some(&moved), None, &both()).unwrap(),
            Decision::Ask { .. }
        ));
        // "None" is a choice too, and it stands.
        let none = MeterFile {
            v: 1,
            meter: None,
            binary: None,
            only_one_found: false,
        };
        assert_eq!(
            decide(None, Some(&none), None, &both()).unwrap(),
            Decision::Keep { meter: None }
        );
        // Naming one overrides what was chosen before.
        assert!(matches!(
            decide(
                None,
                Some(&chosen),
                Some(Selection::Meter(MeterId::AgentUsage)),
                &both()
            )
            .unwrap(),
            Decision::Use {
                meter: MeterId::AgentUsage,
                ..
            }
        ));
        assert_eq!(
            decide(None, Some(&chosen), Some(Selection::NoMeter), &both()).unwrap(),
            Decision::NoneChosen
        );
    }

    #[test]
    fn the_only_one_found_is_chosen_again_so_a_second_one_is_asked_about() {
        let only_ccusage = vec![
            found(
                MeterId::AgentUsage,
                "/x/usage-cli",
                Some("it has no `headroom` noun yet"),
            ),
            found(MeterId::Ccusage, "/opt/homebrew/bin/ccusage", None),
        ];
        let Record::Write(first) = decide(None, None, None, &only_ccusage).unwrap().record() else {
            panic!("the only one found is recorded");
        };
        assert!(first.only_one_found, "{first:?}");
        // Still the only one: the same again, and still no one's choice.
        assert_eq!(
            decide(None, Some(&first), None, &only_ccusage)
                .unwrap()
                .record(),
            Record::Write(first.clone())
        );
        // The tracker can be used now too, and no one ever picked ccusage.
        assert!(matches!(
            decide(None, Some(&first), None, &both()).unwrap(),
            Decision::Ask { .. }
        ));
        // A person's answer, named or picked, is theirs, and it stands.
        let named = decide(
            None,
            Some(&first),
            Some(Selection::Meter(MeterId::Ccusage)),
            &both(),
        )
        .unwrap();
        let chose = picked(Selection::Meter(MeterId::Ccusage), &both()).unwrap();
        for decision in [named, chose] {
            let Record::Write(theirs) = decision.record() else {
                panic!("{decision:?} is recorded");
            };
            assert!(!theirs.only_one_found, "{theirs:?}");
            assert_eq!(
                decide(None, Some(&theirs), None, &both()).unwrap(),
                Decision::Keep {
                    meter: Some(MeterId::Ccusage)
                }
            );
        }
    }

    #[test]
    fn with_nothing_usable_no_earlier_choice_stands_in() {
        let chosen = MeterFile {
            v: 1,
            meter: Some(MeterId::Ccusage),
            binary: Some(PathBuf::from("/opt/homebrew/bin/ccusage")),
            only_one_found: false,
        };
        // It is gone and nothing else can be used: were the file kept, every
        // run would be refused while `install` said the ledger alone gates.
        let decision = decide(None, Some(&chosen), None, &[]).unwrap();
        assert_eq!(decision, Decision::NoneFound);
        assert_eq!(decision.in_effect(), None);
        assert_eq!(decision.record(), Record::Remove);
        // What a person said stays said.
        assert_eq!(
            Decision::NoneChosen.record(),
            Record::Write(MeterFile {
                v: 1,
                meter: None,
                binary: None,
                only_one_found: false,
            })
        );
    }

    #[test]
    fn the_config_file_decides_when_it_names_a_meter() {
        assert_eq!(
            decide(Some(MeterId::AgentUsage), None, None, &both()).unwrap(),
            Decision::Config {
                meter: MeterId::AgentUsage
            }
        );
        let fail = decide(
            Some(MeterId::AgentUsage),
            None,
            Some(Selection::Meter(MeterId::Ccusage)),
            &both(),
        )
        .unwrap_err();
        assert_eq!(fail.exit, Exit::Usage);
    }

    #[test]
    fn a_named_meter_must_be_there_and_usable() {
        let unusable = vec![found(MeterId::AgentUsage, "/x/usage-cli", Some("too old"))];
        let fail = decide(
            None,
            None,
            Some(Selection::Meter(MeterId::AgentUsage)),
            &unusable,
        )
        .unwrap_err();
        assert!(fail.message.contains("too old"), "{}", fail.message);
        let fail = decide(
            None,
            None,
            Some(Selection::Meter(MeterId::Ccusage)),
            &unusable,
        )
        .unwrap_err();
        assert!(fail.message.contains("--meter-binary"), "{}", fail.message);
    }
}
