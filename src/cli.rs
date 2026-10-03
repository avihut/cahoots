//! The command surface. It is a security boundary: once a harness allows
//! `cahoots` outside its sandbox, these verbs and their flags ARE that
//! harness's unsandboxed capability (docs/THREAT-MODEL.md).
//!
//! So the verbs come in tiers. Agent verbs are the only ones the printed
//! allow-rules name. Human verbs change what cahoots may do, and refuse to run
//! without a terminal on stdin. Inspect verbs read and report.
//!
//! This is the command layer, the one place the logic and the interface meet
//! (hard rule 11). It calls the logic, puts the logic's questions to a person
//! (`questions`, on `crate::tui`), hands the answers back, and prints what
//! the verb said: the one JSON envelope, or, for a person reading a human
//! verb, `doctor` or `report` at a terminal, the same in words (`endings`,
//! `settings`).

mod endings;
mod questions;
mod settings;

use clap::{ArgGroup, Parser, Subcommand};

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::dirs::Dirs;
use crate::exit::{self, Envelope, Exit, Fail};
use crate::meter::detect::{self, Decision, Found};
use crate::meter::{MeterFile, MeterId, Selection};
use crate::model::{HarnessId, Role};
use crate::registry::Registry;
use crate::run::client::{self, ResumeArgs, RunArgs};
use crate::run::supervise;
use crate::tui;

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
    /// Choose the target for a role or task kind, without running
    #[command(group(ArgGroup::new("selector").required(true).multiple(true).args(["role", "kind"])))]
    Pick {
        /// What the run may do: advise, review, explore, or implement (writes); optional with --kind, and must match it when given
        #[arg(long)]
        role: Option<Role>,
        /// A task kind defined in config.toml; its role sets what the run may do
        #[arg(long, value_name = "NAME")]
        kind: Option<String>,
        /// Only this harness
        #[arg(long)]
        to: Option<HarnessId>,
        /// The harness that is asking (it is never picked)
        #[arg(long)]
        caller: Option<HarnessId>,
    },
    /// Delegate a brief to another harness, under the gate
    #[command(group(ArgGroup::new("selector").required(true).multiple(true).args(["role", "kind"])))]
    Run {
        /// What the run may do: advise, review, explore, or implement (writes); optional with --kind, and must match it when given
        #[arg(long)]
        role: Option<Role>,
        /// A task kind defined in config.toml; its role sets what the run may do
        #[arg(long, value_name = "NAME")]
        kind: Option<String>,
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
    /// Continue a finished run's conversation with a new brief (a new, gated run)
    Resume {
        run: String,
        /// The follow-up, as a file
        #[arg(long)]
        brief: PathBuf,
        /// The harness that is asking
        #[arg(long)]
        caller: Option<HarnessId>,
        #[arg(long)]
        wait: Option<u64>,
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
    Outcome {
        run: String,
        outcome: crate::history::Outcome,
    },
    /// Show what past reviews on this machine suggest, before you write a brief
    Notes {
        #[arg(long)]
        role: Role,
        /// Only for this target
        #[arg(long)]
        to: Option<HarnessId>,
        /// The harness that is asking (it has no notes about itself)
        #[arg(long)]
        caller: Option<HarnessId>,
    },
    /// Review a run you delegated (opt-in: `[review] enabled = true`)
    Review {
        #[command(subcommand)]
        action: ReviewAction,
    },
    /// Install (or update) the skill and agent definitions for the harnesses on this machine,
    /// and choose the usage meter
    Install {
        /// Only this harness (the shared skill is always written)
        #[arg(long)]
        harness: Option<HarnessId>,
        /// Say what would be written, and write nothing
        #[arg(long)]
        dry_run: bool,
        /// The usage meter: agent-usage, ccusage or none. Default: the one found, and a
        /// question when more than one is
        #[arg(long)]
        meter: Option<Selection>,
        /// Where that meter's CLI is, when it is not where install looks
        #[arg(long, requires = "meter")]
        meter_binary: Option<PathBuf>,
    },
    /// Remove what `install` wrote — and only that
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// Print the skill this version installs
    Skill,
    /// See and change every setting: a page at the terminal, or one at a time
    Settings {
        #[command(subcommand)]
        action: Option<SettingsAction>,
    },
    /// Allow a harness to be used as a target (a run sends it repository content)
    Enable {
        harness: HarnessId,
        /// Stop delegating to it instead
        #[arg(long)]
        off: bool,
    },
    /// See or forget what was learned on this machine
    Learn {
        #[command(subcommand)]
        action: LearnAction,
    },
    /// Show the effective registry of harnesses, models and roles
    Registry,
    /// Check the installation, the harness versions and the permission rules
    Doctor,
    /// Summarise recorded runs: how they ended, what became of them, what they cost
    Report {
        /// How far back to look
        #[arg(long, default_value_t = 30)]
        days: u64,
        /// Also show what the outcome statistics say about each role's order
        #[arg(long)]
        suggest: bool,
    },
    /// Print every exit code, its class and its retry hint, as JSON
    ExitCodes,
    /// Where this build keeps things, and whether overrides are honoured
    #[command(name = "__dirs", hide = true)]
    Dirs,
    /// The detached supervisor of one run. Started by `run`, never by hand.
    #[command(name = "__supervise", hide = true)]
    Supervise { run: String },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ReviewAction {
    /// The next run waiting for your review: its brief, its answer, the rubric
    Next {
        #[arg(long)]
        caller: Option<HarnessId>,
    },
    /// Record what you found (no --finding at all means: nothing to note)
    Submit {
        run: String,
        #[arg(long)]
        caller: Option<HarnessId>,
        /// `<finding>` or `<finding>:<one plain sentence>`; repeatable
        #[arg(long)]
        finding: Vec<String>,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum SettingsAction {
    /// Set one: `cahoots settings set harness.codex.cap 60`
    Set {
        /// Its path in config.toml, as `cahoots settings` lists them
        key: String,
        /// In config.toml's units: on/off, a number, a path, description text, or harness:model:effort,…
        value: String,
    },
    /// Put one back to its default, which takes it out of config.toml
    Reset { key: String },
}

#[derive(Debug, Clone, Subcommand)]
pub enum LearnAction {
    /// What has been learned, and from how much
    List,
    /// Forget what reviews have said so far (the record itself is kept)
    Reset,
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
        "pick" | "run" | "resume" | "wait" | "status" | "result" | "cancel" | "outcome"
        | "notes" | "review" => Tier::Agent,
        "install" | "uninstall" | "settings" | "enable" | "learn" | "registry" => Tier::Human,
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
            Verb::Resume { .. } => "resume",
            Verb::Wait { .. } => "wait",
            Verb::Status { .. } => "status",
            Verb::Result { .. } => "result",
            Verb::Cancel { .. } => "cancel",
            Verb::Outcome { .. } => "outcome",
            Verb::Notes { .. } => "notes",
            Verb::Review { .. } => "review",
            Verb::Install { .. } => "install",
            Verb::Uninstall { .. } => "uninstall",
            Verb::Skill => "skill",
            Verb::Settings { .. } => "settings",
            Verb::Enable { .. } => "enable",
            Verb::Learn { .. } => "learn",
            Verb::Registry => "registry",
            Verb::Doctor => "doctor",
            Verb::Report { .. } => "report",
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

/// Who reads what a verb prints on stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reader {
    /// An agent or a script: one JSON envelope.
    Program,
    /// A person at a terminal: the verb ends in words on the rail.
    Person,
}

/// The inspect verbs a person reads: `doctor`'s checks and `report`'s
/// numbers. No allow rule names them, so an agent runs one only when a
/// person lets it, and nothing promises their output to a program but a
/// pipe.
const READ_BY_A_PERSON: [&str; 2] = ["doctor", "report"];

/// Who reads stdout, by the verb's name. PURE, like `refusal`, so it is
/// tested without running a verb. A person only while stdout is a terminal:
/// piped (`| jq`), it is the envelope, byte for byte. That is enough for a
/// human verb, whose tier already needs a terminal on stdin. `doctor` and
/// `report` run anywhere, so they need the evidence a human verb has: stdin
/// at a terminal too. For every other verb a terminal proves nothing, since
/// an agent may run a command under a pseudo-terminal, and it is always owed
/// the envelope.
pub fn reader(name: &str, stdin_is_terminal: bool, stdout_is_terminal: bool) -> Reader {
    let person = match tier_of(name) {
        Some(Tier::Human) => stdout_is_terminal,
        Some(Tier::Inspect) if READ_BY_A_PERSON.contains(&name) => {
            stdin_is_terminal && stdout_is_terminal
        }
        _ => false,
    };
    if person {
        Reader::Person
    } else {
        Reader::Program
    }
}

/// The verb a command line names, for one clap refused, which has no
/// `Verb`: its first word that is not a flag, if clap knows it.
pub fn verb_named(args: impl IntoIterator<Item = OsString>) -> Option<String> {
    args.into_iter()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .find(|arg| !arg.starts_with('-'))
        .filter(|name| tier_of(name).is_some())
}

/// Who reads what is printed for a command line clap refused, by the verb
/// it names (`verb_named`). PURE, like `reader`. A line that names a verb
/// reads as that verb does. A line that names no verb this build knows, or
/// none at all, is a person's at a terminal: no agent is told to type one,
/// so clap's own words are the answer, with the same exit.
pub fn refused_reader(
    named: Option<&str>,
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
) -> Reader {
    match named {
        Some(name) => reader(name, stdin_is_terminal, stdout_is_terminal),
        None if stdout_is_terminal => Reader::Person,
        None => Reader::Program,
    }
}

/// Why clap refused a command line, as one sentence for the envelope: the
/// first paragraph of clap's error, which names what it is about on the
/// lines under its first. A command group given no command gets its whole
/// help from clap instead, whose first line describes the group, so the
/// sentence says what is missing and where the choices are listed.
pub fn refused_because(error: &clap::Error) -> String {
    let text = error.render().to_string();
    if error.kind() == clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        // `Usage: cahoots learn <COMMAND>` names the group.
        let group = text
            .lines()
            .find_map(|line| line.strip_prefix("Usage: "))
            .and_then(|usage| usage.split(" <").next())
            .unwrap_or("cahoots");
        return format!("`{group}` needs a command: `{group} --help` lists them");
    }
    let paragraph: Vec<&str> = text
        .lines()
        .take_while(|line| !line.trim().is_empty())
        .map(str::trim)
        .collect();
    match paragraph.join(" ").trim_start_matches("error: ") {
        "" => "bad command line".to_string(),
        reason => reason.to_string(),
    }
}

/// What a verb has to say, and the exit that goes with it.
#[derive(Debug)]
pub struct Said {
    pub envelope: Envelope,
    /// The same, in words, when a person reads stdout: how the verb ends on
    /// the rail. Printed instead of the envelope.
    pub words: Option<tui::Ending>,
}

impl From<Envelope> for Said {
    fn from(envelope: Envelope) -> Said {
        Said {
            envelope,
            words: None,
        }
    }
}

/// A verb that stopped short: why, and whether a question it asked left the
/// rail open, for its words to close.
struct Stopped {
    fail: Fail,
    rail_open: bool,
}

impl From<Fail> for Stopped {
    fn from(fail: Fail) -> Stopped {
        Stopped {
            fail,
            rail_open: false,
        }
    }
}

/// The title of a verb's rail: `cahoots install`.
fn title(verb: &Verb) -> String {
    match verb {
        Verb::Learn {
            action: LearnAction::List,
        } => "cahoots learn list".to_string(),
        Verb::Learn {
            action: LearnAction::Reset,
        } => "cahoots learn reset".to_string(),
        verb => format!("cahoots {}", verb.name()),
    }
}

/// Runs a parsed command line and returns what to print and exit with: the
/// envelope, and for a person reading stdout, the same in words.
pub fn dispatch(cli: Cli, stdin_is_terminal: bool, stdout_is_terminal: bool) -> Said {
    let reader = reader(cli.verb.name(), stdin_is_terminal, stdout_is_terminal);
    let title = title(&cli.verb);
    let stopped = match refusal(&cli.verb, stdin_is_terminal) {
        Some(fail) => Stopped::from(fail),
        None => match run_verb(cli.verb, reader, title.clone()) {
            Ok(said) => return said,
            Err(stopped) => stopped,
        },
    };
    // A refusal, in words: the rail closing in red, on the question that
    // opened it if one did.
    let words = (reader == Reader::Person).then(|| {
        tui::Ending::just(
            (!stopped.rail_open).then_some(title),
            tui::Last::Refused(stopped.fail.message.clone()),
        )
    });
    Said {
        envelope: stopped.fail.into(),
        words,
    }
}

fn run_verb(verb: Verb, reader: Reader, title: String) -> Result<Said, Stopped> {
    let person = reader == Reader::Person;
    let ok = |data| Ok(Envelope::new(Exit::Ok, None).with_data(data).into());
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
            let dirs = Dirs::resolve()?;
            let registry = Registry::load(&dirs)?;
            let data = serde_json::to_value(&registry)
                .map_err(|error| Fail::internal(format!("cannot encode the registry: {error}")))?;
            let words = match person {
                true => Some(settings::registry(&dirs, &registry, title)?),
                false => None,
            };
            Ok(Said {
                envelope: Envelope::new(Exit::Ok, None).with_data(data),
                words,
            })
        }
        Verb::Run {
            role,
            kind,
            brief,
            to,
            caller,
            dir,
            fork,
            in_place,
            wait,
            timeout,
        } => Ok(client::run(RunArgs {
            role,
            kind,
            brief,
            to,
            caller,
            dir,
            fork,
            in_place,
            wait_secs: wait,
            timeout_secs: timeout,
        })?
        .into()),
        Verb::Pick {
            role,
            kind,
            to,
            caller,
        } => Ok(client::pick_target(role, kind.as_deref(), caller, to)?.into()),
        Verb::Settings { action } => Ok(settings::settings(action, person, title)?),
        Verb::Enable { harness, off } => {
            let dirs = Dirs::resolve()?;
            let enabled = crate::registry::set_enabled(&dirs, harness, !off)?;
            let words = match person {
                true => Some(settings::enabled(&dirs, harness, !off, title)?),
                false => None,
            };
            Ok(Said {
                envelope: Envelope::new(Exit::Ok, None)
                    .with_data(serde_json::json!({ "enabled": enabled })),
                words,
            })
        }
        Verb::Doctor => {
            let checks = crate::doctor::checks()?;
            let words = match person {
                true => Some(endings::checked(title, &checks, &Dirs::resolve()?.home)),
                false => None,
            };
            Ok(Said {
                envelope: crate::doctor::envelope(&checks),
                words,
            })
        }
        Verb::Notes { role, to, caller } => Ok(crate::learn::notes(role, to, caller)?.into()),
        Verb::Review {
            action: ReviewAction::Next { caller },
        } => Ok(crate::learn::review_next(caller)?.into()),
        Verb::Review {
            action:
                ReviewAction::Submit {
                    run,
                    caller,
                    finding,
                },
        } => Ok(crate::learn::review_submit(&run, caller, &finding)?.into()),
        Verb::Learn {
            action: LearnAction::List,
        } => {
            let envelope = crate::learn::learn_list()?;
            let words = match person {
                true => {
                    let home = Dirs::resolve()?.home;
                    let data = envelope.data.clone().unwrap_or_default();
                    Some(endings::learned(title, &data, &home))
                }
                false => None,
            };
            Ok(Said { envelope, words })
        }
        Verb::Learn {
            action: LearnAction::Reset,
        } => Ok(Said {
            envelope: crate::learn::learn_reset()?,
            words: person.then(|| endings::forgot(title)),
        }),
        Verb::Outcome { run, outcome } => Ok(client::outcome(&run, outcome)?.into()),
        Verb::Report { days, suggest } => {
            let envelope = crate::report::report(days, suggest)?;
            let words = person.then(|| {
                let data = envelope.data.clone().unwrap_or_default();
                endings::reported(title, &data)
            });
            Ok(Said { envelope, words })
        }
        Verb::Skill => ok(serde_json::json!({ "skill": crate::install::files::skill_text() })),
        Verb::Install {
            harness,
            dry_run,
            meter,
            meter_binary,
        } => install(harness, dry_run, meter, meter_binary, reader, title),
        Verb::Uninstall { dry_run } => {
            let dirs = Dirs::resolve()?;
            let files = crate::install::files::uninstall(&dirs, dry_run)?;
            let words = person.then(|| endings::uninstalled(title, dry_run, &files, &dirs.home));
            Ok(Said {
                envelope: Envelope::new(Exit::Ok, None)
                    .with_data(serde_json::json!({ "dry_run": dry_run, "files": files })),
                words,
            })
        }
        Verb::Resume {
            run,
            brief,
            caller,
            wait,
            timeout,
        } => Ok(client::resume(ResumeArgs {
            run,
            brief,
            caller,
            wait_secs: wait,
            timeout_secs: timeout,
        })?
        .into()),
        Verb::Wait { run, timeout } => Ok(client::wait(&run, timeout)?.into()),
        Verb::Status { run } => Ok(client::status(run.as_deref())?.into()),
        Verb::Result { run } => Ok(client::result(&run)?.into()),
        Verb::Cancel { run } => Ok(client::cancel(&run)?.into()),
        Verb::Supervise { run } => {
            supervise::supervise(&Dirs::resolve()?, &run)?;
            Ok(Envelope::new(Exit::Ok, None).into())
        }
    }
}

/// `cahoots install`: the meter decided (and asked, when it comes to that)
/// before anything is written, then the files, then what was found and
/// chosen, and the rules still missing. With a person reading stdout, the
/// words go on from the question's rail, when it asked one.
fn install(
    harness: Option<HarnessId>,
    dry_run: bool,
    meter: Option<Selection>,
    meter_binary: Option<PathBuf>,
    reader: Reader,
    title: String,
) -> Result<Said, Stopped> {
    let dirs = Dirs::resolve()?;
    let config = crate::config::UserConfig::load(&dirs.config_file())?;
    let close = match reader {
        Reader::Person => questions::Close::InWords,
        Reader::Program => questions::Close::Here,
    };
    // Decided — and asked, when it comes to that — before anything is
    // written, so a question left unanswered leaves nothing half-done.
    let (found, decision, asked) = choose_meter(
        &dirs,
        &config.meter,
        meter,
        meter_binary.as_deref(),
        dry_run,
        close,
    )?;
    let rail_open = asked && close == questions::Close::InWords;
    let stopped = |fail: Fail| Stopped { fail, rail_open };
    let kinds = Registry::effective(&config).kinds;
    let files = crate::install::files::install(&dirs, &kinds, harness, dry_run).map_err(stopped)?;
    let mut config = config;
    let mut saved = Vec::new();
    if !dry_run {
        // What was found is cahoots' own record; a choice is the
        // person's, and goes where their other settings are.
        MeterFile::of(&found).save(&dirs).map_err(stopped)?;
        let changes = decision.config_changes(meter_binary.is_some());
        if !changes.is_empty() {
            config = crate::config::edit::apply(&dirs.config_file(), &changes).map_err(stopped)?;
            saved = changes.iter().map(settings::said_change).collect();
        }
    }
    let in_effect = decision.in_effect();
    let still_to_do = detect::still_to_do(in_effect, &config.meter, &found, &dirs.config_file());
    let meter = serde_json::json!({
        "found": found,
        "decision": decision,
        "in_effect": in_effect,
        "still_to_do": still_to_do,
    });
    let rules: Vec<(HarnessId, Vec<String>)> = HarnessId::ALL
        .into_iter()
        .filter(|id| harness.is_none_or(|only| only == *id))
        .map(|id| {
            let missing = crate::install::rules::missing(&dirs.home, id);
            let lines = missing
                .iter()
                .map(|verb| crate::install::rules::rule(id, verb))
                .collect();
            (id, lines)
        })
        .collect();
    let rules_to_add: serde_json::Map<String, serde_json::Value> = rules
        .iter()
        .map(|(id, lines)| {
            (
                id.to_string(),
                serde_json::json!({ "add_to": crate::install::rules::rules_file(*id), "rules": lines }),
            )
        })
        .collect();
    let mut envelope = Envelope::new(
        Exit::Ok,
        format!(
            "{} cahoots never edits a harness's permission rules: to let a harness delegate \
             without a prompt, add the rules below yourself. Then `cahoots enable <harness>` \
             for each target you want (`cahoots settings` shows every setting), and \
             `cahoots doctor` to check.",
            decision.sentence()
        ),
    );
    envelope.data = Some(serde_json::json!({
        "dry_run": dry_run,
        "files": files,
        "rules_to_add": rules_to_add,
        "meter": meter,
    }));
    let words = (reader == Reader::Person).then(|| {
        endings::installed(endings::Installed {
            title: (!rail_open).then_some(title),
            dry_run,
            files: &files,
            meter: decision.sentence(),
            saved,
            still_to_do: still_to_do.as_deref(),
            rules: &rules,
            home: &dirs.home,
        })
    });
    Ok(Said { envelope, words })
}

/// `install`'s usage meter: what is here, which one to use, and whether a
/// question was asked to decide it. The person is asked when the logic
/// says there is a choice to make (never under `--dry-run`), and `close`
/// says whether the question's rail ends with it. Nothing is written here;
/// the caller records the decision once the files are in.
fn choose_meter(
    dirs: &Dirs,
    config: &crate::config::MeterConfig,
    selection: Option<Selection>,
    binary: Option<&std::path::Path>,
    dry_run: bool,
    close: questions::Close,
) -> Result<(Vec<Found>, Decision, bool), Stopped> {
    if selection == Some(Selection::NoMeter) && binary.is_some() {
        return Err(Fail::new(
            Exit::Usage,
            "--meter-binary names a meter's binary, and --meter none names no meter",
        )
        .into());
    }
    // Where config.toml says each meter is, then where it was found last
    // time. A file an older cahoots wrote only loses its hints: this run
    // writes it again.
    let previous = MeterFile::load(dirs).ok().flatten();
    let mut first: Vec<(MeterId, &std::path::Path)> = MeterId::ALL
        .into_iter()
        .filter_map(|id| Some((id, config.binary(id)?)))
        .collect();
    if let Some(previous) = &previous {
        first.extend(
            previous
                .found
                .iter()
                .map(|(id, binary)| (*id, binary.as_path())),
        );
    }
    let path = crate::env::path_var();
    let mut found = detect::detect(&detect::Places::of(&dirs.home, path.as_deref()), &first);
    if let (Some(Selection::Meter(meter)), Some(binary)) = (selection, binary) {
        // A path a person gives replaces the search for that meter.
        let binary = std::path::absolute(binary)
            .map_err(|error| Fail::new(Exit::Usage, format!("{}: {error}", binary.display())))?;
        found.retain(|found| found.meter != meter);
        found.push(detect::probe(meter, &binary));
    }
    let decision = detect::decide(config.use_, selection, &found)?;
    let Decision::Ask { options } = decision else {
        return Ok((found, decision, false));
    };
    if dry_run {
        return Ok((found, Decision::Ask { options }, false));
    }
    // From here the question is on the rail, and a failure closes it.
    let asked = |fail: Fail| Stopped {
        fail,
        rail_open: close == questions::Close::InWords,
    };
    let selection = {
        let Some(mut terminal) = person_at_terminal() else {
            return Err(questions::no_terminal_for_meter().into());
        };
        questions::which_meter(
            &options,
            &mut terminal,
            &mut std::io::stderr(),
            colors(),
            close,
        )
        .map_err(asked)?
        // The terminal is given back here, before install says more.
    };
    let decision = detect::picked(selection, &options).map_err(asked)?;
    Ok((found, decision, true))
}

/// The person's terminal, set up for a question. `None` when there is no
/// terminal to ask at, or it cannot move its cursor (`TERM=dumb`).
fn person_at_terminal() -> Option<tui::Terminal> {
    if crate::env::dumb_terminal() {
        return None;
    }
    tui::Terminal::open()
}

/// The rail in color, unless `NO_COLOR` says otherwise, or the terminal is
/// one that shows no escape codes (`TERM=dumb`).
fn colors() -> tui::Colors {
    if crate::env::no_color() || crate::env::dumb_terminal() {
        tui::Colors::OFF
    } else {
        tui::Colors::ON
    }
}

/// Prints what a verb said and turns it into the process's exit status: its
/// words for a person, drawn on stdout where the envelope would go, and the
/// envelope for everyone else. A closed pipe is a quiet exit, not a panic
/// (docs/SPIKE.md S5).
pub fn emit(said: &Said) -> ExitCode {
    use std::io::IsTerminal;
    let envelope = &said.envelope;
    if let Some(words) = &said.words {
        tui::Rail::new(&mut std::io::stdout(), colors()).end(words);
        return ExitCode::from(envelope.code);
    }
    let encode = if std::io::stdout().is_terminal() {
        serde_json::to_string_pretty // a person may be reading
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
            "pick", "run", "resume", "wait", "status", "result", "cancel", "outcome", "notes",
            "review",
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
            vec!["cahoots", "settings"],
            vec!["cahoots", "settings", "set", "harness.codex.cap", "60"],
            vec!["cahoots", "settings", "reset", "harness.codex.cap"],
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

    /// Who reads is decided, never found out by running a verb: `reader`,
    /// like `refusal`, is tested without `dispatch`.
    #[test]
    fn a_person_reads_a_human_verb_doctor_and_report_at_a_terminal_and_nothing_else() {
        for sub in Cli::command().get_subcommands() {
            let name = sub.get_name();
            for stdin in [false, true] {
                assert_eq!(
                    reader(name, stdin, false),
                    Reader::Program,
                    "{name}: a pipe is read by a program"
                );
            }
            let (both, stdout_only) = match tier_of(name).unwrap() {
                Tier::Human => (Reader::Person, Reader::Person),
                Tier::Inspect if ["doctor", "report"].contains(&name) => {
                    (Reader::Person, Reader::Program)
                }
                Tier::Agent | Tier::Inspect | Tier::Internal => (Reader::Program, Reader::Program),
            };
            assert_eq!(reader(name, true, true), both, "{name}");
            assert_eq!(
                reader(name, false, true),
                stdout_only,
                "{name}: stdin is not a terminal"
            );
        }
    }

    #[test]
    fn a_command_line_clap_refused_still_names_its_verb() {
        let named = |argv: &[&str]| verb_named(argv.iter().map(OsString::from));
        for (argv, verb) in [
            (vec!["cahoots", "enable", "gemini"], "enable"),
            (
                vec!["cahoots", "settings", "set", "harness.codex.cap"],
                "settings",
            ),
            (vec!["cahoots", "--bogus", "install"], "install"),
            (vec!["cahoots", "run", "--role", "deploy"], "run"),
            (vec!["cahoots", "report", "--days", "abc"], "report"),
        ] {
            assert_eq!(named(&argv).as_deref(), Some(verb), "{argv:?}");
        }
        assert_eq!(named(&["cahoots", "conspire"]), None);
        assert_eq!(named(&["cahoots"]), None);
    }

    #[test]
    fn a_refused_line_is_a_persons_at_a_terminal_unless_it_names_a_verb_a_program_runs() {
        assert_eq!(
            refused_reader(None, true, true),
            Reader::Person,
            "no verb at all"
        );
        assert_eq!(refused_reader(None, false, true), Reader::Person);
        assert_eq!(refused_reader(None, true, false), Reader::Program, "piped");
        for name in ["enable", "doctor", "report"] {
            assert_eq!(refused_reader(Some(name), true, true), Reader::Person);
        }
        assert_eq!(refused_reader(Some("report"), false, true), Reader::Program);
        for name in ["run", "status", "exit-codes", "skill", "__supervise"] {
            assert_eq!(
                refused_reader(Some(name), true, true),
                Reader::Program,
                "{name}"
            );
        }
        for name in ["run", "enable", "doctor", "exit-codes", "__supervise"] {
            assert_eq!(
                refused_reader(Some(name), true, false),
                Reader::Program,
                "{name}"
            );
        }
    }

    #[test]
    fn a_refusal_says_what_is_missing_not_what_the_help_begins_with() {
        let because = |argv: &[&str]| refused_because(&Cli::try_parse_from(argv).unwrap_err());
        assert_eq!(
            because(&["cahoots"]),
            "`cahoots` needs a command: `cahoots --help` lists them"
        );
        assert_eq!(
            because(&["cahoots", "learn"]),
            "`cahoots learn` needs a command: `cahoots learn --help` lists them"
        );
        assert_eq!(
            because(&["cahoots", "review"]),
            "`cahoots review` needs a command: `cahoots review --help` lists them"
        );
        assert_eq!(
            because(&["cahoots", "enable"]),
            "the following required arguments were not provided: <HARNESS>",
            "what is missing, not a sentence cut at its colon"
        );
        assert_eq!(
            because(&["cahoots", "run", "--role", "review"]),
            "the following required arguments were not provided: --brief <BRIEF>"
        );
        for (argv, reason) in [
            (
                vec!["cahoots", "conspire"],
                "unrecognized subcommand 'conspire'",
            ),
            (
                vec!["cahoots", "--bogus"],
                "unexpected argument '--bogus' found",
            ),
            (
                vec!["cahoots", "enable", "gemini"],
                "invalid value 'gemini' for '<HARNESS>': unknown harness: \"gemini\"",
            ),
        ] {
            assert_eq!(because(&argv), reason, "{argv:?}: one line, as before");
        }
    }

    #[test]
    fn a_verbs_rail_is_titled_with_its_command() {
        let title_of = |argv: &[&str]| title(&Cli::try_parse_from(argv).unwrap().verb);
        assert_eq!(
            title_of(&["cahoots", "install", "--dry-run"]),
            "cahoots install"
        );
        assert_eq!(title_of(&["cahoots", "enable", "codex"]), "cahoots enable");
        assert_eq!(
            title_of(&["cahoots", "settings", "reset", "harness.codex.cap"]),
            "cahoots settings"
        );
        assert_eq!(
            title_of(&["cahoots", "learn", "list"]),
            "cahoots learn list"
        );
        assert_eq!(
            title_of(&["cahoots", "learn", "reset"]),
            "cahoots learn reset"
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

    #[test]
    fn pick_and_run_accept_a_kind_with_an_optional_role() {
        for verb in ["pick", "run"] {
            for selector in [
                vec!["--role", "review"],
                vec!["--kind", "rust-review"],
                vec!["--role", "review", "--kind", "rust-review"],
            ] {
                let mut args = vec!["cahoots", verb];
                args.extend(selector);
                if verb == "run" {
                    args.extend(["--brief", "b.md"]);
                }
                assert!(Cli::try_parse_from(&args).is_ok(), "{args:?}");
                for flag in ["--model", "--effort"] {
                    let mut bad = args.clone();
                    bad.extend([flag, "high"]);
                    assert!(Cli::try_parse_from(bad).is_err());
                }
            }
            let mut args = vec!["cahoots", verb];
            if verb == "run" {
                args.extend(["--brief", "b.md"]);
            }
            let error = Cli::try_parse_from(args).unwrap_err().to_string();
            assert!(
                error.contains("--role") && error.contains("--kind"),
                "{error}"
            );
        }
        assert!(Cli::try_parse_from(["cahoots", "run", "--kind", "rust-review"]).is_err());
        assert!(
            Cli::try_parse_from([
                "cahoots",
                "resume",
                "r",
                "--kind",
                "rust-review",
                "--brief",
                "b"
            ])
            .is_err()
        );
    }
}
