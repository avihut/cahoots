#![forbid(unsafe_code)]

use std::io::IsTerminal;
use std::process::ExitCode;

use cahoots::cli;
use clap::Parser;

fn main() -> ExitCode {
    // clap exits 2 on a bad command line by itself — the usage-error code.
    let cli = cli::Cli::parse();
    let envelope = cli::dispatch(cli, std::io::stdin().is_terminal());
    cli::emit(&envelope)
}
