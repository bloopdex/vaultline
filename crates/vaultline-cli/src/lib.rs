//! The vaultline CLI: application-aware backup & disaster recovery for Linux
//! VPSes. The command surface, logging, and dispatch live here; the recovery
//! model and configuration validation live in `vaultline-core`.

pub mod cli;
pub mod logging;
pub mod template;

pub use vaultline_core::error::{ErrorKind, VaultlineError};

/// Run a parsed command line. Returns the process exit code per the
/// documented contract (docs/cli.md): 0 success, 1 operational, 2
/// usage/config.
pub fn run(cli: cli::Cli) -> i32 {
    logging::init(cli.log_format);
    match dispatch(cli) {
        Ok(()) => 0,
        Err(err) => {
            tracing::error!(error = %err, exit_code = err.exit_code(), "command failed");
            eprintln!("error: {err}");
            err.exit_code()
        }
    }
}

fn dispatch(cli: cli::Cli) -> Result<(), VaultlineError> {
    match cli.command {
        cli::Command::Init(args) => cli::run_init(args),
        cli::Command::Validate(args) => cli::run_validate(args),
    }
}
