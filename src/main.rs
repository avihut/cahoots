#![forbid(unsafe_code)]

use std::io::IsTerminal;
use std::process::ExitCode;

use cahoots::cli::{self, Reader};
use cahoots::exit::{Envelope, Exit};
use clap::Parser;
use clap::error::ErrorKind;

fn main() -> ExitCode {
    let stdin_is_terminal = std::io::stdin().is_terminal();
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
        // stderr: its error, or for a bare `cahoots`, its help. For a person
        // at a terminal, those words are the answer, unless the line names a
        // verb a program reads there (`cli::reader`). Everyone else also gets
        // the one JSON envelope, since an agent or a script cannot parse
        // clap's prose.
        Err(error) => {
            let _ = error.print();
            let named = cli::verb_named(std::env::args_os());
            let reader =
                cli::refused_reader(named.as_deref(), stdin_is_terminal, stdout_is_terminal);
            if reader == Reader::Person {
                return ExitCode::from(Exit::Usage.code());
            }
            let reason = cli::refused_because(&error);
            return cli::emit(&Envelope::new(Exit::Usage, reason).into());
        }
    };
    let said = cli::dispatch(cli, stdin_is_terminal, stdout_is_terminal);
    cli::emit(&said)
}
