//! The canonical Application Recovery Model (ADR-V0-3).
//!
//! Everything here is **engine-agnostic**. An [`Application`] declares *what*
//! an application needs to be recreated; engine behavior (restic orchestration,
//! ADR-V0-2) and storage I/O are never executed in this crate. The separation
//! between the recovery definition (WHAT) and the storage backend (WHERE) is
//! ADR-V0-4.
//!
//! Every type derives `Serialize`/`Deserialize` so the model doubles as the
//! snapshot-manifest and state-file format. The TOML *configuration* wire
//! format lives in [`crate::config`] and is converted into these types after
//! validation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One declarative, versioned description of everything an application needs
/// to be recreated (ADR-V0-3). This is the product's core unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Application {
    /// Machine name: lowercase alphanumerics and dashes, at most 63 chars.
    pub name: String,
    pub description: Option<String>,
    /// The backup schedule: a 5-field cron expression (minute hour
    /// day-of-month month day-of-week). Validated at configuration time;
    /// executed by `schedule run` and the generated systemd timer.
    #[serde(default)]
    pub schedule: Option<String>,
    pub sources: Vec<Source>,
    pub databases: Vec<Database>,
    pub volumes: Vec<Volume>,
    pub storage: StorageTarget,
    pub retention: RetentionPolicy,
    pub verification: VerificationPolicy,
    pub restore: RestoreProcedure,
    /// Where a full recovery rehearsal executes (verification L6).
    /// Required by validation when the verification level is L6.
    #[serde(default)]
    pub rehearsal: Option<RehearsalTarget>,
}

/// The scratch root for L6 rehearsals: the rehearsal restores into
/// `<target>/.vaultline-rehearsal/<snapshot-id>/` with the procedure's
/// path targets remapped under it — never the live paths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RehearsalTarget {
    pub target: String,
}

/// A named capture source. Names are unique within an application and are the
/// references used by [`RestoreStep::RestoreFiles`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    pub kind: SourceKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SourceKind {
    Files(FileSource),
    Git(GitSource),
    /// A configuration reference: recorded in manifests but **never copied**
    /// into the backup (secrets stay where they are; the recovery procedure
    /// re-provisions them).
    ConfigRef(ConfigRef),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileSource {
    /// Paths captured verbatim. Resolved on the target host, not locally.
    pub paths: Vec<String>,
    /// Glob patterns excluded from the capture.
    #[serde(default)]
    pub excludes: Vec<String>,
    /// Optional pre-capture quiesce rule (shell-free argv — see the security
    /// model).
    #[serde(default)]
    pub quiesce: Option<Quiesce>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quiesce {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitSource {
    pub remote: String,
    /// Branch or tag to capture.
    pub reference: String,
    pub capture: GitCapture,
}

/// How a git source enters the backup: a full clone (mirror) or a shallow
/// reference to the remote (the commit recorded, objects fetched on demand).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitCapture {
    Mirror,
    Reference,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigRef {
    pub path: String,
    /// Free-form note about what this reference holds. Redaction contract:
    /// never the content, only the pointer.
    #[serde(default)]
    pub note: Option<String>,
}

/// A database captured through its engine's consistency mechanism — never by
/// copying its files (ADR-V0-3; live files are not consistent copies).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Database {
    pub name: String,
    pub kind: DatabaseType,
    pub connection: DatabaseConnection,
    pub consistency: ConsistencyStrategy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseType {
    PostgreSql,
    MySql,
    MariaDb,
    Sqlite,
}

impl DatabaseType {
    pub fn as_str(&self) -> &'static str {
        match self {
            DatabaseType::PostgreSql => "postgresql",
            DatabaseType::MySql => "mysql",
            DatabaseType::MariaDb => "mariadb",
            DatabaseType::Sqlite => "sqlite",
        }
    }
}

/// Where credentials live. The security model (ADR-V0-3) requires secrets in
/// environment variables, never in configuration files: server databases are
/// addressed by an env-var *name*; SQLite is addressed by its file path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DatabaseConnection {
    /// The connection string (URL) is read from this environment variable.
    UrlEnv(String),
    /// Local file path (SQLite).
    SqlitePath(String),
}

/// Per-engine consistency strategy. The MVP mechanism is a logical dump
/// (`pg_dump -Fc` for PostgreSQL); physical/PITR strategies are recorded in
/// ADR-V0-3 and arrive in later phases.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConsistencyStrategy {
    LogicalDump { format: DumpFormat },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DumpFormat {
    /// Compressed, portable dump (PostgreSQL `-Fc` custom format).
    Custom,
    /// Plain SQL dump.
    Sql,
}

/// A volume (Docker volume or host directory) with its capture semantics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Volume {
    pub name: String,
    pub capture: CaptureSemantics,
    /// The writer container to pause around the capture (pause-first
    /// semantics only — validation rejects it with any other capture).
    #[serde(default)]
    pub container: Option<String>,
    /// The sidecar image to copy the volume out with (sidecar semantics
    /// only — validation rejects it with any other capture). Defaults to
    /// `alpine` when the definition names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar_image: Option<String>,
}

/// How a volume's bytes are captured (ADR-V0-3): direct through the
/// resolved mountpoint, through a read-only sidecar container, or after
/// pausing the declared writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureSemantics {
    /// Capture the volume's path directly.
    Direct,
    /// Capture through a read-only sidecar container.
    Sidecar,
    /// Pause the writer first, then capture directly.
    PauseFirst,
}

/// The WHERE of the recovery definition (ADR-V0-4): the repository the
/// snapshot lands in. All credential fields are environment-variable names,
/// never values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageTarget {
    pub kind: StorageKind,
    /// Repository name within the backend. Defaults to the application name.
    #[serde(default)]
    pub repository: Option<String>,
    /// Environment variable holding the repository password (restic's
    /// encryption key material) — never a literal.
    pub password_env: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StorageKind {
    Local {
        path: String,
    },
    S3Compatible {
        endpoint: String,
        bucket: String,
        #[serde(default)]
        region: Option<String>,
        access_key_env: String,
        secret_key_env: String,
    },
    Sftp {
        host: String,
        port: u16,
        user: String,
        /// Absolute directory on the remote host holding repositories.
        path: String,
        #[serde(default)]
        key_file: Option<String>,
        #[serde(default)]
        known_hosts: Option<String>,
    },
}

/// Retention is deterministic and explainable: `prune` must state, per
/// snapshot, which rule kept or deleted it (ADR-V0-3). At least one count
/// must be non-zero — an all-zero policy would delete everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    pub keep_last: u32,
    pub keep_daily: u32,
    pub keep_weekly: u32,
    pub keep_monthly: u32,
    pub keep_yearly: u32,
}

impl RetentionPolicy {
    pub fn keeps_anything(&self) -> bool {
        self.keep_last + self.keep_daily + self.keep_weekly + self.keep_monthly + self.keep_yearly
            > 0
    }
}

/// The verification policy: what level must be reached before a snapshot may
/// be reported as healthy, and on what schedule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationPolicy {
    pub level: VerificationLevel,
    /// Cron expression (validated here; executed by `schedule run` and
    /// the systemd timers, ADR-005).
    #[serde(default)]
    pub schedule: Option<String>,
    /// Executable application-semantic checks (ADR-006): shell-free argv
    /// commands run against the rehearsal (or the restore target with
    /// `restore --verify`). Exit 0 passes; anything else fails the
    /// rehearsal. The environment contract: CWD is the rehearsal root and
    /// `VAULTLINE_REHEARSAL_DIR` names it.
    #[serde(default)]
    pub app_checks: Vec<AppCheck>,
}

/// One executable application check (ADR-006). The command is an argv
/// entry point — a path or a tool name — and the arguments are argv,
/// never a shell string (the security model).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppCheck {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// The verification levels — the product's central distinction between
/// "backup created" and "backup proven restorable" (ADR-V0-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum VerificationLevel {
    /// The backup command succeeded.
    L1,
    /// Repository integrity verified (restic `check`).
    L2,
    /// Snapshot metadata verified.
    L3,
    /// Selected files restorable (restore-to-scratch + compare).
    L4,
    /// Databases restorable (scratch instance + integrity + row counts).
    L5,
    /// Full application recovery test (the disaster-recovery end-to-end).
    L6,
}

impl VerificationLevel {
    /// The numeric level (1..=6), used by the TOML wire format.
    pub fn to_number(self) -> u8 {
        match self {
            VerificationLevel::L1 => 1,
            VerificationLevel::L2 => 2,
            VerificationLevel::L3 => 3,
            VerificationLevel::L4 => 4,
            VerificationLevel::L5 => 5,
            VerificationLevel::L6 => 6,
        }
    }

    pub fn from_number(n: u8) -> Option<Self> {
        match n {
            1 => Some(VerificationLevel::L1),
            2 => Some(VerificationLevel::L2),
            3 => Some(VerificationLevel::L3),
            4 => Some(VerificationLevel::L4),
            5 => Some(VerificationLevel::L5),
            6 => Some(VerificationLevel::L6),
            _ => None,
        }
    }
}

/// The executable, ordered restore procedure (ADR-V0-3): a sequence of steps
/// a fresh install can follow to reconstruct the application.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreProcedure {
    #[serde(default)]
    pub steps: Vec<RestoreStep>,
    /// Cross-platform path mappings: a declared (production) target
    /// prefix is replaced by this host's layout (longest prefix wins,
    /// later entries break ties — CLI entries come last). Applies to the
    /// live restore's path targets, never to the rehearsal mirrors.
    #[serde(default)]
    pub path_map: Vec<PathMapEntry>,
}

/// One `from` → `to` prefix mapping of [`RestoreProcedure::path_map`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathMapEntry {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RestoreStep {
    /// Restore a source into a target directory.
    RestoreFiles { source: String, target: String },
    /// Restore a database into a (possibly renamed) target database.
    RestoreDatabase {
        database: String,
        #[serde(default)]
        target_database: Option<String>,
    },
    /// Restore a volume's bytes.
    RestoreVolume { volume: String },
    /// Wait until the application's health endpoint answers.
    WaitHealthy { url: String },
}

/// A completed backup: the recorded outcome of one run against one
/// application (ADR-V0-3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupSnapshot {
    /// Vaultline's own snapshot id (opaque, stable).
    pub id: String,
    pub application: String,
    /// UTC time the backup run completed.
    pub timestamp: DateTime<Utc>,
    pub source_manifest: SourceManifest,
    pub database_metadata: Vec<DatabaseSnapshotMeta>,
    /// Redacted configuration metadata — shapes and names only, never values
    /// (the redaction contract).
    pub configuration_metadata: ConfigMetadata,
    /// Reference into the engine's own snapshot space.
    pub engine_snapshot: EngineSnapshotRef,
    pub integrity: IntegrityInfo,
    /// What this snapshot can reconstruct (source and database names).
    pub restore_metadata: RestoreMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceManifest {
    pub entries: Vec<SourceManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceManifestEntry {
    pub name: String,
    pub kind: String,
    /// The path the bytes were captured from, recorded at backup time.
    /// Restores locate the content through this record — never through
    /// the restore host's own resolution (hosts differ; a docker volume
    /// may not even exist on the restore host).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatabaseSnapshotMeta {
    pub name: String,
    pub engine: DatabaseType,
    /// The consistency mechanism used, e.g. `pg_dump -Fc`.
    pub mechanism: String,
    pub dump_format: DumpFormat,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigMetadata {
    /// Free-form summary of what the snapshot's configuration metadata
    /// contains — never actual values.
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineSnapshotRef {
    /// The engine that produced the snapshot (restic per ADR-V0-2).
    pub engine: String,
    /// The engine's own snapshot identifier.
    pub snapshot_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntegrityInfo {
    /// Whether the engine's own integrity check passed.
    pub engine_verified: bool,
    /// The highest verification level actually reached for this snapshot.
    pub highest_verified_level: VerificationLevel,
    /// When the highest level was last reached.
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreMetadata {
    /// Names of sources and databases this snapshot can reconstruct.
    pub reconstructs: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_application() -> Application {
        Application {
            name: "thornwa".to_string(),
            description: Some("ThornWA compose stack".to_string()),
            schedule: Some("30 2 * * *".to_string()),
            sources: vec![
                Source {
                    name: "uploads".to_string(),
                    kind: SourceKind::Files(FileSource {
                        paths: vec!["/srv/thornwa/uploads".to_string()],
                        excludes: vec!["**/*.tmp".to_string()],
                        quiesce: None,
                    }),
                },
                Source {
                    name: "env".to_string(),
                    kind: SourceKind::ConfigRef(ConfigRef {
                        path: "/srv/thornwa/.env".to_string(),
                        note: Some("secrets — reference only".to_string()),
                    }),
                },
            ],
            databases: vec![Database {
                name: "main".to_string(),
                kind: DatabaseType::PostgreSql,
                connection: DatabaseConnection::UrlEnv("THORNWA_DATABASE_URL".to_string()),
                consistency: ConsistencyStrategy::LogicalDump {
                    format: DumpFormat::Custom,
                },
            }],
            volumes: vec![Volume {
                name: "thornwa_pgdata".to_string(),
                capture: CaptureSemantics::Direct,
                container: None,
                sidecar_image: None,
            }],
            storage: StorageTarget {
                kind: StorageKind::Local {
                    path: "/var/backups/thornwa".to_string(),
                },
                repository: None,
                password_env: "RESTIC_PASSWORD_THORNWA".to_string(),
            },
            retention: RetentionPolicy {
                keep_last: 14,
                keep_daily: 7,
                keep_weekly: 4,
                keep_monthly: 6,
                keep_yearly: 1,
            },
            verification: VerificationPolicy {
                level: VerificationLevel::L3,
                schedule: Some("0 3 * * 7".to_string()),
                app_checks: vec![AppCheck {
                    name: "pg-integrity".to_string(),
                    command: "psql".to_string(),
                    args: vec!["-c".to_string(), "SELECT 1".to_string()],
                }],
            },
            rehearsal: None,
            restore: RestoreProcedure {
                steps: vec![
                    RestoreStep::RestoreFiles {
                        source: "uploads".to_string(),
                        target: "/srv/thornwa/uploads".to_string(),
                    },
                    RestoreStep::RestoreDatabase {
                        database: "main".to_string(),
                        target_database: Some("thornwa".to_string()),
                    },
                    RestoreStep::WaitHealthy {
                        url: "http://localhost:3000/health".to_string(),
                    },
                ],
                path_map: vec![PathMapEntry {
                    from: "/srv".to_string(),
                    to: "C:/srv".to_string(),
                }],
            },
        }
    }

    #[test]
    fn application_round_trips_through_json() {
        let app = sample_application();
        let json = serde_json::to_string(&app).expect("serialize");
        let back: Application = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(app, back);
    }

    #[test]
    fn verification_level_numbers_round_trip() {
        for n in 1..=6 {
            let level = VerificationLevel::from_number(n).expect("valid level");
            assert_eq!(level.to_number(), n);
        }
        assert!(VerificationLevel::from_number(0).is_none());
        assert!(VerificationLevel::from_number(7).is_none());
    }

    #[test]
    fn verification_levels_are_ordered() {
        assert!(VerificationLevel::L1 < VerificationLevel::L6);
        assert!(VerificationLevel::L4 > VerificationLevel::L2);
    }

    #[test]
    fn retention_keeps_anything() {
        let all_zero = RetentionPolicy {
            keep_last: 0,
            keep_daily: 0,
            keep_weekly: 0,
            keep_monthly: 0,
            keep_yearly: 0,
        };
        assert!(!all_zero.keeps_anything());
        assert!(sample_application().retention.keeps_anything());
    }

    #[test]
    fn database_types_have_stable_names() {
        assert_eq!(DatabaseType::PostgreSql.as_str(), "postgresql");
        assert_eq!(DatabaseType::MariaDb.as_str(), "mariadb");
    }

    #[test]
    fn backup_snapshot_round_trips_through_json() {
        let snapshot = BackupSnapshot {
            id: "snap-1".to_string(),
            application: "thornwa".to_string(),
            timestamp: DateTime::parse_from_rfc3339("2026-09-07T10:00:00Z")
                .expect("rfc3339")
                .to_utc(),
            source_manifest: SourceManifest {
                entries: vec![SourceManifestEntry {
                    name: "uploads".to_string(),
                    kind: "files".to_string(),
                    path: Some("/srv/thornwa/uploads".to_string()),
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
                verified_at: None,
            },
            restore_metadata: RestoreMetadata {
                reconstructs: vec!["uploads".to_string(), "main".to_string()],
            },
        };
        let json = serde_json::to_string(&snapshot).expect("serialize");
        let back: BackupSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(snapshot, back);
    }

    #[test]
    fn timestamp_serializes_as_rfc3339() {
        let ts = DateTime::parse_from_rfc3339("2026-09-07T10:00:00Z")
            .expect("rfc3339")
            .to_utc();
        let json = serde_json::to_string(&ts).expect("serialize");
        assert_eq!(json, "\"2026-09-07T10:00:00Z\"");
    }
}
