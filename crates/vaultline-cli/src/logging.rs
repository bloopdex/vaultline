//! Structured logging from day one (SOT Section 14). Logs go to **stderr**;
//! stdout carries machine-readable command output only.

use std::io;

use clap::ValueEnum;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogFormat {
    /// Structured JSON lines — the default and the machine-readable contract.
    Json,
    /// Human-readable key/value lines.
    Pretty,
}

/// Initialize the global tracing subscriber. The log level comes from
/// `RUST_LOG` (default: `vaultline=info`).
pub fn init(format: LogFormat) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("vaultline=info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr);
    match format {
        LogFormat::Json => builder.json().init(),
        LogFormat::Pretty => builder.init(),
    }
}
