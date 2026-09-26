//! The permission rules a human adds so a harness may call cahoots. cahoots
//! PRINTS these; it never writes them (hard rule 5) — widening what an agent
//! may run unprompted is a person's decision, made in that harness's own file.

use crate::model::HarnessId;

/// The verbs a harness is told to allow. Exactly the agent tier
/// (`cli::tier_of`); a test holds the two together.
pub const AGENT_VERBS: [&str; 10] = [
    "pick", "run", "resume", "wait", "status", "result", "cancel", "outcome", "notes", "review",
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
            "settings",
            "enable",
            "learn",
            "registry",
            "__supervise",
        ] {
            assert!(!AGENT_VERBS.contains(&human));
        }
    }
}

/// Which of the agent verbs `harness` has NOT been told to allow — read-only,
/// from the harness's own file under `home`. A broad rule for all of
/// `cahoots` counts for every verb (and `doctor` says it is broader than it
/// needs to be).
pub fn missing(home: &std::path::Path, harness: HarnessId) -> Vec<&'static str> {
    let text = match harness {
        HarnessId::Claude => std::fs::read_to_string(home.join(".claude/settings.json")),
        HarnessId::Codex => std::fs::read_to_string(home.join(".codex/rules/default.rules")),
    }
    .unwrap_or_default();
    AGENT_VERBS
        .into_iter()
        .filter(|verb| !allows(harness, &text, verb))
        .collect()
}

/// True when a rule for the whole of `cahoots` — every verb, the human ones
/// included — is in place. It works, and it is more than was asked for.
pub fn is_broad(home: &std::path::Path, harness: HarnessId) -> bool {
    missing(home, harness).is_empty() && {
        let text = match harness {
            HarnessId::Claude => std::fs::read_to_string(home.join(".claude/settings.json")),
            HarnessId::Codex => std::fs::read_to_string(home.join(".codex/rules/default.rules")),
        }
        .unwrap_or_default();
        allows(harness, &text, "install")
    }
}

fn allows(harness: HarnessId, text: &str, verb: &str) -> bool {
    match harness {
        HarnessId::Claude => {
            let Ok(settings) = serde_json::from_str::<serde_json::Value>(text) else {
                return false;
            };
            let accepted = [
                format!("Bash(cahoots {verb}:*)"),
                format!("Bash(cahoots {verb} *)"),
                "Bash(cahoots:*)".to_string(),
                "Bash(cahoots *)".to_string(),
            ];
            settings["permissions"]["allow"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|rule| rule.as_str())
                .any(|rule| accepted.iter().any(|ok| ok == rule))
        }
        HarnessId::Codex => text.lines().any(|line| {
            let line: String = line.chars().filter(|c| !c.is_whitespace()).collect();
            !line.starts_with('#')
                && line.starts_with("prefix_rule(")
                && line.contains("decision=\"allow\"")
                && (line.contains(&format!("pattern=[\"cahoots\",\"{verb}\"]"))
                    || line.contains("pattern=[\"cahoots\"]"))
        }),
    }
}

#[cfg(test)]
mod rule_file_tests {
    use super::*;
    use std::fs;

    #[test]
    fn rules_are_read_from_each_harnesss_own_file() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            missing(home.path(), HarnessId::Claude).len(),
            AGENT_VERBS.len()
        );

        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::write(
            home.path().join(".claude/settings.json"),
            r#"{"permissions":{"allow":["Bash(cahoots run:*)","Bash(cahoots wait:*)","Bash(ls:*)"]}}"#,
        )
        .unwrap();
        let missing_claude = missing(home.path(), HarnessId::Claude);
        assert!(!missing_claude.contains(&"run") && missing_claude.contains(&"result"));

        fs::create_dir_all(home.path().join(".codex/rules")).unwrap();
        fs::write(
            home.path().join(".codex/rules/default.rules"),
            "prefix_rule(pattern=[\"swift\", \"test\"], decision=\"allow\")\n\
             # prefix_rule(pattern=[\"cahoots\", \"result\"], decision=\"allow\")\n\
             prefix_rule(pattern=[\"cahoots\", \"cancel\"], decision=\"forbid\")\n\
             prefix_rule(pattern=[\"cahoots\", \"run\"], decision=\"allow\")\n",
        )
        .unwrap();
        let missing_codex = missing(home.path(), HarnessId::Codex);
        assert!(!missing_codex.contains(&"run"));
        assert!(
            missing_codex.contains(&"result"),
            "a commented-out rule is not a rule"
        );
        assert!(
            missing_codex.contains(&"cancel"),
            "a forbid rule is not an allow rule"
        );
        assert!(!is_broad(home.path(), HarnessId::Codex));
    }

    #[test]
    fn every_printed_rule_is_recognised_when_pasted_back() {
        for harness in HarnessId::ALL {
            for verb in AGENT_VERBS {
                let text = match harness {
                    HarnessId::Claude => {
                        format!(
                            "{{\"permissions\":{{\"allow\":[{}]}}}}",
                            rule(harness, verb)
                        )
                    }
                    HarnessId::Codex => rule(harness, verb),
                };
                assert!(allows(harness, &text, verb), "{harness} {verb}");
            }
        }
    }
}
