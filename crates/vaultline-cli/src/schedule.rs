//! `vaultline schedule run` — the due-ness executor. For hosts without
//! systemd (or for testing the same logic the generated timers express):
//! computes what is due from the definition's crons and the state
//! bookkeeping, runs the due jobs, and explains what ran and why.
//!
//! Bookkeeping rules (ADR-V0-5):
//! - a backup is due when `application.schedule` has an occurrence
//!   strictly after the last recorded backup (the newest snapshot
//!   timestamp) that has already arrived;
//! - a verification run is due when `verification.schedule` has an
//!   occurrence strictly after the last recorded verification run that
//!   has already arrived — and a snapshot exists to verify;
//! - the verification attempt time is recorded BEFORE the run: a failing
//!   verification must not hot-loop on every invocation until the next
//!   scheduled occurrence (the failure itself is the signal).
//!
//! Manual `backup run` / `backup verify` do not touch the bookkeeping —
//! they run on demand, not on schedule.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use vaultline_core::config;
use vaultline_core::error::{ErrorKind, Result, VaultlineError};
use vaultline_core::state::State;

use crate::backup::state_dir;
use crate::metrics;

#[derive(clap::Args)]
pub struct ScheduleArgs {
    #[command(subcommand)]
    pub command: ScheduleCommand,
}

#[derive(clap::Subcommand)]
pub enum ScheduleCommand {
    /// Compute what the schedules make due, run it, and explain what ran
    /// and why.
    Run(ScheduleRunArgs),
}

#[derive(clap::Args)]
pub struct ScheduleRunArgs {
    #[arg(long, default_value = "vaultline.toml")]
    pub config: PathBuf,

    /// Machine-readable result on stdout (exit code unchanged).
    #[arg(long)]
    pub json: bool,
}

/// Is the job due? `last_ran` is the job's bookkeeping anchor.
fn due_for(
    schedule: Option<&str>,
    last_ran: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<bool> {
    let Some(schedule) = schedule else {
        return Ok(false);
    };
    let schedule = vaultline_core::cron::parse_schedule(schedule).map_err(|e| {
        VaultlineError::new(
            ErrorKind::Config,
            format!("the schedule in the configuration is invalid: {e}"),
        )
    })?;
    match last_ran {
        None => Ok(true), // never ran — due immediately
        Some(last) => {
            Ok(vaultline_core::cron::next_after(&schedule, last).is_some_and(|next| next <= now))
        }
    }
}

/// The next occurrence after the anchor, for explanations.
fn next_at(schedule: Option<&str>, anchor: Option<DateTime<Utc>>) -> String {
    match (schedule, anchor) {
        (None, _) => "no schedule declared".to_string(),
        (Some(_), None) => "due immediately (never ran)".to_string(),
        (Some(expr), Some(anchor)) => match vaultline_core::cron::parse_schedule(expr) {
            Ok(schedule) => match vaultline_core::cron::next_after(&schedule, anchor) {
                Some(next) => format!("{} (next occurrence)", next.to_rfc3339()),
                None => "no future occurrence".to_string(),
            },
            Err(_) => "unparsable".to_string(),
        },
    }
}

/// The most recent backup time: the newest recorded snapshot timestamp.
fn last_backup_at(state: &State, app_name: &str) -> Option<DateTime<Utc>> {
    state
        .applications
        .get(app_name)
        .and_then(|app_state| app_state.snapshots.iter().map(|s| s.timestamp).max())
}

pub fn run_schedule(args: ScheduleRunArgs) -> Result<()> {
    let app = config::load(&args.config)?;
    let state_dir = state_dir()?;
    let state_path = state_dir.join("state.json");
    let now = Utc::now();

    // The due computation is read-only (no lock); the jobs it launches
    // acquire the state lock themselves.
    let state = State::load(&state_path)?;
    let has_snapshots = state
        .applications
        .get(&app.name)
        .is_some_and(|app_state| !app_state.snapshots.is_empty());
    let last_backup = last_backup_at(&state, &app.name);
    let last_verify = state
        .applications
        .get(&app.name)
        .and_then(|app_state| app_state.operations.last_verify_at);

    let backup_due = due_for(app.schedule.as_deref(), last_backup, now)?;
    let verify_due =
        has_snapshots && due_for(app.verification.schedule.as_deref(), last_verify, now)?;

    let explanation = serde_json::json!({
        "backup": {
            "schedule": app.schedule,
            "last_run": last_backup,
            "next": next_at(app.schedule.as_deref(), last_backup),
            "due": backup_due,
        },
        "verification": {
            "schedule": app.verification.schedule,
            "last_run": last_verify,
            "next": next_at(app.verification.schedule.as_deref(), last_verify),
            "due": verify_due,
            "has_snapshots": has_snapshots,
        },
    });

    if !backup_due && !verify_due {
        if args.json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "ran": "nothing",
                    "explanation": explanation,
                }))
                .expect("json serializes")
            );
        } else {
            println!("nothing due — no jobs ran");
            println!(
                "backup: {}",
                explanation["backup"]["next"].as_str().expect("string")
            );
            println!(
                "verification: {}",
                explanation["verification"]["next"]
                    .as_str()
                    .expect("string")
            );
        }
        return Ok(());
    }

    let backup_ran = backup_due;
    if backup_due {
        println!(
            "backup due (schedule \"{}\", last ran {}) — running",
            app.schedule.as_deref().unwrap_or("?"),
            last_backup
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "never".to_string())
        );
        crate::backup::run_backup(crate::backup::BackupRunArgs {
            config: args.config.clone(),
            json: args.json,
        })?;
        metrics::emit_u64("schedule_backups_due_run", 1);
    }

    let verify_due = verify_due
        || (backup_ran && due_for(app.verification.schedule.as_deref(), last_verify, now)?);
    if verify_due {
        // Record the attempt first — a failing verification must not
        // hot-loop on every invocation until the next occurrence.
        {
            let _lock = crate::backup::state_lock(&state_dir)?;
            let mut state = State::load(&state_path)?;
            state
                .applications
                .entry(app.name.clone())
                .or_default()
                .operations
                .last_verify_at = Some(now);
            state.save(&state_path)?;
        }
        println!(
            "verification due (schedule \"{}\", last ran {}) — verifying latest",
            app.verification.schedule.as_deref().unwrap_or("?"),
            last_verify
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "never".to_string())
        );
        crate::restore::run_verify(crate::restore::BackupVerifyArgs {
            config: args.config.clone(),
            snapshot: "latest".to_string(),
            json: args.json,
        })?;
        metrics::emit_u64("schedule_verifications_due_run", 1);
    }

    metrics::emit_u64("schedule_runs", 1);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).expect("rfc3339").to_utc()
    }

    #[test]
    fn no_schedule_is_never_due() {
        assert!(!due_for(None, None, at("2026-09-07T10:00:00Z")).expect("ok"));
        assert!(
            !due_for(
                None,
                Some(at("2026-09-01T10:00:00Z")),
                at("2026-09-07T10:00:00Z")
            )
            .expect("ok")
        );
    }

    #[test]
    fn never_ran_is_due_immediately() {
        assert!(due_for(Some("0 3 * * *"), None, at("2026-09-07T10:00:00Z")).expect("ok"));
    }

    #[test]
    fn due_once_the_next_occurrence_arrives() {
        let now = at("2026-09-08T03:00:00Z");
        // Last ran exactly at the previous occurrence: not due at 02:59:59,
        // due at 03:00:00 (the next occurrence).
        assert!(
            !due_for(
                Some("0 3 * * *"),
                Some(at("2026-09-07T03:00:00Z")),
                at("2026-09-08T02:59:59Z")
            )
            .expect("ok")
        );
        assert!(due_for(Some("0 3 * * *"), Some(at("2026-09-07T03:00:00Z")), now).expect("ok"));
    }

    #[test]
    fn an_invalid_schedule_is_a_config_error() {
        let err = due_for(Some("not cron"), None, at("2026-09-07T10:00:00Z")).expect_err("invalid");
        assert_eq!(err.exit_code(), 2);
    }
}
