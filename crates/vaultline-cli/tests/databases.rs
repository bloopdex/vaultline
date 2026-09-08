//! Database capture integration tests: SQLite (runs wherever sqlite3 is
//! available), PostgreSQL (container-gated: needs Docker + a pg_dump shim),
//! MinIO S3 (container-gated: needs Docker). Each test skips with a note
//! when its requirements are absent — docs/testing.md documents the gating.

use std::path::{Path, PathBuf};
use std::process::Command;

use predicates::prelude::*;

fn vaultline() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("vaultline").expect("binary built by cargo")
}

/// The restic binary for the tests, or `None` (tests skip).
fn restic_bin() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("VAULTLINE_RESTIC_BIN") {
        let path = PathBuf::from(bin);
        return path.is_file().then_some(path);
    }
    let output = Command::new("restic").arg("version").output().ok()?;
    output.status.success().then(|| PathBuf::from("restic"))
}

/// The sqlite3 binary (same override the product uses), or `None`.
fn sqlite3_bin() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("VAULTLINE_SQLITE3") {
        let path = PathBuf::from(bin);
        return path.is_file().then_some(path);
    }
    let output = Command::new("sqlite3").arg("--version").output().ok()?;
    output.status.success().then(|| PathBuf::from("sqlite3"))
}

/// Docker available (engine responding), or `None`.
fn docker_available() -> Option<()> {
    let output = Command::new("docker").arg("info").output().ok()?;
    output.status.success().then_some(())
}

fn restic(repo: &Path, args: &[&str]) -> std::process::Output {
    let bin = restic_bin().expect("restic present (test is gated)");
    Command::new(bin)
        .arg("-r")
        .arg(repo)
        .args(args)
        .env("RESTIC_PASSWORD", "test-password")
        .output()
        .expect("restic runs")
}

/// Restore one file from the latest snapshot by include pattern and
/// return its bytes. (`restic dump` cannot address Windows snapshots —
/// their paths are stored as /C/... and the dump path is cleaned with
/// Windows semantics, a restic-on-Windows quirk recorded here.)
fn restore_file(repo: &Path, include: &str, file_name: &str, target: &Path) -> Vec<u8> {
    let output = restic(
        repo,
        &[
            "restore",
            "latest",
            "--target",
            target.to_str().expect("utf8"),
            "--include",
            include,
        ],
    );
    // The verification target is the FILE. restic may exit 1 on Windows
    // after restoring it, because it cannot set timestamps on the
    // intermediate directories it recreated ("failed to restore timestamp
    // ... Access is denied") — an environmental quirk, recorded here; the
    // file itself is what proves restorability.
    let found = find_file(target, file_name);
    if found.is_none() {
        panic!(
            "restic restore produced no {file_name}: exit {:?}, stderr {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::read(found.expect("restored file present")).expect("read restored file")
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

fn sqlite3(args: &[&str]) -> std::process::Output {
    let bin = sqlite3_bin().expect("sqlite3 present (test is gated)");
    Command::new(bin).args(args).output().expect("sqlite3 runs")
}

/// A local-storage fixture with one file source and room for databases.
struct Fixture {
    _dir: tempfile::TempDir,
    config: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let uploads = dir.path().join("uploads");
        std::fs::create_dir_all(&uploads).expect("uploads dir");
        std::fs::write(uploads.join("hello.txt"), "hello world").expect("file");
        let repo_parent = dir.path().join("repos");
        let config = dir.path().join("vaultline.toml");
        let toml_path = |p: &Path| p.display().to_string().replace('\\', "/");
        let contents = format!(
            r#"
[application]
name = "thornwa"
[[application.sources.files]]
name = "uploads"
paths = ["{uploads}"]
[application.storage]
kind = "local"
path = "{repo_parent}"
password_env = "VAULTLINE_TEST_PASSWORD"
[application.retention]
keep_last = 14
[application.verification]
level = 2
"#,
            uploads = toml_path(&uploads),
            repo_parent = toml_path(&repo_parent),
        );
        std::fs::write(&config, contents).expect("config");
        Self {
            _dir: dir,
            config,
            repo: repo_parent.join("thornwa"),
        }
    }

    fn state_dir(&self) -> PathBuf {
        self._dir.path().join("state")
    }
}

fn append_database(config: &Path, block: &str) {
    let contents = std::fs::read_to_string(config).expect("config") + block;
    std::fs::write(config, contents).expect("config");
}

/// SQLite end-to-end (no containers): the dump is captured through the
/// Online Backup API, recorded in the snapshot metadata, lands in the
/// repository, and the restored copy passes integrity_check + row counts
/// (the ADR's restore-side semantic check, L5-lite).
#[test]
fn sqlite_database_is_dumped_verified_and_restorable() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    }
    if sqlite3_bin().is_none() {
        eprintln!("skipping: sqlite3 not available (VAULTLINE_SQLITE3 or PATH)");
        return;
    }
    let fixture = Fixture::new();
    let db_path = fixture._dir.path().join("app.sqlite");

    // A WAL-mode database with data (WAL is exactly the mode where `cp`
    // would be unsafe — the backup API is the point).
    let create = sqlite3(&[
        db_path.to_str().expect("utf8"),
        "PRAGMA journal_mode=WAL; CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1),(2),(3);",
    ]);
    assert!(create.status.success(), "sqlite3 create failed");

    append_database(
        &fixture.config,
        &format!(
            r#"
[[application.databases]]
name = "appdb"
kind = "sqlite"
path = "{}"
consistency = {{ logical = {{ format = "sql" }} }}
"#,
            db_path.display().to_string().replace('\\', "/")
        ),
    );

    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("backup complete"));

    // The snapshot metadata records the engine, the mechanism, and the
    // format; the database is in the reconstructs list.
    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.state_dir().join("state.json")).expect("state file"),
    )
    .expect("state parses");
    let snapshot = &state["applications"]["thornwa"]["snapshots"][0];
    let meta = &snapshot["database_metadata"][0];
    assert_eq!(meta["name"], "appdb");
    assert_eq!(meta["engine"], "sqlite");
    assert!(
        meta["mechanism"]
            .as_str()
            .unwrap()
            .contains("Online Backup API")
    );
    assert!(
        snapshot["restore_metadata"]["reconstructs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str() == Some("appdb"))
    );

    // The dump is in the repository; the live database file is not a
    // backup path and must not appear.
    let ls = restic(&fixture.repo, &["ls", "latest"]);
    let listing = String::from_utf8_lossy(&ls.stdout);
    assert!(listing.contains("appdb.db"), "{listing}");
    assert!(!listing.contains("app.sqlite"), "{listing}");

    // The defining proof: restore the dump and check it like a real
    // restore would (integrity_check + row counts).
    let restore_target = fixture._dir.path().join("restore");
    let dump_bytes = restore_file(&fixture.repo, "*appdb.db", "appdb.db", &restore_target);
    let restored = fixture._dir.path().join("restored.db");
    std::fs::write(&restored, &dump_bytes).expect("write restored db");
    let check = sqlite3(&[
        restored.to_str().expect("utf8"),
        "PRAGMA integrity_check; SELECT COUNT(*) FROM t;",
    ]);
    let check_out = String::from_utf8_lossy(&check.stdout);
    assert!(check.status.success(), "{check_out}");
    assert!(check_out.contains("ok"), "{check_out}");
    assert!(check_out.contains('3'), "{check_out}");
}

/// The live SQLite file is never captured directly — only the dump enters
/// the repository. (Also: the dump of a WAL-mode database is consistent
/// while a plain copy may not be — the mechanism itself is the guarantee.)
#[test]
fn sqlite_dump_excludes_the_live_database() {
    // Covered by sqlite_database_is_dumped_verified_and_restorable via the
    // repository listing assertion; this test pins the config-validation
    // half: sqlite requires format = "sql" (the backup API only produces
    // plain SQL), so no custom-format sqlite dump can slip through.
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
    let err = vaultline_core::config::parse_str(contents).expect("parses");
    let outcome = vaultline_core::config::validate(err);
    assert!(!outcome.is_valid());
}

/// PostgreSQL end-to-end (container-gated): a real testcontainers
/// PostgreSQL, a pg_dump shim that forwards into a container, and the
/// full capture → record → verify cycle. Requires Docker.
#[test]
fn postgresql_database_is_dumped_and_recorded() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available");
        return;
    }
    if docker_available().is_none() {
        eprintln!("skipping: Docker engine not running (container-gated test)");
        return;
    }
    use testcontainers::runners::SyncRunner as _;

    let fixture = Fixture::new();

    // The database container.
    let postgres = testcontainers_modules::postgres::Postgres::default();
    let node = postgres.start().expect("start postgres");
    let port = node.get_host_port_ipv4(5432).expect("mapped port");

    // Seed data (psql runs inside the container, reaching the host-mapped
    // port through host.docker.internal — the same path pg_dump will use).
    // The testcontainers postgres module defaults: user postgres,
    // password postgres, database postgres. Seed with psql INSIDE the
    // container (docker exec — uniform on every platform).
    let psql = |sql: &str| {
        Command::new("docker")
            .args([
                "exec",
                node.id(),
                "psql",
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-c",
                sql,
            ])
            .output()
            .expect("docker exec psql runs")
    };
    let seed = psql("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1),(2),(3);");
    assert!(
        seed.status.success(),
        "{:?}",
        String::from_utf8_lossy(&seed.stderr)
    );

    // The connection URL: from the host, the mapped port. On Windows there
    // is no host pg_dump, so the product's VAULTLINE_PGDUMP points at a
    // shim that runs the real pg_dump inside the postgres image, reaching
    // the host-mapped port through host.docker.internal and translating
    // PGPASSFILE into PGPASSWORD (the test password has no colons; the
    // shim is test-only — the product's argv contract is what is under
    // test). On Unix, pg_dump comes from PATH (postgresql-client on CI).
    let db_url = if cfg!(windows) {
        format!("postgresql://postgres:postgres@host.docker.internal:{port}/postgres")
    } else {
        // 127.0.0.1, never localhost: on the hosted runner localhost
        // resolves to ::1 first (pg) and the mysql client maps the
        // name to the unix socket — both miss the container's
        // published TCP port (first-hosted-run finding).
        format!("postgresql://postgres:postgres@127.0.0.1:{port}/postgres")
    };
    append_database(
        &fixture.config,
        r#"
[[application.databases]]
name = "main"
kind = "postgresql"
url_env = "TEST_DATABASE_URL"
consistency = { logical = { format = "custom" } }
"#,
    );

    let mut command = vaultline();
    command
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .env("TEST_DATABASE_URL", &db_url);
    if cfg!(windows) {
        let shim = fixture._dir.path().join("pgdump.cmd");
        // cmd cannot open the dot-prefixed temp passfile directly; the
        // shim extracts the password with PowerShell and forwards it as
        // PGPASSWORD into the container (the test password has no colons).
        std::fs::write(
            &shim,
            "@echo off\r\nfor /f \"usebackq delims=\" %%a in (`powershell -NoProfile -Command \"(Get-Content -Raw $env:PGPASSFILE).Trim().Split(':')[4]\"`) do set PGPASSWORD=%%a\r\ndocker run --rm -i -e PGPASSWORD=%PGPASSWORD% postgres:16 pg_dump %*\r\n",
        )
        .expect("shim");
        command.env("VAULTLINE_PGDUMP", &shim);
    }
    command
        .assert()
        .success()
        .stdout(predicate::str::contains("backup complete"));

    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.state_dir().join("state.json")).expect("state file"),
    )
    .expect("state parses");
    let snapshot = &state["applications"]["thornwa"]["snapshots"][0];
    let meta = &snapshot["database_metadata"][0];
    assert_eq!(meta["name"], "main");
    assert_eq!(meta["engine"], "postgresql");
    assert_eq!(meta["dump_format"], "custom");
    assert!(meta["mechanism"].as_str().unwrap().contains("pg_dump"));

    // The dump file is in the repository and is a valid custom-format dump.
    let ls = restic(&fixture.repo, &["ls", "latest"]);
    assert!(String::from_utf8_lossy(&ls.stdout).contains("main.dump"));
    let restore_target = fixture._dir.path().join("restore");
    let dump_bytes = restore_file(&fixture.repo, "*main.dump", "main.dump", &restore_target);
    // A valid pg_dump -Fc archive begins with the PGDMP signature.
    assert_eq!(&dump_bytes[..5], b"PGDMP", "not a custom-format dump");
}

/// MinIO S3-compatible end-to-end (container-gated): the s3 URL builder
/// and credential forwarding run a real backup into a MinIO bucket.
/// Requires Docker.
#[test]
fn s3_compatible_storage_backs_up_into_minio() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available");
        return;
    }
    if docker_available().is_none() {
        eprintln!("skipping: Docker engine not running (container-gated test)");
        return;
    }
    use testcontainers::GenericImage;
    use testcontainers::core::{ImageExt, IntoContainerPort, WaitFor};
    use testcontainers::runners::SyncRunner as _;

    let minio = GenericImage::new("minio/minio", "latest")
        .with_exposed_port(9000.tcp())
        .with_wait_for(WaitFor::seconds(10))
        .with_env_var("MINIO_ROOT_USER", "minioadmin")
        .with_env_var("MINIO_ROOT_PASSWORD", "minioadmin")
        .with_cmd(["server", "/data"]);
    let node = minio.start().expect("start minio");
    let port = node.get_host_port_ipv4(9000).expect("mapped port");

    let fixture = Fixture::new();
    // Replace the local storage block with an s3 target.
    let contents = std::fs::read_to_string(&fixture.config)
        .expect("config")
        .replace(
            "[application.storage]\nkind = \"local\"",
            &format!(
                "[application.storage]\nkind = \"s3\"\nendpoint = \"http://localhost:{port}\"\nbucket = \"backups\"\naccess_key_env = \"MINIO_KEY\"\nsecret_key_env = \"MINIO_SECRET\""
            ),
        );
    std::fs::write(&fixture.config, contents).expect("config");

    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .env("MINIO_KEY", "minioadmin")
        .env("MINIO_SECRET", "minioadmin")
        .assert()
        .success()
        .stdout(predicate::str::contains("backup complete"));

    // The repository lives in the bucket: verify through restic itself
    // with the same credentials.
    let bin = restic_bin().expect("restic present");
    let repo = format!("s3:http://localhost:{port}/backups/thornwa");
    let ls = Command::new(bin)
        .arg("-r")
        .arg(&repo)
        .arg("ls")
        .arg("latest")
        .env("RESTIC_PASSWORD", "test-password")
        .env("AWS_ACCESS_KEY_ID", "minioadmin")
        .env("AWS_SECRET_ACCESS_KEY", "minioadmin")
        .output()
        .expect("restic runs");
    assert!(
        ls.status.success(),
        "{:?}",
        String::from_utf8_lossy(&ls.stderr)
    );
    assert!(String::from_utf8_lossy(&ls.stdout).contains("hello.txt"));
}

/// A database whose connection environment variable is missing fails fast
/// with the variable named — before restic ever runs.
#[test]
fn missing_database_url_env_fails_fast() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available");
        return;
    }
    let fixture = Fixture::new();
    append_database(
        &fixture.config,
        r#"
[[application.databases]]
name = "main"
kind = "postgresql"
url_env = "MISSING_DB_URL"
consistency = { logical = { format = "custom" } }
"#,
    );
    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .env_remove("MISSING_DB_URL")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("MISSING_DB_URL"));
}

/// A volume with direct semantics whose name is a host path is captured;
/// the manifest records the volume kind.
#[test]
fn direct_volume_host_path_is_captured() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available");
        return;
    }
    let fixture = Fixture::new();
    let volume_dir = fixture._dir.path().join("data-volume");
    std::fs::create_dir_all(&volume_dir).expect("volume dir");
    std::fs::write(volume_dir.join("payload.bin"), "volume data").expect("payload");

    append_database(
        &fixture.config,
        &format!(
            r#"
[[application.volumes]]
name = "{}"
capture = "direct"
"#,
            volume_dir.display().to_string().replace('\\', "/")
        ),
    );

    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success();

    let ls = restic(&fixture.repo, &["ls", "latest"]);
    let listing = String::from_utf8_lossy(&ls.stdout);
    assert!(listing.contains("payload.bin"), "{listing}");

    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.state_dir().join("state.json")).expect("state file"),
    )
    .expect("state parses");
    let snapshot = &state["applications"]["thornwa"]["snapshots"][0];
    assert!(
        snapshot["source_manifest"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "volume-direct")
    );
}

/// A named Docker volume with direct semantics is captured through its
/// resolved mountpoint (Unix only: on Windows/macOS Desktop the mountpoint
/// lives inside the Docker VM, not on the host — recorded in
/// docs/limitations.md). The round trip additionally requires the
/// mountpoint to be REACHABLE from the test process: standard CI
/// runners keep the docker-data directory untraversable for the runner
/// user (the daemon answers on its socket; the filesystem does not —
/// the product aborts that case honestly, naming the sidecar remedy),
/// so the proof skips there with a note and runs wherever the
/// mountpoint is visible.
#[cfg(unix)]
#[test]
fn named_docker_volume_direct_capture() {
    if restic_bin().is_none() || docker_available().is_none() {
        eprintln!("skipping: restic or Docker not available (container-gated)");
        return;
    }
    let fixture = Fixture::new();
    let volume_name = format!("vaultline-test-{}", std::process::id());

    let create = Command::new("docker")
        .args(["volume", "create", &volume_name])
        .output()
        .expect("docker volume create");
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );

    // The mountpoint reachability gate (the environmental fact above).
    let inspect = Command::new("docker")
        .args([
            "volume",
            "inspect",
            "--format",
            "{{.Mountpoint}}",
            &volume_name,
        ])
        .output()
        .expect("docker volume inspect");
    let mountpoint = String::from_utf8_lossy(&inspect.stdout).trim().to_string();
    if !Path::new(&mountpoint).exists() {
        eprintln!(
            "skipping: the docker-reported mountpoint {mountpoint} is not reachable from the test process (the docker-data directory is not traversable here — Docker Desktop VMs and standard CI runners); the sidecar and pause-first proofs carry the capture round trip on such hosts"
        );
        let _ = Command::new("docker")
            .args(["volume", "rm", &volume_name])
            .output();
        return;
    }

    // Write data INTO the volume through a throwaway container.
    let write = Command::new("docker")
        .args([
            "run",
            "--rm",
            "-v",
            &format!("{volume_name}:/data"),
            "alpine",
            "sh",
            "-c",
            "echo volume-payload > /data/vol.txt",
        ])
        .output()
        .expect("docker run");
    assert!(
        write.status.success(),
        "{}",
        String::from_utf8_lossy(&write.stderr)
    );

    append_database(
        &fixture.config,
        &format!(
            r#"
[[application.volumes]]
name = "{volume_name}"
capture = "direct"
"#
        ),
    );

    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success();

    // The volume's content entered the repository through the resolved
    // mountpoint (/var/lib/docker/volumes/<name>/_data on the host).
    let ls = restic(&fixture.repo, &["ls", "latest"]);
    let listing = String::from_utf8_lossy(&ls.stdout);
    assert!(listing.contains("vol.txt"), "{listing}");
    assert!(listing.contains(volume_name.as_str()), "{listing}");

    let rm = Command::new("docker")
        .args(["volume", "rm", &volume_name])
        .output()
        .expect("docker volume rm");
    assert!(
        rm.status.success(),
        "{}",
        String::from_utf8_lossy(&rm.stderr)
    );
}

/// The sidecar capture end-to-end (Phase 8): the volume is copied out
/// through a read-only sidecar container, the snapshot's manifest
/// RECORDS the captured path, and the restore locates the bytes through
/// that record (never the restore host's resolution). A host-path
/// volume makes the disaster round (wipe → restore → data intact)
/// runnable on Desktop VMs and native Linux alike.
#[test]
fn sidecar_capture_copies_the_volume_into_the_snapshot() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available");
        return;
    }
    if docker_available().is_none() {
        eprintln!("skipping: docker not available");
        return;
    }
    let fixture = Fixture::new();
    let volume = fixture._dir.path().join("app-data");
    std::fs::create_dir_all(&volume).expect("volume dir");
    std::fs::write(volume.join("sessions.bin"), b"irreplaceable session state").expect("data");
    append_database(
        &fixture.config,
        &format!(
            r#"
[[application.volumes]]
name = "{volume}"
capture = "sidecar"
image = "alpine"
[[application.restore.steps]]
restore_volume = {{ volume = "{volume}" }}
"#,
            volume = volume.display().to_string().replace('\\', "/"),
        ),
    );

    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("backup complete"));

    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.state_dir().join("state.json")).expect("state file"),
    )
    .expect("state parses");
    let snapshot = &state["applications"]["thornwa"]["snapshots"][0];
    let volume_name = volume.display().to_string().replace('\\', "/");
    let entry = snapshot["source_manifest"]["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|e| e["name"] == volume_name)
        .expect("the volume has a manifest entry");
    assert_eq!(entry["kind"], "volume-sidecar");
    assert!(
        entry["path"].is_string(),
        "the captured path is recorded for the restore"
    );
    assert!(
        snapshot["restore_metadata"]["reconstructs"]
            .as_array()
            .expect("reconstructs")
            .iter()
            .any(|n| n.as_str() == Some(volume_name.as_str())),
        "the volume joins the reconstructs list"
    );
    assert!(
        snapshot["configuration_metadata"]["note"]
            .as_str()
            .expect("note")
            .contains("full capture declared"),
        "no declared-but-not-captured honesty note remains"
    );

    // The disaster round: destroy the volume, restore through the
    // recorded capture path, and find the bytes again.
    std::fs::remove_dir_all(&volume).expect("destroy the volume");
    std::fs::create_dir_all(&volume).expect("empty volume dir");
    let snapshot_id = snapshot["id"].as_str().expect("snapshot id");
    vaultline()
        .args(["restore", snapshot_id, "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(fixture._dir.path().join("restored"))
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success();
    let restored = std::fs::read(volume.join("sessions.bin")).expect("restored data");
    assert_eq!(restored, b"irreplaceable session state");
}

/// The pause-first capture end-to-end (Phase 8): the writer container
/// is paused around a direct capture and — the CRITICAL property — is
/// unpaused again afterwards, verified by the engine's own state.
#[test]
fn pause_first_capture_pauses_and_unpauses_the_writer() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available");
        return;
    }
    if docker_available().is_none() {
        eprintln!("skipping: docker not available");
        return;
    }
    let fixture = Fixture::new();
    let volume = fixture._dir.path().join("writer-data");
    std::fs::create_dir_all(&volume).expect("volume dir");
    std::fs::write(volume.join("journal.txt"), "consistent bytes").expect("data");
    let writer = format!("vl-writer-{}", std::process::id());
    let cleanup = |writer: &str| {
        let _ = Command::new("docker").args(["rm", "-f", writer]).output();
    };
    cleanup(&writer);
    let started = Command::new("docker")
        .args(["run", "-d", "--name", &writer, "alpine", "sleep", "300"])
        .output()
        .expect("docker run");
    assert!(started.status.success(), "writer starts: {:?}", started);

    append_database(
        &fixture.config,
        &format!(
            r#"
[[application.volumes]]
name = "{volume}"
capture = "pause-first"
container = "{writer}"
[[application.restore.steps]]
restore_volume = {{ volume = "{volume}" }}
"#,
            volume = volume.display().to_string().replace('\\', "/"),
        ),
    );

    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("backup complete"));

    // The writer must be unpaused and running again — the pause window
    // spans the capture, never beyond it.
    let state = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{.State.Paused}} {{.State.Running}}",
            &writer,
        ])
        .output()
        .expect("docker inspect");
    assert!(
        state.status.success() && String::from_utf8_lossy(&state.stdout).trim() == "false true",
        "the writer is unpaused and running after the capture: {:?}",
        String::from_utf8_lossy(&state.stdout)
    );

    let state_file: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.state_dir().join("state.json")).expect("state file"),
    )
    .expect("state parses");
    let snapshot = &state_file["applications"]["thornwa"]["snapshots"][0];
    let volume_name = volume.display().to_string().replace('\\', "/");
    let entry = snapshot["source_manifest"]["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|e| e["name"] == volume_name)
        .expect("the volume has a manifest entry");
    assert_eq!(entry["kind"], "volume-pause-first");
    assert!(entry["path"].is_string(), "the captured path is recorded");

    // The disaster round as well: wipe, restore through the record.
    std::fs::remove_dir_all(&volume).expect("destroy the volume");
    std::fs::create_dir_all(&volume).expect("empty volume dir");
    let snapshot_id = snapshot["id"].as_str().expect("snapshot id");
    vaultline()
        .args(["restore", snapshot_id, "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(fixture._dir.path().join("restored"))
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success();
    let restored = std::fs::read(volume.join("journal.txt")).expect("restored data");
    assert_eq!(restored, b"consistent bytes");
    cleanup(&writer);
}

/// The pause guard's CRITICAL property: when the capture FAILS after
/// the pause, the container is still unpaused (a backup error must
/// never leave a production container paused).
#[test]
fn pause_guard_unpauses_even_when_the_capture_fails() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available");
        return;
    }
    if docker_available().is_none() {
        eprintln!("skipping: docker not available");
        return;
    }
    let fixture = Fixture::new();
    let writer = format!("vl-writer-fail-{}", std::process::id());
    let _ = Command::new("docker").args(["rm", "-f", &writer]).output();
    let started = Command::new("docker")
        .args(["run", "-d", "--name", &writer, "alpine", "sleep", "300"])
        .output()
        .expect("docker run");
    assert!(started.status.success(), "writer starts: {:?}", started);

    // The volume name resolves to nothing: not an existing path, not a
    // docker volume — the capture errors AFTER the pause engaged.
    let missing = fixture._dir.path().join("missing-volume");
    append_database(
        &fixture.config,
        &format!(
            r#"
[[application.volumes]]
name = "{missing}"
capture = "pause-first"
container = "{writer}"
"#,
            missing = missing.display().to_string().replace('\\', "/"),
        ),
    );

    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            missing.display().to_string().replace('\\', "/"),
        ));

    // The guard dropped on the error path — the writer is unpaused.
    let state = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{.State.Paused}} {{.State.Running}}",
            &writer,
        ])
        .output()
        .expect("docker inspect");
    assert!(
        state.status.success() && String::from_utf8_lossy(&state.stdout).trim() == "false true",
        "the writer is unpaused even though the capture failed: {:?}",
        String::from_utf8_lossy(&state.stdout)
    );
    let _ = Command::new("docker").args(["rm", "-f", &writer]).output();
}

/// A STOPPED writer is already quiescent — pause-first proceeds with a
/// note instead of failing on the un-pausable container (evidence from
/// the engine's state, never stderr-message matching).
#[test]
fn pause_first_proceeds_when_the_writer_is_stopped() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available");
        return;
    }
    if docker_available().is_none() {
        eprintln!("skipping: docker not available");
        return;
    }
    let fixture = Fixture::new();
    let volume = fixture._dir.path().join("stopped-writer-data");
    std::fs::create_dir_all(&volume).expect("volume dir");
    std::fs::write(volume.join("data.txt"), "quiescent bytes").expect("data");
    let writer = format!("vl-writer-stopped-{}", std::process::id());
    let _ = Command::new("docker").args(["rm", "-f", &writer]).output();
    let created = Command::new("docker")
        .args(["create", "--name", &writer, "alpine", "sleep", "300"])
        .output()
        .expect("docker create");
    assert!(created.status.success(), "writer created: {:?}", created);

    append_database(
        &fixture.config,
        &format!(
            r#"
[[application.volumes]]
name = "{volume}"
capture = "pause-first"
container = "{writer}"
"#,
            volume = volume.display().to_string().replace('\\', "/"),
        ),
    );

    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stderr(predicate::str::contains("quiescent"));
    let _ = Command::new("docker").args(["rm", "-f", &writer]).output();
}
