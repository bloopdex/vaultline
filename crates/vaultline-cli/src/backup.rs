//! The backup command: `vaultline backup run` executes the Application
//! Recovery Definition against restic (ADR-V0-2), records the resulting
//! `BackupSnapshot` in the state file, and emits the named metrics.
//! `vaultline backup list` reads the state file.
//!
//! Honesty contract (failure-first): a snapshot record states exactly what
//! it captured. Sources not implemented yet (databases, volumes) are
//! declared-but-not-captured — warned on stderr and recorded in the
//! snapshot's configuration metadata, never silently included.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use chrono::Utc;
use tempfile::TempDir;
use tracing::{info, warn};

use vaultline_core::config;
use vaultline_core::error::{ErrorKind, Result, VaultlineError};
use vaultline_core::model::{
    Application, BackupSnapshot, CaptureSemantics, ConfigMetadata, EngineSnapshotRef,
    IntegrityInfo, RestoreMetadata, SourceKind, SourceManifest, SourceManifestEntry, StorageKind,
    VerificationLevel,
};
use vaultline_core::state::{State, StateLock};

use crate::engine::Restic;
use crate::metrics;

#[derive(clap::Args)]
pub struct BackupRunArgs {
    /// The configuration file (one application per file).
    #[arg(long, default_value = "vaultline.toml")]
    pub config: PathBuf,

    /// Machine-readable result on stdout (exit code unchanged).
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct BackupListArgs {
    #[arg(long, default_value = "vaultline.toml")]
    pub config: PathBuf,

    /// Machine-readable result on stdout.
    #[arg(long)]
    pub json: bool,
}

/// The state directory: `VAULTLINE_STATE_DIR` overrides, else the platform
/// data directory (e.g. `~/.local/state/vaultline` on Linux,
/// `%LOCALAPPDATA%\vaultline` on Windows).
pub fn state_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("VAULTLINE_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .map(|d| d.join("vaultline"))
        .ok_or_else(|| {
            VaultlineError::new(
                ErrorKind::Internal,
                "cannot determine a state directory on this platform; set VAULTLINE_STATE_DIR",
            )
        })
}

/// Resolve the repository password from the environment variable named by
/// the definition. Never from the configuration file.
pub fn resolve_password(app: &Application) -> Result<String> {
    std::env::var(&app.storage.password_env).map_err(|_| {
        VaultlineError::new(
            ErrorKind::Config,
            format!(
                "environment variable {} (application.storage.password_env) is not set — export it before backing up",
                app.storage.password_env
            ),
        )
    })
}

/// The restic repository URL for the declared storage target (ADR-V0-4:
/// thin adapters — restic's own backends carry the connectivity).
pub fn repo_url(app: &Application) -> Result<String> {
    let repo = app.storage.repository.as_deref().unwrap_or(&app.name);
    match &app.storage.kind {
        StorageKind::Local { path } => Ok(format!("{}/{}", path.trim_end_matches('/'), repo)),
        StorageKind::S3Compatible {
            endpoint, bucket, ..
        } => Ok(format!(
            "s3:{}/{}/{}",
            endpoint.trim_end_matches('/'),
            bucket,
            repo
        )),
        StorageKind::Sftp {
            host,
            port,
            user,
            path,
            ..
        } => Ok(format!(
            "sftp:{user}@{host}:{port}:{}/{}",
            path.trim_end_matches('/'),
            repo
        )),
    }
}

/// Environment variables restic needs for non-local backends (credential
/// names resolved from the environment, values forwarded — never literals).
pub fn storage_envs(app: &Application) -> Result<Vec<(String, String)>> {
    match &app.storage.kind {
        StorageKind::Local { .. } => Ok(Vec::new()),
        StorageKind::S3Compatible {
            region,
            access_key_env,
            secret_key_env,
            ..
        } => {
            let mut envs = vec![
                (
                    String::from("AWS_ACCESS_KEY_ID"),
                    resolve_env(access_key_env)?,
                ),
                (
                    String::from("AWS_SECRET_ACCESS_KEY"),
                    resolve_env(secret_key_env)?,
                ),
            ];
            if let Some(region_env) = region {
                envs.push((String::from("AWS_DEFAULT_REGION"), resolve_env(region_env)?));
            }
            Ok(envs)
        }
        StorageKind::Sftp {
            key_file,
            known_hosts,
            ..
        } => {
            // The custom-ssh-command path (restic -o sftp.command) involves
            // restic's own shell splitting of that string — that conflicts
            // with the argv-only discipline. Deferred: ssh-agent and
            // ~/.ssh/config work today (ADR-V0-2).
            if key_file.is_some() || known_hosts.is_some() {
                return Err(VaultlineError::new(
                    ErrorKind::Unsupported,
                    "application.storage: key_file/known_hosts are not wired yet (ssh-agent and ~/.ssh/config work today)",
                ));
            }
            Ok(Vec::new())
        }
    }
}

fn resolve_env(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| {
        VaultlineError::new(
            ErrorKind::Config,
            format!(
                "environment variable {name} is not set (referenced by the storage configuration)"
            ),
        )
    })
}

/// Stage a git source as a mirror clone (a bare clone with all refs — the
/// most complete local capture of a remote).
fn stage_git_mirror(name: &str, remote: &str) -> Result<(TempDir, PathBuf)> {
    let staging = tempfile::tempdir().map_err(|e| {
        VaultlineError::with_source(ErrorKind::Io, "cannot create the staging directory", e)
    })?;
    let target = staging.path().join(name);
    let output = Command::new("git")
        .args(["clone", "--mirror", remote])
        .arg(&target)
        .output()
        .map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Operational,
                "cannot start git (is git installed?)",
                e,
            )
        })?;
    if !output.status.success() {
        let tail = String::from_utf8_lossy(&output.stderr);
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "git clone --mirror failed for source \"{name}\": {}",
                tail.lines().rev().take(3).collect::<Vec<_>>().join(" | ")
            ),
        ));
    }
    Ok((staging, target))
}

/// Run a source's quiesce rule (shell-free argv). Non-zero exit aborts the
/// backup — a source that did not quiesce is not captured half-way.
fn run_quiesce(name: &str, quiesce: &vaultline_core::model::Quiesce) -> Result<()> {
    info!(source = name, command = %quiesce.command, "running quiesce rule");
    let output = Command::new(&quiesce.command)
        .args(&quiesce.args)
        .output()
        .map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Operational,
                format!("cannot start quiesce command for source \"{name}\""),
                e,
            )
        })?;
    if !output.status.success() {
        let tail = String::from_utf8_lossy(&output.stderr);
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "quiesce command failed for source \"{name}\": {}",
                tail.lines().rev().take(3).collect::<Vec<_>>().join(" | ")
            ),
        ));
    }
    Ok(())
}

/// Resolve a volume's capture path for direct semantics: a host path that
/// exists is used as-is; otherwise the name is treated as a Docker volume
/// and its mountpoint is resolved via `docker volume inspect` (the
/// sidecar / pause-first semantics remain deferred).
pub fn resolve_volume_path(name: &str) -> Result<PathBuf> {
    let candidate = Path::new(name);
    if candidate.exists() {
        return Ok(candidate.to_path_buf());
    }
    info!(volume = name, "resolving docker volume mountpoint");
    let output = Command::new("docker")
        .args(["volume", "inspect", "--format", "{{.Mountpoint}}", name])
        .output()
        .map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Operational,
                format!(
                    "volume \"{name}\" is not an existing path and docker is not available to inspect it"
                ),
                e,
            )
        })?;
    if !output.status.success() {
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "volume \"{name}\" is neither an existing path nor a docker volume: {}",
                String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .rev()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
        ));
    }
    let mountpoint = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if mountpoint.is_empty() {
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!("docker reported no mountpoint for volume \"{name}\""),
        ));
    }
    Ok(PathBuf::from(mountpoint))
}

pub fn run_backup(args: BackupRunArgs) -> Result<()> {
    let app = config::load(&args.config)?;

    let state_dir = state_dir()?;
    let _lock = StateLock::acquire(&state_dir)?;
    let restic = Restic::locate()?;
    let password = resolve_password(&app)?;
    let repo = repo_url(&app)?;
    let extra_envs = storage_envs(&app)?;

    // Capture plan: walk the declared sources, stage what needs staging,
    // and record the manifest exactly.
    let mut backup_paths: Vec<PathBuf> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut manifest_entries: Vec<SourceManifestEntry> = Vec::new();
    let mut reconstructs: Vec<String> = Vec::new();
    // Keep staging dirs alive until the backup finishes.
    let mut _staging: Vec<TempDir> = Vec::new();

    for source in &app.sources {
        match &source.kind {
            SourceKind::Files(files) => {
                if let Some(quiesce) = &files.quiesce {
                    run_quiesce(&source.name, quiesce)?;
                }
                for path in &files.paths {
                    backup_paths.push(PathBuf::from(path));
                }
                excludes.extend(files.excludes.clone());
                manifest_entries.push(SourceManifestEntry {
                    name: source.name.clone(),
                    kind: "files".to_string(),
                });
                reconstructs.push(source.name.clone());
            }
            SourceKind::Git(git) => {
                match git.capture {
                    vaultline_core::model::GitCapture::Mirror => {
                        let (staging, target) = stage_git_mirror(&source.name, &git.remote)?;
                        backup_paths.push(target);
                        _staging.push(staging);
                        manifest_entries.push(SourceManifestEntry {
                            name: source.name.clone(),
                            kind: "git-mirror".to_string(),
                        });
                        reconstructs.push(source.name.clone());
                    }
                    vaultline_core::model::GitCapture::Reference => {
                        // Recorded, not cloned: the remote + reference are in
                        // the definition; restore re-fetches them.
                        manifest_entries.push(SourceManifestEntry {
                            name: source.name.clone(),
                            kind: "git-reference".to_string(),
                        });
                        reconstructs.push(source.name.clone());
                    }
                }
            }
            SourceKind::ConfigRef(config_ref) => {
                // The redaction contract: config references are recorded as
                // pointers, never copied into the backup.
                manifest_entries.push(SourceManifestEntry {
                    name: source.name.clone(),
                    kind: "config_ref".to_string(),
                });
                let note = config_ref
                    .note
                    .clone()
                    .unwrap_or_else(|| "recorded, not captured".to_string());
                info!(
                    source = source.name,
                    path = config_ref.path,
                    note,
                    "config reference recorded, not captured"
                );
            }
        }
    }

    // Databases: capture each through its engine's consistency mechanism
    // into a staging dump, then let the dumps enter this same backup run
    // (one engine snapshot covers files + dumps — ADR-V0-3).
    let mut database_metadata: Vec<vaultline_core::model::DatabaseSnapshotMeta> = Vec::new();
    let mut _dump_staging: Vec<tempfile::TempDir> = Vec::new();
    for db in &app.databases {
        let capture = crate::database::capture_database(db)?;
        backup_paths.push(capture.dump_path);
        database_metadata.push(capture.meta);
        _dump_staging.push(capture.staging);
        reconstructs.push(db.name.clone());
    }

    // Volumes: direct semantics resolve to a path (host path or docker
    // volume mountpoint) and join the same backup run.
    let mut honesty_notes: Vec<String> = Vec::new();
    for volume in &app.volumes {
        match volume.capture {
            CaptureSemantics::Direct => {
                backup_paths.push(resolve_volume_path(&volume.name)?);
                manifest_entries.push(SourceManifestEntry {
                    name: volume.name.clone(),
                    kind: "volume-direct".to_string(),
                });
                reconstructs.push(volume.name.clone());
            }
            CaptureSemantics::Sidecar | CaptureSemantics::PauseFirst => {
                let note = format!(
                    "volume \"{}\" declared but not captured ({:?} semantics not implemented yet)",
                    volume.name, volume.capture
                );
                warn!(note, "volume capture semantics deferred");
                honesty_notes.push(note);
            }
        }
    }

    let started = Instant::now();
    restic.init_if_needed(&repo, &password, &extra_envs)?;
    let path_refs: Vec<&Path> = backup_paths.iter().map(|p| p.as_path()).collect();
    let summary = restic.backup(&repo, &password, &path_refs, &excludes, &extra_envs)?;
    let duration_ms = started.elapsed().as_millis() as u64;

    // The verification policy: L2 (repository integrity) runs inline when
    // the policy demands it. Higher levels are the verification phase's job.
    let mut engine_verified = false;
    let mut highest_level = VerificationLevel::L1;
    if app.verification.level >= VerificationLevel::L2 {
        match restic.check(&repo, &password, &extra_envs) {
            Ok(()) => {
                engine_verified = true;
                highest_level = VerificationLevel::L2;
                info!("repository integrity check passed (L2)");
            }
            Err(e) => {
                warn!(error = %e, "repository integrity check FAILED — the snapshot is recorded at L1, not L2")
            }
        }
    }

    let now = Utc::now();
    let snapshot = BackupSnapshot {
        id: format!(
            "{}-{}-{}",
            app.name,
            now.timestamp(),
            summary.snapshot_id.chars().take(8).collect::<String>()
        ),
        application: app.name.clone(),
        timestamp: now,
        source_manifest: SourceManifest {
            entries: manifest_entries,
        },
        database_metadata,
        configuration_metadata: ConfigMetadata {
            note: if honesty_notes.is_empty() {
                "full capture declared".to_string()
            } else {
                honesty_notes.join("; ")
            },
        },
        engine_snapshot: EngineSnapshotRef {
            engine: "restic".to_string(),
            snapshot_id: summary.snapshot_id.clone(),
        },
        integrity: IntegrityInfo {
            engine_verified,
            highest_verified_level: highest_level,
            verified_at: engine_verified.then_some(now),
        },
        restore_metadata: RestoreMetadata { reconstructs },
    };

    let state_path = state_dir.join("state.json");
    let mut state = State::load(&state_path)?;
    state.record_snapshot(&app.name, snapshot.clone());
    state.save(&state_path)?;

    metrics::emit_u64("backups_run", 1);
    metrics::emit_u64("backup_duration_ms", duration_ms);
    metrics::emit_u64(
        "verification_level_reached",
        highest_level.to_number() as u64,
    );
    metrics::emit_u64("backup_files_new", summary.files_new);
    metrics::emit_u64("backup_bytes_processed", summary.total_bytes_processed);

    if args.json {
        println!(
            "{}",
            serde_json::to_string(&snapshot).expect("snapshot serializes")
        );
    } else {
        println!(
            "backup complete: {} (engine snapshot {}, verification L{}, {} files, {} bytes)",
            snapshot.id,
            snapshot.engine_snapshot.snapshot_id,
            highest_level.to_number(),
            summary.files_new,
            summary.total_bytes_processed
        );
    }
    Ok(())
}

pub fn run_list(args: BackupListArgs) -> Result<()> {
    let app = config::load(&args.config)?;
    let state = State::load(&state_dir()?.join("state.json"))?;
    let snapshots = state
        .applications
        .get(&app.name)
        .map(|s| s.snapshots.as_slice())
        .unwrap_or(&[]);

    if args.json {
        println!(
            "{}",
            serde_json::to_string(snapshots).expect("snapshots serialize")
        );
        return Ok(());
    }

    if snapshots.is_empty() {
        println!("no snapshots recorded for {}", app.name);
        return Ok(());
    }
    for snapshot in snapshots {
        println!(
            "{}\t{}\tL{}\treconstructs: {}",
            snapshot.id,
            snapshot.timestamp.to_rfc3339(),
            snapshot.integrity.highest_verified_level.to_number(),
            snapshot.restore_metadata.reconstructs.join(", "),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vaultline_core::model::{StorageKind, StorageTarget};

    fn app_with(storage: StorageTarget) -> Application {
        Application {
            name: "thornwa".to_string(),
            description: None,
            sources: Vec::new(),
            databases: Vec::new(),
            volumes: Vec::new(),
            storage,
            retention: vaultline_core::model::RetentionPolicy {
                keep_last: 7,
                keep_daily: 0,
                keep_weekly: 0,
                keep_monthly: 0,
                keep_yearly: 0,
            },
            verification: vaultline_core::model::VerificationPolicy {
                level: VerificationLevel::L1,
                schedule: None,
                app_checks: Vec::new(),
            },
            restore: vaultline_core::model::RestoreProcedure { steps: Vec::new() },
        }
    }

    #[test]
    fn local_repo_url_is_path_plus_repository() {
        let app = app_with(StorageTarget {
            kind: StorageKind::Local {
                path: "/var/backups".to_string(),
            },
            repository: None,
            password_env: "PW".to_string(),
        });
        assert_eq!(repo_url(&app).expect("url"), "/var/backups/thornwa");
    }

    #[test]
    fn local_repo_url_honors_repository_name_and_trailing_slash() {
        let app = app_with(StorageTarget {
            kind: StorageKind::Local {
                path: "/var/backups/".to_string(),
            },
            repository: Some("repo-a".to_string()),
            password_env: "PW".to_string(),
        });
        assert_eq!(repo_url(&app).expect("url"), "/var/backups/repo-a");
    }

    #[test]
    fn s3_repo_url_is_a_restic_s3_url() {
        let app = app_with(StorageTarget {
            kind: StorageKind::S3Compatible {
                endpoint: "https://s3.example.com".to_string(),
                bucket: "backups".to_string(),
                region: None,
                access_key_env: "AK".to_string(),
                secret_key_env: "SK".to_string(),
            },
            repository: Some("repo-a".to_string()),
            password_env: "PW".to_string(),
        });
        assert_eq!(
            repo_url(&app).expect("url"),
            "s3:https://s3.example.com/backups/repo-a"
        );
    }

    #[test]
    fn sftp_repo_url_includes_user_host_port_path() {
        let app = app_with(StorageTarget {
            kind: StorageKind::Sftp {
                host: "backup.example.com".to_string(),
                port: 2222,
                user: "backup".to_string(),
                path: "/srv/backups".to_string(),
                key_file: None,
                known_hosts: None,
            },
            repository: None,
            password_env: "PW".to_string(),
        });
        assert_eq!(
            repo_url(&app).expect("url"),
            "sftp:backup@backup.example.com:2222:/srv/backups/thornwa"
        );
    }

    #[test]
    fn sftp_key_file_is_deferred_with_a_clear_error() {
        let app = app_with(StorageTarget {
            kind: StorageKind::Sftp {
                host: "backup.example.com".to_string(),
                port: 22,
                user: "backup".to_string(),
                path: "/srv/backups".to_string(),
                key_file: Some("/home/me/key".to_string()),
                known_hosts: None,
            },
            repository: None,
            password_env: "PW".to_string(),
        });
        let err = storage_envs(&app).expect_err("deferred");
        assert!(err.to_string().contains("key_file"));
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn missing_password_env_names_the_variable() {
        let app = app_with(StorageTarget {
            kind: StorageKind::Local {
                path: "/tmp/x".to_string(),
            },
            repository: None,
            password_env: "RESTIC_PASSWORD_THORNWA".to_string(),
        });
        let err = resolve_password(&app).expect_err("unset");
        assert!(err.to_string().contains("RESTIC_PASSWORD_THORNWA"));
        assert_eq!(err.exit_code(), 2);
    }
}
