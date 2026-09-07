//! Per-engine database capture (ADR-V0-3): consistency through each
//! engine's own tooling — never by copying live database files.
//!
//! - PostgreSQL: `pg_dump --format=custom` (the MVP mechanism). The
//!   connection info travels **sanitized** (password stripped) as
//!   `--dbname`; authentication goes through a temporary PGPASSFILE
//!   (removed after the dump). Credentials never appear in argv.
//! - MySQL/MariaDB: `mysqldump --single-transaction --routines --triggers
//!   --events`; the password travels in the `MYSQL_PWD` environment
//!   variable, never argv.
//! - SQLite: the CLI's `.backup` meta-command (the Online Backup API) —
//!   safe under WAL mode, unlike `cp`.
//!
//! Tool location: `VAULTLINE_PGDUMP` / `VAULTLINE_MYSQLDUMP` /
//! `VAULTLINE_SQLITE3` overrides, else the tool on PATH.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use tracing::info;

use vaultline_core::error::{ErrorKind, Result, VaultlineError};
use vaultline_core::model::{
    Database, DatabaseConnection, DatabaseSnapshotMeta, DatabaseType, DumpFormat,
};

/// A captured dump: the staging directory (kept alive until the backup
/// finishes) and the dump file inside it.
pub struct Capture {
    pub staging: TempDir,
    pub dump_path: PathBuf,
    pub meta: DatabaseSnapshotMeta,
}

/// Capture one database into a staging dump file. Returns the dump path
/// (which the caller adds to the restic backup) and the snapshot metadata
/// that records exactly how the dump was made.
pub fn capture_database(db: &Database) -> Result<Capture> {
    let staging = tempfile::tempdir().map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            format!(
                "cannot create the staging directory for database \"{}\"",
                db.name
            ),
            e,
        )
    })?;

    let (dump_path, meta) = match db.kind {
        DatabaseType::PostgreSql => {
            let conninfo = resolve_connection_string(&db.connection)?;
            let parsed = parse_conninfo(&conninfo)?;
            let dump_path = staging.path().join(format!("{}.dump", db.name));
            capture_postgresql(&db.name, &parsed, &dump_path)?;
            (
                dump_path,
                DatabaseSnapshotMeta {
                    name: db.name.clone(),
                    engine: DatabaseType::PostgreSql,
                    mechanism: "pg_dump -Fc".to_string(),
                    dump_format: DumpFormat::Custom,
                },
            )
        }
        DatabaseType::MySql | DatabaseType::MariaDb => {
            let conninfo = resolve_connection_string(&db.connection)?;
            let parsed = parse_conninfo(&conninfo)?;
            let dump_path = staging.path().join(format!("{}.sql", db.name));
            capture_mysql(&db.name, &parsed, &dump_path)?;
            (
                dump_path,
                DatabaseSnapshotMeta {
                    name: db.name.clone(),
                    engine: db.kind,
                    mechanism: "mysqldump --single-transaction --routines --triggers --events"
                        .to_string(),
                    dump_format: DumpFormat::Sql,
                },
            )
        }
        DatabaseType::Sqlite => {
            let DatabaseConnection::SqlitePath(path) = &db.connection else {
                return Err(VaultlineError::new(
                    ErrorKind::Internal,
                    format!(
                        "database \"{}\" is sqlite but has no path — this is a bug",
                        db.name
                    ),
                ));
            };
            let dump_path = staging.path().join(format!("{}.db", db.name));
            capture_sqlite(&db.name, path, &dump_path)?;
            (
                dump_path,
                DatabaseSnapshotMeta {
                    name: db.name.clone(),
                    engine: DatabaseType::Sqlite,
                    mechanism: "sqlite3 .backup (Online Backup API)".to_string(),
                    dump_format: DumpFormat::Sql,
                },
            )
        }
    };

    Ok(Capture {
        staging,
        dump_path,
        meta,
    })
}

fn resolve_connection_string(
    connection: &vaultline_core::model::DatabaseConnection,
) -> Result<String> {
    let DatabaseConnection::UrlEnv(env_name) = connection else {
        return Err(VaultlineError::new(
            ErrorKind::Internal,
            "a server database without a connection environment variable — this is a bug"
                .to_string(),
        ));
    };
    std::env::var(env_name).map_err(|_| {
        VaultlineError::new(
            ErrorKind::Config,
            format!("environment variable {env_name} (the database connection string) is not set"),
        )
    })
}

/// A parsed connection description: either a URI (`scheme://user:pass@
/// host:port/db`) or a libpq-style key=value string.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnInfo {
    pub scheme: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub dbname: Option<String>,
    /// Extra key=value pairs (kept verbatim, minus secrets).
    pub extras: BTreeMap<String, String>,
}

impl ConnInfo {
    /// The connection description with every secret removed — safe for
    /// argv.
    pub fn sanitized(&self) -> String {
        if let Some(scheme) = &self.scheme {
            let mut out = format!("{scheme}://");
            if let Some(user) = &self.user {
                out.push_str(user);
                out.push('@');
            }
            if let Some(host) = &self.host {
                out.push_str(host);
                if let Some(port) = self.port {
                    out.push_str(&format!(":{port}"));
                }
            }
            if let Some(db) = &self.dbname {
                out.push('/');
                out.push_str(db);
            }
            out
        } else {
            let mut pairs: Vec<String> = Vec::new();
            if let Some(host) = &self.host {
                pairs.push(format!("host={host}"));
            }
            if let Some(port) = self.port {
                pairs.push(format!("port={port}"));
            }
            if let Some(user) = &self.user {
                pairs.push(format!("user={user}"));
            }
            if let Some(db) = &self.dbname {
                pairs.push(format!("dbname={db}"));
            }
            pairs.extend(self.extras.iter().map(|(k, v)| format!("{k}={v}")));
            pairs.join(" ")
        }
    }
}

/// Parse a connection string: URI form or libpq key=value form. The
/// parser is deliberately minimal (the goal is secret separation, not a
/// full driver) — unrecognized pieces are kept as extras.
pub fn parse_conninfo(value: &str) -> Result<ConnInfo> {
    if value.contains("://") {
        parse_uri(value)
    } else if value.contains('=') {
        parse_key_value(value)
    } else {
        Err(VaultlineError::new(
            ErrorKind::Config,
            "the database connection string is neither a URI (scheme://user:pass@host:port/db) nor a key=value string".to_string(),
        ))
    }
}

fn parse_uri(value: &str) -> Result<ConnInfo> {
    let (scheme, rest) = value.split_once("://").expect("checked contains");
    let (userinfo_host, dbname) = match rest.split_once('/') {
        Some((left, db)) => (left, Some(db.to_string())),
        None => (rest, None),
    };
    let (userinfo, hostport) = match userinfo_host.rsplit_once('@') {
        Some((user, host)) => (Some(user.to_string()), host),
        None => (None, userinfo_host),
    };
    let (user, password) = match userinfo.as_deref().and_then(|u| u.split_once(':')) {
        Some((u, p)) => (Some(u.to_string()), Some(p.to_string())),
        None => (userinfo, None),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (Some(h.to_string()), p.parse::<u16>().ok()),
        None => {
            if hostport.is_empty() {
                (None, None)
            } else {
                (Some(hostport.to_string()), None)
            }
        }
    };
    Ok(ConnInfo {
        scheme: Some(scheme.to_string()),
        user,
        password,
        host,
        port,
        dbname,
        extras: BTreeMap::new(),
    })
}

fn parse_key_value(value: &str) -> Result<ConnInfo> {
    let mut info = ConnInfo {
        scheme: None,
        user: None,
        password: None,
        host: None,
        port: None,
        dbname: None,
        extras: BTreeMap::new(),
    };
    for pair in value.split_whitespace() {
        let Some((key, val)) = pair.split_once('=') else {
            return Err(VaultlineError::new(
                ErrorKind::Config,
                format!("invalid connection parameter \"{pair}\""),
            ));
        };
        match key {
            "host" => info.host = Some(val.to_string()),
            "port" => {
                info.port = Some(val.parse::<u16>().map_err(|_| {
                    VaultlineError::new(ErrorKind::Config, format!("invalid port \"{val}\""))
                })?)
            }
            "user" => info.user = Some(val.to_string()),
            "password" => info.password = Some(val.to_string()),
            "dbname" => info.dbname = Some(val.to_string()),
            _ => {
                info.extras.insert(key.to_string(), val.to_string());
            }
        }
    }
    Ok(info)
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

fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines().rev().take(3).collect::<Vec<_>>().join(" | ")
}

/// PostgreSQL: dump to the staging file via stdout capture; authentication
/// through a temporary PGPASSFILE so the password never reaches argv.
fn capture_postgresql(name: &str, conn: &ConnInfo, dump_path: &Path) -> Result<()> {
    let pg_dump = locate_tool("VAULTLINE_PGDUMP", "pg_dump")?;
    let mut auth = PgAuth::None;
    if let Some(password) = &conn.password {
        auth = PgAuth::write_passfile(conn, password)?;
    }
    info!(database = name, tool = %pg_dump.display(), "running pg_dump");
    let mut command = Command::new(&pg_dump);
    command
        .args(["--format=custom", "--dbname"])
        .arg(conn.sanitized());
    if let PgAuth::PassFile { env, .. } = &auth {
        command.env("PGPASSFILE", env);
    }
    let output = command.output().map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Operational,
            format!(
                "cannot start pg_dump ({}): install it or set VAULTLINE_PGDUMP",
                pg_dump.display()
            ),
            e,
        )
    })?;
    if !output.status.success() {
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "pg_dump failed for database \"{name}\": {}",
                stderr_tail(&output.stderr)
            ),
        ));
    }
    std::fs::write(dump_path, &output.stdout).map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            format!("cannot write the dump file {}", dump_path.display()),
            e,
        )
    })
}

/// Where PostgreSQL authentication comes from: nothing (trust auth), or a
/// temporary passfile whose contents follow the pgpass format
/// (host:port:dbname:user:password).
enum PgAuth {
    None,
    PassFile {
        _file: tempfile::NamedTempFile,
        env: PathBuf,
    },
}

impl PgAuth {
    fn write_passfile(conn: &ConnInfo, password: &str) -> Result<Self> {
        let mut file = tempfile::NamedTempFile::new().map_err(|e| {
            VaultlineError::with_source(ErrorKind::Io, "cannot create the PGPASSFILE", e)
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600)).map_err(
                |e| {
                    VaultlineError::with_source(
                        ErrorKind::Io,
                        "cannot restrict the PGPASSFILE permissions",
                        e,
                    )
                },
            )?;
        }
        let line = format!(
            "{}:{}:{}:{}:{}\n",
            conn.host.as_deref().unwrap_or("localhost"),
            conn.port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "*".to_string()),
            conn.dbname.as_deref().unwrap_or("*"),
            conn.user.as_deref().unwrap_or("*"),
            password,
        );
        file.write_all(line.as_bytes()).map_err(|e| {
            VaultlineError::with_source(ErrorKind::Io, "cannot write the PGPASSFILE", e)
        })?;
        file.flush().map_err(|e| {
            VaultlineError::with_source(ErrorKind::Io, "cannot write the PGPASSFILE", e)
        })?;
        let env = file.path().to_path_buf();
        Ok(PgAuth::PassFile { _file: file, env })
    }
}

/// MySQL/MariaDB: `mysqldump` with the transactional consistency flags;
/// the password travels in MYSQL_PWD, never argv.
fn capture_mysql(name: &str, conn: &ConnInfo, dump_path: &Path) -> Result<()> {
    let mysqldump = locate_tool("VAULTLINE_MYSQLDUMP", "mysqldump")?;
    let mut command = Command::new(&mysqldump);
    command.args([
        "--single-transaction",
        "--routines",
        "--triggers",
        "--events",
    ]);
    if let Some(host) = &conn.host {
        command.arg("--host").arg(host);
    }
    if let Some(port) = conn.port {
        command.arg("--port").arg(port.to_string());
    }
    if let Some(user) = &conn.user {
        command.arg("--user").arg(user);
    }
    if let Some(password) = &conn.password {
        command.env("MYSQL_PWD", password);
    }
    if let Some(dbname) = &conn.dbname {
        command.arg("--databases").arg(dbname);
    }
    info!(database = name, tool = %mysqldump.display(), "running mysqldump");
    let output = command.output().map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Operational,
            format!(
                "cannot start mysqldump ({}): install it or set VAULTLINE_MYSQLDUMP",
                mysqldump.display()
            ),
            e,
        )
    })?;
    if !output.status.success() {
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "mysqldump failed for database \"{name}\": {}",
                stderr_tail(&output.stderr)
            ),
        ));
    }
    std::fs::write(dump_path, &output.stdout).map_err(|e| {
        VaultlineError::with_source(
            ErrorKind::Io,
            format!("cannot write the dump file {}", dump_path.display()),
            e,
        )
    })
}

/// SQLite: the CLI's `.backup` meta-command — the Online Backup API,
/// consistent even under WAL mode (unlike `cp`).
fn capture_sqlite(name: &str, db_path: &str, dump_path: &Path) -> Result<()> {
    let sqlite3 = locate_tool("VAULTLINE_SQLITE3", "sqlite3")?;
    info!(database = name, tool = %sqlite3.display(), "running sqlite3 .backup");
    let output = Command::new(&sqlite3)
        .arg(db_path)
        .arg(format!(".backup '{}'", dump_path.display()))
        .output()
        .map_err(|e| {
            VaultlineError::with_source(
                ErrorKind::Operational,
                format!(
                    "cannot start sqlite3 ({}): install it or set VAULTLINE_SQLITE3",
                    sqlite3.display()
                ),
                e,
            )
        })?;
    if !output.status.success() {
        return Err(VaultlineError::new(
            ErrorKind::Operational,
            format!(
                "sqlite3 .backup failed for database \"{name}\": {}",
                stderr_tail(&output.stderr)
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_parsing_extracts_every_component() {
        let info =
            parse_conninfo("postgresql://user:secret@db.example.com:5433/app").expect("parse");
        assert_eq!(info.scheme.as_deref(), Some("postgresql"));
        assert_eq!(info.user.as_deref(), Some("user"));
        assert_eq!(info.password.as_deref(), Some("secret"));
        assert_eq!(info.host.as_deref(), Some("db.example.com"));
        assert_eq!(info.port, Some(5433));
        assert_eq!(info.dbname.as_deref(), Some("app"));
    }

    #[test]
    fn uri_without_password_or_port_parses() {
        let info = parse_conninfo("mysql://root@localhost/app").expect("parse");
        assert_eq!(info.user.as_deref(), Some("root"));
        assert_eq!(info.password, None);
        assert_eq!(info.port, None);
    }

    #[test]
    fn key_value_parsing_extracts_and_preserves_extras() {
        let info = parse_conninfo(
            "host=localhost port=5432 user=app password=hunter2 dbname=app sslmode=require",
        )
        .expect("parse");
        assert_eq!(info.host.as_deref(), Some("localhost"));
        assert_eq!(info.port, Some(5432));
        assert_eq!(info.password.as_deref(), Some("hunter2"));
        assert_eq!(
            info.extras.get("sslmode").map(String::as_str),
            Some("require")
        );
    }

    #[test]
    fn sanitized_uri_never_contains_the_password() {
        let info =
            parse_conninfo("postgresql://user:secret@db.example.com:5433/app").expect("parse");
        let clean = info.sanitized();
        assert!(!clean.contains("secret"), "{clean}");
        assert!(clean.contains("user@db.example.com:5433/app"), "{clean}");
    }

    #[test]
    fn sanitized_key_value_never_contains_the_password() {
        let info = parse_conninfo("host=h port=1 user=u password=s3cret dbname=d").expect("parse");
        let clean = info.sanitized();
        assert!(!clean.contains("s3cret"), "{clean}");
        assert!(clean.contains("host=h"), "{clean}");
        assert!(clean.contains("dbname=d"), "{clean}");
    }

    #[test]
    fn unparsable_connection_strings_are_config_errors() {
        let err = parse_conninfo("just-a-dbname").expect_err("no scheme, no =");
        assert_eq!(err.kind, ErrorKind::Config);
        let err = parse_conninfo("host=localhost nonsense").expect_err("broken pair");
        assert!(err.to_string().contains("nonsense"));
    }

    #[test]
    fn pgpass_line_has_the_documented_shape() {
        let info =
            parse_conninfo("postgresql://user:secret@db.example.com:5433/app").expect("parse");
        let auth = PgAuth::write_passfile(&info, "secret").expect("passfile");
        let PgAuth::PassFile { env, .. } = auth else {
            panic!("test sets a password, so a passfile must be written");
        };
        let contents = std::fs::read_to_string(&env).expect("read");
        assert_eq!(contents, "db.example.com:5433:app:user:secret\n");
    }
}
