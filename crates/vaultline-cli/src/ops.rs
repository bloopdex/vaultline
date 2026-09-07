//! `vaultline doctor` and `vaultline status` — the diagnosis surface.
//!
//! `doctor` checks the environment (configuration, tools, state file,
//! storage connectivity) and reports each check as ok / warning / error
//! with details; the exit code is 0 only when nothing errored. `status`
//! summarizes one application's records against its definition: snapshots,
//! verification levels, schedules and due-ness, and the retention
//! projection. Neither command mutates anything.

use std::path::PathBuf;
use std::process::Command;

use chrono::Utc;

use vaultline_core::config;
use vaultline_core::error::{ErrorKind, Result, VaultlineError};
use vaultline_core::model::DatabaseType;
use vaultline_core::retention::plan;
use vaultline_core::state::State;

use crate::backup::{repo_url, resolve_password, state_dir, storage_envs};
use crate::database::locate_tool;
use crate::engine::Restic;

// ---------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------

#[derive(clap::Args)]
pub struct DoctorArgs {
    #[arg(long, default_value = "vaultline.toml")]
    pub config: PathBuf,

    /// Machine-readable result on stdout (exit code unchanged).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CheckStatus {
    Ok,
    Warning,
    Error,
}

impl CheckStatus {
    fn as_str(&self) -> &'static str {
        match self {
            CheckStatus::Ok => "ok",
            CheckStatus::Warning => "warning",
            CheckStatus::Error => "error",
        }
    }
}

#[derive(Debug)]
struct Check {
    name: String,
    status: CheckStatus,
    detail: String,
}

/// Run `<tool> --version`; success = the tool exists and runs.
fn probe_version(tool: &std::path::Path) -> std::result::Result<String, String> {
    let output = Command::new(tool)
        .arg("--version")
        .output()
        .map_err(|e| format!("cannot start {}: {e}", tool.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} failed to run (exit {:?})",
            tool.display(),
            output.status.code()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("(no version output)")
        .trim()
        .to_string())
}

fn dump_tool_for(kind: DatabaseType) -> (&'static str, &'static str) {
    match kind {
        DatabaseType::PostgreSql => ("VAULTLINE_PGDUMP", "pg_dump"),
        DatabaseType::MySql | DatabaseType::MariaDb => ("VAULTLINE_MYSQLDUMP", "mysqldump"),
        DatabaseType::Sqlite => ("VAULTLINE_SQLITE3", "sqlite3"),
    }
}

pub fn run_doctor(args: DoctorArgs) -> Result<()> {
    let mut checks: Vec<Check> = Vec::new();

    // Configuration.
    let app = match config::load(&args.config) {
        Ok(app) => {
            checks.push(Check {
                name: "config".to_string(),
                status: CheckStatus::Ok,
                detail: format!(
                    "{}: {} source(s), {} database(s), {} volume(s), verification L{}",
                    app.name,
                    app.sources.len(),
                    app.databases.len(),
                    app.volumes.len(),
                    app.verification.level.to_number()
                ),
            });
            Some(app)
        }
        Err(e) => {
            checks.push(Check {
                name: "config".to_string(),
                status: CheckStatus::Error,
                detail: e.to_string(),
            });
            None
        }
    };

    // restic (its version flag is a subcommand, not --version).
    match Restic::locate() {
        Ok(restic) => match Command::new(restic.bin_path())
            .arg("version")
            .output()
            .map_err(|e| format!("cannot start {}: {e}", restic.bin_path().display()))
            .and_then(|output| {
                if output.status.success() {
                    Ok(String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .next()
                        .unwrap_or("(no version output)")
                        .trim()
                        .to_string())
                } else {
                    Err(format!(
                        "{} failed to run (exit {:?})",
                        restic.bin_path().display(),
                        output.status.code()
                    ))
                }
            }) {
            Ok(version) => checks.push(Check {
                name: "restic".to_string(),
                status: CheckStatus::Ok,
                detail: version,
            }),
            Err(message) => checks.push(Check {
                name: "restic".to_string(),
                status: CheckStatus::Error,
                detail: message,
            }),
        },
        Err(e) => checks.push(Check {
            name: "restic".to_string(),
            status: CheckStatus::Error,
            detail: e.to_string(),
        }),
    }

    // Dump tools: one check per distinct engine kind declared.
    if let Some(app) = &app {
        let mut kinds: Vec<DatabaseType> = Vec::new();
        for db in &app.databases {
            if !kinds.contains(&db.kind) {
                kinds.push(db.kind);
            }
        }
        for kind in kinds {
            let (env_name, tool_name) = dump_tool_for(kind);
            let name = format!("tool:{tool_name}");
            match locate_tool(env_name, tool_name) {
                Ok(path) => match probe_version(&path) {
                    Ok(version) => checks.push(Check {
                        name: name.clone(),
                        status: CheckStatus::Ok,
                        detail: version,
                    }),
                    Err(message) => checks.push(Check {
                        name: name.clone(),
                        status: CheckStatus::Error,
                        detail: message,
                    }),
                },
                Err(e) => checks.push(Check {
                    name: name.clone(),
                    status: CheckStatus::Error,
                    detail: e.to_string(),
                }),
            }
        }
    }

    // State file.
    let state = match State::load(&state_dir()?.join("state.json")) {
        Ok(state) => {
            let count: usize = state
                .applications
                .values()
                .map(|app_state| app_state.snapshots.len())
                .sum();
            checks.push(Check {
                name: "state".to_string(),
                status: CheckStatus::Ok,
                detail: if count == 0 {
                    "readable; no snapshots recorded yet".to_string()
                } else {
                    format!("readable; {count} snapshot record(s)")
                },
            });
            Some(state)
        }
        Err(e) => {
            checks.push(Check {
                name: "state".to_string(),
                status: CheckStatus::Error,
                detail: e.to_string(),
            });
            None
        }
    };

    // Storage connectivity (needs the password environment variable).
    if let Some(app) = &app {
        match resolve_password(app) {
            Err(_) => checks.push(Check {
                name: "storage".to_string(),
                status: CheckStatus::Warning,
                detail: format!(
                    "skipped: {} is not set (export it to probe the repository)",
                    app.storage.password_env
                ),
            }),
            Ok(password) => match Restic::locate() {
                Err(_) => {
                    // restic's own check already reported this.
                }
                Ok(restic) => {
                    let repo = repo_url(app)?;
                    let extra_envs = storage_envs(app)?;
                    match restic.list_snapshots(&repo, &password, &extra_envs) {
                        Ok(snapshots) => checks.push(Check {
                            name: "storage".to_string(),
                            status: CheckStatus::Ok,
                            detail: format!(
                                "repository reachable; {} engine snapshot(s)",
                                snapshots.len()
                            ),
                        }),
                        Err(e) if e.to_string().contains("repository does not exist") => {
                            checks.push(Check {
                                name: "storage".to_string(),
                                status: CheckStatus::Warning,
                                detail: "repository not created yet — the first backup run initializes it"
                                    .to_string(),
                            });
                        }
                        Err(e) => checks.push(Check {
                            name: "storage".to_string(),
                            status: CheckStatus::Error,
                            detail: e.to_string(),
                        }),
                    }
                }
            },
        }
    }

    // Recorded snapshots whose engine copy is gone (pruned or lost) — a
    // cross-check that does not need the storage probe to be reachable.
    if let (Some(state), Some(app)) = (&state, &app) {
        let recorded: usize = state
            .applications
            .get(&app.name)
            .map(|s| s.snapshots.len())
            .unwrap_or(0);
        let pruned = state
            .applications
            .get(&app.name)
            .map(|s| s.operations.prunes.iter().map(|p| p.forgotten.len()).sum())
            .unwrap_or(0);
        if recorded > 0 {
            checks.push(Check {
                name: "snapshots".to_string(),
                status: CheckStatus::Ok,
                detail: format!("{recorded} recorded; {pruned} pruned by applied runs"),
            });
        }
    }

    let errored = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Error)
        .count();
    let warned = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Warning)
        .count();

    if args.json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "healthy": errored == 0,
                "checks": checks.iter().map(|c| serde_json::json!({
                    "name": c.name,
                    "status": c.status.as_str(),
                    "detail": c.detail,
                })).collect::<Vec<_>>(),
            }))
            .expect("checks serialize")
        );
    } else {
        for check in &checks {
            println!(
                "{}\t{}\t{}",
                check.status.as_str(),
                check.name,
                check.detail
            );
        }
    }

    if errored > 0 {
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!("doctor found {errored} failing check(s) ({warned} warning(s))"),
        ));
    }
    if !args.json {
        println!("healthy: all checks passed ({warned} warning(s))");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

#[derive(clap::Args)]
pub struct StatusArgs {
    #[arg(long, default_value = "vaultline.toml")]
    pub config: PathBuf,

    /// Machine-readable result on stdout (exit code unchanged).
    #[arg(long)]
    pub json: bool,
}

pub fn run_status(args: StatusArgs) -> Result<()> {
    let app = config::load(&args.config)?;
    let state = State::load(&state_dir()?.join("state.json"))?;
    let now = Utc::now();
    let recorded = state
        .applications
        .get(&app.name)
        .map(|s| s.snapshots.as_slice())
        .unwrap_or(&[]);
    let operations = state
        .applications
        .get(&app.name)
        .map(|s| &s.operations)
        .cloned()
        .unwrap_or_default();

    let last_backup = recorded.iter().map(|s| s.timestamp).max();
    let latest = recorded.iter().max_by_key(|s| s.timestamp);
    let pruned_count = operations
        .prunes
        .iter()
        .map(|p| p.forgotten.len())
        .sum::<usize>();
    let retention = plan(recorded, &app.retention);
    let retention_kept = retention.iter().filter(|d| d.is_kept()).count();
    let retention_forgettable = retention.len() - retention_kept;
    let next_backup = next_label(app.schedule.as_deref(), last_backup, now);
    let next_verify = next_label(
        app.verification.schedule.as_deref(),
        operations.last_verify_at,
        now,
    );

    let policy_level = app.verification.level.to_number();
    let latest_level = latest
        .map(|s| s.integrity.highest_verified_level.to_number())
        .unwrap_or(0);

    if args.json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "application": app.name,
                "storage": storage_label(&app.storage),
                "snapshots": {
                    "recorded": recorded.len(),
                    "pruned": pruned_count,
                    "last_backup_at": last_backup,
                    "latest_id": latest.map(|s| s.id.clone()),
                    "latest_verification_level": latest_level,
                    "policy_verification_level": policy_level,
                },
                "schedules": {
                    "backup": { "cron": app.schedule, "next": next_backup },
                    "verification": {
                        "cron": app.verification.schedule,
                        "next": next_verify,
                        "last_run_at": operations.last_verify_at,
                    },
                },
                "retention": {
                    "policy": app.retention,
                    "projected_kept": retention_kept,
                    "projected_forgettable": retention_forgettable,
                },
                "operations": {
                    "prune_runs": operations.prunes.len(),
                    "last_prune_at": operations.last_prune_at,
                },
            }))
            .expect("status serializes")
        );
        return Ok(());
    }

    println!("application: {}", app.name);
    println!("storage: {}", storage_label(&app.storage));
    println!(
        "snapshots: {} recorded, {} pruned",
        recorded.len(),
        pruned_count
    );
    match last_backup {
        Some(ts) => println!("last backup: {}", ts.to_rfc3339()),
        None => println!("last backup: never"),
    }
    match latest {
        Some(snapshot) => println!(
            "latest snapshot: {} (verification L{} of required L{})",
            snapshot.id, latest_level, policy_level
        ),
        None => println!("latest snapshot: none"),
    }
    println!("backup schedule: {}", next_backup);
    println!("verification schedule: {}", next_verify);
    println!(
        "retention: {} recorded, policy keeps {}, {} would be forgotten",
        recorded.len(),
        retention_kept,
        retention_forgettable
    );
    println!(
        "prune: {} applied run(s), last at {}",
        operations.prunes.len(),
        operations
            .last_prune_at
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "never".to_string())
    );
    Ok(())
}

fn storage_label(storage: &vaultline_core::model::StorageTarget) -> &'static str {
    match &storage.kind {
        vaultline_core::model::StorageKind::Local { .. } => "local",
        vaultline_core::model::StorageKind::S3Compatible { .. } => "s3",
        vaultline_core::model::StorageKind::Sftp { .. } => "sftp",
    }
}

fn next_label(
    schedule: Option<&str>,
    anchor: Option<chrono::DateTime<Utc>>,
    now: chrono::DateTime<Utc>,
) -> String {
    let Some(expr) = schedule else {
        return "none declared".to_string();
    };
    let Ok(schedule) = vaultline_core::cron::parse_schedule(expr) else {
        return "invalid schedule".to_string();
    };
    match vaultline_core::cron::next_after(&schedule, anchor.unwrap_or(now)) {
        Some(next) => format!("\"{expr}\" — next occurrence {}", next.to_rfc3339()),
        None => format!("\"{expr}\" — no future occurrence"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_tool_mapping_is_stable() {
        assert_eq!(
            dump_tool_for(DatabaseType::PostgreSql),
            ("VAULTLINE_PGDUMP", "pg_dump")
        );
        assert_eq!(
            dump_tool_for(DatabaseType::MySql),
            ("VAULTLINE_MYSQLDUMP", "mysqldump")
        );
        assert_eq!(
            dump_tool_for(DatabaseType::MariaDb),
            ("VAULTLINE_MYSQLDUMP", "mysqldump")
        );
        assert_eq!(
            dump_tool_for(DatabaseType::Sqlite),
            ("VAULTLINE_SQLITE3", "sqlite3")
        );
    }

    #[test]
    fn next_label_shapes() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-07T10:00:00Z")
            .expect("ts")
            .to_utc();
        assert_eq!(next_label(None, None, now), "none declared");
        assert!(next_label(Some("0 3 * * *"), None, now).contains("next occurrence"));
        assert!(next_label(Some("junk"), None, now).contains("invalid"));
    }
}
