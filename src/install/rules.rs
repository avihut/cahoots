//! The permission rules a human adds so a harness may call cahoots. cahoots
//! PRINTS these; it never writes them (hard rule 5) — widening what an agent
//! may run unprompted is a person's decision, made in that harness's own file.

use crate::model::HarnessId;

/// The verbs a harness is told to allow. Exactly the agent tier
/// (`cli::tier_of`); a test holds the two together.
pub const AGENT_VERBS: [&str; 9] = [
    "pick", "run", "wait", "status", "result", "cancel", "outcome", "notes", "review",
];

/// Where the rule goes, for a person reading the message.
pub const fn rules_file(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::Claude => "~/.claude/settings.json (permissions.allow)",
        HarnessId::Codex => "~/.codex/rules/default.rules",
    }
}

/// The rule that lets `harness` run one cahoots verb without a prompt and —
/// for Codex — outside its sandbox.
pub fn rule(harness: HarnessId, verb: &str) -> String {
    match harness {
        HarnessId::Claude => format!("\"Bash(cahoots {verb}:*)\""),
        HarnessId::Codex => {
            format!("prefix_rule(pattern=[\"cahoots\", \"{verb}\"], decision=\"allow\")")
        }
    }
}

/// What a sandboxed Codex caller is told: which rule is missing, where it
/// goes, and the one way of passing a brief that the rule can match
/// (docs/SPIKE.md S1).
pub fn sandboxed_codex_hint(verb: &str) -> String {
    format!(
        "`cahoots {verb}` is running inside Codex's sandbox, where it has no network and cannot \
         record a run. A person adds this line to {} and then you ask again, passing the brief as \
         a file (--brief <path>), never through a pipe or a heredoc: {}",
        rules_file(HarnessId::Codex),
        rule(HarnessId::Codex, verb)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Tier, tier_of};

    #[test]
    fn the_printed_rules_cover_exactly_the_agent_tier() {
        for verb in AGENT_VERBS {
            assert_eq!(tier_of(verb), Some(Tier::Agent), "{verb}");
        }
        for human in [
            "install",
            "uninstall",
            "enable",
            "learn",
            "registry",
            "__supervise",
        ] {
            assert!(!AGENT_VERBS.contains(&human));
        }
    }
}
