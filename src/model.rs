//! The closed vocabulary: which harnesses exist, what a run may be for, and
//! how hard a model should think. Everything that reaches a command line is
//! one of these enums or a validated [`ModelName`] — never a free string.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::exit::{Exit, Fail};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HarnessId {
    Claude,
    Codex,
}

impl HarnessId {
    pub const ALL: [HarnessId; 2] = [HarnessId::Claude, HarnessId::Codex];

    pub const fn as_str(self) -> &'static str {
        match self {
            HarnessId::Claude => "claude",
            HarnessId::Codex => "codex",
        }
    }
}

/// What a run is for. `advise`, `review` and `explore` are read-only. The one
/// writer, `implement`, only ever works in a worktree of its own (or, if the
/// user's config says so, in place) — docs/THREAT-MODEL.md → Writers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// A second opinion on an approach, a design or a decision.
    Advise,
    /// Find what is wrong with a change or a piece of code.
    Review,
    /// Read the codebase and report what is there.
    Explore,
    /// Make a change. The only role that may write.
    Implement,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::Advise, Role::Review, Role::Explore, Role::Implement];

    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Advise => "advise",
            Role::Review => "review",
            Role::Explore => "explore",
            Role::Implement => "implement",
        }
    }

    pub const fn is_read_only(self) -> bool {
        match self {
            Role::Advise | Role::Review | Role::Explore => true,
            Role::Implement => false,
        }
    }

    /// Percentage points of plan kept free for the run itself, so that
    /// check-then-act does not admit the run that crosses the cap.
    pub const fn reserve(self) -> u8 {
        match self {
            Role::Advise | Role::Review | Role::Explore => 3,
            // Writing takes longer and costs more than reading.
            Role::Implement => 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub const fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

macro_rules! closed_vocabulary {
    ($ty:ident, $what:literal) => {
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
        impl FromStr for $ty {
            type Err = Fail;
            fn from_str(s: &str) -> Result<Self, Fail> {
                serde_json::from_value(serde_json::Value::String(s.to_string()))
                    .map_err(|_| Fail::new(Exit::Usage, format!("unknown {}: {s:?}", $what)))
            }
        }
    };
}
closed_vocabulary!(HarnessId, "harness");
closed_vocabulary!(Role, "role");
closed_vocabulary!(Effort, "effort");

/// A model name as a vendor spells it. It becomes the value of a `--model`
/// flag, so it is held to a shape that cannot be mistaken for a flag or carry
/// anything a shell, a path or a config parser would interpret.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModelName(String);

impl ModelName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ModelName {
    type Error = String;
    fn try_from(name: String) -> Result<Self, String> {
        let shape_ok = (1..=64).contains(&name.len())
            && name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if shape_ok {
            Ok(ModelName(name))
        } else {
            Err(format!(
                "model name {name:?} must be 1–64 of [A-Za-z0-9._-] and start with a letter or digit"
            ))
        }
    }
}

impl From<ModelName> for String {
    fn from(name: ModelName) -> String {
        name.0
    }
}

/// One way to staff a role: a harness, a model on it, and an effort level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub harness: HarnessId,
    pub model: ModelName,
    pub effort: Effort,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_name_cannot_look_like_a_flag_or_a_path() {
        for bad in [
            "",
            "-m",
            "--dangerously",
            "a b",
            "../x",
            "a/b",
            "a;b",
            "a=b",
            "$(x)",
        ] {
            assert!(ModelName::try_from(bad.to_string()).is_err(), "{bad:?}");
        }
        for good in ["opus", "gpt-5.6-sol", "claude-haiku-4-5-20251001", "gpt_6"] {
            assert!(ModelName::try_from(good.to_string()).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn vocabulary_round_trips_through_strings() {
        for harness in HarnessId::ALL {
            assert_eq!(harness.as_str().parse::<HarnessId>().unwrap(), harness);
        }
        for role in Role::ALL {
            assert_eq!(role.as_str().parse::<Role>().unwrap(), role);
        }
        assert_eq!("xhigh".parse::<Effort>().unwrap(), Effort::Xhigh);
        assert!("ultra-mega".parse::<Effort>().is_err());
        assert!("deploy".parse::<Role>().is_err());
        assert!(!Role::Implement.is_read_only());
        assert!(Role::Implement.reserve() > Role::Review.reserve());
    }
}
