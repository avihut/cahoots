#![forbid(unsafe_code)]

use std::io::IsTerminal;
use std::process::ExitCode;

use cahoots::cli;
use cahoots::exit::{Envelope, Exit};
use clap::Parser;
use clap::error::ErrorKind;

fn main() -> ExitCode {
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
        // A bad command line is a usage error (exit 2) — and "every exit
        // prints one JSON envelope" includes this one: the caller is usually
        // an agent, and clap's prose on stderr is not something to parse.
        Err(error) => {
            let _ = error.print();
            let text = error.render().to_string();
            let reason = text
                .lines()
                .next()
                .unwrap_or("bad command line")
                .trim_start_matches("error: ")
                .to_string();
            return cli::emit(&Envelope::new(Exit::Usage, reason));
        }
    };
    let envelope = cli::dispatch(cli, std::io::stdin().is_terminal());
    cli::emit(&envelope)
}
