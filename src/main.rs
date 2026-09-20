#![forbid(unsafe_code)]

mod cli;
mod exit;

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    // clap exits 2 on a bad command line by itself — the usage-error code.
    let cli = cli::Cli::parse();
    let envelope = cli::dispatch(&cli, std::io::stdin().is_terminal());
    match serde_json::to_string(&envelope) {
        Ok(json) => println!("{json}"),
        Err(error) => eprintln!("cahoots: could not encode the exit envelope: {error}"),
    }
    ExitCode::from(envelope.code)
}
