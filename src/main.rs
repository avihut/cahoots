#![forbid(unsafe_code)]

use std::io::IsTerminal;
use std::process::ExitCode;

use cahoots::cli::{self, Reader};
use cahoots::exit::{Envelope, Exit};
use clap::Parser;
use clap::error::ErrorKind;

fn main() -> ExitCode {
    let stdout_is_terminal = std::io::stdout().is_terminal();
    let cli = match cli::Cli::try_parse() {
        Ok(cli) => cli,
        // `--help` and `--version` are answers, not errors: clap prints them
        // and exits 0.
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            error.exit()
        }
        // A bad command line is a usage error (exit 2), and clap says why on
        // stderr. For a person who typed a human verb at a terminal, those
        // words are the answer. Everyone else also gets the one JSON
        // envelope, since an agent or a script cannot parse clap's prose.
        Err(error) => {
            let _ = error.print();
            let named = cli::tier_named(std::env::args_os());
            if named.is_some_and(|tier| cli::reader(tier, stdout_is_terminal) == Reader::Person) {
                return ExitCode::from(Exit::Usage.code());
            }
            let text = error.render().to_string();
            let reason = text
                .lines()
                .next()
                .unwrap_or("bad command line")
                .trim_start_matches("error: ")
                .to_string();
            return cli::emit(&Envelope::new(Exit::Usage, reason).into());
        }
    };
    let said = cli::dispatch(cli, std::io::stdin().is_terminal(), stdout_is_terminal);
    cli::emit(&said)
}
