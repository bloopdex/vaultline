//! Plain-file state (ADR-V0-3: a JSON state file + lockfile first; embedded
//! SQLite only on evidence, not by default).
//!
//! The state file is the durable record of backup snapshots per application.
//! Writes are atomic (write-to-temp + rename); the lockfile prevents two
//! vaultline processes from mutating the state concurrently (create-new
//! semantics — a held lock is an explicit, documented error, never a race).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{ErrorKind, Result, VaultlineError};
use crate::model::BackupSnapshot;

/// The state-file schema version. Bump only with a migration story.
/// Phase 5 added the optional [`Operations`] records with
/// `#[serde(default)]` — additive and backward-compatible both ways, so
/// the version stays 1.
pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// Application name → its recorded state (deterministic order).
    pub applications: BTreeMap<String, AppState>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            applications: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppState {
    pub snapshots: Vec<BackupSnapshot>,
    /// Operations bookkeeping (Phase 5): schedule runs and prune outcomes.
    /// Optional for backward compatibility with state files written before
    /// the operations phase.
    #[serde(default)]
    pub operations: Operations,
}

/// The operations record: what the schedule executor and the prune command
/// have done, and when. Snapshots themselves stay immutable — pruning
/// records which ids were forgotten, never rewrites a snapshot's record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Operations {
    /// When the verification schedule last ran (the `schedule run`
    /// executor's bookkeeping — manual `backup verify` does not count).
    pub last_verify_at: Option<DateTime<Utc>>,
    /// When a prune last executed (dry-runs do not count).
    pub last_prune_at: Option<DateTime<Utc>>,
    /// The applied prune outcomes, oldest first.
    #[serde(default)]
    pub prunes: Vec<PruneRecord>,
}

/// One applied prune: which recorded snapshots were forgotten from the
/// engine, and how many were kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PruneRecord {
    pub at: DateTime<Utc>,
    /// Recorded snapshot ids forgotten from the engine.
    #[serde(default)]
    pub forgotten: Vec<String>,
    /// How many recorded snapshots the retention plan kept.
    pub kept: usize,
}

impl Operations {
    /// Whether any prune has forgotten this snapshot id.
    pub fn is_pruned(&self, snapshot_id: &str) -> bool {
        self.prunes
            .iter()
            .any(|record| record.forgotten.iter().any(|id| id == snapshot_id))
    }
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a state file; a missing file is an empty state (first run).
    pub fn load(path: &Path) -> Result<Self> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::new()),
            Err(e) => {
                return Err(VaultlineError::with_source(
                    ErrorKind::Io,
                    format!("cannot read state file {}", path.display()),
                    e,
                ));
            }
        };
        let state: State = serde_json::from_str(&contents).map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Io,
                format!("state file {} is corrupt", path.display()),
                e,
            )
        })?;
        if state.version != STATE_VERSION {
            return Err(VaultlineError::new(
                ErrorKind::Unsupported,
                format!(
                    "state file {} has version {}, this build supports version {}",
                    path.display(),
                    state.version,
                    STATE_VERSION
                ),
            ));
        }
        Ok(state)
    }

    /// Save atomically: write `state.json.tmp`, then rename over the target.
    /// `std::fs::rename` replaces an existing destination on both platforms.
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        let contents = serde_json::to_string_pretty(self).map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Internal,
                "cannot serialize the state file — this is a bug",
                e,
            )
        })?;
        std::fs::write(&tmp, contents).map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Io,
                format!("cannot write state file {}", tmp.display()),
                e,
            )
        })?;
        std::fs::rename(&tmp, path).map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Io,
                format!("cannot replace state file {}", path.display()),
                e,
            )
        })
    }

    /// Append a snapshot to an application's record.
    pub fn record_snapshot(&mut self, application: &str, snapshot: BackupSnapshot) {
        self.applications
            .entry(application.to_string())
            .or_default()
            .snapshots
            .push(snapshot);
    }
}

/// An exclusive lock on the state directory, held for the duration of a
/// mutating command. Released on drop.
#[derive(Debug)]
pub struct StateLock {
    path: PathBuf,
}

impl StateLock {
    /// Acquire the lock with the strict contract: a held lock is an
    /// explicit error, never reclaimed (a stale lock is removed manually
    /// — the error message says which file and why).
    pub fn acquire(state_dir: &Path) -> Result<Self> {
        Self::acquire_with(state_dir, &|_| true)
    }

    /// Acquire the lock with stale-lock recovery (ADR-007): on
    /// contention, the lock's recorded holder pid is tested with
    /// `is_alive`. A DEAD holder (a crashed run) is reclaimed with a
    /// warning; an ALIVE holder errors as before. An unreadable or
    /// unparsable lock file is never reclaimed — without evidence of the
    /// holder's death, the strict contract stands.
    pub fn acquire_with(state_dir: &Path, is_alive: &dyn Fn(u32) -> bool) -> Result<Self> {
        std::fs::create_dir_all(state_dir).map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Io,
                format!("cannot create state directory {}", state_dir.display()),
                e,
            )
        })?;
        let path = state_dir.join("lock");
        match Self::try_create(&path) {
            Ok(lock) => Ok(lock),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let holder_pid = read_lock_pid(&path);
                match holder_pid {
                    Some(pid) if !is_alive(pid) => {
                        // Evidence of death: reclaim and retry once.
                        tracing::warn!(
                            pid,
                            lock = %path.display(),
                            "reclaiming the stale lock of a dead process"
                        );
                        if let Err(remove_err) = std::fs::remove_file(&path) {
                            return Err(VaultlineError::with_source(
                                ErrorKind::Io,
                                format!(
                                    "the lock {} is stale (holder pid {pid} is dead) but cannot be removed",
                                    path.display()
                                ),
                                remove_err,
                            ));
                        }
                        match Self::try_create(&path) {
                            Ok(lock) => Ok(lock),
                            Err(again) if again.kind() == std::io::ErrorKind::AlreadyExists => {
                                Err(held_error(&path))
                            }
                            Err(again) => Err(VaultlineError::with_source(
                                ErrorKind::Io,
                                format!("cannot create lock file {}", path.display()),
                                again,
                            )),
                        }
                    }
                    _ => Err(held_error(&path)),
                }
            }
            Err(e) => Err(VaultlineError::with_source(
                ErrorKind::Io,
                format!("cannot create lock file {}", path.display()),
                e,
            )),
        }
    }

    fn try_create(path: &Path) -> std::io::Result<Self> {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut file) => {
                use std::io::Write;
                let _ = writeln!(file, "pid {}", std::process::id());
                Ok(Self {
                    path: path.to_path_buf(),
                })
            }
            Err(e) => Err(e),
        }
    }
}

fn held_error(path: &Path) -> VaultlineError {
    VaultlineError::new(
        ErrorKind::Operational,
        format!(
            "another vaultline process is running (lock held: {}); if no process is running, remove the stale lock file",
            path.display()
        ),
    )
}

/// The holder pid recorded in the lock file ("pid <n>"), or None when the
/// file is unreadable or malformed.
fn read_lock_pid(path: &Path) -> Option<u32> {
    let contents = std::fs::read_to_string(path).ok()?;
    let pid = contents.lines().next()?.strip_prefix("pid ")?;
    pid.trim().parse().ok()
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ConfigMetadata, DatabaseSnapshotMeta, DatabaseType, DumpFormat, EngineSnapshotRef,
        IntegrityInfo, RestoreMetadata, SourceManifest, SourceManifestEntry, VerificationLevel,
    };
    use chrono::DateTime;

    fn sample_snapshot(id: &str) -> BackupSnapshot {
        BackupSnapshot {
            id: id.to_string(),
            application: "thornwa".to_string(),
            timestamp: DateTime::parse_from_rfc3339("2026-09-07T10:00:00Z")
                .expect("rfc3339")
                .to_utc(),
            source_manifest: SourceManifest {
                entries: vec![SourceManifestEntry {
                    name: "uploads".to_string(),
                    kind: "files".to_string(),
                }],
            },
            database_metadata: vec![DatabaseSnapshotMeta {
                name: "main".to_string(),
                engine: DatabaseType::PostgreSql,
                mechanism: "pg_dump -Fc".to_string(),
                dump_format: DumpFormat::Custom,
            }],
            configuration_metadata: ConfigMetadata {
                note: "env references recorded, no values".to_string(),
            },
            engine_snapshot: EngineSnapshotRef {
                engine: "restic".to_string(),
                snapshot_id: "abc123".to_string(),
            },
            integrity: IntegrityInfo {
                engine_verified: true,
                highest_verified_level: VerificationLevel::L3,
                verified_at: Some(
                    DateTime::parse_from_rfc3339("2026-09-07T10:01:00Z")
                        .expect("rfc3339")
                        .to_utc(),
                ),
            },
            restore_metadata: RestoreMetadata {
                reconstructs: vec!["uploads".to_string()],
            },
        }
    }

    #[test]
    fn missing_state_file_loads_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = State::load(&dir.path().join("state.json")).expect("missing = empty");
        assert_eq!(state, State::new());
    }

    #[test]
    fn state_round_trips_through_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");

        let mut state = State::new();
        state.record_snapshot("thornwa", sample_snapshot("snap-1"));
        state.save(&path).expect("save");
        state.record_snapshot("thornwa", sample_snapshot("snap-2"));
        state.save(&path).expect("save replaces");

        let loaded = State::load(&path).expect("load");
        assert_eq!(loaded.applications["thornwa"].snapshots.len(), 2);
        assert_eq!(loaded.version, STATE_VERSION);
        assert!(!dir.path().join("state.json.tmp").exists(), "tmp cleaned");
    }

    #[test]
    fn unsupported_state_version_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"version": 99, "applications": {}}"#).expect("write");
        let err = State::load(&path).expect_err("version mismatch");
        assert!(err.to_string().contains("version 99"));
    }

    #[test]
    fn corrupt_state_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        std::fs::write(&path, "not json").expect("write");
        assert!(State::load(&path).is_err());
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = StateLock::acquire(dir.path()).expect("first acquire");
        let second = StateLock::acquire(dir.path());
        assert!(
            second.is_err(),
            "a second acquisition must fail while the lock is held"
        );
        drop(lock);
        let again = StateLock::acquire(dir.path()).expect("acquire after drop");
        drop(again);
    }

    #[test]
    fn lock_error_message_points_at_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = StateLock::acquire(dir.path()).expect("acquire");
        let err = StateLock::acquire(dir.path()).expect_err("second");
        assert!(err.to_string().contains("lock"));
        assert!(err.to_string().contains("stale"));
        drop(lock);
    }

    fn write_lock_with_pid(dir: &Path, pid: u32) {
        std::fs::create_dir_all(dir).expect("state dir");
        std::fs::write(dir.join("lock"), format!("pid {pid}\n")).expect("lock file");
    }

    #[test]
    fn dead_holder_is_reclaimed_by_evidence() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_lock_with_pid(dir.path(), 999_999_999);
        // The checker reports the holder dead: the lock is reclaimed and
        // the acquisition succeeds.
        let lock = StateLock::acquire_with(dir.path(), &|_| false).expect("reclaimed");
        assert!(dir.path().join("lock").exists(), "reacquired");
        drop(lock);
    }

    #[test]
    fn alive_holder_is_never_reclaimed() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_lock_with_pid(dir.path(), 42);
        let err = StateLock::acquire_with(dir.path(), &|_| true).expect_err("alive");
        assert!(
            err.to_string().contains("another vaultline process"),
            "{err}"
        );
        assert!(
            dir.path().join("lock").exists(),
            "the held lock must survive"
        );
    }

    #[test]
    fn unparsable_lock_is_never_reclaimed() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path()).expect("state dir");
        std::fs::write(dir.path().join("lock"), "garbage").expect("lock file");
        // Even with the checker saying dead, an unreadable pid is no
        // evidence — the strict contract stands.
        let err = StateLock::acquire_with(dir.path(), &|_| false).expect_err("unparsable");
        assert!(
            err.to_string().contains("another vaultline process"),
            "{err}"
        );
        assert!(dir.path().join("lock").exists());
    }
}
