use clap::Parser;
use std::process::ExitCode;

use vaultline_cli::cli::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();
    ExitCode::from(vaultline_cli::run(cli) as u8)
}
