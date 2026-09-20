//! The command surface. It is a security boundary: once a harness allows
//! `cahoots` outside its sandbox, these verbs and their flags ARE that
//! harness's unsandboxed capability (docs/THREAT-MODEL.md).
//!
//! So the verbs come in tiers. Agent verbs are the only ones the printed
//! allow-rules name. Human verbs change what cahoots may do, and refuse to run
//! without a terminal on stdin. Inspect verbs read and report.

use clap::{Parser, Subcommand};

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::dirs::Dirs;
use crate::exit::{self, Envelope, Exit, Fail, Res};
use crate::model::{HarnessId, Role};
use crate::registry::Registry;
use crate::run::client::{self, RunArgs};
use crate::run::supervise;

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

#[derive(Debug, Clone, Subcommand)]
pub enum Verb {
    /// Choose the target (harness, model, effort) for a role, without running
    Pick {
        #[arg(long)]
        role: Role,
        /// Only this harness
        #[arg(long)]
        to: Option<HarnessId>,
        /// The harness that is asking (it is never picked)
        #[arg(long)]
        caller: Option<HarnessId>,
    },
    /// Delegate a brief to another harness, under the gate
    Run {
        /// What the run is for: advise, review, explore — or implement, which writes
        #[arg(long)]
        role: Role,
        /// The brief, as a file (in the working directory, its repository, or a temp dir)
        #[arg(long)]
        brief: PathBuf,
        /// Only this harness
        #[arg(long)]
        to: Option<HarnessId>,
        /// The harness that is asking (it is never picked)
        #[arg(long)]
        caller: Option<HarnessId>,
        /// Where the callee works: another worktree of this repository
        #[arg(long)]
        dir: Option<PathBuf>,
        /// For a role that writes: work in a fresh worktree cut from HEAD
        #[arg(long, conflicts_with = "in_place")]
        fork: bool,
        /// For a role that writes: work in this very tree (your config must allow it)
        #[arg(long)]
        in_place: bool,
        /// Seconds to wait for the answer before returning "not finished"
        #[arg(long)]
        wait: Option<u64>,
        /// Seconds the run may take (never more than the configured limit)
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Wait for a run to finish
    Wait {
        run: String,
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Show a run, or the runs of the current directory
    Status { run: Option<String> },
    /// Print a finished run's final message
    Result { run: String },
    /// Cancel a run
    Cancel { run: String },
    /// Record what became of a run's result: accepted, reworked or discarded
    Outcome,
    /// Show the locally learned briefing notes for a target
    Notes,
    /// Review a sampled run (opt-in)
    Review,
    /// Install (or update) the skill and agent definitions for the harnesses on this machine
    Install {
        /// Only this harness (the shared skill is always written)
        #[arg(long)]
        harness: Option<HarnessId>,
        /// Say what would be written, and write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove what `install` wrote — and only that
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Print the skill this version installs
    Skill,
    /// Allow a harness to be used as a target (a run sends it repository content)
    Enable {
        harness: HarnessId,
        /// Stop delegating to it instead
        #[arg(long)]
        off: bool,
    },
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
    /// Where this build keeps things, and whether overrides are honoured
    #[command(name = "__dirs", hide = true)]
    Dirs,
    /// The detached supervisor of one run. Started by `run`, never by hand.
    #[command(name = "__supervise", hide = true)]
    Supervise { run: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// What a harness calls. The printed allow-rules name exactly these.
    Agent,
    /// Changes authority or learned state. Needs a terminal on stdin.
    Human,
    /// Reads and reports. No rule is printed for it, and none is needed.
    Inspect,
    /// cahoots calling itself.
    Internal,
}

/// The tier of a verb, by its command-line name. The ONE table: `Verb::tier`
/// reads it, and a test walks clap's tree to prove no verb is missing from it.
pub fn tier_of(name: &str) -> Option<Tier> {
    Some(match name {
        "pick" | "run" | "wait" | "status" | "result" | "cancel" | "outcome" | "notes"
        | "review" => Tier::Agent,
        "install" | "uninstall" | "enable" | "learn" | "registry" => Tier::Human,
        "doctor" | "report" | "exit-codes" | "skill" | "__dirs" => Tier::Inspect,
        "__supervise" => Tier::Internal,
        _ => return None,
    })
}

impl Verb {
    pub fn name(&self) -> &'static str {
        match self {
            Verb::Pick { .. } => "pick",
            Verb::Run { .. } => "run",
            Verb::Wait { .. } => "wait",
            Verb::Status { .. } => "status",
            Verb::Result { .. } => "result",
            Verb::Cancel { .. } => "cancel",
            Verb::Outcome => "outcome",
            Verb::Notes => "notes",
            Verb::Review => "review",
            Verb::Install { .. } => "install",
            Verb::Uninstall { .. } => "uninstall",
            Verb::Skill => "skill",
            Verb::Enable { .. } => "enable",
            Verb::Learn => "learn",
            Verb::Registry => "registry",
            Verb::Doctor => "doctor",
            Verb::Report => "report",
            Verb::ExitCodes => "exit-codes",
            Verb::Dirs => "__dirs",
            Verb::Supervise { .. } => "__supervise",
        }
    }

    pub fn tier(&self) -> Tier {
        tier_of(self.name()).unwrap_or(Tier::Human)
    }
}

/// Why a verb may not run at all, if it may not. PURE — it decides, it does
/// nothing — so the policy can be tested without ever executing a verb. (A
/// test that called `dispatch` to check this once ran a real `install`
/// against a real home. Test THIS function.)
pub fn refusal(verb: &Verb, stdin_is_terminal: bool) -> Option<Fail> {
    (verb.tier() == Tier::Human && !stdin_is_terminal).then(|| {
        Fail::policy("this verb changes what cahoots may do, so it only runs from a terminal")
    })
}

/// Runs a parsed command line and returns what to print and exit with.
pub fn dispatch(cli: Cli, stdin_is_terminal: bool) -> Envelope {
    if let Some(fail) = refusal(&cli.verb, stdin_is_terminal) {
        return fail.into();
    }
    run_verb(cli.verb).unwrap_or_else(Envelope::from)
}

fn run_verb(verb: Verb) -> Res<Envelope> {
    let ok = |data| Ok(Envelope::new(Exit::Ok, None).with_data(data));
    match verb {
        Verb::ExitCodes => ok(exit::taxonomy()),
        Verb::Dirs => {
            let dirs = Dirs::resolve()?;
            ok(serde_json::json!({
                "config": dirs.config,
                "state": dirs.state,
                "home": dirs.home,
                "overridden": dirs.overridden,
                "dev_overrides_honoured": crate::env::dev_overrides_honoured(),
            }))
        }
        Verb::Registry => {
            let registry = Registry::load(&Dirs::resolve()?)?;
            ok(serde_json::to_value(&registry)
                .map_err(|error| Fail::internal(format!("cannot encode the registry: {error}")))?)
        }
        Verb::Run {
            role,
            brief,
            to,
            caller,
            dir,
            fork,
            in_place,
            wait,
            timeout,
        } => client::run(RunArgs {
            role,
            brief,
            to,
            caller,
            dir,
            fork,
            in_place,
            wait_secs: wait,
            timeout_secs: timeout,
        }),
        Verb::Pick { role, to, caller } => client::pick_target(role, caller, to),
        Verb::Enable { harness, off } => {
            let enabled = crate::registry::set_enabled(&Dirs::resolve()?, harness, !off)?;
            ok(serde_json::json!({ "enabled": enabled }))
        }
        Verb::Doctor => crate::doctor::doctor(),
        Verb::Skill => ok(serde_json::json!({ "skill": crate::install::files::skill_text() })),
        Verb::Install { harness, dry_run } => {
            let dirs = Dirs::resolve()?;
            let files = crate::install::files::install(&dirs, harness, dry_run)?;
            let rules: serde_json::Map<String, serde_json::Value> = HarnessId::ALL
                .into_iter()
                .filter(|id| harness.is_none_or(|only| only == *id))
                .map(|id| {
                    let missing = crate::install::rules::missing(&dirs.home, id);
                    let lines: Vec<String> =
                        missing.iter().map(|verb| crate::install::rules::rule(id, verb)).collect();
                    (
                        id.to_string(),
                        serde_json::json!({ "add_to": crate::install::rules::rules_file(id), "rules": lines }),
                    )
                })
                .collect();
            let mut envelope = Envelope::new(
                Exit::Ok,
                "cahoots never edits a harness's permission rules: to let a harness delegate without \
                 a prompt, add the rules below yourself. Then `cahoots enable <harness>` for each \
                 target you want, and `cahoots doctor` to check."
                    .to_string(),
            );
            envelope.data = Some(
                serde_json::json!({ "dry_run": dry_run, "files": files, "rules_to_add": rules }),
            );
            Ok(envelope)
        }
        Verb::Uninstall { dry_run } => {
            let files = crate::install::files::uninstall(&Dirs::resolve()?, dry_run)?;
            ok(serde_json::json!({ "dry_run": dry_run, "files": files }))
        }
        Verb::Wait { run, timeout } => client::wait(&run, timeout),
        Verb::Status { run } => client::status(run.as_deref()),
        Verb::Result { run } => client::result(&run),
        Verb::Cancel { run } => client::cancel(&run),
        Verb::Supervise { run } => {
            supervise::supervise(&Dirs::resolve()?, &run)?;
            Ok(Envelope::new(Exit::Ok, None))
        }
        _ => Err(Fail::internal(
            "not implemented yet — this build does not carry this verb",
        )),
    }
}

/// Prints the envelope and turns it into the process's exit status. A closed
/// pipe is a quiet exit, not a panic (docs/SPIKE.md S5).
pub fn emit(envelope: &Envelope) -> ExitCode {
    use std::io::IsTerminal;
    let encode = if std::io::stdout().is_terminal() {
        serde_json::to_string_pretty // a person is reading
    } else {
        serde_json::to_string // one line, for an agent
    };
    let line = encode(envelope).unwrap_or_else(|_| {
        format!(
            r#"{{"v":1,"code":{},"class":"{}"}}"#,
            envelope.code, envelope.class
        )
    });
    let _ = writeln!(std::io::stdout(), "{line}");
    ExitCode::from(envelope.code)
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
    /// threat-model change, so it is pinned here by name — and every verb clap
    /// knows must be in the tier table, or it would default to nothing.
    #[test]
    fn the_agent_tier_is_exactly_the_documented_verbs() {
        let expected = [
            "pick", "run", "wait", "status", "result", "cancel", "outcome", "notes", "review",
        ];
        let mut agent = Vec::new();
        for sub in Cli::command().get_subcommands() {
            let tier = tier_of(sub.get_name())
                .unwrap_or_else(|| panic!("`{}` has no tier", sub.get_name()));
            if tier == Tier::Agent {
                agent.push(sub.get_name().to_string());
            }
            if tier == Tier::Internal {
                assert!(sub.is_hide_set(), "an internal verb is advertised");
            }
        }
        agent.sort();
        let mut expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(agent, expected);
    }

    #[test]
    fn a_human_verb_is_refused_without_a_terminal() {
        // `refusal`, never `dispatch`: deciding must not mean doing.
        for argv in [
            vec!["cahoots", "install"],
            vec!["cahoots", "uninstall"],
            vec!["cahoots", "enable", "codex"],
            vec!["cahoots", "registry"],
        ] {
            let verb = Cli::try_parse_from(argv).unwrap().verb;
            let fail = refusal(&verb, false).expect("refused without a terminal");
            assert_eq!(fail.exit, Exit::Policy);
            assert!(refusal(&verb, true).is_none());
        }
        let status = Cli::try_parse_from(["cahoots", "status"]).unwrap().verb;
        assert!(
            refusal(&status, false).is_none(),
            "an agent verb needs no terminal"
        );
    }

    #[test]
    fn role_and_harness_arguments_are_closed_vocabularies() {
        let run = |extra: &[&str]| {
            let mut argv = vec!["cahoots", "run", "--brief", "b.md"];
            argv.extend(extra);
            Cli::try_parse_from(argv)
        };
        assert!(run(&["--role", "review"]).is_ok());
        assert!(run(&["--role", "implement", "--fork"]).is_ok());
        assert!(run(&["--role", "implement", "--fork", "--in-place"]).is_err());
        assert!(run(&["--role", "deploy"]).is_err());
        assert!(run(&["--role", "review", "--to", "gemini"]).is_err());
        assert!(run(&["--role", "review", "--ungated"]).is_err());
    }
}
