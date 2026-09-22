//! ccusage (github.com/ccusage/ccusage): token counts, read from the
//! harnesses' own session logs on this machine.
//!
//! ccusage cannot see a plan's limit — no vendor publishes one in tokens — so
//! a percentage exists only against a limit the person declares:
//! `claude_block_tokens` for Claude Code's 5-hour block, `codex_day_tokens`
//! for a day of Codex. Without one, Claude Code is still held to the one
//! thing its logs do say: that it has hit its limit, and when that resets.
//! Codex without one is not measured at all, and only the ledger holds it.
//!
//! There is no forecast. ccusage's projection is a straight line through a
//! burn rate that counts cache reads, so during any busy session it says the
//! block is about to be blown — refusing on it would refuse nearly every run.
//! `cap` decides admission; the watchdog stops a run that passes `abort_at`.
//!
//! Always `--offline`: ccusage's own switch against fetching a price list.
//! Run with the network and every write under the home directory denied, it
//! gave the same answer, and the kernel's sandbox log — which does record a
//! denied attempt — showed none.

use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Value, json};

use super::{Answer, Ask, Exe, tokens};
use crate::config::CcusageConfig;
use crate::exit::Exit;
use crate::harness::Version;
use crate::model::HarnessId;
use crate::run::record::now;

/// The oldest version the command lines and the parser were written
/// against, and the first they have NOT been checked against.
pub const TESTED: (Version, Version) = (Version(20, 0, 0), Version(21, 0, 0));

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Ccusage {
    pub binary: Option<PathBuf>,
    /// Tokens in one 5-hour block that count as Claude Code's whole limit.
    pub claude_block_tokens: Option<u64>,
    /// Tokens in one day that count as Codex's whole limit.
    pub codex_day_tokens: Option<u64>,
}

/// What one report says about a harness's current window.
#[derive(Debug, Clone, PartialEq)]
struct Window {
    tokens: u64,
    /// Where the window is heading at the current rate, when ccusage says.
    projected: Option<u64>,
    /// Claude Code logged that it hit its usage limit, and it has not reset.
    limit_notice: Option<Notice>,
}

#[derive(Debug, Clone, PartialEq)]
struct Notice {
    /// When the limit resets (Unix seconds), if the time could be read.
    until: Option<u64>,
    raw: String,
}

impl Ccusage {
    pub fn from_config(table: &CcusageConfig, recorded: Option<PathBuf>) -> Ccusage {
        Ccusage {
            binary: table.binary.clone().or(recorded),
            claude_block_tokens: table.claude_block_tokens,
            codex_day_tokens: table.codex_day_tokens,
        }
    }

    pub fn limit(&self, harness: HarnessId) -> Option<u64> {
        match harness {
            HarnessId::Claude => self.claude_block_tokens,
            HarnessId::Codex => self.codex_day_tokens,
        }
    }

    /// Claude Code always (its limit notice needs no declared limit); Codex
    /// only against a declared one.
    pub fn measures(&self, harness: HarnessId) -> bool {
        harness == HarnessId::Claude || self.limit(harness).is_some()
    }

    pub fn argv(harness: HarnessId) -> Vec<String> {
        let args: &[&str] = match harness {
            // The 5-hour block that is running now, with its projection.
            HarnessId::Claude => &["blocks", "--active", "--json", "--offline"],
            // Today — in this machine's time zone, as ccusage counts days.
            HarnessId::Codex => &["codex", "daily", "--last", "1", "--json", "--offline"],
        };
        args.iter().map(|arg| arg.to_string()).collect()
    }

    pub fn ask(&self, exe: &Exe, ask: &Ask) -> Answer {
        let output = match super::run(exe, &Self::argv(ask.harness)) {
            Ok(output) => output,
            Err(why) => return Answer::no_answer(why),
        };
        if output.status != Some(0) {
            let status = output
                .status
                .map_or("on a signal".to_string(), |code| format!("{code}"));
            return Answer::no_answer(format!("ccusage exited {status}"));
        }
        let window = match ask.harness {
            HarnessId::Claude => read_blocks(&output.stdout, now()),
            HarnessId::Codex => read_daily(&output.stdout),
        };
        match window {
            Some(window) => self.judge(&window, ask),
            None => Answer::no_answer(format!(
                "ccusage printed something other than the report cahoots reads — cahoots reads ccusage {}",
                TESTED.0
            )),
        }
    }

    fn judge(&self, window: &Window, ask: &Ask) -> Answer {
        let limit = self.limit(ask.harness);
        let percent = limit.map(|limit| window.tokens as f64 * 100.0 / limit as f64);
        let reading = json!({
            "meter": "ccusage",
            "window": window_name(ask.harness),
            "tokens": window.tokens,
            "limit_tokens": limit,
            "percent": percent,
            "projected_tokens": window.projected,
            "limit_reached_until": window.limit_notice.as_ref().map(|notice| &notice.raw),
        });
        // `ask.forecast` is not acted on: see the module's note on projections.
        let over = percent.is_some_and(|percent| percent > f64::from(ask.cap));
        let (verdict, why) = match &window.limit_notice {
            Some(notice) => (Exit::OverCap, Some(limit_reached(notice))),
            None if over => (Exit::OverCap, None),
            None => (Exit::Ok, None),
        };
        Answer {
            verdict,
            percent,
            reading: Some(reading),
            why,
        }
    }

    pub fn refusal(&self, ask: &Ask, answer: &Answer) -> String {
        let harness = ask.harness;
        match (&answer.why, answer.verdict) {
            (Some(why), Exit::NoDigest | Exit::Config) => {
                format!("the usage meter gave no answer for {harness}: {why}")
            }
            (Some(why), _) => why.clone(),
            (None, Exit::OverCap) => {
                let window = window_name(harness);
                let limit = self.limit(harness).map(tokens).unwrap_or_default();
                let percent = answer.percent.unwrap_or_default();
                format!(
                    "{harness} is over its cap of {}% ({percent:.0}% of the {limit} tokens you set for a {window})",
                    ask.cap
                )
            }
            (None, _) => format!("the usage meter gave no answer for {harness}"),
        }
    }
}

const fn window_name(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::Claude => "5-hour block",
        HarnessId::Codex => "day",
    }
}

fn limit_reached(notice: &Notice) -> String {
    let when = match notice.until {
        Some(until) => {
            let minutes = until.saturating_sub(now()).div_ceil(60);
            if minutes >= 60 {
                format!("it resets in {}h {}m", minutes / 60, minutes % 60)
            } else {
                format!("it resets in {minutes}m")
            }
        }
        None => format!("it resets at {}", notice.raw),
    };
    format!("Claude Code has hit its usage limit — ccusage found the notice in its log; {when}")
}

/// A token count, however ccusage printed the number.
fn count(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.is_finite() && *f >= 0.0)
            .map(|f| f as u64)
    })
}

/// `blocks --active --json`: the running block, if one is running. No
/// running block is nothing used in this window — not a missing answer.
fn read_blocks(stdout: &str, now: u64) -> Option<Window> {
    let report: Value = serde_json::from_str(stdout.trim()).ok()?;
    let active = report
        .get("blocks")?
        .as_array()?
        .iter()
        .find(|block| block["isActive"] == true && block["isGap"] != true);
    let Some(block) = active else {
        return Some(Window {
            tokens: 0,
            projected: None,
            limit_notice: None,
        });
    };
    let limit_notice = block["usageLimitResetTime"]
        .as_str()
        .map(|raw| Notice {
            until: parse_utc(raw),
            raw: raw.to_string(),
        })
        // A reset that has passed is no longer a limit. One whose time
        // cannot be read still holds — it cannot outlast the block.
        .filter(|notice| notice.until.is_none_or(|until| until > now));
    Some(Window {
        tokens: count(&block["totalTokens"])?,
        projected: count(&block["projection"]["totalTokens"]),
        limit_notice,
    })
}

/// `codex daily --last 1 --json`: today's total. No row for today is nothing
/// used today.
fn read_daily(stdout: &str) -> Option<Window> {
    let report: Value = serde_json::from_str(stdout.trim()).ok()?;
    report.get("daily")?.as_array()?;
    Some(Window {
        tokens: count(&report["totals"]["totalTokens"])?,
        projected: None,
        limit_notice: None,
    })
}

/// `2026-09-21T10:00:00.000Z` → Unix seconds. UTC only, as ccusage writes it.
fn parse_utc(text: &str) -> Option<u64> {
    let text = text
        .strip_suffix('Z')
        .or_else(|| text.strip_suffix("+00:00"))?;
    let (date, time) = text.split_once('T')?;
    let numbers = |text: &str, sep: char| -> Option<Vec<i64>> {
        text.split(sep).map(|part| part.parse().ok()).collect()
    };
    let date = numbers(date, '-')?;
    let time = numbers(time.split('.').next()?, ':')?;
    let ([year, month, day], [hour, minute, second]) = (date.as_slice(), time.as_slice()) else {
        return None;
    };
    let valid = (1..=12).contains(month)
        && (1..=31).contains(day)
        && (0..=23).contains(hour)
        && (0..=59).contains(minute)
        && (0..=60).contains(second);
    if !valid {
        return None;
    }
    let secs = days_from_civil(*year, *month, *day) * 86_400 + hour * 3_600 + minute * 60 + second;
    u64::try_from(secs).ok()
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Whether a binary is ccusage, and new enough: `Ok` carries its version and
/// a note when it is newer than anything cahoots was checked against.
pub fn probe(exe: &Exe) -> Result<(Version, Option<String>), String> {
    let output = super::run(exe, &["--version".to_string()])?;
    let version = fingerprint(&output.stdout)
        .ok_or_else(|| "it does not identify itself as ccusage".to_string())?;
    if version < TESTED.0 {
        return Err(format!(
            "ccusage {version} is older than {}, the oldest cahoots reads",
            TESTED.0
        ));
    }
    let note = (version >= TESTED.1)
        .then(|| format!("{version} is newer than the versions cahoots was checked against"));
    Ok((version, note))
}

fn fingerprint(version_output: &str) -> Option<Version> {
    version_output
        .trim_start()
        .starts_with("ccusage")
        .then(|| Version::find_in(version_output))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIVE: &str = include_str!("../../tests/fixtures/meters/ccusage-blocks-active.json");
    const NONE_ACTIVE: &str = include_str!("../../tests/fixtures/meters/ccusage-blocks-idle.json");
    const LIMIT_HIT: &str = include_str!("../../tests/fixtures/meters/ccusage-blocks-limit.json");
    const CODEX_TODAY: &str = include_str!("../../tests/fixtures/meters/ccusage-codex-daily.json");
    const CODEX_IDLE: &str = include_str!("../../tests/fixtures/meters/ccusage-codex-idle.json");

    /// 2026-09-21T08:00:00Z, inside the fixtures' block.
    const NOW: u64 = 1_789_977_600;

    fn ask(harness: HarnessId, cap: u8) -> Ask {
        Ask {
            harness,
            cap,
            fresh: true,
            forecast: true,
        }
    }

    fn meter(claude: Option<u64>, codex: Option<u64>) -> Ccusage {
        Ccusage {
            binary: None,
            claude_block_tokens: claude,
            codex_day_tokens: codex,
        }
    }

    #[test]
    fn a_running_block_is_read_from_the_real_report_shape() {
        let window = read_blocks(ACTIVE, NOW).unwrap();
        assert_eq!(window.tokens, 151_119_380);
        assert_eq!(window.projected, Some(569_523_452));
        assert_eq!(window.limit_notice, None);
        let idle = read_blocks(NONE_ACTIVE, NOW).unwrap();
        assert_eq!((idle.tokens, idle.projected), (0, None));
    }

    #[test]
    fn a_declared_limit_makes_a_percentage_and_the_cap_holds() {
        let window = read_blocks(ACTIVE, NOW).unwrap();
        // 151M of 300M: 50%, under a cap of 72. The block's projection is
        // 569M — ccusage's straight line through a busy burn rate — and that
        // refuses nothing: the cap decides, the watchdog stops an overrun.
        let answer = meter(Some(300_000_000), None).judge(&window, &ask(HarnessId::Claude, 72));
        assert_eq!(answer.verdict, Exit::Ok);
        assert!((answer.percent.unwrap() - 50.37).abs() < 0.01);
        assert_eq!(answer.reading.unwrap()["projected_tokens"], 569_523_452);
        // 151M of 180M: 84%, over a cap of 72.
        let over = meter(Some(180_000_000), None).judge(&window, &ask(HarnessId::Claude, 72));
        assert_eq!(over.verdict, Exit::OverCap);
        let message = meter(Some(180_000_000), None).refusal(&ask(HarnessId::Claude, 72), &over);
        assert!(
            message.contains("cap of 72%") && message.contains("84% of the 180M tokens"),
            "{message}"
        );
        // Room to spare, and heading nowhere near the limit.
        let roomy = meter(Some(900_000_000), None).judge(&window, &ask(HarnessId::Claude, 72));
        assert_eq!(roomy.verdict, Exit::Ok);
        assert_eq!(roomy.reading.unwrap()["limit_tokens"], 900_000_000);
    }

    #[test]
    fn without_a_declared_limit_only_the_limit_notice_refuses() {
        let window = read_blocks(ACTIVE, NOW).unwrap();
        let answer = meter(None, None).judge(&window, &ask(HarnessId::Claude, 72));
        assert_eq!((answer.verdict, answer.percent), (Exit::Ok, None));

        let hit = read_blocks(LIMIT_HIT, NOW).unwrap();
        let answer = meter(None, None).judge(&hit, &ask(HarnessId::Claude, 72));
        assert_eq!(answer.verdict, Exit::OverCap);
        assert!(
            answer
                .why
                .as_deref()
                .unwrap()
                .contains("hit its usage limit")
        );
        // Once the reset time has passed, the notice no longer holds.
        let later = read_blocks(LIMIT_HIT, NOW + 3 * 3_600).unwrap();
        assert_eq!(later.limit_notice, None);
    }

    #[test]
    fn codex_is_measured_by_the_day_and_only_against_a_declared_limit() {
        let today = read_daily(CODEX_TODAY).unwrap();
        assert_eq!(today.tokens, 1_994_056);
        assert_eq!(read_daily(CODEX_IDLE).unwrap().tokens, 0);
        assert!(!meter(None, None).measures(HarnessId::Codex));
        assert!(meter(None, None).measures(HarnessId::Claude));
        let answer = meter(None, Some(2_000_000)).judge(&today, &ask(HarnessId::Codex, 72));
        assert_eq!(answer.verdict, Exit::OverCap, "99.7% of the day's limit");
        let answer = meter(None, Some(40_000_000)).judge(&today, &ask(HarnessId::Codex, 72));
        assert_eq!(answer.verdict, Exit::Ok);
    }

    #[test]
    fn a_report_of_another_shape_is_not_an_answer() {
        for text in [
            "",
            "not json",
            "{}",
            r#"{"blocks": {}}"#,
            r#"{"blocks":[{"isActive":true}]}"#,
        ] {
            assert_eq!(read_blocks(text, NOW), None, "{text:?}");
        }
        for text in [
            "",
            "{}",
            r#"{"daily": []}"#,
            r#"{"totals": {"totalTokens": 5}}"#,
        ] {
            assert_eq!(read_daily(text), None, "{text:?}");
        }
    }

    #[test]
    fn utc_timestamps_read_as_unix_seconds() {
        assert_eq!(parse_utc("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_utc("2026-09-21T08:00:00.000Z"), Some(NOW));
        assert_eq!(parse_utc("2026-09-21T08:00:00Z"), Some(NOW));
        assert_eq!(parse_utc("2026-09-21T08:00:00+00:00"), Some(NOW));
        assert_eq!(parse_utc("2024-02-29T12:30:15.5Z"), Some(1_709_209_815));
        for bad in [
            "2026-09-21T08:00:00",
            "2026-09-21T08:00:00+02:00",
            "2026-13-01T00:00:00Z",
            "2026-09-21 08:00:00Z",
            "soon",
        ] {
            assert_eq!(parse_utc(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_version_is_read_from_ccusage_itself() {
        assert_eq!(fingerprint("ccusage 20.0.23\n"), Some(Version(20, 0, 23)));
        assert_eq!(fingerprint("codex-cli 0.155.1"), None);
        assert_eq!(fingerprint("20.0.23"), None);
    }

    /// Drift: every flag cahoots passes exists in ccusage's real `--help`,
    /// captured at the tested version (tests/fixtures/help).
    #[test]
    fn every_emitted_flag_exists_in_the_captured_help() {
        for harness in HarnessId::ALL {
            let help = match harness {
                HarnessId::Claude => {
                    include_str!("../../tests/fixtures/help/ccusage-blocks-20.0.23.txt")
                }
                HarnessId::Codex => {
                    include_str!("../../tests/fixtures/help/ccusage-codex-daily-20.0.23.txt")
                }
            };
            for arg in Ccusage::argv(harness) {
                if arg.starts_with('-') {
                    assert!(
                        help.contains(&arg),
                        "ccusage's help for {harness} has no {arg}"
                    );
                }
            }
        }
    }
}
