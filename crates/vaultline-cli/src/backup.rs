//! The backup command: `vaultline backup run` executes the Application
//! Recovery Definition against restic (ADR-V0-2), records the resulting
//! `BackupSnapshot` in the state file, and emits the named metrics.
//! `vaultline backup list` reads the state file.
//!
//! Honesty contract (failure-first): a snapshot record states exactly what
//! it captured — every declared source, database, and volume executes
//! (the three volume capture semantics included, ADR-V0-8), and the
//! snapshot's manifest records each capture path. The pre-flight defends
//! the run: a vanished source aborts the backup naming the path rather
//! than recording a silently-incomplete snapshot.

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
use vaultline_core::state::State;

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

/// Whether the process with the given pid is alive on this host
/// (ADR-007, the stale-lock liveness check). Fail-safe: when the
/// platform probe cannot run, the answer is "alive" — a lock is never
/// reclaimed without evidence of the holder's death.
pub fn process_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(true)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
            .unwrap_or(true)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// The start time of the process with the given pid, as the platform can
/// best express it (ADR-007's pid-reuse defense: a live pid whose start
/// time differs from the lock's record is a REUSED pid, not the holder).
/// Linux reads /proc/<pid>/stat directly (no spawn); Windows asks
/// PowerShell for StartTime (a spawn — paid once per command at lock
/// acquisition, and only on contention after that; recorded cost). None
/// when the platform has no probe (non-Linux unix) or the probe cannot
/// answer — the lock then degrades to liveness-only, honestly.
pub fn process_started(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The stat line is "pid (comm) rest...": the comm may contain
        // spaces and parens, so everything after the LAST ')' holds the
        // numeric fields — field 3 (state) onwards. Starttime is field
        // 22, i.e. index 19 of that tail.
        let after = stat.rsplit_once(')')?.1;
        Some(after.split_whitespace().nth(19)?.to_string())
    }
    #[cfg(windows)]
    {
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("(Get-Process -Id {pid}).StartTime.ToFileTimeUtc()"),
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Some(text.trim().to_string()).filter(|s| !s.is_empty())
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = pid;
        None
    }
}

/// The contention checker (ADR-007): a holder is alive when it is the
/// SAME process the lock recorded — the pid is live AND (when the lock
/// recorded a start time) the current process at that pid started at the
/// recorded time. A pid-only lock (an earlier build's) falls back to
/// liveness alone. Fail-safe: an unanswerable probe reads as alive —
/// never reclaim without evidence.
fn holder_is_alive(pid: u32, recorded_started: Option<&str>) -> bool {
    match recorded_started {
        Some(recorded) => process_started(pid).is_none_or(|current| current == recorded),
        None => process_is_alive(pid),
    }
}

/// The state lock with the platform probes wired: the holder's start
/// time is recorded at acquisition and verified at contention.
pub fn state_lock(state_dir: &Path) -> Result<vaultline_core::state::StateLock> {
    vaultline_core::state::StateLock::acquire_with(
        state_dir,
        &|| process_started(std::process::id()),
        &holder_is_alive,
    )
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
            // The URI form is the ONLY restic sftp shape that carries a
            // port: the bare `sftp:user@host:path` form treats the
            // first colon as the path separator, so an embedded port
            // silently becomes part of the directory and ssh connects
            // to port 22 (verified against restic 0.16.4's config
            // parser — the first hosted SFTP run surfaced the bug the
            // agent-gated test had never executed).
            "sftp://{user}@{host}:{port}//{}/{}",
            path.trim_matches('/'),
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
            // The declared files must exist — a missing key surfaces as a
            // named configuration error, not an opaque ssh failure.
            for (field, path) in [("key_file", key_file), ("known_hosts", known_hosts)] {
                if let Some(path) = path {
                    if Path::new(path).is_file() {
                        continue;
                    }
                    return Err(VaultlineError::new(
                        ErrorKind::Config,
                        format!(
                            "application.storage.{field}: {path} does not exist or is not a file"
                        ),
                    ));
                }
            }
            Ok(Vec::new())
        }
    }
}

/// The extra restic arguments for an sftp target whose `key_file` /
/// `known_hosts` are declared (ADR-002 amendment part 3): `-o
/// sftp.args=<tokens>` extends the native ssh argv restic builds —
/// `-o BatchMode=yes` (prompts become clean failures: restic's stdin is
/// the sftp pipe) plus `-i '<key_file>'` / `-o
/// UserKnownHostsFile='<known_hosts>'`. Evidence (restic source,
/// 2026-09-08): the sftp backend always execs the native ssh client and
/// tokenizes the option string with its shell-splitter, then hands the
/// tokens to exec.Command as argv — no shell executes anywhere in the
/// chain. Single-quoting is unambiguous because validation rejects quote
/// and newline characters in the paths. Returns None when neither field
/// is declared (the agent / ~/.ssh/config path, unchanged).
pub fn restic_sftp_opts(app: &Application) -> Option<Vec<String>> {
    let StorageKind::Sftp {
        key_file,
        known_hosts,
        ..
    } = &app.storage.kind
    else {
        return None;
    };
    if key_file.is_none() && known_hosts.is_none() {
        return None;
    }
    let mut tokens = vec!["-o BatchMode=yes".to_string()];
    if let Some(key) = key_file {
        tokens.push(format!("-i '{key}'"));
    }
    if let Some(hosts) = known_hosts {
        tokens.push(format!("-o UserKnownHostsFile='{hosts}'"));
    }
    Some(vec![
        "-o".to_string(),
        format!("sftp.args={}", tokens.join(" ")),
    ])
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

/// The resolution of a volume name to a host-reachable path.
pub struct ResolvedVolumePath {
    pub path: PathBuf,
    /// True when the path came from `docker volume inspect`. On Docker
    /// Desktop such mountpoints live inside the VM and may not be
    /// reachable from the host — the sidecar semantics exist for that.
    pub from_docker: bool,
}

/// Resolve a volume's capture path for direct semantics: a host path that
/// exists is used as-is; otherwise the name is treated as a Docker volume
/// and its mountpoint is resolved via `docker volume inspect`.
pub fn resolve_volume_path(name: &str) -> Result<ResolvedVolumePath> {
    let candidate = Path::new(name);
    if candidate.exists() {
        return Ok(ResolvedVolumePath {
            path: candidate.to_path_buf(),
            from_docker: false,
        });
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
    Ok(ResolvedVolumePath {
        path: PathBuf::from(mountpoint),
        from_docker: true,
    })
}

/// The pause-first guard: unpauses its container when dropped — ALWAYS,
/// including on capture errors. An unpause failure is CRITICAL (the
/// container stays paused) and is logged with the manual fix.
pub struct ContainerPause {
    container: String,
    engaged: bool,
}

impl Drop for ContainerPause {
    fn drop(&mut self) {
        if !self.engaged {
            return;
        }
        match Command::new("docker")
            .args(["unpause", &self.container])
            .output()
        {
            Ok(output) if output.status.success() => {
                info!(
                    container = self.container,
                    "writer container unpaused after the capture"
                );
            }
            Ok(output) => {
                tracing::error!(
                    container = self.container,
                    stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                    "CRITICAL: docker unpause FAILED — the container is still paused; run `docker unpause {}`",
                    self.container
                );
            }
            Err(e) => {
                tracing::error!(
                    container = self.container,
                    error = %e,
                    "CRITICAL: docker unpause could not run — the container may still be paused; run `docker unpause {}`",
                    self.container
                );
            }
        }
    }
}

/// Pause the writer container (the pause-first semantics, ADR-V0-3's
/// third capture kind). A running container is paused and the returned
/// guard unpauses it; a container that is NOT running is already
/// quiescent, so the capture proceeds with a note; a running container
/// whose pause fails aborts — never capture live writes under a pause
/// that never happened.
fn pause_container(container: &str) -> Result<ContainerPause> {
    let output = Command::new("docker")
        .args(["pause", container])
        .output()
        .map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Operational,
                format!("docker is not available to pause container \"{container}\""),
                e,
            )
        })?;
    if output.status.success() {
        info!(container, "writer container paused for the capture");
        return Ok(ContainerPause {
            container: container.to_string(),
            engaged: true,
        });
    }
    // The pause failed — is the container even running? Evidence, not
    // stderr-message matching (the messages are docker's, not a contract).
    let running = Command::new("docker")
        .args(["inspect", "--format", "{{.State.Running}}", container])
        .output();
    match running {
        Ok(state) if state.status.success() => {
            let running = String::from_utf8_lossy(&state.stdout).trim() == "true";
            if running {
                Err(VaultlineError::new(
                    ErrorKind::Operational,
                    format!(
                        "container \"{container}\" is running but docker pause failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                ))
            } else {
                warn!(
                    container,
                    "container is not running — the data is already quiescent; capturing without pausing"
                );
                Ok(ContainerPause {
                    container: container.to_string(),
                    engaged: false,
                })
            }
        }
        _ => Err(VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "docker pause failed for container \"{container}\" and its state could not be confirmed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        )),
    }
}

/// The docker argv for a sidecar capture (pure so the image choice is
/// unit-testable): the volume is mounted READ-ONLY into a throwaway
/// container that copies it into a host staging directory (bind-mounted
/// into the container) — argv-only, no shell.
fn sidecar_argv(volume_name: &str, host_half: &str, image: &str) -> Vec<String> {
    vec![
        "run".to_string(),
        "--rm".to_string(),
        "-v".to_string(),
        format!("{volume_name}:/vaultline-src:ro"),
        "-v".to_string(),
        format!("{host_half}:/vaultline-out:rw"),
        image.to_string(),
        "cp".to_string(),
        "-a".to_string(),
        "/vaultline-src/.".to_string(),
        "/vaultline-out/".to_string(),
    ]
}

/// Sidecar capture (the second of ADR-V0-3's volume semantics): the
/// volume is mounted READ-ONLY into a throwaway container that copies it
/// into a host staging directory (bind-mounted into the container) —
/// argv-only, no shell. This is the semantics that works where the host
/// cannot reach the volume's mountpoint (Docker Desktop keeps volumes
/// inside its VM). The image is the volume's declared sidecar image (or
/// `alpine`); it is pulled on first use. The staging dir must stay alive
/// for the backup run.
pub fn stage_sidecar_capture(
    volume_name: &str,
    image: &str,
) -> Result<(tempfile::TempDir, PathBuf)> {
    let staging = tempfile::tempdir().map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            format!(
                "cannot create the staging directory for the sidecar capture of \"{volume_name}\""
            ),
            e,
        )
    })?;
    let target = staging.path().to_path_buf();
    // Forward slashes so the Windows path parses unambiguously as the
    // host half of the bind mount.
    let host_half = target.display().to_string().replace('\\', "/");
    info!(
        volume = volume_name,
        image, "capturing the volume through a read-only sidecar container"
    );
    let output = Command::new("docker")
        .args(sidecar_argv(volume_name, &host_half, image))
        .output()
        .map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Operational,
                format!("cannot run the sidecar container for volume \"{volume_name}\""),
                e,
            )
        })?;
    if !output.status.success() {
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "the sidecar capture of volume \"{volume_name}\" failed: {} (the {image} image must be present or pullable)",
                String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .rev()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
        ));
    }
    Ok((staging, target))
}

pub fn run_backup(args: BackupRunArgs) -> Result<()> {
    let app = config::load(&args.config)?;

    let state_dir = state_dir()?;
    let _lock = state_lock(&state_dir)?;
    let restic = Restic::locate()?;
    let password = resolve_password(&app)?;
    let repo = repo_url(&app)?;
    let extra_envs = storage_envs(&app)?;
    let sftp_opts = restic_sftp_opts(&app).unwrap_or_default();

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
                    path: None,
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
                            path: None,
                        });
                        reconstructs.push(source.name.clone());
                    }
                    vaultline_core::model::GitCapture::Reference => {
                        // Recorded, not cloned: the remote + reference are in
                        // the definition; restore re-fetches them.
                        manifest_entries.push(SourceManifestEntry {
                            name: source.name.clone(),
                            kind: "git-reference".to_string(),
                            path: None,
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
                    path: None,
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

    // Volumes: all three capture semantics are executable now. Direct
    // resolves to a path (host path or docker volume mountpoint) and
    // joins the backup run; sidecar copies the volume out through a
    // read-only sidecar container into a staging dir (the semantics for
    // hosts that cannot reach docker mountpoints — Desktop VMs);
    // pause-first pauses the declared writer container around a direct
    // capture and ALWAYS unpauses (the guard's drop does it even on
    // error paths). Each entry records WHERE the bytes were captured —
    // restores locate the content through the record, never through the
    // restore host's own resolution.
    let mut pause_guards: Vec<ContainerPause> = Vec::new();
    // Paths docker reported as mountpoints — the pre-flight below
    // phrases their absence as the Desktop-VM signature, not a vanished
    // source.
    let mut docker_resolved: Vec<PathBuf> = Vec::new();
    for volume in &app.volumes {
        match volume.capture {
            CaptureSemantics::Direct => {
                let resolved = resolve_volume_path(&volume.name)?;
                backup_paths.push(resolved.path.clone());
                if resolved.from_docker {
                    docker_resolved.push(resolved.path.clone());
                }
                manifest_entries.push(SourceManifestEntry {
                    name: volume.name.clone(),
                    kind: "volume-direct".to_string(),
                    path: Some(resolved.path.display().to_string()),
                });
                reconstructs.push(volume.name.clone());
            }
            CaptureSemantics::PauseFirst => {
                let container = volume
                    .container
                    .as_deref()
                    .expect("validated: pause-first capture requires the writer container");
                pause_guards.push(pause_container(container)?);
                let resolved = resolve_volume_path(&volume.name)?;
                backup_paths.push(resolved.path.clone());
                if resolved.from_docker {
                    docker_resolved.push(resolved.path.clone());
                }
                manifest_entries.push(SourceManifestEntry {
                    name: volume.name.clone(),
                    kind: "volume-pause-first".to_string(),
                    path: Some(resolved.path.display().to_string()),
                });
                reconstructs.push(volume.name.clone());
            }
            CaptureSemantics::Sidecar => {
                let image = volume.sidecar_image.as_deref().unwrap_or("alpine");
                let (staging, target) = stage_sidecar_capture(&volume.name, image)?;
                backup_paths.push(target.clone());
                _staging.push(staging);
                manifest_entries.push(SourceManifestEntry {
                    name: volume.name.clone(),
                    kind: "volume-sidecar".to_string(),
                    path: Some(target.display().to_string()),
                });
                reconstructs.push(volume.name.clone());
            }
        }
    }

    // The vanished-source defense (ADR-007): restic SKIPS missing paths
    // silently ("does not exist, skipping" — verified empirically,
    // 2026-09-07), which would produce a silently-incomplete snapshot.
    // Every declared capture path is existence-checked first: a vanished
    // source aborts the backup naming the path.
    for path in &backup_paths {
        if !path.exists() {
            if docker_resolved.contains(path) {
                return Err(VaultlineError::new(
                    ErrorKind::Operational,
                    format!(
                        "the docker-reported mountpoint {} is not reachable from this host (Docker Desktop keeps volumes inside its VM; on native hosts the docker-data directory may not be traversable by this user) — declare the volume with capture = \"sidecar\" instead",
                        path.display()
                    ),
                ));
            }
            return Err(VaultlineError::new(
                ErrorKind::Operational,
                format!(
                    "source path {} does not exist (it vanished or the definition points nowhere) — the backup is aborted rather than recording an incomplete snapshot",
                    path.display()
                ),
            ));
        }
    }

    let started = Instant::now();
    restic.init_if_needed(&repo, &password, &extra_envs, &sftp_opts)?;
    let path_refs: Vec<&Path> = backup_paths.iter().map(|p| p.as_path()).collect();
    let summary = restic.backup(
        &repo,
        &password,
        &path_refs,
        &excludes,
        &extra_envs,
        &sftp_opts,
    )?;
    let duration_ms = started.elapsed().as_millis() as u64;

    // The verification policy: L2 (repository integrity) runs inline when
    // the policy demands it. Higher levels are the verification phase's job.
    let mut engine_verified = false;
    let mut highest_level = VerificationLevel::L1;
    if app.verification.level >= VerificationLevel::L2 {
        match restic.check(&repo, &password, &extra_envs, &sftp_opts) {
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
            // Every declared capture semantics is executable now — the
            // declared-but-not-captured era is over (Phase 8).
            note: "full capture declared — every declared source, database, and volume captured"
                .to_string(),
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
            schedule: None,
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
            rehearsal: None,
            restore: vaultline_core::model::RestoreProcedure {
                steps: Vec::new(),
                path_map: Vec::new(),
            },
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
            // The URI form: the bare sftp: form would swallow the port
            // into the directory (restic's parser — recorded).
            "sftp://backup@backup.example.com:2222//srv/backups/thornwa"
        );
    }

    #[test]
    fn sftp_opts_are_none_without_declared_key_paths() {
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
        assert!(restic_sftp_opts(&app).is_none(), "no fields → no opts");
    }

    #[test]
    fn sftp_opts_quote_the_key_and_hosts_file_argv_safely() {
        let app = app_with(StorageTarget {
            kind: StorageKind::Sftp {
                host: "backup.example.com".to_string(),
                port: 2222,
                user: "backup".to_string(),
                path: "/srv/backups".to_string(),
                key_file: Some("/home/me/key".to_string()),
                known_hosts: Some("/home/me/known hosts".to_string()),
            },
            repository: None,
            password_env: "PW".to_string(),
        });
        assert_eq!(
            restic_sftp_opts(&app).expect("opts"),
            // The single restic argv pair: the tokens extend the native
            // ssh invocation restic builds. Paths are single-quoted
            // (restic's own tokenizer; no shell executes anywhere);
            // BatchMode turns prompts into clean failures.
            vec![
                "-o".to_string(),
                "sftp.args=-o BatchMode=yes -i '/home/me/key' -o UserKnownHostsFile='/home/me/known hosts'"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn sftp_opts_cover_each_field_independently() {
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
        assert_eq!(
            restic_sftp_opts(&app).expect("opts"),
            vec![
                "-o".to_string(),
                "sftp.args=-o BatchMode=yes -i '/home/me/key'".to_string()
            ]
        );
        let app = app_with(StorageTarget {
            kind: StorageKind::Sftp {
                host: "backup.example.com".to_string(),
                port: 22,
                user: "backup".to_string(),
                path: "/srv/backups".to_string(),
                key_file: None,
                known_hosts: Some("/home/me/known_hosts".to_string()),
            },
            repository: None,
            password_env: "PW".to_string(),
        });
        assert_eq!(
            restic_sftp_opts(&app).expect("opts"),
            vec![
                "-o".to_string(),
                "sftp.args=-o BatchMode=yes -o UserKnownHostsFile='/home/me/known_hosts'"
                    .to_string()
            ]
        );
    }

    #[test]
    fn sftp_missing_key_file_is_a_named_configuration_error() {
        let app = app_with(StorageTarget {
            kind: StorageKind::Sftp {
                host: "backup.example.com".to_string(),
                port: 22,
                user: "backup".to_string(),
                path: "/srv/backups".to_string(),
                key_file: Some("/definitely/not/a/real/key".to_string()),
                known_hosts: None,
            },
            repository: None,
            password_env: "PW".to_string(),
        });
        let err = storage_envs(&app).expect_err("missing file");
        assert!(err.to_string().contains("key_file"));
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn sidecar_argv_uses_the_declared_image_and_defaults_to_alpine() {
        let args = sidecar_argv("data", "/tmp/stage", "alpine:3.20");
        assert_eq!(args[0], "run");
        assert_eq!(args[1], "--rm");
        assert_eq!(args[2], "-v");
        assert_eq!(args[3], "data:/vaultline-src:ro");
        assert_eq!(args[4], "-v");
        assert_eq!(args[5], "/tmp/stage:/vaultline-out:rw");
        assert_eq!(args[6], "alpine:3.20", "the declared image is used");
        assert_eq!(args[7], "cp");
        assert_eq!(args[8], "-a");
        assert_eq!(args[9], "/vaultline-src/.");
        assert_eq!(args[10], "/vaultline-out/");
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
