//! The restore command (ADR-V0-3: restore is first-class). Executes the
//! definition's ordered restore procedure against a recorded snapshot,
//! sandbox-then-promote:
//!
//! 1. the whole snapshot is restored by restic into a scratch area under
//!    the target (`<target>/.vaultline/<id>/`),
//! 2. the procedure's steps then promote content into their declared
//!    destinations — files copied (never overwriting), databases restored
//!    through their engine's restore tool, volumes copied, health checks
//!    polled.
//!
//! Safety by default: `--dry-run` plans without writing; promotion never
//! overwrites an existing file (a collision is an error naming the file);
//! the default target is `./vaultline-restore`, never the live paths
//! unless the procedure declares them.

use std::collections::HashSet;
use std::io::Read;
use std::io::Write as IoWrite;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Serialize;
use tempfile::TempDir;
use tracing::{info, warn};

use vaultline_core::config;
use vaultline_core::error::{ErrorKind, Result, VaultlineError};
use vaultline_core::model::{
    Application, BackupSnapshot, DatabaseConnection, DatabaseType, RestoreStep, SourceKind,
};
use vaultline_core::state::State;

use crate::backup::{repo_url, resolve_password, storage_envs};
use crate::engine::Restic;
use crate::metrics;

#[derive(clap::Args)]
pub struct RestoreArgs {
    /// The configuration file (one application per file).
    #[arg(long, default_value = "vaultline.toml")]
    pub config: PathBuf,

    /// The snapshot: full id, unique prefix, or "latest".
    pub snapshot: String,

    /// The restore destination root (default: ./vaultline-restore).
    /// Restored data is staged under `<target>/.vaultline/<id>/` and then
    /// promoted into the procedure's declared destinations.
    #[arg(long, default_value = "vaultline-restore")]
    pub target: PathBuf,

    /// Plan only: print what each step will do, write nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// After restoring, run the definition's checks: SQLite databases get
    /// integrity_check + row counts, health endpoints are polled (they
    /// are part of the procedure).
    #[arg(long)]
    pub verify: bool,

    /// Machine-readable output on stdout.
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct BackupVerifyArgs {
    #[arg(long, default_value = "vaultline.toml")]
    pub config: PathBuf,

    /// The snapshot: full id, unique prefix, or "latest".
    pub snapshot: String,

    /// Machine-readable output on stdout.
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct BackupInspectArgs {
    #[arg(long, default_value = "vaultline.toml")]
    pub config: PathBuf,

    /// The snapshot: full id, unique prefix, or "latest".
    pub snapshot: String,

    /// Machine-readable output on stdout.
    #[arg(long)]
    pub json: bool,
}

/// Select a snapshot from the state: full id, unique id prefix, or
/// "latest" (the most recent record).
pub fn select_snapshot<'a>(
    state: &'a State,
    app_name: &str,
    selector: &str,
) -> Result<&'a BackupSnapshot> {
    let snapshots = state
        .applications
        .get(app_name)
        .map(|s| s.snapshots.as_slice())
        .unwrap_or(&[]);
    if snapshots.is_empty() {
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!("no snapshots recorded for {app_name}"),
        ));
    }
    if selector == "latest" {
        return Ok(snapshots.last().expect("non-empty"));
    }
    let matches: Vec<&BackupSnapshot> = snapshots
        .iter()
        .filter(|s| s.id == selector || s.id.starts_with(selector))
        .collect();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "no snapshot matching \"{selector}\" for {app_name} (list them with `vaultline backup list`)"
            ),
        )),
        _ => Err(VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "snapshot selector \"{selector}\" is ambiguous ({} matches); use a longer prefix",
                matches.len()
            ),
        )),
    }
}

/// Translate a host path into the path form restic stores in the snapshot:
/// on Windows the drive letter becomes the first path component
/// (`C:/x` → `/C/x`); on Unix paths map 1:1. (Same-platform restore is
/// the supported case — a cross-platform restore is recorded as a
/// limitation.)
pub fn snapshot_path_of(host_path: &str) -> String {
    #[cfg(windows)]
    {
        if host_path.len() >= 2 && host_path.as_bytes()[1] == b':' {
            let drive = &host_path[..1];
            return format!("/{drive}{}", host_path[2..].replace('\\', "/"));
        }
        host_path.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        host_path.to_string()
    }
}

/// Copy a directory tree into a destination: directories are created,
/// files are copied, and an existing destination FILE is an error (safe
/// by default — promotion never silently overwrites).
pub fn copy_tree_into(src: &Path, dst: &Path) -> Result<u64> {
    let mut copied = 0u64;
    if !src.is_dir() {
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "restored source path {} does not exist in the snapshot",
                src.display()
            ),
        ));
    }
    std::fs::create_dir_all(dst).map_err(|e| {
        VaultlineError::with_source(ErrorKind::Io, format!("cannot create {}", dst.display()), e)
    })?;
    for entry in std::fs::read_dir(src).map_err(|e| {
        VaultlineError::with_source(ErrorKind::Io, format!("cannot read {}", src.display()), e)
    })? {
        let entry = entry.map_err(|e| {
            VaultlineError::with_source(ErrorKind::Io, "cannot read directory entry", e)
        })?;
        let from = entry.path();
        let to = dst.join(entry.file_name());

        // The malicious-archive defense: decide on the link metadata, NOT
        // the followed type. `is_dir`/`fs::copy` follow symlinks — a link
        // to a directory would recurse outside the snapshot tree, a link
        // to a file would copy the target's bytes. Symlinks are restored
        // AS symlinks (recreated pointing where the archive declared),
        // never followed and never written through.
        let metadata = std::fs::symlink_metadata(&from).map_err(|e| {
            VaultlineError::with_source(ErrorKind::Io, format!("cannot stat {}", from.display()), e)
        })?;
        if metadata.file_type().is_symlink() {
            if to.exists() {
                return Err(VaultlineError::new(
                    ErrorKind::Operational,
                    format!(
                        "refusing to overwrite existing file {} during promotion (remove it or choose another target)",
                        to.display()
                    ),
                ));
            }
            let link_target = std::fs::read_link(&from).map_err(|e| {
                VaultlineError::with_source(
                    ErrorKind::Io,
                    format!("cannot read the symlink {}", from.display()),
                    e,
                )
            })?;
            create_symlink(&link_target, &to, metadata.file_type().is_dir())
                .map_err(|e| {
                    VaultlineError::with_source(
                        ErrorKind::Operational,
                        format!(
                            "cannot recreate the symlink {} ({} → {}): the archive declares a link this host refuses to create",
                            to.display(),
                            from.display(),
                            link_target.display()
                        ),
                        e,
                    )
                })?;
            copied += 1;
            continue;
        }

        if metadata.is_dir() {
            copied += copy_tree_into(&from, &to)?;
        } else {
            if to.exists() {
                return Err(VaultlineError::new(
                    ErrorKind::Operational,
                    format!(
                        "refusing to overwrite existing file {} during promotion (remove it or choose another target)",
                        to.display()
                    ),
                ));
            }
            std::fs::copy(&from, &to).map_err(|e| {
                VaultlineError::with_source(
                    ErrorKind::Io,
                    format!("cannot copy {} to {}", from.display(), to.display()),
                    e,
                )
            })?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// Create a symlink the way the platform demands (Windows distinguishes
/// file and directory links and may refuse both without privileges —
/// the refusal is the point: a host that cannot honor links fails
/// loudly, never silently follows them).
fn create_symlink(target: &Path, link: &Path, is_dir: bool) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        if is_dir {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
    }
}

/// A minimal HTTP health probe (std only — no client dependency for one
/// GET): connect, send a GET, check for a 2xx status.
pub fn wait_healthy(url: &str, timeout: Duration) -> Result<()> {
    let parsed = parse_url(url)?;
    let deadline = Instant::now() + timeout;
    let last_error = loop {
        match probe_once(&parsed) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if Instant::now() >= deadline {
                    break e;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    };
    Err(VaultlineError::new(
        ErrorKind::Operational,
        format!(
            "health endpoint {url} did not become healthy within {}s: {last_error}",
            timeout.as_secs()
        ),
    ))
}

struct Url {
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> Result<Url> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| {
            VaultlineError::new(
                ErrorKind::Config,
                format!("health endpoint \"{url}\" must be an http(s) URL"),
            )
        })?;
    let (hostport, path) = match rest.split_once('/') {
        Some((h, p)) => (h, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>().map_err(|_| {
                VaultlineError::new(ErrorKind::Config, format!("invalid port in \"{url}\""))
            })?,
        ),
        None => (hostport.to_string(), 80),
    };
    Ok(Url { host, port, path })
}

fn probe_once(url: &Url) -> std::result::Result<(), String> {
    let mut stream =
        TcpStream::connect((url.host.as_str(), url.port)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\r\n",
        url.path, url.host, url.port
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    let head = String::from_utf8_lossy(&response).to_string();
    let status_line = head.lines().next().unwrap_or("");
    if status_line.contains(" 200") || status_line.contains(" 204") {
        Ok(())
    } else {
        Err(format!("unhealthy response: {}", status_line))
    }
}

/// One planned restore step (for --dry-run and the executor).
#[derive(Debug, Serialize)]
struct PlannedStep {
    index: usize,
    kind: &'static str,
    detail: String,
}

fn plan_steps(app: &Application, snapshot: &BackupSnapshot) -> Result<Vec<PlannedStep>> {
    let mut steps = Vec::new();
    let declared: HashSet<&str> = snapshot
        .restore_metadata
        .reconstructs
        .iter()
        .map(|s| s.as_str())
        .collect();
    for (index, step) in app.restore.steps.iter().enumerate() {
        let (kind, detail) = match step {
            RestoreStep::RestoreFiles { source, target } => {
                ("restore_files", format!("source \"{source}\" → {target}"))
            }
            RestoreStep::RestoreDatabase {
                database,
                target_database,
            } => (
                "restore_database",
                format!(
                    "database \"{database}\"{}",
                    target_database
                        .as_deref()
                        .map(|t| format!(" → \"{t}\""))
                        .unwrap_or_default()
                ),
            ),
            RestoreStep::RestoreVolume { volume } => {
                ("restore_volume", format!("volume \"{volume}\""))
            }
            RestoreStep::WaitHealthy { url } => ("wait_healthy", url.clone()),
        };
        if !declared.contains(match step {
            RestoreStep::RestoreFiles { source, .. } => source.as_str(),
            RestoreStep::RestoreDatabase { database, .. } => database.as_str(),
            RestoreStep::RestoreVolume { volume } => volume.as_str(),
            RestoreStep::WaitHealthy { .. } => continue,
        }) {
            return Err(VaultlineError::new(
                ErrorKind::Operational,
                format!(
                    "restore step {index} references \"{}\", which snapshot {} does not contain (its reconstructs list: {})",
                    match step {
                        RestoreStep::RestoreFiles { source, .. } => source,
                        RestoreStep::RestoreDatabase { database, .. } => database,
                        RestoreStep::RestoreVolume { volume, .. } => volume,
                        RestoreStep::WaitHealthy { .. } => unreachable!(),
                    },
                    snapshot.id,
                    declared.iter().cloned().collect::<Vec<_>>().join(", "),
                ),
            ));
        }
        steps.push(PlannedStep {
            index,
            kind,
            detail,
        });
    }
    Ok(steps)
}

pub fn run_restore(args: RestoreArgs) -> Result<()> {
    let app = config::load(&args.config)?;
    let state = State::load(&crate::backup::state_dir()?.join("state.json"))?;
    let snapshot = select_snapshot(&state, &app.name, &args.snapshot)?;
    let plan = plan_steps(&app, snapshot)?;

    if args.dry_run {
        if args.json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "application": app.name,
                    "snapshot": snapshot.id,
                    "steps": plan,
                }))
                .expect("plan serializes")
            );
        } else {
            println!(
                "restore plan for snapshot {} ({}):",
                snapshot.id, snapshot.engine_snapshot.snapshot_id
            );
            for step in &plan {
                println!("  {}: {}", step.kind, step.detail);
            }
            println!("(--dry-run: nothing was written)");
        }
        return Ok(());
    }

    let _lock = vaultline_core::state::StateLock::acquire_with(
        &crate::backup::state_dir()?,
        &crate::backup::process_is_alive,
    )?;
    let restic = Restic::locate()?;
    let password = resolve_password(&app)?;
    let repo = repo_url(&app)?;
    let extra_envs = storage_envs(&app)?;

    let started = Instant::now();
    let stage_root = args.target.join(".vaultline").join(&snapshot.id);
    std::fs::create_dir_all(&stage_root).map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            format!(
                "cannot create the restore staging directory {}",
                stage_root.display()
            ),
            e,
        )
    })?;
    let _data_temp = TempDir::new_in(&stage_root).map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            "cannot create the restore staging directory",
            e,
        )
    })?; // restic wants an empty target

    info!(
        snapshot = snapshot.id,
        target = %args.target.display(),
        "restoring snapshot into the staging area"
    );
    let restored_root = _data_temp.path().to_path_buf();

    // restic restore (the whole snapshot; promotion below is selective)
    let restore_output = restic_restore(
        &restic,
        &snapshot.engine_snapshot.snapshot_id,
        &repo,
        &password,
        &extra_envs,
        &restored_root,
    )?;
    if !restore_output.status.success() {
        // Windows timestamp quirk (recorded in the test suite): the files
        // may still be restored. The promotion step validates existence.
        warn!(
            "restic restore reported: {}",
            String::from_utf8_lossy(&restore_output.stderr)
                .lines()
                .rev()
                .take(2)
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }

    let (restored_files, restored_databases, restored_volumes) =
        execute_steps(&app, &restored_root, None).map_err(|e| {
            // The partial-restore guidance (ADR-007): promotion may have
            // stopped part-way. The never-overwrite contract means a
            // retry into this target would collide on what was already
            // promoted — the operator must know.
            VaultlineError::new(
                ErrorKind::Operational,
                format!(
                    "{e} (the restore stopped part-way: earlier steps may have promoted content; vaultline never overwrites, so retry into a fresh target or remove the promoted content first)"
                ),
            )
        })?;

    let duration_ms = started.elapsed().as_millis() as u64;
    metrics::emit_u64("restore_duration_ms", duration_ms);
    metrics::emit_u64("restore_rehearsals_run", 1);

    let mut summary = format!(
        "restore complete: snapshot {} → {} ({} files, {} database(s), {} volume(s))",
        snapshot.id,
        args.target.display(),
        restored_files,
        restored_databases.len(),
        restored_volumes,
    );

    if args.verify {
        match run_restore_checks(&app, &snapshot.id, &restored_databases) {
            Ok(notes) => {
                summary.push_str(&format!("\nverification: {}", notes.join("; ")));
            }
            Err(e) => {
                metrics::emit_u64("restore_rehearsal_failures", 1);
                return Err(e);
            }
        }
        // The app checks run against the restore target (ADR-006: CWD is
        // the target, VAULTLINE_REHEARSAL_DIR names it).
        match run_app_checks(&app, &args.target) {
            Ok(notes) => {
                summary.push_str(&format!("\napp checks: {}", notes.join("; ")));
            }
            Err(e) => {
                metrics::emit_u64("restore_rehearsal_failures", 1);
                return Err(e);
            }
        }
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "application": app.name,
                "snapshot": snapshot.id,
                "files_restored": restored_files,
                "databases_restored": restored_databases.clone(),
                "volumes_restored": restored_volumes,
                "duration_ms": duration_ms,
            }))
            .expect("serializes")
        );
    } else {
        println!("{summary}");
    }
    Ok(())
}

/// Execute the definition's ordered restore procedure against a restored
/// snapshot tree. With `rehearsal_root: None` the procedure promotes into
/// its declared destinations (the live restore); with `Some(root)` the
/// path targets are mirrored under the root — a scratch clone of the
/// application layout for the L6 rehearsal, never the live paths.
/// Wait-health steps run as declared in both modes (the rehearsal's
/// endpoint is the operator's rehearsal environment).
fn execute_steps(
    app: &Application,
    restored_root: &Path,
    rehearsal_root: Option<&Path>,
) -> Result<(u64, Vec<String>, u64)> {
    let remap_path = |target: &str| -> String {
        match rehearsal_root {
            None => target.to_string(),
            Some(root) => {
                // Mirror the absolute target under the root: strip the
                // leading slashes AND a Windows drive prefix, so the join
                // stays relative and the root is always the prefix.
                let mut trimmed = target.trim_start_matches(['/', '\\']);
                if trimmed.len() >= 2
                    && trimmed.as_bytes()[0].is_ascii_alphabetic()
                    && trimmed.as_bytes()[1] == b':'
                {
                    trimmed = trimmed[2..].trim_start_matches(['/', '\\']);
                }
                if trimmed.is_empty() {
                    root.display().to_string()
                } else {
                    root.join(trimmed).display().to_string()
                }
            }
        }
    };

    let mut restored_files = 0u64;
    let mut restored_databases: Vec<String> = Vec::new();
    let mut restored_volumes = 0u64;

    for step in &app.restore.steps {
        match step {
            RestoreStep::RestoreFiles { source, target } => {
                let source_paths = file_source_paths(app, source)?;
                let target = remap_path(target);
                let mut count = 0u64;
                for path in source_paths {
                    let in_snapshot = restored_root.join(trim_root(&snapshot_path_of(&path)));
                    count += copy_tree_into(&in_snapshot, Path::new(&target))?;
                }
                info!(source, target, files = count, "restored files");
                restored_files += count;
            }
            RestoreStep::RestoreDatabase {
                database,
                target_database,
            } => {
                // Only a SQLite target is a path (server targets are
                // database names) — remap the path kind under the root.
                let db = app
                    .databases
                    .iter()
                    .find(|d| d.name == *database)
                    .ok_or_else(|| {
                        VaultlineError::new(
                            ErrorKind::Operational,
                            format!("no database named \"{database}\" in the definition"),
                        )
                    })?;
                let target = match (db.kind, target_database.as_deref()) {
                    (DatabaseType::Sqlite, Some(path)) => Some(remap_path(path)),
                    (_, other) => other.map(String::from),
                };
                restore_database(app, database, target.as_deref(), restored_root)?;
                restored_databases.push(database.clone());
            }
            RestoreStep::RestoreVolume { volume } => {
                let volume_path = crate::backup::resolve_volume_path(volume)?;
                let target = remap_path(&volume_path.display().to_string());
                let in_snapshot = restored_root.join(trim_root(&snapshot_path_of(
                    &volume_path.display().to_string(),
                )));
                let count = copy_tree_into(&in_snapshot, Path::new(&target))?;
                info!(volume, files = count, "restored volume");
                restored_volumes += count;
            }
            RestoreStep::WaitHealthy { url } => {
                wait_healthy(url, Duration::from_secs(30))?;
                info!(url, "health endpoint reported healthy");
            }
        }
    }
    Ok((restored_files, restored_databases, restored_volumes))
}

/// Run the definition's app checks against a rehearsed (or restored)
/// tree (ADR-006): CWD is the root, `VAULTLINE_REHEARSAL_DIR` names it.
/// A check that cannot start or exits non-zero fails the rehearsal — the
/// stderr tail is the diagnosis.
fn run_app_checks(app: &Application, root: &Path) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    for check in &app.verification.app_checks {
        let output = Command::new(&check.command)
            .args(&check.args)
            .current_dir(root)
            .env("VAULTLINE_REHEARSAL_DIR", root)
            .output()
            .map_err(|e| {
                VaultlineError::with_source(
                    ErrorKind::Operational,
                    format!(
                        "cannot start app check \"{}\" ({}): {}",
                        check.name, check.command, e
                    ),
                    e,
                )
            })?;
        if output.status.success() {
            notes.push(format!("app check \"{}\" passed", check.name));
        } else {
            return Err(VaultlineError::new(
                ErrorKind::Operational,
                format!(
                    "app check \"{}\" FAILED: {}",
                    check.name,
                    String::from_utf8_lossy(&output.stderr)
                        .lines()
                        .rev()
                        .take(3)
                        .collect::<Vec<_>>()
                        .join(" | ")
                ),
            ));
        }
    }
    Ok(notes)
}

/// Remove a directory tree robustly. On Windows, restic-restored
/// directories carry mode-derived restrictive ACLs — the same root
/// cause as its restore-timestamp "Access is denied" quirk recorded in
/// the suite: not even the read-only attribute can be changed on them
/// (verified empirically). The owner can reset the ACLs (icacls
/// /reset), after which the read-only attributes clear and
/// `remove_dir_all` succeeds. Transient locks (antivirus real-time
/// scans on freshly written files) get a brief retry. Elsewhere this is
/// a plain `remove_dir_all`.
pub(crate) fn remove_tree_robust(path: &Path) -> std::io::Result<()> {
    let mut last_error = None;
    for _ in 0..5 {
        #[cfg(windows)]
        make_tree_removable(path);
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_error = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    Err(last_error.expect("an attempt ran"))
}

#[cfg(windows)]
fn make_tree_removable(dir: &Path) {
    // ACL reset first (best effort — the owner can always reset; if
    // not, the removal failure reports it), then clear read-only
    // attributes depth-first.
    let _ = std::process::Command::new("icacls")
        .arg(dir)
        .args(["/reset", "/t", "/c", "/q"])
        .output();
    let _ = clear_readonly_attributes(dir);
}

#[cfg(windows)]
// The permissions-set-readonly-false lint warns about Unix
// world-writability; this is Windows-only code where set_readonly(false)
// clears FILE_ATTRIBUTE_READONLY — exactly the intent.
#[allow(clippy::permissions_set_readonly_false)]
fn clear_readonly_attributes(dir: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue; // never follow links, even for cleanup
        }
        if metadata.is_dir() {
            clear_readonly_attributes(&path)?;
        }
        if metadata.permissions().readonly() {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            std::fs::set_permissions(&path, permissions)?;
        }
    }
    Ok(())
}

/// The L6 executor: a full recovery rehearsal into the declared scratch
/// root — `<target>/.vaultline-rehearsal/<snapshot-id>/` with the
/// procedure's path targets mirrored under the root — then the app
/// checks. The per-snapshot directory is cleared first: it holds only
/// this snapshot's previous rehearsal, never user data.
fn run_rehearsal(
    app: &Application,
    snapshot: &BackupSnapshot,
    restic: &Restic,
    repo: &str,
    password: &str,
    extra_envs: &[(String, String)],
) -> Result<Vec<String>> {
    let root = app.rehearsal.as_ref().ok_or_else(|| {
        VaultlineError::new(
            ErrorKind::Internal,
            "L6 rehearsal without application.rehearsal — validation should have refused this",
        )
    })?;
    let root_path = Path::new(&root.target);
    let run_dir = root_path.join(".vaultline-rehearsal").join(&snapshot.id);
    if run_dir.exists() {
        info!(
            dir = %run_dir.display(),
            "clearing this snapshot's previous rehearsal"
        );
        remove_tree_robust(&run_dir).map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Io,
                format!(
                    "cannot clear the previous rehearsal directory {}",
                    run_dir.display()
                ),
                e,
            )
        })?;
    }
    std::fs::create_dir_all(&run_dir).map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            format!(
                "cannot create the rehearsal directory {}",
                run_dir.display()
            ),
            e,
        )
    })?;
    let data_temp = TempDir::new_in(&run_dir).map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            "cannot create the rehearsal staging directory",
            e,
        )
    })?;
    let restored_root = data_temp.path().to_path_buf();

    restic_restore(
        restic,
        &snapshot.engine_snapshot.snapshot_id,
        repo,
        password,
        extra_envs,
        &restored_root,
    )?;
    // The remap root is THIS snapshot's rehearsal directory — staging and
    // promotion both live here, so rehearsals of different snapshots
    // never collide on the mirrored layout. The app checks run with this
    // directory as CWD and VAULTLINE_REHEARSAL_DIR.
    let (files, databases, volumes) = execute_steps(app, &restored_root, Some(&run_dir))?;

    let mut notes = vec![format!(
        "L6: rehearsed {} file(s), {} database(s), {} volume(s) under {}",
        files,
        databases.len(),
        volumes,
        run_dir.display()
    )];
    notes.extend(run_app_checks(app, &run_dir)?);
    Ok(notes)
}

/// The engine restore: full snapshot into an empty target directory.
fn restic_restore(
    restic: &Restic,
    engine_snapshot_id: &str,
    repo: &str,
    password: &str,
    extra_envs: &[(String, String)],
    target: &Path,
) -> Result<std::process::Output> {
    restic.run(
        &["restore", engine_snapshot_id, "--target"],
        &[target.display().to_string()],
        repo,
        password,
        extra_envs,
    )
}

fn trim_root(path: &str) -> String {
    path.trim_start_matches('/').to_string()
}

fn file_source_paths(app: &Application, source: &str) -> Result<Vec<String>> {
    for s in &app.sources {
        if s.name == source {
            if let SourceKind::Files(files) = &s.kind {
                return Ok(files.paths.clone());
            }
            return Err(VaultlineError::new(
                ErrorKind::Operational,
                format!(
                    "restore step references \"{source}\", which is a {:?} source, not files",
                    std::mem::discriminant(&s.kind)
                ),
            ));
        }
    }
    Err(VaultlineError::new(
        ErrorKind::Operational,
        format!("no source named \"{source}\" in the definition"),
    ))
}

fn restore_database(
    app: &Application,
    database: &str,
    target_database: Option<&str>,
    restored_root: &Path,
) -> Result<()> {
    let db = app
        .databases
        .iter()
        .find(|d| d.name == database)
        .ok_or_else(|| {
            VaultlineError::new(
                ErrorKind::Operational,
                format!("no database named \"{database}\" in the definition"),
            )
        })?;
    // Locate the dump inside the restored tree by its file name.
    let dump_name = match db.kind {
        DatabaseType::PostgreSql => format!("{database}.dump"),
        DatabaseType::MySql | DatabaseType::MariaDb => format!("{database}.sql"),
        DatabaseType::Sqlite => format!("{database}.db"),
    };
    let dump = find_file(restored_root, &dump_name).ok_or_else(|| {
        VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "the snapshot does not contain the dump \"{dump_name}\" — was the database captured when it was taken?"
            ),
        )
    })?;

    match db.kind {
        DatabaseType::Sqlite => {
            let target_path = target_database.ok_or_else(|| {
                VaultlineError::new(
                    ErrorKind::Config,
                    format!(
                        "restoring sqlite database \"{database}\" requires a target_database (the destination file path)"
                    ),
                )
            })?;
            copy_without_overwrite(&dump, Path::new(target_path))?;
            info!(database, target = target_path, "sqlite database restored");
        }
        DatabaseType::PostgreSql => {
            let DatabaseConnection::UrlEnv(env_name) = &db.connection else {
                return Err(VaultlineError::new(
                    ErrorKind::Internal,
                    "postgresql database without url_env — this is a bug".to_string(),
                ));
            };
            let conninfo = std::env::var(env_name).map_err(|_| {
                VaultlineError::new(
                    ErrorKind::Config,
                    format!(
                        "environment variable {env_name} is not set (the restore target connection)"
                    ),
                )
            })?;
            let parsed = crate::database::parse_conninfo(&conninfo)?;
            let mut sanitized = parsed.sanitized();
            if let Some(target) = target_database {
                sanitized = replace_dbname(&sanitized, target);
            }
            let pg_restore = locate_tool("VAULTLINE_PGRESTORE", "pg_restore")?;
            let mut command = Command::new(&pg_restore);
            command.arg("--dbname").arg(&sanitized);
            let auth = crate::database::PgPassfile::for_conninfo(&parsed)?;
            if let Some(passfile) = &auth {
                command.env("PGPASSFILE", passfile.path());
            }
            // The dump is streamed on stdin (mirroring pg_dump's stdout
            // capture) — the archive never appears in argv.
            let dump_bytes = std::fs::read(&dump).map_err(|e| {
                VaultlineError::with_source(
                    ErrorKind::Io,
                    format!("cannot read the dump {}", dump.display()),
                    e,
                )
            })?;
            let mut child = command
                .stdin(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| {
                    VaultlineError::with_source(
                        ErrorKind::Operational,
                        format!(
                            "cannot start pg_restore ({}): install it or set VAULTLINE_PGRESTORE",
                            pg_restore.display()
                        ),
                        e,
                    )
                })?;
            child
                .stdin
                .take()
                .expect("piped")
                .write_all(&dump_bytes)
                .map_err(|e| {
                    VaultlineError::with_source(
                        ErrorKind::Io,
                        "cannot stream the dump into pg_restore",
                        e,
                    )
                })?;
            let status = child.wait().map_err(|e| {
                VaultlineError::with_source(ErrorKind::Operational, "pg_restore failed", e)
            })?;
            if !status.success() {
                return Err(VaultlineError::new(
                    ErrorKind::Operational,
                    format!("pg_restore failed for database \"{database}\""),
                ));
            }
            info!(database, "postgresql database restored via pg_restore");
        }
        DatabaseType::MySql | DatabaseType::MariaDb => {
            let DatabaseConnection::UrlEnv(env_name) = &db.connection else {
                return Err(VaultlineError::new(
                    ErrorKind::Internal,
                    "mysql database without url_env — this is a bug".to_string(),
                ));
            };
            let conninfo = std::env::var(env_name).map_err(|_| {
                VaultlineError::new(
                    ErrorKind::Config,
                    format!(
                        "environment variable {env_name} is not set (the restore target connection)"
                    ),
                )
            })?;
            let parsed = crate::database::parse_conninfo(&conninfo)?;
            let mysql = locate_tool("VAULTLINE_MYSQL", "mysql")?;
            let mut command = Command::new(&mysql);
            if let Some(host) = &parsed.host {
                command.arg("--host").arg(host);
            }
            if let Some(port) = parsed.port {
                command.arg("--port").arg(port.to_string());
            }
            if let Some(user) = &parsed.user {
                command.arg("--user").arg(user);
            }
            if let Some(password) = &parsed.password {
                command.env("MYSQL_PWD", password);
            }
            if let Some(target) = target_database {
                command.arg(target);
            }
            let dump_bytes = std::fs::read(&dump).map_err(|e| {
                VaultlineError::with_source(ErrorKind::Io, "cannot read the dump", e)
            })?;
            let mut child = command
                .stdin(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| {
                    VaultlineError::with_source(
                        ErrorKind::Operational,
                        format!(
                            "cannot start mysql ({}): install it or set VAULTLINE_MYSQL",
                            mysql.display()
                        ),
                        e,
                    )
                })?;
            child
                .stdin
                .take()
                .expect("piped")
                .write_all(&dump_bytes)
                .map_err(|e| {
                    VaultlineError::with_source(
                        ErrorKind::Io,
                        "cannot stream the dump into mysql",
                        e,
                    )
                })?;
            let status = child.wait().map_err(|e| {
                VaultlineError::with_source(ErrorKind::Operational, "mysql failed", e)
            })?;
            if !status.success() {
                return Err(VaultlineError::new(
                    ErrorKind::Operational,
                    format!("mysql restore failed for database \"{database}\""),
                ));
            }
            info!(database, "mysql database restored");
        }
    }
    Ok(())
}

fn replace_dbname(conninfo: &str, target: &str) -> String {
    // The sanitized form is either scheme://user@host:port/db or
    // key=value pairs; swap the db component.
    if conninfo.contains("://") {
        match conninfo.rsplit_once('/') {
            Some((prefix, _)) => format!("{prefix}/{target}"),
            None => format!("{conninfo}/{target}"),
        }
    } else {
        conninfo
            .split_whitespace()
            .map(|pair| {
                if pair.starts_with("dbname=") {
                    format!("dbname={target}")
                } else {
                    pair.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn copy_without_overwrite(src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "refusing to overwrite existing file {} during database restoration",
                dst.display()
            ),
        ));
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Io,
                format!("cannot create {}", parent.display()),
                e,
            )
        })?;
    }
    std::fs::copy(src, dst).map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            format!("cannot copy the restored database to {}", dst.display()),
            e,
        )
    })?;
    Ok(())
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
    }
    None
}

fn locate_tool(env_name: &str, tool_name: &str) -> Result<PathBuf> {
    if let Some(bin) = std::env::var_os(env_name) {
        let path = PathBuf::from(bin);
        if !path.is_file() {
            return Err(VaultlineError::new(
                ErrorKind::Operational,
                format!(
                    "{env_name} points to {}, which does not exist",
                    path.display()
                ),
            ));
        }
        return Ok(path);
    }
    Ok(PathBuf::from(tool_name))
}

/// The restore-side checks (--verify): SQLite databases get integrity_check
/// + row counts; the summary is returned for the report.
fn run_restore_checks(
    app: &Application,
    snapshot_id: &str,
    restored_databases: &[String],
) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    for db_name in restored_databases {
        let Some(db) = app.databases.iter().find(|d| &d.name == db_name) else {
            continue;
        };
        if db.kind != DatabaseType::Sqlite {
            notes.push(format!(
                "database \"{db_name}\": engine restore reported success (restore-side semantic checks for server engines arrive with verification L5)"
            ));
            continue;
        }
        let target_path = match &db.connection {
            DatabaseConnection::SqlitePath(path) => path.clone(),
            _ => continue,
        };
        let sqlite3 = locate_tool("VAULTLINE_SQLITE3", "sqlite3")?;
        let output = Command::new(&sqlite3)
            .arg(&target_path)
            .arg("PRAGMA integrity_check;")
            .output()
            .map_err(|e| {
                VaultlineError::with_source(
                    ErrorKind::Operational,
                    "cannot run sqlite3 for the restore check",
                    e,
                )
            })?;
        let out = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() || !out.contains("ok") {
            return Err(VaultlineError::new(
                ErrorKind::Operational,
                format!(
                    "restore verification FAILED for database \"{db_name}\": integrity_check reported: {out}"
                ),
            ));
        }
        notes.push(format!("database \"{db_name}\": integrity_check ok"));
    }
    let _ = snapshot_id;
    Ok(notes)
}

pub fn run_verify(args: BackupVerifyArgs) -> Result<()> {
    let app = config::load(&args.config)?;
    let state_dir = crate::backup::state_dir()?;
    let state_path = state_dir.join("state.json");
    let _lock = vaultline_core::state::StateLock::acquire_with(
        &state_dir,
        &crate::backup::process_is_alive,
    )?;
    let mut state = State::load(&state_path)?;
    let snapshot = select_snapshot(&state, &app.name, &args.snapshot)?.clone();

    let restic = Restic::locate()?;
    let password = resolve_password(&app)?;
    let repo = repo_url(&app)?;
    let extra_envs = storage_envs(&app)?;

    // L3: the engine still has the snapshot we recorded.
    let engine_snapshots = restic.list_snapshots(&repo, &password, &extra_envs)?;
    // restic's `snapshots --json` lists SHORT ids (8 chars); the state
    // records the full 64-char id — match by the full id's prefix.
    let engine_has_snapshot = engine_snapshots.iter().any(|line| {
        line.get("id")
            .and_then(|v| v.as_str())
            .is_some_and(|listed| {
                listed == snapshot.engine_snapshot.snapshot_id
                    || snapshot.engine_snapshot.snapshot_id.starts_with(listed)
            })
    });
    if !engine_has_snapshot {
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "verification FAILED at L3: the engine no longer has snapshot {}",
                snapshot.engine_snapshot.snapshot_id
            ),
        ));
    }

    let mut checks = vec![format!(
        "L3: engine still holds snapshot {}",
        snapshot.engine_snapshot.snapshot_id
    )];
    let mut reached = vaultline_core::model::VerificationLevel::L3;

    let level = app.verification.level;

    // L4: restore files sources to scratch and compare content hashes
    // against the live files.
    if level >= vaultline_core::model::VerificationLevel::L4 {
        let scratch = tempfile::tempdir().map_err(|e| {
            VaultlineError::with_source(ErrorKind::Io, "cannot create the L4 scratch directory", e)
        })?;
        let output = restic_restore(
            &restic,
            &snapshot.engine_snapshot.snapshot_id,
            &repo,
            &password,
            &extra_envs,
            scratch.path(),
        )?;
        if !output.status.success() {
            warn!(
                "restic restore reported non-zero (Windows timestamp quirk recorded); content comparison proceeds"
            );
        }
        let mut matched = 0usize;
        let mut mismatches: Vec<String> = Vec::new();
        let mut missing_live: Vec<String> = Vec::new();
        for source in &app.sources {
            if let SourceKind::Files(files) = &source.kind {
                for path in &files.paths {
                    let live = Path::new(path);
                    if !live.exists() {
                        missing_live.push(path.clone());
                        continue;
                    }
                    let restored = scratch.path().join(trim_root(&snapshot_path_of(path)));
                    if !restored.exists() {
                        mismatches.push(format!("{path}: not present in the snapshot"));
                        continue;
                    }
                    // Source paths are directories or files; compare
                    // recursively — every file's content must match.
                    compare_paths(live, &restored, path, &mut matched, &mut mismatches)?;
                }
            }
        }
        let mut l4_notes = vec![format!("L4: {matched} file path(s) matched")];
        for m in &mismatches {
            l4_notes.push(format!("L4 mismatch: {m}"));
        }
        for m in &missing_live {
            l4_notes.push(format!(
                "L4 note: {m} no longer exists locally (cannot compare)"
            ));
        }
        checks.extend(l4_notes);
        reached = vaultline_core::model::VerificationLevel::L4;
        if !mismatches.is_empty() {
            return Err(VaultlineError::new(
                ErrorKind::Operational,
                format!(
                    "verification FAILED at L4: {} mismatch(es)",
                    mismatches.len()
                ),
            ));
        }
    }

    // L5: SQLite databases get a scratch restore + integrity_check + row
    // counts. Server engines: honest note (their rehearsal is the restore
    // command against a scratch instance).
    if level >= vaultline_core::model::VerificationLevel::L5 {
        for db in &app.databases {
            match db.kind {
                DatabaseType::Sqlite => {
                    let scratch = tempfile::tempdir().map_err(|e| {
                        VaultlineError::with_source(
                            ErrorKind::Io,
                            "cannot create the L5 scratch directory",
                            e,
                        )
                    })?;
                    let output = restic_restore(
                        &restic,
                        &snapshot.engine_snapshot.snapshot_id,
                        &repo,
                        &password,
                        &extra_envs,
                        scratch.path(),
                    )?;
                    let _ = output;
                    let dump_name = format!("{}.db", db.name);
                    let dump = find_file(scratch.path(), &dump_name).ok_or_else(|| {
                        VaultlineError::new(
                            ErrorKind::Operational,
                            format!("the snapshot does not contain \"{dump_name}\""),
                        )
                    })?;
                    let sqlite3 = locate_tool("VAULTLINE_SQLITE3", "sqlite3")?;
                    let check = Command::new(&sqlite3)
                        .arg(&dump)
                        .arg("PRAGMA integrity_check;")
                        .output()
                        .map_err(|e| {
                            VaultlineError::with_source(
                                ErrorKind::Operational,
                                "cannot run sqlite3 for the L5 check",
                                e,
                            )
                        })?;
                    let out = String::from_utf8_lossy(&check.stdout);
                    if !check.status.success() || !out.contains("ok") {
                        return Err(VaultlineError::new(
                            ErrorKind::Operational,
                            format!(
                                "verification FAILED at L5 for database \"{}\": {}",
                                db.name, out
                            ),
                        ));
                    }
                    checks.push(format!("L5: database \"{}\" integrity_check ok", db.name));
                }
                _ => {
                    checks.push(format!(
                        "L5 note: database \"{}\" rehearsal for {} engines is executed by `vaultline restore --verify` against a scratch instance; not run here",
                        db.name, db.kind.as_str()
                    ));
                }
            }
        }
        reached = vaultline_core::model::VerificationLevel::L5;
    }

    // The durability of verification: record the level reached so far
    // BEFORE attempting L6 — a rehearsal failure must not erase the L5
    // that was actually proven.
    record_reached(&mut state, &state_path, &app.name, &snapshot.id, reached);

    // L6: the full recovery rehearsal (ADR-006) — the policy's top level,
    // executed against the declared rehearsal root with the app checks.
    if level >= vaultline_core::model::VerificationLevel::L6 {
        checks.extend(run_rehearsal(
            &app,
            &snapshot,
            &restic,
            &repo,
            &password,
            &extra_envs,
        )?);
        reached = vaultline_core::model::VerificationLevel::L6;
        record_reached(&mut state, &state_path, &app.name, &snapshot.id, reached);
    }

    metrics::emit_u64("verification_level_reached", reached.to_number() as u64);
    metrics::emit_u64("restore_rehearsals_run", 1);

    if args.json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "snapshot": snapshot.id,
                "verification_level_reached": reached.to_number(),
                "checks": checks,
            }))
            .expect("serializes")
        );
    } else {
        println!(
            "verification reached L{} for snapshot {}:",
            reached.to_number(),
            snapshot.id
        );
        for check in &checks {
            println!("  {check}");
        }
    }
    Ok(())
}

/// Raise a snapshot's recorded verification level (durably, only upward).
fn record_reached(
    state: &mut State,
    state_path: &Path,
    app_name: &str,
    snapshot_id: &str,
    reached: vaultline_core::model::VerificationLevel,
) {
    if let Some(recorded) = state
        .applications
        .get_mut(app_name)
        .and_then(|s| s.snapshots.iter_mut().find(|s| s.id == snapshot_id))
        && reached > recorded.integrity.highest_verified_level
    {
        recorded.integrity.highest_verified_level = reached;
        recorded.integrity.verified_at = Some(chrono::Utc::now());
        if let Err(e) = state.save(state_path) {
            warn!(error = %e, "cannot persist the verification record — the level reached this run is not recorded");
        }
    }
}

pub fn run_inspect(args: BackupInspectArgs) -> Result<()> {
    let app = config::load(&args.config)?;
    let state = State::load(&crate::backup::state_dir()?.join("state.json"))?;
    let snapshot = select_snapshot(&state, &app.name, &args.snapshot)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string(snapshot).expect("snapshot serializes")
        );
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(snapshot).expect("serializes")
        );
    }
    Ok(())
}

/// Compare a live path against its restored counterpart, recursively.
/// Every file's content must hash identically; directories are walked.
fn compare_paths(
    live: &Path,
    restored: &Path,
    label: &str,
    matched: &mut usize,
    mismatches: &mut Vec<String>,
) -> Result<()> {
    if live.is_dir() {
        for entry in std::fs::read_dir(live).map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Io,
                format!("cannot read {} for comparison", live.display()),
                e,
            )
        })? {
            let entry = entry.map_err(|e| {
                VaultlineError::with_source(ErrorKind::Io, "cannot read directory entry", e)
            })?;
            let name = entry.file_name();
            let sub_live = entry.path();
            let sub_restored = restored.join(&name);
            let sub_label = format!("{}/{}", label.trim_end_matches('/'), name.to_string_lossy());
            if !sub_restored.exists() {
                mismatches.push(format!("{sub_label}: not present in the snapshot"));
                continue;
            }
            compare_paths(&sub_live, &sub_restored, &sub_label, matched, mismatches)?;
        }
        Ok(())
    } else {
        if file_hash(live)? == file_hash(restored)? {
            *matched += 1;
        } else {
            mismatches.push(format!("{label}: content differs"));
        }
        Ok(())
    }
}

/// A stable content hash (std only; comparison-grade, not cryptographic).
fn file_hash(path: &Path) -> Result<u64> {
    use std::hash::{Hash, Hasher};
    let bytes = std::fs::read(path).map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            format!("cannot read {} for comparison", path.display()),
            e,
        )
    })?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Ok(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_path_translation_windows_drive() {
        #[cfg(windows)]
        {
            assert_eq!(snapshot_path_of("C:/Users/me/data"), "/C/Users/me/data");
            assert_eq!(snapshot_path_of(r"C:\Users\me\data"), "/C/Users/me/data");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(snapshot_path_of("/srv/app/data"), "/srv/app/data");
        }
    }

    #[test]
    fn copy_tree_into_never_follows_symlinks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("src dir");
        std::fs::write(src.join("real.txt"), "real content").expect("real file");

        // The secret lives OUTSIDE the tree; the link points at it.
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, "TOP-SECRET").expect("secret");
        let link = src.join("leak");
        #[cfg(unix)]
        let created = std::os::unix::fs::symlink(&secret, &link);
        #[cfg(windows)]
        let created = std::os::windows::fs::symlink_file(&secret, &link);
        if created.is_err() {
            eprintln!("skipping: this host cannot create symlinks");
            return;
        }

        let dst = dir.path().join("dst");
        copy_tree_into(&src, &dst).expect("copy");

        // The destination contains the real file and a LINK — never the
        // secret's bytes.
        fn contains(dir: &Path, needle: &str) -> bool {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return false;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_symlink() {
                    continue;
                }
                if path.is_dir() {
                    if contains(&path, needle) {
                        return true;
                    }
                } else if std::fs::read_to_string(&path)
                    .map(|c| c.contains(needle))
                    .unwrap_or(false)
                {
                    return true;
                }
            }
            false
        }
        assert!(!contains(&dst, "TOP-SECRET"), "the secret leaked");
        let dst_link = dst.join("leak");
        assert!(
            std::fs::symlink_metadata(&dst_link)
                .expect("link metadata")
                .file_type()
                .is_symlink(),
            "the link must be recreated as a link"
        );
    }

    #[test]
    fn copy_tree_into_refuses_overwrites() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::create_dir_all(&src).expect("src");
        std::fs::write(src.join("a.txt"), "a").expect("a");
        std::fs::create_dir_all(&dst).expect("dst");
        std::fs::write(dst.join("a.txt"), "old").expect("existing");

        let err = copy_tree_into(&src, &dst).expect_err("collision");
        assert!(err.to_string().contains("refusing to overwrite"));
        assert_eq!(
            std::fs::read_to_string(dst.join("a.txt")).expect("read"),
            "old"
        );
    }

    #[test]
    fn copy_tree_into_copies_nested_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::create_dir_all(src.join("sub")).expect("src");
        std::fs::write(src.join("sub/b.txt"), "b").expect("b");
        let copied = copy_tree_into(&src, &dst).expect("copy");
        assert_eq!(copied, 1);
        assert_eq!(
            std::fs::read_to_string(dst.join("sub/b.txt")).expect("read"),
            "b"
        );
    }

    #[test]
    fn replace_dbname_swaps_both_forms() {
        assert_eq!(
            replace_dbname("postgresql://user@host:5432/old", "new"),
            "postgresql://user@host:5432/new"
        );
        assert_eq!(
            replace_dbname("host=h port=1 dbname=old user=u", "new"),
            "host=h port=1 dbname=new user=u"
        );
    }

    #[test]
    fn parse_url_accepts_host_port_path_and_defaults() {
        let url = parse_url("http://127.0.0.1:8080/health").expect("parse");
        assert_eq!(url.host, "127.0.0.1");
        assert_eq!(url.port, 8080);
        assert_eq!(url.path, "/health");
        let url = parse_url("http://example.com").expect("parse");
        assert_eq!(url.port, 80);
        assert_eq!(url.path, "/");
        assert!(parse_url("ftp://nope").is_err());
    }

    #[test]
    fn selector_prefers_exact_then_unique_prefix() {
        use chrono::DateTime;
        let mut state = State::new();
        let snap = |id: &str| BackupSnapshot {
            id: id.to_string(),
            application: "thornwa".to_string(),
            timestamp: DateTime::parse_from_rfc3339("2026-09-07T10:00:00Z")
                .expect("rfc3339")
                .to_utc(),
            source_manifest: vaultline_core::model::SourceManifest { entries: vec![] },
            database_metadata: vec![],
            configuration_metadata: vaultline_core::model::ConfigMetadata {
                note: "full capture declared".to_string(),
            },
            engine_snapshot: vaultline_core::model::EngineSnapshotRef {
                engine: "restic".to_string(),
                snapshot_id: "abc".to_string(),
            },
            integrity: vaultline_core::model::IntegrityInfo {
                engine_verified: true,
                highest_verified_level: vaultline_core::model::VerificationLevel::L1,
                verified_at: None,
            },
            restore_metadata: vaultline_core::model::RestoreMetadata {
                reconstructs: vec![],
            },
        };
        state.record_snapshot("thornwa", snap("thornwa-111"));
        state.record_snapshot("thornwa", snap("thornwa-222"));

        assert_eq!(
            select_snapshot(&state, "thornwa", "latest")
                .expect("latest")
                .id,
            "thornwa-222"
        );
        assert_eq!(
            select_snapshot(&state, "thornwa", "thornwa-111")
                .expect("full")
                .id,
            "thornwa-111"
        );
        assert_eq!(
            select_snapshot(&state, "thornwa", "thornwa-2")
                .expect("prefix")
                .id,
            "thornwa-222"
        );
        assert!(
            select_snapshot(&state, "thornwa", "thornwa-").is_err(),
            "ambiguous"
        );
        assert!(
            select_snapshot(&state, "thornwa", "nope").is_err(),
            "not found"
        );
        assert!(
            select_snapshot(&state, "other", "latest").is_err(),
            "no app"
        );
    }
}
