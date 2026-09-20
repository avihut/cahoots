//! Exit codes are API. Every refusal and every failure a caller can act on
//! has its own code, and the same answer is printed as a JSON envelope so an
//! agent never has to parse prose.
//!
//! 11–26 belong to `usage-cli` (the Agent Usage tracker); the gate's refusals
//! reuse its `headroom` codes unchanged so a caller learns one vocabulary.
//! cahoots' own codes start at 30. A callee's exit code is never passed
//! through.

use serde::Serialize;

/// The envelope's schema version.
pub const ENVELOPE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Ok,
    Internal,
    Usage,
    // ── gate refusals, identical to `usage-cli headroom` ──
    NoDigest,
    Stale,
    OverCap,
    Forecast,
    NoData,
    // ── admission ──
    NoEligibleTarget,
    TargetUnavailable,
    Busy,
    Policy,
    Config,
    // ── run outcomes ──
    RunFailed,
    TimedOut,
    Cancelled,
    Budget,
    // ── run lookup ──
    NoSuchRun,
    NotFinished,
}

/// What a caller should do about a non-zero exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Retry {
    Never,
    Later,
    AfterReset,
    OtherTarget,
    FixConfig,
}

impl Exit {
    pub const ALL: [Exit; 19] = [
        Exit::Ok,
        Exit::Internal,
        Exit::Usage,
        Exit::NoDigest,
        Exit::Stale,
        Exit::OverCap,
        Exit::Forecast,
        Exit::NoData,
        Exit::NoEligibleTarget,
        Exit::TargetUnavailable,
        Exit::Busy,
        Exit::Policy,
        Exit::Config,
        Exit::RunFailed,
        Exit::TimedOut,
        Exit::Cancelled,
        Exit::Budget,
        Exit::NoSuchRun,
        Exit::NotFinished,
    ];

    pub const fn code(self) -> u8 {
        match self {
            Exit::Ok => 0,
            Exit::Internal => 1,
            Exit::Usage => 2,
            Exit::NoDigest => 13,
            Exit::Stale => 21,
            Exit::OverCap => 24,
            Exit::Forecast => 25,
            Exit::NoData => 26,
            Exit::NoEligibleTarget => 30,
            Exit::TargetUnavailable => 31,
            Exit::Busy => 32,
            Exit::Policy => 33,
            Exit::Config => 34,
            Exit::RunFailed => 40,
            Exit::TimedOut => 41,
            Exit::Cancelled => 42,
            Exit::Budget => 43,
            Exit::NoSuchRun => 50,
            Exit::NotFinished => 51,
        }
    }

    pub const fn class(self) -> &'static str {
        match self {
            Exit::Ok => "ok",
            Exit::Internal => "internal_error",
            Exit::Usage => "usage_error",
            Exit::NoDigest => "no_usage_data_source",
            Exit::Stale => "usage_data_stale",
            Exit::OverCap => "over_cap",
            Exit::Forecast => "forecast_refused",
            Exit::NoData => "no_usage_data",
            Exit::NoEligibleTarget => "no_eligible_target",
            Exit::TargetUnavailable => "target_unavailable",
            Exit::Busy => "busy",
            Exit::Policy => "refused_by_policy",
            Exit::Config => "config_error",
            Exit::RunFailed => "run_failed",
            Exit::TimedOut => "run_timed_out",
            Exit::Cancelled => "run_cancelled",
            Exit::Budget => "run_stopped_by_budget",
            Exit::NoSuchRun => "no_such_run",
            Exit::NotFinished => "not_finished",
        }
    }

    pub const fn retry(self) -> Retry {
        match self {
            Exit::Ok | Exit::Internal | Exit::Usage => Retry::Never,
            Exit::Policy | Exit::Cancelled | Exit::NoSuchRun => Retry::Never,
            Exit::NoEligibleTarget | Exit::Busy | Exit::TimedOut | Exit::NotFinished => {
                Retry::Later
            }
            Exit::OverCap | Exit::Forecast | Exit::Budget => Retry::AfterReset,
            Exit::NoDigest
            | Exit::Stale
            | Exit::NoData
            | Exit::TargetUnavailable
            | Exit::RunFailed => Retry::OtherTarget,
            Exit::Config => Retry::FixConfig,
        }
    }
}

/// What every non-interactive exit prints on stdout.
#[derive(Debug, Serialize)]
pub struct Envelope {
    pub v: u32,
    pub code: u8,
    pub class: &'static str,
    pub retry: Retry,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The verb's own answer, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Envelope {
    pub fn new(exit: Exit, message: impl Into<Option<String>>) -> Self {
        Envelope {
            v: ENVELOPE_VERSION,
            code: exit.code(),
            class: exit.class(),
            retry: exit.retry(),
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// A refusal or a failure on its way to becoming an exit: the code, and one
/// sentence a person (or an agent) can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fail {
    pub exit: Exit,
    pub message: String,
}

impl Fail {
    pub fn new(exit: Exit, message: impl Into<String>) -> Self {
        Fail {
            exit,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Fail::new(Exit::Internal, message)
    }

    pub fn config(message: impl Into<String>) -> Self {
        Fail::new(Exit::Config, message)
    }

    pub fn policy(message: impl Into<String>) -> Self {
        Fail::new(Exit::Policy, message)
    }
}

impl From<Fail> for Envelope {
    fn from(fail: Fail) -> Envelope {
        Envelope::new(fail.exit, fail.message)
    }
}

pub type Res<T> = Result<T, Fail>;

/// The whole taxonomy, as data — what `cahoots exit-codes` prints, so a skill
/// or a script can read the contract from the binary it is about to call.
pub fn taxonomy() -> serde_json::Value {
    Exit::ALL
        .iter()
        .map(|exit| {
            serde_json::json!({
                "code": exit.code(),
                "class": exit.class(),
                "retry": exit.retry(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn codes_and_classes_are_unique() {
        let codes: HashSet<u8> = Exit::ALL.iter().map(|e| e.code()).collect();
        let classes: HashSet<&str> = Exit::ALL.iter().map(|e| e.class()).collect();
        assert_eq!(codes.len(), Exit::ALL.len());
        assert_eq!(classes.len(), Exit::ALL.len());
    }

    /// 11–26 are `usage-cli`'s. cahoots may only reuse the five `headroom`
    /// codes there, with their meanings; everything of its own is 30 or above.
    #[test]
    fn own_codes_stay_out_of_the_trackers_range() {
        let borrowed = [13, 21, 24, 25, 26];
        for exit in Exit::ALL {
            let code = exit.code();
            if (11..=29).contains(&code) {
                assert!(borrowed.contains(&code), "{exit:?} squats on {code}");
            }
        }
    }

    #[test]
    fn envelope_is_stable_json() {
        let json = serde_json::to_string(&Envelope::new(Exit::OverCap, None)).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"code":24,"class":"over_cap","retry":"after_reset"}"#
        );
    }
}
