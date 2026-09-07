//! The command surface. Two commands exist today: `init` (write a
//! configuration template, safe-by-default) and `validate` (strict
//! configuration checking). Backup, restore, verify, prune, doctor, and
//! status arrive with their implementing phases — the full surface is
//! recorded in ADR-V0-3's CLI hypothesis and docs/cli.md.
//!
//! Exit codes (the documented contract): 0 success · 1 operational · 2
//! usage/config. Logs go to stderr; stdout carries command output only.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde::Serialize;
use tracing::info;
use vaultline_core::config::{parse_str, validate, validate_application_name};
use vaultline_core::error::{ErrorKind, VaultlineError};
use vaultline_core::model::VerificationLevel;

use crate::logging::LogFormat;
use crate::template;

#[derive(Parser)]
#[command(
    name = "vaultline",
    version,
    about = "Application-aware backup & disaster recovery for Linux VPSes",
    long_about = "vaultline turns one declarative Application Recovery Definition into \
                  scheduled, verified, restorable backups — orchestrating mature backup \
                  engines (restic) instead of reimplementing them."
)]
pub struct Cli {
    /// Log output format: json (default, the machine-readable contract) or pretty.
    #[arg(
        long,
        global = true,
        value_enum,
        default_value = "json",
        env = "VAULTLINE_LOG_FORMAT"
    )]
    pub log_format: LogFormat,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Write a commented vaultline.toml template (never overwrites an existing file).
    Init(InitArgs),
    /// Validate a vaultline.toml and report every problem.
    Validate(ValidateArgs),
    /// Back up an application and inspect its recorded snapshots.
    Backup(BackupArgs),
    /// Restore a snapshot by executing the definition's restore procedure
    /// (sandbox-then-promote; never overwrites existing files).
    Restore(crate::restore::RestoreArgs),
    /// Run whatever the definition's schedules make due (backup and/or
    /// verification) and explain what ran and why.
    Schedule(crate::schedule::ScheduleArgs),
    /// Check the environment: configuration, tools, state, storage.
    Doctor(crate::ops::DoctorArgs),
    /// Summarize one application's records against its definition.
    Status(crate::ops::StatusArgs),
    /// Generate or install the systemd service/timer units.
    Timer(crate::timer::TimerArgs),
    /// Report the versioned surfaces (binary, state schema, engine).
    Version(VersionArgs),
}

#[derive(clap::Args)]
pub struct VersionArgs {
    /// Machine-readable output on stdout.
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct BackupArgs {
    #[command(subcommand)]
    pub command: BackupCommand,
}

#[derive(Subcommand)]
pub enum BackupCommand {
    /// Execute the recovery definition: capture the sources, record the snapshot.
    Run(crate::backup::BackupRunArgs),
    /// List the snapshots recorded for this application.
    List(crate::backup::BackupListArgs),
    /// Verify a snapshot against the definition's policy (L3 cross-check,
    /// L4 content comparison, L5 SQLite rehearsal) and record the level
    /// actually reached in the state.
    Verify(crate::restore::BackupVerifyArgs),
    /// Show a snapshot's full record.
    Inspect(crate::restore::BackupInspectArgs),
    /// Enforce the retention policy: plan per-snapshot keep/forget
    /// decisions with reasons; forget + reclaim only with --apply.
    Prune(crate::prune::BackupPruneArgs),
}

#[derive(clap::Args)]
pub struct InitArgs {
    /// Application name for the template (lowercase alphanumerics and dashes).
    #[arg(long, default_value = "example-app")]
    pub name: String,

    /// Where to write the template.
    #[arg(long, default_value = "vaultline.toml")]
    pub config: PathBuf,
}

#[derive(clap::Args)]
pub struct ValidateArgs {
    /// The configuration file to validate.
    #[arg(long, default_value = "vaultline.toml")]
    pub config: PathBuf,

    /// Machine-readable result on stdout (exit code unchanged).
    #[arg(long)]
    pub json: bool,
}

pub fn run_version(args: VersionArgs) -> Result<(), VaultlineError> {
    // The versioned surfaces (the release-checklist contract): the
    // binary version, the state-file schema version, and the engine
    // the CLI orchestrates. `--json` is machine-readable; the human
    // form stays plain for scripts.
    if args.json {
        println!(
            "{}",
            serde_json::json!({
                "name": "vaultline",
                "version": env!("CARGO_PKG_VERSION"),
                "state_schema": vaultline_core::state::STATE_VERSION,
                "engine": "restic",
            })
        );
    } else {
        println!("vaultline {}", env!("CARGO_PKG_VERSION"));
        println!("state schema: v{}", vaultline_core::state::STATE_VERSION);
        println!("backup engine: restic (orchestrated via its --json contract)");
    }
    Ok(())
}

pub fn run_init(args: InitArgs) -> Result<(), VaultlineError> {
    validate_application_name(&args.name)
        .map_err(|e| VaultlineError::new(ErrorKind::Config, e.to_string()))?;

    // Safe by default: `init` is generative, not destructive — it never
    // overwrites. Removing or moving an existing file is an explicit human act.
    if args.config.exists() {
        return Err(VaultlineError::new(
            ErrorKind::Config,
            format!(
                "refusing to overwrite existing file {} (safe by default — move or remove it first)",
                args.config.display()
            ),
        ));
    }

    let contents = template::render(&args.name);
    std::fs::write(&args.config, &contents).map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            format!("cannot write {}", args.config.display()),
            e,
        )
    })?;
    info!(config = %args.config.display(), bytes = contents.len(), "wrote configuration template");

    // The template is promised to be valid — re-check the invariant at the
    // boundary rather than trusting it.
    let outcome = validate(parse_str(&contents).map_err(|e| {
        VaultlineError::new(
            ErrorKind::Internal,
            format!("the generated template does not parse — this is a bug: {e}"),
        )
    })?);
    if !outcome.is_valid() {
        return Err(VaultlineError::new(
            ErrorKind::Internal,
            format!(
                "the generated template is invalid — this is a bug: {:?}",
                outcome.errors
            ),
        ));
    }

    println!(
        "wrote {} ({} bytes); configuration valid",
        args.config.display(),
        contents.len()
    );
    println!("next: fill in your application's real paths, then `vaultline validate`");
    Ok(())
}

pub fn run_validate(args: ValidateArgs) -> Result<(), VaultlineError> {
    let contents = std::fs::read_to_string(&args.config).map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            format!("cannot read configuration file {}", args.config.display()),
            e,
        )
    })?;

    let parsed = parse_str(&contents)?;
    let outcome = validate(parsed);

    if args.json {
        emit_json(&outcome);
        if !outcome.is_valid() {
            // The JSON payload already carries the full error list; the exit
            // code still follows the documented contract.
            return Err(VaultlineError::new(
                ErrorKind::Config,
                format!("configuration invalid: {} error(s)", outcome.errors.len()),
            ));
        }
        return Ok(());
    }

    if let Some(app) = &outcome.application {
        for warning in &outcome.warnings {
            eprintln!("warning: {warning}");
        }
        println!(
            "configuration valid: {} ({} source(s), {} database(s), {} volume(s), verification {}, storage {}, {} restore step(s))",
            app.name,
            app.sources.len(),
            app.databases.len(),
            app.volumes.len(),
            verification_label(app.verification.level),
            storage_label(&app.storage),
            app.restore.steps.len(),
        );
    } else {
        // All failures, never first-error-only (SOT Section 10.8).
        for error in &outcome.errors {
            eprintln!("error: {error}");
        }
        return Err(VaultlineError::new(
            ErrorKind::Config,
            format!(
                "configuration invalid: {} error(s) in {}",
                outcome.errors.len(),
                args.config.display()
            ),
        ));
    }

    Ok(())
}

fn verification_label(level: VerificationLevel) -> String {
    format!("L{}", level.to_number())
}

fn storage_label(storage: &vaultline_core::model::StorageTarget) -> String {
    match &storage.kind {
        vaultline_core::model::StorageKind::Local { .. } => "local".to_string(),
        vaultline_core::model::StorageKind::S3Compatible { .. } => "s3".to_string(),
        vaultline_core::model::StorageKind::Sftp { .. } => "sftp".to_string(),
    }
}

#[derive(Serialize)]
struct ValidateJson<'a> {
    valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    application: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    errors: Vec<ValidateJsonError>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<ValidateJsonSummary>,
}

#[derive(Serialize)]
struct ValidateJsonError {
    path: String,
    message: String,
}

#[derive(Serialize)]
struct ValidateJsonSummary {
    sources: usize,
    databases: usize,
    volumes: usize,
    verification_level: u8,
    storage: String,
    restore_steps: usize,
}

fn emit_json(outcome: &vaultline_core::config::ValidationOutcome) {
    let json = match &outcome.application {
        Some(app) => ValidateJson {
            valid: true,
            application: Some(&app.name),
            errors: Vec::new(),
            warnings: outcome.warnings.iter().map(|w| w.message.clone()).collect(),
            summary: Some(ValidateJsonSummary {
                sources: app.sources.len(),
                databases: app.databases.len(),
                volumes: app.volumes.len(),
                verification_level: app.verification.level.to_number(),
                storage: storage_label(&app.storage),
                restore_steps: app.restore.steps.len(),
            }),
        },
        None => ValidateJson {
            valid: false,
            application: None,
            errors: outcome
                .errors
                .iter()
                .map(|e| ValidateJsonError {
                    path: e.path.clone(),
                    message: e.message.clone(),
                })
                .collect(),
            warnings: Vec::new(),
            summary: None,
        },
    };
    // stdout is the machine channel; if this serialization fails the error is
    // reported as an operational failure.
    if let Err(e) = serde_json::to_writer(std::io::stdout(), &json) {
        eprintln!("error: cannot write JSON output: {e}");
    }
}
