//! Strongly typed TOML configuration (ADR-V0-4: the recovery definition is
//! WHAT; the file it lives in is a presentation detail).
//!
//! Strict by construction: `deny_unknown_fields` rejects misspelled keys at
//! parse time, and validation aggregates **all** failures into one report
//! (SOT Section 10.8 — diagnostics explain, never first-error-only).
//!
//! The wire types below mirror the TOML shape; [`validate`] converts a parsed
//! file into the canonical [`crate::model::Application`] after checking every
//! cross-reference and constraint the model assumes.

use std::path::Path;

use serde::Deserialize;

use crate::error::{ErrorKind, Result, ValidationError, VaultlineError, Warning};
use crate::model::{
    Application, CaptureSemantics, ConfigRef, ConsistencyStrategy, Database, DatabaseConnection,
    DatabaseType, DumpFormat, FileSource, GitCapture, GitSource, Quiesce, RestoreProcedure,
    RestoreStep, RetentionPolicy, Source, SourceKind, StorageKind, StorageTarget,
    VerificationLevel, VerificationPolicy, Volume,
};

// ---------------------------------------------------------------------------
// Wire types (the TOML shape)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub application: WireApplication,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireApplication {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// The backup schedule (5-field cron). Executed by `schedule run` and
    /// the generated systemd timer.
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub sources: WireSources,
    #[serde(default)]
    pub databases: Vec<WireDatabase>,
    #[serde(default)]
    pub volumes: Vec<WireVolume>,
    pub storage: WireStorage,
    pub retention: WireRetention,
    pub verification: WireVerification,
    #[serde(default)]
    pub rehearsal: Option<WireRehearsal>,
    #[serde(default)]
    pub restore: WireRestore,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireSources {
    #[serde(default)]
    pub files: Vec<WireFileSource>,
    #[serde(default)]
    pub git: Vec<WireGitSource>,
    #[serde(default)]
    pub config_refs: Vec<WireConfigRef>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireFileSource {
    pub name: String,
    pub paths: Vec<String>,
    #[serde(default)]
    pub excludes: Vec<String>,
    #[serde(default)]
    pub quiesce: Option<WireQuiesce>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireQuiesce {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireGitSource {
    pub name: String,
    pub remote: String,
    pub reference: String,
    pub capture: WireGitCapture,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireGitCapture {
    Mirror,
    Reference,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireConfigRef {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireDatabase {
    pub name: String,
    pub kind: WireDatabaseType,
    #[serde(default)]
    pub url_env: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    pub consistency: WireConsistency,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireDatabaseType {
    PostgreSql,
    MySql,
    MariaDb,
    Sqlite,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireConsistency {
    pub logical: WireLogicalDump,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireLogicalDump {
    pub format: WireDumpFormat,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireDumpFormat {
    Custom,
    Sql,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireVolume {
    pub name: String,
    pub capture: WireCaptureSemantics,
    /// The writer container to pause around the capture (pause-first
    /// only — validation rejects it with any other capture).
    #[serde(default)]
    pub container: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireCaptureSemantics {
    Direct,
    Sidecar,
    PauseFirst,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireStorage {
    pub kind: WireStorageKind,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub bucket: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub access_key_env: Option<String>,
    #[serde(default)]
    pub secret_key_env: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub key_file: Option<String>,
    #[serde(default)]
    pub known_hosts: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    pub password_env: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireStorageKind {
    Local,
    S3,
    Sftp,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRetention {
    #[serde(default)]
    pub keep_last: u32,
    #[serde(default)]
    pub keep_daily: u32,
    #[serde(default)]
    pub keep_weekly: u32,
    #[serde(default)]
    pub keep_monthly: u32,
    #[serde(default)]
    pub keep_yearly: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireVerification {
    pub level: u8,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub app_checks: Vec<WireAppCheck>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireAppCheck {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRehearsal {
    pub target: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRestore {
    #[serde(default)]
    pub steps: Vec<WireRestoreStep>,
    /// Cross-platform path mappings (longest prefix wins; later
    /// entries break ties).
    #[serde(default)]
    pub path_map: Vec<WirePathMapEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WirePathMapEntry {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireRestoreStep {
    RestoreFiles {
        source: String,
        target: String,
    },
    RestoreDatabase {
        database: String,
        #[serde(default)]
        target_database: Option<String>,
    },
    RestoreVolume {
        volume: String,
    },
    WaitHealthy {
        url: String,
    },
}

// ---------------------------------------------------------------------------
// Parsing and validation
// ---------------------------------------------------------------------------

/// Parse a configuration string into wire form. Unknown fields, type
/// mismatches, and TOML syntax errors all surface here with the parser's
/// line/column span.
pub fn parse_str(contents: &str) -> Result<ConfigFile> {
    toml::from_str(contents).map_err(|e| {
        VaultlineError::with_source(ErrorKind::Config, "invalid TOML configuration", e)
    })
}

/// The result of validating a configuration file: either a converted
/// [`Application`] (errors empty) or the full aggregated failure report.
#[derive(Debug)]
pub struct ValidationOutcome {
    pub application: Option<Application>,
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<Warning>,
}

impl ValidationOutcome {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validate a parsed configuration file. **Every** failure is collected —
/// callers print the whole list, never just the first.
pub fn validate(config: ConfigFile) -> ValidationOutcome {
    let mut errors: Vec<ValidationError> = Vec::new();
    let mut warnings: Vec<Warning> = Vec::new();

    validate_application_name(&config.application.name)
        .err()
        .into_iter()
        .for_each(|e| errors.push(e));

    let app = &config.application;

    // Capture targets: an application with nothing to back up is a mistake.
    let source_count =
        app.sources.files.len() + app.sources.git.len() + app.sources.config_refs.len();
    if source_count + app.databases.len() + app.volumes.len() == 0 {
        errors.push(ValidationError::new(
            "application",
            "nothing to back up: declare at least one source, database, or volume",
        ));
    }

    // Source names: unique across all source kinds — restore steps reference
    // them by name.
    let mut source_names = std::collections::HashSet::new();
    let mut collect_source_name =
        |index: usize, kind: &str, name: &str, errors: &mut Vec<ValidationError>| {
            if name.is_empty() {
                errors.push(ValidationError::new(
                    format!("application.sources.{kind}[{index}].name"),
                    "must not be empty",
                ));
            } else if !source_names.insert(name.to_string()) {
                errors.push(ValidationError::new(
                    format!("application.sources.{kind}[{index}].name"),
                    format!("duplicate source name \"{name}\""),
                ));
            }
        };

    for (i, source) in app.sources.files.iter().enumerate() {
        collect_source_name(i, "files", &source.name, &mut errors);
        if source.paths.is_empty() {
            errors.push(ValidationError::new(
                format!("application.sources.files[{i}].paths"),
                "must contain at least one path",
            ));
        }
        for (j, path) in source.paths.iter().enumerate() {
            if path.is_empty() {
                errors.push(ValidationError::new(
                    format!("application.sources.files[{i}].paths[{j}]"),
                    "must not be empty",
                ));
            } else if !path.starts_with('/') {
                warnings.push(Warning::new(format!(
                    "application.sources.files[{i}].paths[{j}]: \"{path}\" is relative — it is resolved on the target host, not locally"
                )));
            }
        }
    }
    for (i, source) in app.sources.git.iter().enumerate() {
        collect_source_name(i, "git", &source.name, &mut errors);
        if source.remote.is_empty() {
            errors.push(ValidationError::new(
                format!("application.sources.git[{i}].remote"),
                "must not be empty",
            ));
        }
        if source.reference.is_empty() {
            errors.push(ValidationError::new(
                format!("application.sources.git[{i}].reference"),
                "must not be empty",
            ));
        }
    }
    for (i, source) in app.sources.config_refs.iter().enumerate() {
        collect_source_name(i, "config_refs", &source.name, &mut errors);
        if source.path.is_empty() {
            errors.push(ValidationError::new(
                format!("application.sources.config_refs[{i}].path"),
                "must not be empty",
            ));
        }
    }

    // Databases: unique names, per-engine connection rules, per-engine dump
    // format rules (SQLite's backup API only produces a plain SQL file).
    let mut database_names = std::collections::HashSet::new();
    for (i, db) in app.databases.iter().enumerate() {
        if db.name.is_empty() {
            errors.push(ValidationError::new(
                format!("application.databases[{i}].name"),
                "must not be empty",
            ));
        } else if !database_names.insert(db.name.clone()) {
            errors.push(ValidationError::new(
                format!("application.databases[{i}].name"),
                format!("duplicate database name \"{}\"", db.name),
            ));
        }

        let base = format!("application.databases[{i}]");
        match db.kind {
            WireDatabaseType::PostgreSql | WireDatabaseType::MySql | WireDatabaseType::MariaDb => {
                if db.url_env.as_deref().unwrap_or("").is_empty() {
                    errors.push(ValidationError::new(
                        format!("{base}.url_env"),
                        "server databases require url_env (the environment variable holding the connection string)",
                    ));
                }
                if db.path.is_some() {
                    errors.push(ValidationError::new(
                        format!("{base}.path"),
                        "path is only valid for sqlite databases",
                    ));
                }
            }
            WireDatabaseType::Sqlite => {
                if db.path.as_deref().unwrap_or("").is_empty() {
                    errors.push(ValidationError::new(
                        format!("{base}.path"),
                        "sqlite databases require the file path",
                    ));
                }
                if db.url_env.is_some() {
                    errors.push(ValidationError::new(
                        format!("{base}.url_env"),
                        "url_env is not valid for sqlite databases",
                    ));
                }
                if matches!(db.consistency.logical.format, WireDumpFormat::Custom) {
                    errors.push(ValidationError::new(
                        format!("{base}.consistency.logical.format"),
                        "sqlite is captured with its backup API; format must be \"sql\"",
                    ));
                }
            }
        }
    }

    // Volumes: unique names (their identity IS their name).
    let mut volume_names = std::collections::HashSet::new();
    for (i, volume) in app.volumes.iter().enumerate() {
        if volume.name.is_empty() {
            errors.push(ValidationError::new(
                format!("application.volumes[{i}].name"),
                "must not be empty",
            ));
        } else if !volume_names.insert(volume.name.clone()) {
            errors.push(ValidationError::new(
                format!("application.volumes[{i}].name"),
                format!("duplicate volume name \"{}\"", volume.name),
            ));
        }
        // Pause-first needs its writer; a container on any other capture
        // is a meaningless field (rejected, never silently ignored).
        match (&volume.capture, volume.container.as_deref()) {
            (WireCaptureSemantics::PauseFirst, None | Some("")) => {
                errors.push(ValidationError::new(
                    format!("application.volumes[{i}].container"),
                    "pause-first capture requires the writer container to pause",
                ));
            }
            (WireCaptureSemantics::Direct | WireCaptureSemantics::Sidecar, Some(_)) => {
                errors.push(ValidationError::new(
                    format!("application.volumes[{i}].container"),
                    "container is only meaningful with capture = \"pause-first\"",
                ));
            }
            _ => {}
        }
    }

    // Storage: per-kind required fields; credentials are env-var names.
    if app.storage.password_env.is_empty() {
        errors.push(ValidationError::new(
            "application.storage.password_env",
            "must name the environment variable holding the repository password (never a literal)",
        ));
    }
    match app.storage.kind {
        WireStorageKind::Local => {
            if app.storage.path.as_deref().unwrap_or("").is_empty() {
                errors.push(ValidationError::new(
                    "application.storage.path",
                    "local storage requires a path",
                ));
            }
        }
        WireStorageKind::S3 => {
            for (field, value) in [
                ("endpoint", &app.storage.endpoint),
                ("bucket", &app.storage.bucket),
                ("access_key_env", &app.storage.access_key_env),
                ("secret_key_env", &app.storage.secret_key_env),
            ] {
                if value.as_deref().unwrap_or("").is_empty() {
                    errors.push(ValidationError::new(
                        format!("application.storage.{field}"),
                        format!("s3 storage requires {field}"),
                    ));
                }
            }
        }
        WireStorageKind::Sftp => {
            if app.storage.host.as_deref().unwrap_or("").is_empty() {
                errors.push(ValidationError::new(
                    "application.storage.host",
                    "sftp storage requires host",
                ));
            }
            if app.storage.user.as_deref().unwrap_or("").is_empty() {
                errors.push(ValidationError::new(
                    "application.storage.user",
                    "sftp storage requires user",
                ));
            }
            if app.storage.path.as_deref().unwrap_or("").is_empty() {
                errors.push(ValidationError::new(
                    "application.storage.path",
                    "sftp storage requires path (the remote directory holding repositories)",
                ));
            }
            match app.storage.port {
                None | Some(0) => errors.push(ValidationError::new(
                    "application.storage.port",
                    "sftp storage requires a port between 1 and 65535",
                )),
                Some(_) => {}
            }
        }
    }

    // Retention: an all-zero policy would delete everything on the first
    // prune — refuse it at configuration time.
    let retention = RetentionPolicy {
        keep_last: app.retention.keep_last,
        keep_daily: app.retention.keep_daily,
        keep_weekly: app.retention.keep_weekly,
        keep_monthly: app.retention.keep_monthly,
        keep_yearly: app.retention.keep_yearly,
    };
    if !retention.keeps_anything() {
        errors.push(ValidationError::new(
            "application.retention",
            "at least one keep count must be non-zero (an all-zero policy would delete everything)",
        ));
    }

    // Schedules (Phase 5): both crons are validated now — they execute
    // through `schedule run` and the generated systemd timers. A cron the
    // crate accepts but OnCalendar cannot express still schedules fine
    // under `schedule run`; timer generation will refuse it — warned here
    // so systemd hosts learn at validate time.
    if let Some(schedule) = &app.schedule {
        match crate::cron::parse_schedule(schedule) {
            Err(message) => errors.push(ValidationError::new("application.schedule", message)),
            Ok(_) => {
                if let Err(message) = crate::cron::to_on_calendar(schedule) {
                    warnings.push(Warning::new(format!(
                        "application.schedule: {message} (vaultline timer generate will refuse it; vaultline schedule run still works)"
                    )));
                }
            }
        }
    }
    if let Some(schedule) = &app.verification.schedule {
        match crate::cron::parse_schedule(schedule) {
            Err(message) => errors.push(ValidationError::new(
                "application.verification.schedule",
                message,
            )),
            Ok(_) => {
                if let Err(message) = crate::cron::to_on_calendar(schedule) {
                    warnings.push(Warning::new(format!(
                        "application.verification.schedule: {message} (vaultline timer generate will refuse it; vaultline schedule run still works)"
                    )));
                }
            }
        }
    }

    // Verification: level range, and an honest warning for policy fields
    // that are recorded but not yet executed.
    let level = match VerificationLevel::from_number(app.verification.level) {
        Some(level) => Some(level),
        None => {
            errors.push(ValidationError::new(
                "application.verification.level",
                "must be between 1 and 6 (L1..L6)",
            ));
            None
        }
    };
    // App checks (ADR-006): executable, shell-free; unique names.
    let mut check_names = std::collections::HashSet::new();
    for (i, check) in app.verification.app_checks.iter().enumerate() {
        let base = format!("application.verification.app_checks[{i}]");
        if check.name.is_empty() {
            errors.push(ValidationError::new(
                format!("{base}.name"),
                "must not be empty",
            ));
        } else if !check_names.insert(check.name.clone()) {
            errors.push(ValidationError::new(
                format!("{base}.name"),
                format!("duplicate app check name \"{}\"", check.name),
            ));
        }
        if check.command.is_empty() {
            errors.push(ValidationError::new(
                format!("{base}.command"),
                "must not be empty",
            ));
        }
    }

    // L6 requires a rehearsal root: the full recovery rehearsal needs a
    // scratch destination the definition declares as such.
    if level == Some(VerificationLevel::L6) && app.rehearsal.is_none() {
        errors.push(ValidationError::new(
            "application.rehearsal",
            "verification level L6 requires application.rehearsal.target — the scratch root the full recovery rehearsal restores into",
        ));
    }
    if let Some(rehearsal) = &app.rehearsal
        && rehearsal.target.trim().is_empty()
    {
        errors.push(ValidationError::new(
            "application.rehearsal.target",
            "must not be empty",
        ));
    }

    // Restore steps: every reference must resolve to a declared name.
    let source_names_for_restore = source_names;
    for (i, step) in app.restore.steps.iter().enumerate() {
        let base = format!("application.restore.steps[{i}]");
        match step {
            WireRestoreStep::RestoreFiles { source, target } => {
                if !source_names_for_restore.contains(source) {
                    errors.push(ValidationError::new(
                        format!("{base}.restore_files.source"),
                        format!("unknown source \"{source}\""),
                    ));
                }
                if target.is_empty() {
                    errors.push(ValidationError::new(
                        format!("{base}.restore_files.target"),
                        "must not be empty",
                    ));
                }
            }
            WireRestoreStep::RestoreDatabase {
                database,
                target_database,
            } => {
                if !database_names.contains(database) {
                    errors.push(ValidationError::new(
                        format!("{base}.restore_database.database"),
                        format!("unknown database \"{database}\""),
                    ));
                }
                if target_database.as_deref().is_some_and(|t| t.is_empty()) {
                    errors.push(ValidationError::new(
                        format!("{base}.restore_database.target_database"),
                        "must not be empty",
                    ));
                }
            }
            WireRestoreStep::RestoreVolume { volume } => {
                if !volume_names.contains(volume) {
                    errors.push(ValidationError::new(
                        format!("{base}.restore_volume.volume"),
                        format!("unknown volume \"{volume}\""),
                    ));
                }
            }
            WireRestoreStep::WaitHealthy { url } => {
                if url.is_empty() {
                    errors.push(ValidationError::new(
                        format!("{base}.wait_healthy.url"),
                        "must not be empty",
                    ));
                }
            }
        }
    }

    // Path map: non-empty halves, unique `from` prefixes (duplicates
    // would make the tie-break order-dependent instead of explicit).
    let mut seen_from = std::collections::HashSet::new();
    for (i, entry) in app.restore.path_map.iter().enumerate() {
        let base = format!("application.restore.path_map[{i}]");
        if entry.from.is_empty() {
            errors.push(ValidationError::new(
                format!("{base}.from"),
                "must not be empty",
            ));
        } else if !seen_from.insert(entry.from.as_str()) {
            errors.push(ValidationError::new(
                format!("{base}.from"),
                format!("duplicate from prefix \"{}\"", entry.from),
            ));
        }
        if entry.to.is_empty() {
            errors.push(ValidationError::new(
                format!("{base}.to"),
                "must not be empty",
            ));
        }
    }

    let application = if errors.is_empty() {
        Some(convert(
            &config.application,
            retention,
            level.expect("validated"),
        ))
    } else {
        None
    };

    ValidationOutcome {
        application,
        errors,
        warnings,
    }
}

/// The application-name rule: lowercase alphanumerics and dashes, at most 63
/// characters, starting and ending with an alphanumeric. Shared by
/// configuration validation and `vaultline init --name`.
pub fn validate_application_name(name: &str) -> std::result::Result<(), ValidationError> {
    let valid = !name.is_empty()
        && name.len() <= 63
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_alphanumeric());
    if valid {
        Ok(())
    } else {
        Err(ValidationError::new(
            "application.name",
            "must be 1-63 characters, lowercase alphanumerics and dashes, starting and ending with an alphanumeric",
        ))
    }
}

/// The configuration size limit (hardening, Phase 6): a recovery
/// definition is a small document; 10 MiB is thousands of times larger
/// than any real one and bounds the parser's worst case.
pub const CONFIG_SIZE_LIMIT: u64 = 10 * 1024 * 1024;

/// Load a configuration file end to end: read, parse, validate, convert.
/// Validation failures are aggregated into one [`VaultlineError`].
pub fn load(path: &Path) -> Result<Application> {
    let metadata = std::fs::metadata(path).map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            format!("cannot read configuration file {}", path.display()),
            e,
        )
    })?;
    if metadata.len() > CONFIG_SIZE_LIMIT {
        return Err(VaultlineError::new(
            ErrorKind::Config,
            format!(
                "configuration file {} is {} bytes — over the {} MiB limit",
                path.display(),
                metadata.len(),
                CONFIG_SIZE_LIMIT / (1024 * 1024)
            ),
        ));
    }
    let contents = std::fs::read_to_string(path).map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            format!("cannot read configuration file {}", path.display()),
            e,
        )
    })?;
    let parsed = parse_str(&contents)?;
    let outcome = validate(parsed);
    if outcome.is_valid() {
        Ok(outcome
            .application
            .expect("valid outcome carries the application"))
    } else {
        let report: Vec<String> = outcome.errors.iter().map(|e| e.to_string()).collect();
        Err(VaultlineError::new(
            ErrorKind::Config,
            format!(
                "{} configuration error(s):\n{}",
                outcome.errors.len(),
                report.join("\n")
            ),
        ))
    }
}

// ---------------------------------------------------------------------------
// Wire → model conversion (only called on fully validated input)
// ---------------------------------------------------------------------------

fn convert(
    app: &WireApplication,
    retention: RetentionPolicy,
    level: VerificationLevel,
) -> Application {
    let mut sources = Vec::new();
    for s in &app.sources.files {
        sources.push(Source {
            name: s.name.clone(),
            kind: SourceKind::Files(FileSource {
                paths: s.paths.clone(),
                excludes: s.excludes.clone(),
                quiesce: s.quiesce.as_ref().map(|q| Quiesce {
                    command: q.command.clone(),
                    args: q.args.clone(),
                }),
            }),
        });
    }
    for s in &app.sources.git {
        sources.push(Source {
            name: s.name.clone(),
            kind: SourceKind::Git(GitSource {
                remote: s.remote.clone(),
                reference: s.reference.clone(),
                capture: match s.capture {
                    WireGitCapture::Mirror => GitCapture::Mirror,
                    WireGitCapture::Reference => GitCapture::Reference,
                },
            }),
        });
    }
    for s in &app.sources.config_refs {
        sources.push(Source {
            name: s.name.clone(),
            kind: SourceKind::ConfigRef(ConfigRef {
                path: s.path.clone(),
                note: s.note.clone(),
            }),
        });
    }

    let databases = app
        .databases
        .iter()
        .map(|db| Database {
            name: db.name.clone(),
            kind: match db.kind {
                WireDatabaseType::PostgreSql => DatabaseType::PostgreSql,
                WireDatabaseType::MySql => DatabaseType::MySql,
                WireDatabaseType::MariaDb => DatabaseType::MariaDb,
                WireDatabaseType::Sqlite => DatabaseType::Sqlite,
            },
            connection: match db.kind {
                WireDatabaseType::Sqlite => {
                    DatabaseConnection::SqlitePath(db.path.clone().expect("validated"))
                }
                _ => DatabaseConnection::UrlEnv(db.url_env.clone().expect("validated")),
            },
            consistency: ConsistencyStrategy::LogicalDump {
                format: match db.consistency.logical.format {
                    WireDumpFormat::Custom => DumpFormat::Custom,
                    WireDumpFormat::Sql => DumpFormat::Sql,
                },
            },
        })
        .collect();

    let volumes = app
        .volumes
        .iter()
        .map(|v| Volume {
            name: v.name.clone(),
            capture: match v.capture {
                WireCaptureSemantics::Direct => CaptureSemantics::Direct,
                WireCaptureSemantics::Sidecar => CaptureSemantics::Sidecar,
                WireCaptureSemantics::PauseFirst => CaptureSemantics::PauseFirst,
            },
            container: v.container.clone(),
        })
        .collect();

    let storage = StorageTarget {
        kind: match app.storage.kind {
            WireStorageKind::Local => StorageKind::Local {
                path: app.storage.path.clone().expect("validated"),
            },
            WireStorageKind::S3 => StorageKind::S3Compatible {
                endpoint: app.storage.endpoint.clone().expect("validated"),
                bucket: app.storage.bucket.clone().expect("validated"),
                region: app.storage.region.clone(),
                access_key_env: app.storage.access_key_env.clone().expect("validated"),
                secret_key_env: app.storage.secret_key_env.clone().expect("validated"),
            },
            WireStorageKind::Sftp => StorageKind::Sftp {
                host: app.storage.host.clone().expect("validated"),
                port: app.storage.port.expect("validated"),
                user: app.storage.user.clone().expect("validated"),
                path: app.storage.path.clone().expect("validated"),
                key_file: app.storage.key_file.clone(),
                known_hosts: app.storage.known_hosts.clone(),
            },
        },
        repository: app.storage.repository.clone(),
        password_env: app.storage.password_env.clone(),
    };

    Application {
        name: app.name.clone(),
        description: app.description.clone(),
        schedule: app.schedule.clone(),
        sources,
        databases,
        volumes,
        storage,
        retention,
        verification: VerificationPolicy {
            level,
            schedule: app.verification.schedule.clone(),
            app_checks: app
                .verification
                .app_checks
                .iter()
                .map(|check| crate::model::AppCheck {
                    name: check.name.clone(),
                    command: check.command.clone(),
                    args: check.args.clone(),
                })
                .collect(),
        },
        rehearsal: app
            .rehearsal
            .as_ref()
            .map(|rehearsal| crate::model::RehearsalTarget {
                target: rehearsal.target.clone(),
            }),
        restore: RestoreProcedure {
            steps: app
                .restore
                .steps
                .iter()
                .map(|step| match step {
                    WireRestoreStep::RestoreFiles { source, target } => RestoreStep::RestoreFiles {
                        source: source.clone(),
                        target: target.clone(),
                    },
                    WireRestoreStep::RestoreDatabase {
                        database,
                        target_database,
                    } => RestoreStep::RestoreDatabase {
                        database: database.clone(),
                        target_database: target_database.clone(),
                    },
                    WireRestoreStep::RestoreVolume { volume } => RestoreStep::RestoreVolume {
                        volume: volume.clone(),
                    },
                    WireRestoreStep::WaitHealthy { url } => {
                        RestoreStep::WaitHealthy { url: url.clone() }
                    }
                })
                .collect(),
            path_map: app
                .restore
                .path_map
                .iter()
                .map(|entry| crate::model::PathMapEntry {
                    from: entry.from.clone(),
                    to: entry.to.clone(),
                })
                .collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full valid configuration — every field exercised. This is also the
    /// shape `vaultline init` writes (the CLI template is a comment-poorer
    /// variant of this).
    const VALID: &str = r#"
[application]
name = "thornwa"
description = "ThornWA compose stack"
schedule = "30 2 * * *"

[[application.sources.files]]
name = "uploads"
paths = ["/srv/thornwa/uploads"]
excludes = ["**/*.tmp"]

[[application.sources.git]]
name = "code"
remote = "https://example.com/thornwa.git"
reference = "main"
capture = "mirror"

[[application.sources.config_refs]]
name = "env"
path = "/srv/thornwa/.env"
note = "secrets — reference only"

[[application.databases]]
name = "main"
kind = "postgresql"
url_env = "THORNWA_DATABASE_URL"
consistency = { logical = { format = "custom" } }

[[application.volumes]]
name = "thornwa_pgdata"
capture = "direct"

[application.storage]
kind = "local"
path = "/var/backups/thornwa"
password_env = "RESTIC_PASSWORD_THORNWA"

[application.retention]
keep_last = 14
keep_daily = 7
keep_weekly = 4
keep_monthly = 6
keep_yearly = 1

[application.verification]
level = 3
schedule = "0 3 * * 7"

[[application.verification.app_checks]]
name = "pg-integrity"
command = "psql"
args = ["-c", "SELECT 1"]

[[application.restore.steps]]
restore_files = { source = "uploads", target = "/srv/thornwa/uploads" }
[[application.restore.steps]]
restore_database = { database = "main", target_database = "thornwa" }
[[application.restore.steps]]
restore_volume = { volume = "thornwa_pgdata" }
[[application.restore.steps]]
wait_healthy = { url = "http://localhost:3000/health" }

[[application.restore.path_map]]
from = "/srv"
to = "C:/srv"
"#;

    #[test]
    fn valid_config_converts() {
        let outcome = validate(parse_str(VALID).expect("parse"));
        assert!(
            outcome.is_valid(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        let app = outcome.application.expect("application");
        assert_eq!(app.name, "thornwa");
        assert_eq!(app.sources.len(), 3);
        assert_eq!(app.databases.len(), 1);
        assert_eq!(app.volumes.len(), 1);
        assert_eq!(app.restore.steps.len(), 4);
        assert_eq!(app.verification.level, VerificationLevel::L3);
        assert_eq!(
            app.schedule.as_deref(),
            Some("30 2 * * *"),
            "the backup schedule converts"
        );
        assert_eq!(app.verification.app_checks.len(), 1);
        assert_eq!(app.verification.app_checks[0].name, "pg-integrity");
        assert_eq!(app.verification.app_checks[0].command, "psql");
        assert_eq!(app.restore.path_map.len(), 1);
        assert_eq!(app.restore.path_map[0].from, "/srv");
        assert_eq!(app.restore.path_map[0].to, "C:/srv");
        // Everything the policy declares is executable now — no warnings.
        assert_eq!(outcome.warnings.len(), 0);
    }

    #[test]
    fn unknown_field_is_rejected_at_parse_time() {
        let contents = VALID.replace("password_env", "password");
        let err = parse_str(&contents).expect_err("unknown field must fail");
        assert!(err.to_string().contains("unknown field"), "{err}");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn invalid_toml_syntax_reports_span() {
        let err = parse_str("[application\nname =").expect_err("syntax error");
        assert!(err.to_string().contains("line 1"), "{err}");
    }

    #[test]
    fn bad_name_is_rejected() {
        let mut contents = VALID.to_string();
        contents = contents.replace("name = \"thornwa\"", "name = \"ThornWA!\"");
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(outcome.errors[0].path, "application.name");
    }

    #[test]
    fn empty_targets_are_rejected() {
        let contents = r#"
[application]
name = "empty"
[application.storage]
kind = "local"
path = "/var/backups/empty"
password_env = "RESTIC_PASSWORD"
[application.retention]
keep_last = 7
[application.verification]
level = 1
"#;
        let outcome = validate(parse_str(contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome.errors[0].message.contains("nothing to back up"),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let contents = r#"
[application]
name = "dup"
[[application.sources.files]]
name = "same"
paths = ["/a"]
[[application.sources.config_refs]]
name = "same"
path = "/b"
[application.storage]
kind = "local"
path = "/var/backups/dup"
password_env = "RESTIC_PASSWORD"
[application.retention]
keep_last = 7
[application.verification]
level = 1
"#;
        let outcome = validate(parse_str(contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.message.contains("duplicate")),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn all_zero_retention_is_rejected() {
        let contents = VALID
            .replace("keep_last = 14", "keep_last = 0")
            .replace("keep_daily = 7", "keep_daily = 0")
            .replace("keep_weekly = 4", "keep_weekly = 0")
            .replace("keep_monthly = 6", "keep_monthly = 0")
            .replace("keep_yearly = 1", "keep_yearly = 0");
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.message.contains("delete everything"))
        );
    }

    #[test]
    fn level_out_of_range_is_rejected() {
        let contents = VALID.replace("level = 3", "level = 9");
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.message.contains("between 1 and 6"))
        );
    }

    #[test]
    fn dangling_restore_reference_is_rejected() {
        let contents = VALID.replace("source = \"uploads\"", "source = \"nope\"");
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.message.contains("unknown source \"nope\""))
        );
    }

    #[test]
    fn pause_first_requires_its_writer_container() {
        let contents = VALID.replace("capture = \"direct\"", "capture = \"pause-first\"");
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome.errors.iter().any(|e| {
                e.path.contains(".container") && e.message.contains("pause-first capture requires")
            }),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn pause_first_with_container_converts() {
        let contents = VALID.replace(
            "capture = \"direct\"",
            "capture = \"pause-first\"\ncontainer = \"app\"",
        );
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(outcome.is_valid(), "{:?}", outcome.errors);
        let app = outcome.application.expect("application");
        assert_eq!(
            app.volumes[0].container.as_deref(),
            Some("app"),
            "the writer container converts"
        );
    }

    #[test]
    fn container_is_rejected_on_non_pause_first_capture() {
        let contents = VALID.replace(
            "capture = \"direct\"",
            "capture = \"direct\"\ncontainer = \"app\"",
        );
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome.errors.iter().any(|e| {
                e.path.contains(".container") && e.message.contains("only meaningful with capture")
            }),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn path_map_entries_are_validated() {
        let empty_from = VALID.replace("from = \"/srv\"", "from = \"\"");
        let outcome = validate(parse_str(&empty_from).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.path.contains(".path_map[0].from")),
            "{:?}",
            outcome.errors
        );

        let empty_to = VALID.replace("to = \"C:/srv\"", "to = \"\"");
        let outcome = validate(parse_str(&empty_to).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.path.contains(".path_map[0].to")),
            "{:?}",
            outcome.errors
        );

        let duplicate = format!(
            "{}\n[[application.restore.path_map]]\nfrom = \"/srv\"\nto = \"D:/srv\"\n",
            VALID
        );
        let outcome = validate(parse_str(&duplicate).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome.errors.iter().any(|e| {
                e.path.contains(".path_map[1].from") && e.message.contains("duplicate from prefix")
            }),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn server_database_requires_url_env() {
        let contents = VALID.replace("url_env = \"THORNWA_DATABASE_URL\"\n", "");
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(outcome.errors.iter().any(|e| e.path.contains("url_env")));
    }

    #[test]
    fn sqlite_forbids_custom_format() {
        let contents = r#"
[application]
name = "sqlite-app"
[[application.databases]]
name = "db"
kind = "sqlite"
path = "/srv/app/app.db"
consistency = { logical = { format = "custom" } }
[application.storage]
kind = "local"
path = "/var/backups/sqlite-app"
password_env = "RESTIC_PASSWORD"
[application.retention]
keep_last = 7
[application.verification]
level = 1
"#;
        let outcome = validate(parse_str(contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.message.contains("backup API"))
        );
    }

    #[test]
    fn sqlite_requires_path_not_url() {
        let contents = r#"
[application]
name = "sqlite-app"
[[application.databases]]
name = "db"
kind = "sqlite"
url_env = "SQLITE_URL"
consistency = { logical = { format = "sql" } }
[application.storage]
kind = "local"
path = "/var/backups/sqlite-app"
password_env = "RESTIC_PASSWORD"
[application.retention]
keep_last = 7
[application.verification]
level = 1
"#;
        let outcome = validate(parse_str(contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.message.contains("require the file path"))
        );
    }

    #[test]
    fn s3_requires_credentials() {
        let contents = VALID.replace(
            "[application.storage]\nkind = \"local\"\npath = \"/var/backups/thornwa\"\n",
            "[application.storage]\nkind = \"s3\"\nendpoint = \"https://s3.example.com\"\nbucket = \"backups\"\n",
        );
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.path.contains("access_key_env"))
        );
    }

    #[test]
    fn sftp_requires_host_user_and_path() {
        let contents = VALID.replace(
            "[application.storage]\nkind = \"local\"\npath = \"/var/backups/thornwa\"\n",
            "[application.storage]\nkind = \"sftp\"\nhost = \"backup.example.com\"\nuser = \"backup\"\n",
        );
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.path.contains("storage.path"))
        );
    }

    #[test]
    fn name_validator_cases() {
        assert!(validate_application_name("thornwa").is_ok());
        assert!(validate_application_name("thorn-wa2").is_ok());
        assert!(validate_application_name("a").is_ok());
        assert!(validate_application_name("").is_err());
        assert!(validate_application_name("Thornwa").is_err());
        assert!(validate_application_name("thorn_wa").is_err());
        assert!(validate_application_name("-thornwa").is_err());
        assert!(validate_application_name("thornwa-").is_err());
        assert!(validate_application_name(&"a".repeat(64)).is_err());
        assert!(validate_application_name(&"a".repeat(63)).is_ok());
    }

    #[test]
    fn invalid_schedules_are_rejected_at_validation() {
        let mut contents = VALID.to_string();
        contents = contents.replace(
            "schedule = \"30 2 * * *\"",
            "schedule = \"2 30 * * * 2026\"",
        );
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.path == "application.schedule"),
            "{:?}",
            outcome.errors
        );

        let mut contents = VALID.to_string();
        contents = contents.replace("schedule = \"0 3 * * 7\"", "schedule = \"at three\"");
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.path == "application.verification.schedule"),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn l6_requires_a_rehearsal_target() {
        let mut contents = VALID.to_string();
        contents = contents.replace("level = 3", "level = 6");
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.path == "application.rehearsal"),
            "{:?}",
            outcome.errors
        );

        // With a rehearsal target the L6 definition validates.
        contents.push_str("[application.rehearsal]\ntarget = \"/srv/rehearsal\"\n");
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(outcome.is_valid(), "{:?}", outcome.errors);
        let app = outcome.application.expect("application");
        assert_eq!(app.rehearsal.expect("rehearsal").target, "/srv/rehearsal");
    }

    #[test]
    fn duplicate_app_check_names_are_rejected() {
        let mut contents = VALID.to_string();
        contents = contents.replace(
            "name = \"pg-integrity\"",
            "name = \"pg-integrity\"\ncommand = \"psql\"\nargs = [\"-c\", \"SELECT 1\"]\n\n[[application.verification.app_checks]]\nname = \"pg-integrity\"",
        );
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.message.contains("duplicate app check")),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn app_checks_require_name_and_command() {
        let mut contents = VALID.to_string();
        contents = contents.replace("command = \"psql\"", "command = \"\"");
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(!outcome.is_valid());
        assert!(
            outcome.errors.iter().any(|e| e.path.ends_with(".command")),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn oversized_configuration_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("huge.toml");
        // 10 MiB + 1 byte of comments — over the limit.
        let mut contents = String::with_capacity((CONFIG_SIZE_LIMIT + 1) as usize);
        contents.push_str("# ");
        contents.push_str(&"x".repeat(CONFIG_SIZE_LIMIT as usize));
        std::fs::write(&path, contents).expect("write");
        let err = load(&path).expect_err("over the limit");
        assert!(err.to_string().contains("limit"), "{err}");
        assert_eq!(err.exit_code(), 2);
    }

    /// The hardening harness (Phase 6): hostile configuration input must
    /// never panic the parser or the validator — errors are the contract,
    /// not crashes. Deterministic mutations of the valid template
    /// (byte flips, insertions, deletions) with a fixed seed, so a
    /// regression reproduces exactly.
    #[test]
    fn mutated_configurations_never_panic() {
        // A tiny deterministic PRNG (no dependency for the harness).
        let mut state: u64 = 0x5eed_2026_0907;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };

        let bytes = VALID.as_bytes().to_vec();
        for round in 0..2000 {
            let mut mutated = bytes.clone();
            let mutations = 1 + next() % 8;
            for _ in 0..mutations {
                match next() % 3 {
                    0 if !mutated.is_empty() => {
                        // Flip one byte.
                        let at = next() % mutated.len();
                        mutated[at] = mutated[at].wrapping_add(1 + (next() % 255) as u8);
                    }
                    1 if mutated.len() < 4096 => {
                        // Insert a byte.
                        let at = next() % (mutated.len() + 1);
                        mutated.insert(at, next() as u8);
                    }
                    _ if mutated.len() > 1 => {
                        // Delete a byte.
                        let at = next() % mutated.len();
                        mutated.remove(at);
                    }
                    _ => {}
                }
            }
            let input = String::from_utf8_lossy(&mutated).into_owned();
            let result = std::panic::catch_unwind(|| {
                if let Ok(parsed) = parse_str(&input) {
                    let _ = validate(parsed);
                }
            });
            assert!(
                result.is_ok(),
                "mutation round {round} panicked on input: {input:?}"
            );
        }
    }

    #[test]
    fn relative_paths_warn_but_do_not_fail() {
        let contents = VALID.replace("/srv/thornwa/uploads", "uploads");
        let outcome = validate(parse_str(&contents).expect("parse"));
        assert!(outcome.is_valid());
        assert!(
            outcome
                .warnings
                .iter()
                .any(|w| w.message.contains("relative"))
        );
    }
}
