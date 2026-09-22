//! The Agent Usage tracker (github.com/avihut/coding-agent-usage-tracker), by
//! its `usage-cli headroom`: what share of the plan is used, how old that
//! number is, and where it is heading. Its exit codes ARE the gate's refusal
//! codes, so an answer passes through unchanged.
//!
//! `usage-cli` is not on PATH by default: it lives in the app bundle.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use super::{Answer, Ask, Exe};
use crate::config::AgentUsageConfig;
use crate::exit::Exit;
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
            Exit::Stale => format!(
                "{harness}'s usage data is older than {} — is the tracker's daemon running?",
                self.max_age_minutes()
            ),
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
    use crate::model::HarnessId;

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
