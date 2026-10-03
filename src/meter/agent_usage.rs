//! The Agent Usage tracker (github.com/avihut/coding-agent-usage-tracker), by
//! its `usage-cli headroom`: what share of the plan is used, how old that
//! number is, and where it is heading. Its exit codes ARE the gate's refusal
//! codes, so an answer passes through unchanged.
//!
//! `usage-cli` is not on PATH by default: it lives in the app bundle.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Answer, Ask, Exe};
use crate::config::AgentUsageConfig;
use crate::exit::Exit;
use crate::model::HarnessId;
use crate::registry::DEFAULT_MAX_DATA_AGE_SECS;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentUsage {
    pub binary: Option<PathBuf>,
    /// How old a measurement may be before it counts as stale.
    pub max_data_age_secs: u64,
}

/// The app bundle's name, and where its launch agent is registered.
const BUNDLE: &str = "AgentUsage.app/Contents/MacOS";
const LAUNCH_AGENT: &str = "Library/LaunchAgents/io.github.avihut.usaged.plist";

impl AgentUsage {
    pub fn from_config(table: &AgentUsageConfig, recorded: Option<PathBuf>) -> AgentUsage {
        AgentUsage {
            binary: table.binary.clone().or(recorded),
            max_data_age_secs: table.max_data_age_secs.unwrap_or(DEFAULT_MAX_DATA_AGE_SECS),
        }
    }

    /// `headroom --provider <h> --cap <n> [--forecast red] --json
    /// [--max-data-age <m>m]` — built here, from typed values only.
    pub fn argv(&self, ask: &Ask) -> Vec<String> {
        let mut args = vec![
            "headroom".to_string(),
            "--provider".to_string(),
            ask.harness.as_str().to_string(),
            "--cap".to_string(),
            ask.cap.to_string(),
        ];
        if ask.forecast {
            args.extend(["--forecast".to_string(), "red".to_string()]);
        }
        args.push("--json".to_string());
        if ask.fresh {
            args.extend(["--max-data-age".to_string(), self.max_age_minutes()]);
        }
        args
    }

    fn max_age_minutes(&self) -> String {
        format!("{}m", self.max_data_age_secs.div_ceil(60))
    }

    /// Whether the tracker is still polling `harness`, asked after a stale
    /// answer: a quiet Claude is polled as seldom as hourly with nothing
    /// wrong (issue #73), so stale alone does not say the tracker stopped.
    /// `Err` says why the answer is not a yes, in words for the refusal.
    pub fn still_polling(&self, exe: &Exe, harness: HarnessId, now: u64) -> Result<(), String> {
        let polling = match super::run(exe, &status_argv(harness)) {
            Ok(output) => polling(output.status, output.stdout.trim(), harness, now),
            Err(why) => Polling::Unknown(why),
        };
        match polling {
            Polling::Yes => Ok(()),
            Polling::Stopped {
                published_ago,
                due_ago,
            } => Err(format!(
                "and the tracker last published {} ago, its next poll due {} ago",
                minutes(published_ago),
                minutes(due_ago)
            )),
            Polling::Unknown(why) => Err(format!(
                "and the tracker could not say whether it is still polling ({why})"
            )),
        }
    }

    pub fn ask(&self, exe: &Exe, ask: &Ask) -> Answer {
        let output = match super::run(exe, &self.argv(ask)) {
            Ok(output) => output,
            Err(why) => return Answer::no_answer(format!("the usage meter: {why}")),
        };
        let reading: Option<Value> = serde_json::from_str(output.stdout.trim()).ok();
        let percent = reading.as_ref().and_then(|r| r["percent"].as_f64());
        let verdict = verdict(output.status);
        let why = match output.status {
            // 19 is a query this usage-cli does not understand: one from
            // before the `headroom` noun.
            Some(19) => {
                Some("this usage-cli has no `headroom` noun — update Agent Usage".to_string())
            }
            _ => None,
        };
        Answer {
            verdict,
            percent,
            reading,
            why,
        }
    }

    pub fn refusal(&self, ask: &Ask, answer: &Answer) -> String {
        let harness = ask.harness;
        let cap = ask.cap;
        let used = answer
            .percent
            .map_or(String::new(), |p| format!(" ({p:.0}% used)"));
        match answer.verdict {
            Exit::Stale => {
                let age = answer
                    .reading
                    .as_ref()
                    .and_then(|r| r["dataAge"].as_u64())
                    .map_or_else(
                        || "older than".to_string(),
                        |age| format!("{} old, past", minutes(age)),
                    );
                let why = answer
                    .why
                    .as_deref()
                    .map_or(String::new(), |why| format!(", {why}"));
                format!(
                    "{harness}'s usage data is {age} the {} allowed{why} — `usage-cli status` \
                     shows when it last measured and when it polls next",
                    self.max_age_minutes()
                )
            }
            Exit::OverCap => format!("{harness} is over its cap of {cap}%{used}"),
            Exit::Forecast => {
                format!("{harness} is on course to run out before its limit resets{used}")
            }
            Exit::NoData => format!("the tracker has no usage numbers for {harness}"),
            _ => match &answer.why {
                Some(why) => format!("the usage meter gave no answer for {harness}: {why}"),
                None => format!(
                    "the usage meter gave no answer for {harness} — cahoots needs a usage-cli that has the `headroom` noun, and a running tracker"
                ),
            },
        }
    }
}

/// The tracker's exit code, as the gate's verdict. 13 (no digest), 19 (a
/// usage-cli too old to know `headroom`), a signal, anything new: not an
/// answer, so not a yes.
fn verdict(status: Option<i32>) -> Exit {
    match status {
        Some(0) => Exit::Ok,
        Some(21) => Exit::Stale,
        Some(24) => Exit::OverCap,
        Some(25) => Exit::Forecast,
        Some(26) => Exit::NoData,
        _ => Exit::NoDigest,
    }
}

/// `status --provider <h> --fields provider,generated,next-poll --json
/// --unix`: whom the tracker answered for, when its engine last published,
/// and when it polls next, in Unix seconds.
fn status_argv(harness: HarnessId) -> Vec<String> {
    [
        "status",
        "--provider",
        harness.as_str(),
        "--fields",
        "provider,generated,next-poll",
        "--json",
        "--unix",
    ]
    .map(String::from)
    .to_vec()
}

/// What `status_argv` answers, and nothing else: an unknown key, a stamp
/// that is not a whole number, or one missing is not an answer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Heartbeat {
    provider: String,
    generated: u64,
    #[serde(rename = "next-poll")]
    next_poll: u64,
}

#[derive(Debug, PartialEq)]
enum Polling {
    /// It published within twice its own poll horizon: slow, not stopped.
    Yes,
    /// Its next poll is long past, by the tracker's own stamps.
    Stopped { published_ago: u64, due_ago: u64 },
    /// No answer that says either way — which refuses, like a no.
    Unknown(String),
}

/// How long a heartbeat may be silent before the engine counts as stopped,
/// however soon it meant to poll — the tracker's `takeoverFloor`.
const HEARTBEAT_FLOOR: u64 = 180;
/// How far ahead of this clock a stamp may sit and still read as just taken
/// — the tracker's `clockTolerance`. Further means the clock was set back.
const CLOCK_TOLERANCE: u64 = 60;

/// The tracker's own rule for an engine that stopped
/// (`EngineHostBroker.heartbeatStale`): silent for longer than twice the gap
/// it left to its next poll, and longer than the floor. Its next poll counts
/// a 429 backoff, so an engine sitting one out is still polling.
fn polling(status: Option<i32>, stdout: &str, harness: HarnessId, now: u64) -> Polling {
    if status != Some(0) {
        return Polling::Unknown(match status {
            Some(13) => "it has no numbers".to_string(),
            Some(code) => format!("`usage-cli status` exited {code}"),
            None => "`usage-cli status` was killed".to_string(),
        });
    }
    let Ok(beat) = serde_json::from_str::<Heartbeat>(stdout) else {
        return Polling::Unknown(
            "`usage-cli status` answered in a shape cahoots does not know".into(),
        );
    };
    if beat.provider != harness.as_str() {
        return Polling::Unknown(format!("it answered for {}, not {harness}", beat.provider));
    }
    if beat.generated > now.saturating_add(CLOCK_TOLERANCE) {
        return Polling::Unknown("its stamps do not fit this clock".into());
    }
    let silent = now.saturating_sub(beat.generated);
    // A next poll already due when it published (a timer that drifted) is
    // a horizon of nothing, and the floor alone holds — as in the tracker.
    let horizon = beat.next_poll.saturating_sub(beat.generated);
    if silent > horizon.saturating_mul(2).max(HEARTBEAT_FLOOR) {
        Polling::Stopped {
            published_ago: silent,
            due_ago: now.saturating_sub(beat.next_poll),
        }
    } else {
        Polling::Yes
    }
}

/// Seconds as whole minutes, rounded up: `41m`, `2h 3m`.
fn minutes(secs: u64) -> String {
    let minutes = secs.div_ceil(60);
    match minutes {
        0..60 => format!("{minutes}m"),
        _ => format!("{}h {}m", minutes / 60, minutes % 60),
    }
}

/// Where `install` looks, in order: the copy the tracker's launch agent runs
/// (the daemon that writes the numbers), then PATH, then the app in each
/// Applications folder.
pub fn candidates(home: &Path, on_path: Option<PathBuf>, applications: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Some(daemon) = launch_agent_program(&home.join(LAUNCH_AGENT))
        && let Some(dir) = daemon.parent()
    {
        found.push(dir.join("usage-cli"));
    }
    found.extend(on_path);
    found.extend(
        applications
            .iter()
            .map(|folder| folder.join(BUNDLE).join("usage-cli")),
    );
    found
}

/// The program a launch agent's XML plist runs: the first string of its
/// `ProgramArguments`. `None` for anything else, a binary plist included.
fn launch_agent_program(plist: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(plist)
        .ok()
        .filter(|t| t.len() < 64 * 1024)?;
    let after = &text[text.find("<key>ProgramArguments</key>")?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = start + after[start..].find("</string>")?;
    let path = after[start..end]
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&amp;", "&");
    let path = PathBuf::from(path.trim());
    path.is_absolute().then_some(path)
}

/// Whether a binary is Agent Usage's `usage-cli` and can gate: it answers the
/// `headroom` noun. Every noun is answered from the tracker's digest — never
/// the network, never a credential — and an unknown one exits 19.
pub fn probe(exe: &Exe) -> Result<Option<String>, String> {
    let args = ["headroom", "--provider", "claude", "--cap", "100", "--json"].map(String::from);
    match super::run(exe, &args)?.status {
        Some(0 | 21 | 24 | 25 | 26) => Ok(None),
        Some(13) => Ok(Some(
            "it has no numbers yet — is the tracker running?".to_string(),
        )),
        Some(19) => Err(
            "it has no `headroom` noun yet — cahoots needs an Agent Usage that has it".to_string(),
        ),
        _ => Err("it does not answer like Agent Usage's usage-cli".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meter() -> AgentUsage {
        AgentUsage::from_config(&AgentUsageConfig::default(), None)
    }

    #[test]
    fn the_command_line_is_the_headroom_contract() {
        let ask = Ask {
            harness: HarnessId::Codex,
            cap: 77,
            fresh: true,
            forecast: true,
        };
        assert_eq!(
            meter().argv(&ask).join(" "),
            "headroom --provider codex --cap 77 --forecast red --json --max-data-age 15m"
        );
        let watch = Ask {
            harness: HarnessId::Claude,
            cap: 90,
            fresh: true,
            forecast: false,
        };
        assert_eq!(
            meter().argv(&watch).join(" "),
            "headroom --provider claude --cap 90 --json --max-data-age 15m"
        );
    }

    #[test]
    fn the_heartbeat_is_asked_by_its_own_command_line() {
        assert_eq!(
            status_argv(HarnessId::Claude).join(" "),
            "status --provider claude --fields provider,generated,next-poll --json --unix"
        );
    }

    #[test]
    fn a_tracker_is_polling_until_twice_its_horizon_has_passed() {
        let beat = |generated: u64, next: u64| {
            format!(r#"{{"provider": "claude", "generated": {generated}, "next-poll": {next}}}"#)
        };
        let at = |now: u64, text: &str| polling(Some(0), text, HarnessId::Claude, now);
        // Published at 10 000, next poll 40 minutes on: polling for 80.
        let quiet = beat(10_000, 12_400);
        assert_eq!(at(12_000, &quiet), Polling::Yes, "between polls");
        assert_eq!(
            at(14_800, &quiet),
            Polling::Yes,
            "exactly twice the horizon"
        );
        assert_eq!(
            at(14_801, &quiet),
            Polling::Stopped {
                published_ago: 4_801,
                due_ago: 2_401
            }
        );
        // A poll due at once still has the floor.
        let due = beat(10_000, 10_000);
        assert_eq!(at(10_180, &due), Polling::Yes);
        assert!(matches!(at(10_181, &due), Polling::Stopped { .. }));
        // A clock set back since: no age can be read off it.
        assert_eq!(at(9_940, &quiet), Polling::Yes, "within the tolerance");
        assert!(matches!(at(9_939, &quiet), Polling::Unknown(_)));
        // A poll already due when it published: the floor alone.
        let overdue = beat(10_000, 9_000);
        assert_eq!(at(10_100, &overdue), Polling::Yes);
        assert!(matches!(at(10_181, &overdue), Polling::Stopped { .. }));
        // A horizon too large to double is never a panic.
        assert_eq!(at(12_000, &beat(10_000, u64::MAX)), Polling::Yes);
    }

    #[test]
    fn only_a_heartbeat_for_the_harness_asked_says_anything() {
        let at = |status: Option<i32>, text: &str| polling(status, text, HarnessId::Claude, 10_000);
        let good = r#"{"provider": "claude", "generated": 9900, "next-poll": 10200}"#;
        assert_eq!(at(Some(0), good), Polling::Yes);
        for (status, text) in [
            (Some(13), ""),
            (Some(19), good),
            (None, good),
            (Some(0), ""),
            (
                Some(0),
                r#"{"provider": "codex", "generated": 9900, "next-poll": 10200}"#,
            ),
            (Some(0), r#"{"provider": "claude", "generated": 9900}"#),
            (
                Some(0),
                r#"{"provider": "claude", "generated": 9900, "next-poll": null}"#,
            ),
            (
                Some(0),
                r#"{"provider": "claude", "generated": "9900", "next-poll": 10200}"#,
            ),
            (
                Some(0),
                r#"{"provider": "claude", "generated": 9900.5, "next-poll": 10200}"#,
            ),
            (
                Some(0),
                r#"{"provider": "claude", "generated": 9900, "next-poll": 10200, "stale": false}"#,
            ),
        ] {
            assert!(
                matches!(at(status, text), Polling::Unknown(_)),
                "{status:?} {text}"
            );
        }
    }

    #[test]
    fn minutes_round_up() {
        for (secs, said) in [
            (0, "0m"),
            (1, "1m"),
            (2_460, "41m"),
            (3_600, "1h 0m"),
            (7_380, "2h 3m"),
        ] {
            assert_eq!(minutes(secs), said);
        }
    }

    #[test]
    fn only_the_five_headroom_codes_are_answers() {
        assert_eq!(verdict(Some(0)), Exit::Ok);
        assert_eq!(verdict(Some(21)), Exit::Stale);
        assert_eq!(verdict(Some(24)), Exit::OverCap);
        assert_eq!(verdict(Some(25)), Exit::Forecast);
        assert_eq!(verdict(Some(26)), Exit::NoData);
        for status in [Some(13), Some(19), Some(1), Some(99), None] {
            assert_eq!(verdict(status), Exit::NoDigest, "{status:?}");
        }
    }

    #[test]
    fn the_launch_agent_names_the_copy_that_runs() {
        let dir = tempfile::tempdir().unwrap();
        let plist = dir.path().join("agent.plist");
        fs::write(
            &plist,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>io.github.avihut.usaged</string>
	<key>ProgramArguments</key>
	<array>
		<string>/Users/someone/src/tracker/AgentUsage.app/Contents/MacOS/usaged</string>
	</array>
</dict>
</plist>"#,
        )
        .unwrap();
        assert_eq!(
            launch_agent_program(&plist),
            Some(PathBuf::from(
                "/Users/someone/src/tracker/AgentUsage.app/Contents/MacOS/usaged"
            ))
        );
        fs::write(&plist, "bplist00\u{1}garbage").unwrap();
        assert_eq!(launch_agent_program(&plist), None);
        fs::write(
            &plist,
            "<key>ProgramArguments</key><array><string>usaged</string></array>",
        )
        .unwrap();
        assert_eq!(
            launch_agent_program(&plist),
            None,
            "a relative path is not a place"
        );
    }
}
