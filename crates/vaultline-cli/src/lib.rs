//! The vaultline CLI: application-aware backup & disaster recovery for Linux
//! VPSes. The command surface, logging, and dispatch live here; the recovery
//! model and configuration validation live in `vaultline-core`.

pub mod backup;
pub mod cli;
pub mod database;
pub mod engine;
pub mod logging;
pub mod metrics;
pub mod ops;
pub mod prune;
pub mod restore;
pub mod schedule;
pub mod template;
pub mod timer;

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
        cli::Command::Backup(args) => match args.command {
            cli::BackupCommand::Run(args) => backup::run_backup(args),
            cli::BackupCommand::List(args) => backup::run_list(args),
            cli::BackupCommand::Verify(args) => restore::run_verify(args),
            cli::BackupCommand::Inspect(args) => restore::run_inspect(args),
            cli::BackupCommand::Prune(args) => prune::run_prune(args),
        },
        cli::Command::Restore(args) => restore::run_restore(args),
        cli::Command::Schedule(args) => match args.command {
            schedule::ScheduleCommand::Run(args) => schedule::run_schedule(args),
        },
        cli::Command::Doctor(args) => ops::run_doctor(args),
        cli::Command::Status(args) => ops::run_status(args),
        cli::Command::Timer(args) => timer::run_timer(args),
        cli::Command::Version(args) => cli::run_version(args),
    }
}
