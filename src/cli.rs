//! The command surface. It is a security boundary: once a harness allows
//! `cahoots` outside its sandbox, these verbs and their flags ARE that
//! harness's unsandboxed capability (docs/THREAT-MODEL.md).
//!
//! So the verbs come in tiers. Agent verbs are the only ones the printed
//! allow-rules name. Human verbs change what cahoots may do, and refuse to run
//! without a terminal on stdin. Inspect verbs read and report.

use clap::{Parser, Subcommand};

use crate::exit::{self, Envelope, Exit};

pub const VERSION: &str = if cfg!(cahoots_dev_build) {
    concat!(
        env!("CARGO_PKG_VERSION"),
        " (dev build: CAHOOTS_*_DIR overrides honoured)"
    )
} else {
    env!("CARGO_PKG_VERSION")
};

#[derive(Debug, Parser)]
#[command(
    name = "cahoots",
    version = VERSION,
    about = "Lets coding-agent harnesses delegate to each other — usage-gated, model-aware, local-only"
)]
pub struct Cli {
    #[command(subcommand)]
    pub verb: Verb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum Verb {
    /// Choose the target (harness, model, effort) for a role, without running
    Pick,
    /// Delegate a brief to another harness, under the gate
    Run,
    /// Wait for a run to finish
    Wait,
    /// Show a run, or the runs of the current directory
    Status,
    /// Print a finished run's final message
    Result,
    /// Cancel a run
    Cancel,
    /// Record what became of a run's result: accepted, reworked or discarded
    Outcome,
    /// Show the locally learned briefing notes for a target
    Notes,
    /// Review a sampled run (opt-in)
    Review,
    /// Install the skill and agent definitions into the harnesses on this machine
    Install,
    /// Remove what `install` wrote
    Uninstall,
    /// Allow a harness to be used as a target
    Enable,
    /// List, show, revert or reset what was learned locally
    Learn,
    /// Show the effective registry of harnesses, models and roles
    Registry,
    /// Check the installation, the harness versions and the permission rules
    Doctor,
    /// Summarise recorded runs
    Report,
    /// Print every exit code, its class and its retry hint, as JSON
    ExitCodes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// What a harness calls. The printed allow-rules name exactly these.
    Agent,
    /// Changes authority or learned state. Needs a terminal on stdin.
    Human,
    /// Reads and reports. No rule is printed for it, and none is needed.
    Inspect,
}

impl Verb {
    pub const fn tier(self) -> Tier {
        match self {
            Verb::Pick
            | Verb::Run
            | Verb::Wait
            | Verb::Status
            | Verb::Result
            | Verb::Cancel
            | Verb::Outcome
            | Verb::Notes
            | Verb::Review => Tier::Agent,
            Verb::Install | Verb::Uninstall | Verb::Enable | Verb::Learn | Verb::Registry => {
                Tier::Human
            }
            Verb::Doctor | Verb::Report | Verb::ExitCodes => Tier::Inspect,
        }
    }
}

/// Decides the exit for a parsed command line. `stdin_is_terminal` is passed
/// in so the policy is testable without a pty.
pub fn dispatch(cli: &Cli, stdin_is_terminal: bool) -> Envelope {
    if cli.verb.tier() == Tier::Human && !stdin_is_terminal {
        return Envelope::new(
            Exit::Policy,
            "this verb changes what cahoots may do, so it only runs from a terminal".to_string(),
        );
    }
    if cli.verb == Verb::ExitCodes {
        return Envelope::new(Exit::Ok, None).with_data(exit::taxonomy());
    }
    Envelope::new(
        Exit::Internal,
        "not implemented yet — this build only carries the command surface".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    /// The agent tier is what gets allowed outside a sandbox. Growing it is a
    /// threat-model change, so it is pinned here by name.
    #[test]
    fn the_agent_tier_is_exactly_the_documented_verbs() {
        let expected = [
            "pick", "run", "wait", "status", "result", "cancel", "outcome", "notes", "review",
        ];
        let mut agent: Vec<String> = Cli::command()
            .get_subcommands()
            .map(|sub| sub.get_name().to_string())
            .filter(|name| {
                let cli = Cli::try_parse_from(["cahoots", name]).unwrap();
                cli.verb.tier() == Tier::Agent
            })
            .collect();
        agent.sort();
        let mut expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(agent, expected);
    }

    #[test]
    fn a_human_verb_is_refused_without_a_terminal() {
        let cli = Cli::try_parse_from(["cahoots", "install"]).unwrap();
        assert_eq!(dispatch(&cli, false).code, Exit::Policy.code());
        assert_ne!(dispatch(&cli, true).code, Exit::Policy.code());
    }

    #[test]
    fn an_agent_verb_needs_no_terminal() {
        let cli = Cli::try_parse_from(["cahoots", "status"]).unwrap();
        assert_ne!(dispatch(&cli, false).code, Exit::Policy.code());
    }
}
