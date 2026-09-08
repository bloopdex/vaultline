//! Restore & verification integration tests: the sandbox-then-promote
//! executor, the no-overwrite contract, verification L3/L4/L5, and the
//! disaster-recovery end-to-end (L6 shape). Each test skips with a note
//! when its requirements (restic, sqlite3, Docker) are absent.

use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use predicates::prelude::*;

fn vaultline() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("vaultline").expect("binary built by cargo")
}

fn restic_bin() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("VAULTLINE_RESTIC_BIN") {
        let path = PathBuf::from(bin);
        return path.is_file().then_some(path);
    }
    let output = Command::new("restic").arg("version").output().ok()?;
    output.status.success().then(|| PathBuf::from("restic"))
}

fn sqlite3_bin() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("VAULTLINE_SQLITE3") {
        let path = PathBuf::from(bin);
        return path.is_file().then_some(path);
    }
    let output = Command::new("sqlite3").arg("--version").output().ok()?;
    output.status.success().then(|| PathBuf::from("sqlite3"))
}

fn docker_available() -> Option<()> {
    let output = Command::new("docker").arg("info").output().ok()?;
    output.status.success().then_some(())
}

fn sqlite3(args: &[&str]) -> std::process::Output {
    let bin = sqlite3_bin().expect("sqlite3 present (test is gated)");
    Command::new(bin).args(args).output().expect("sqlite3 runs")
}

fn toml_path(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// A fixture with a files source, a SQLite database, and a full restore
/// procedure. The target directory is a sibling of the live data.
struct Fixture {
    _dir: tempfile::TempDir,
    config: PathBuf,
    uploads: PathBuf,
    db_path: PathBuf,
    restore_target: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let uploads = dir.path().join("uploads");
        std::fs::create_dir_all(&uploads).expect("uploads dir");
        std::fs::write(uploads.join("hello.txt"), "hello world").expect("file");
        std::fs::create_dir_all(uploads.join("nested")).expect("nested dir");
        std::fs::write(uploads.join("nested/data.txt"), "nested data").expect("nested file");
        let db_path = dir.path().join("app.sqlite");
        let create = sqlite3(&[
            db_path.to_str().expect("utf8"),
            "CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1),(2),(3);",
        ]);
        assert!(create.status.success(), "sqlite3 create failed");
        let repo_parent = dir.path().join("repos");
        let restore_target = dir.path().join("restored");
        let config = dir.path().join("vaultline.toml");
        let contents = format!(
            r#"
[application]
name = "thornwa"
[[application.sources.files]]
name = "uploads"
paths = ["{uploads}"]
[[application.databases]]
name = "appdb"
kind = "sqlite"
path = "{db}"
consistency = {{ logical = {{ format = "sql" }} }}
[application.storage]
kind = "local"
path = "{repo_parent}"
password_env = "VAULTLINE_TEST_PASSWORD"
[application.retention]
keep_last = 14
[application.verification]
level = 2
[[application.restore.steps]]
restore_files = {{ source = "uploads", target = "{restore_target}/uploads" }}
[[application.restore.steps]]
restore_database = {{ database = "appdb", target_database = "{restore_target}/restored.db" }}
"#,
            uploads = toml_path(&uploads),
            db = toml_path(&db_path),
            repo_parent = toml_path(&repo_parent),
            restore_target = toml_path(&restore_target),
        );
        std::fs::write(&config, contents).expect("config");
        Self {
            _dir: dir,
            config,
            uploads,
            db_path,
            restore_target,
            repo: repo_parent.join("thornwa"),
        }
    }

    fn state_dir(&self) -> PathBuf {
        self._dir.path().join("state")
    }

    fn backup(&self) {
        vaultline()
            .args(["backup", "run", "--config"])
            .arg(&self.config)
            .env("VAULTLINE_TEST_PASSWORD", "test-password")
            .env("VAULTLINE_STATE_DIR", self.state_dir())
            .assert()
            .success();
    }
}

/// The core scenario: restore executes the procedure into the target —
/// files promoted, the SQLite database restored and checkable — and
/// `--dry-run` plans without writing anything.
#[test]
fn restore_executes_the_procedure_sandbox_then_promote() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = Fixture::new();
    fixture.backup();

    // Dry run first: nothing may be written.
    vaultline()
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&fixture.restore_target)
        .arg("--dry-run")
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("restore_files"))
        .stdout(predicate::str::contains("restore_database"));
    assert!(!fixture.restore_target.exists(), "--dry-run must not write");

    vaultline()
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&fixture.restore_target)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("restore complete"));

    // Files promoted: content identical, nested directories included.
    assert_eq!(
        std::fs::read_to_string(fixture.restore_target.join("uploads/hello.txt")).expect("file"),
        "hello world"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.restore_target.join("uploads/nested/data.txt"))
            .expect("nested"),
        "nested data"
    );

    // Database restored and checkable (the restore-side semantic check).
    let check = sqlite3(&[
        fixture
            .restore_target
            .join("restored.db")
            .to_str()
            .expect("utf8"),
        "PRAGMA integrity_check; SELECT COUNT(*) FROM t;",
    ]);
    let out = String::from_utf8_lossy(&check.stdout);
    assert!(out.contains("ok"), "{out}");
    assert!(out.contains('3'), "{out}");
}

/// The promotion contract: an existing file at the destination is an
/// error naming the file — never a silent overwrite.
#[test]
fn restore_refuses_to_overwrite_existing_files() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = Fixture::new();
    fixture.backup();
    std::fs::create_dir_all(fixture.restore_target.join("uploads")).expect("dir");
    std::fs::write(
        fixture.restore_target.join("uploads/hello.txt"),
        "DO NOT TOUCH",
    )
    .expect("existing file");

    vaultline()
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&fixture.restore_target)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .code(1)
        .stderr(predicate::str::contains("refusing to overwrite"))
        // The partial-restore guidance (ADR-007): the operator is told the
        // promotion stopped part-way and how to retry.
        .stderr(predicate::str::contains("stopped part-way"));

    assert_eq!(
        std::fs::read_to_string(fixture.restore_target.join("uploads/hello.txt")).expect("file"),
        "DO NOT TOUCH",
        "the existing file must be untouched"
    );
}

/// Cross-platform restore (Phase 8): a declared production target in
/// Unix shape translates through the configuration's path map into this
/// host's layout, the dry-run plan shows the mapping explicitly, and
/// the files land at the mapped location.
#[test]
fn cross_platform_restore_lands_at_the_mapped_target() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available");
        return;
    }
    let fixture = Fixture::new();
    fixture.backup();
    let mapped_root = fixture._dir.path().join("mapped");
    let extra = format!(
        r#"
[[application.restore.steps]]
restore_files = {{ source = "uploads", target = "/srv/app/uploads" }}
[[application.restore.path_map]]
from = "/srv"
to = "{mapped}"
"#,
        mapped = toml_path(&mapped_root),
    );
    let contents = std::fs::read_to_string(&fixture.config).expect("config") + &extra;
    std::fs::write(&fixture.config, contents).expect("config");

    // The dry-run plan shows the mapping explicitly.
    vaultline()
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&fixture.restore_target)
        .arg("--dry-run")
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("mapped from /srv/app/uploads"));

    vaultline()
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&fixture.restore_target)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success();

    let landed = std::fs::read(mapped_root.join("app").join("uploads").join("hello.txt"))
        .expect("the mapped restore lands at the translated target");
    assert_eq!(landed, b"hello world");
}

/// A CLI `--path-map` entry overrides a configuration entry of equal
/// prefix length (ties go to the later entry — the CLI comes last).
#[test]
fn cli_path_map_overrides_the_configuration_on_a_tie() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available");
        return;
    }
    let fixture = Fixture::new();
    fixture.backup();
    let from_config = fixture._dir.path().join("from-config");
    let from_cli = fixture._dir.path().join("from-cli");
    let extra = format!(
        r#"
[[application.restore.steps]]
restore_files = {{ source = "uploads", target = "/srv/app/uploads" }}
[[application.restore.path_map]]
from = "/srv"
to = "{from_config}"
"#,
        from_config = toml_path(&from_config),
    );
    let contents = std::fs::read_to_string(&fixture.config).expect("config") + &extra;
    std::fs::write(&fixture.config, contents).expect("config");

    let cli_entry = format!("/srv={}", toml_path(&from_cli));
    vaultline()
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&fixture.restore_target)
        .arg("--path-map")
        .arg(&cli_entry)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success();

    let landed = std::fs::read(from_cli.join("app").join("uploads").join("hello.txt"))
        .expect("the CLI entry wins the tie");
    assert_eq!(landed, b"hello world");
    assert!(
        !from_config
            .join("app")
            .join("uploads")
            .join("hello.txt")
            .exists(),
        "the configuration entry lost the tie"
    );
}

/// The defining disaster test (L6 shape): backup → destroy the data →
/// restore → the application's data exists again and the database is
/// intact.
#[test]
fn disaster_recovery_end_to_end() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = Fixture::new();
    fixture.backup();

    // Destroy.
    std::fs::remove_dir_all(&fixture.uploads).expect("destroy uploads");
    std::fs::remove_file(&fixture.db_path).expect("destroy database");
    assert!(!fixture.uploads.exists() && !fixture.db_path.exists());

    vaultline()
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&fixture.restore_target)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success();

    assert!(fixture.restore_target.join("uploads/hello.txt").exists());
    let check = sqlite3(&[
        fixture
            .restore_target
            .join("restored.db")
            .to_str()
            .expect("utf8"),
        "PRAGMA integrity_check; SELECT COUNT(*) FROM t;",
    ]);
    let out = String::from_utf8_lossy(&check.stdout);
    assert!(out.contains("ok") && out.contains('3'), "{out}");
}

/// A WaitHealthy step polls the endpoint and the restore completes only
/// when it answers.
#[test]
fn restore_polls_the_health_endpoint() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    // The server thread is deliberately NOT joined: `incoming()` blocks
    // between connections, so a join would never return. The test
    // process's exit terminates the thread.
    let _server = thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });

    let contents = std::fs::read_to_string(&fixture.config).expect("config")
        + &format!(
            "[[application.restore.steps]]\nwait_healthy = {{ url = \"http://127.0.0.1:{port}/health\" }}\n"
        );
    std::fs::write(&fixture.config, contents).expect("config");
    fixture.backup();

    vaultline()
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&fixture.restore_target)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stderr(predicate::str::contains("healthy"));

    // (server thread dies with the process)
}

/// Verification: L3 (the engine still holds the snapshot) + L4 (content
/// comparison against the live files), and the reached level is recorded
/// durably in the state.
#[test]
fn verify_reaches_l4_and_records_it() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = Fixture::new();
    // Policy level 4.
    let contents = std::fs::read_to_string(&fixture.config)
        .expect("config")
        .replace("level = 2", "level = 4");
    std::fs::write(&fixture.config, contents).expect("config");
    fixture.backup();

    vaultline()
        .args(["backup", "verify", "latest", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("reached L4"));

    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.state_dir().join("state.json")).expect("state file"),
    )
    .expect("state parses");
    let snapshot = &state["applications"]["thornwa"]["snapshots"][0];
    assert_eq!(snapshot["integrity"]["highest_verified_level"], "L4");
    assert!(snapshot["integrity"]["verified_at"].is_string());

    // Tamper with a live file → L4 must fail with a mismatch.
    std::fs::write(fixture.uploads.join("hello.txt"), "tampered").expect("tamper");
    vaultline()
        .args(["backup", "verify", "latest", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .code(1)
        .stderr(predicate::str::contains("FAILED at L4"));
}

/// The restore-side check verifies the RESTORED copy, not the live
/// definition path (the 1.0-finalization finding): after corrupting the
/// live database file, `restore --verify` still passes because the
/// restored file is what integrity_check examines.
#[test]
fn restore_verify_checks_the_restored_sqlite_copy() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = Fixture::new();
    fixture.backup();

    // Corrupt the LIVE database file.
    std::fs::write(&fixture.db_path, b"not a sqlite database").expect("corrupt the live db");

    vaultline()
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&fixture.restore_target)
        .arg("--verify")
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("integrity_check ok"));

    // The restored copy itself is a healthy database.
    let check = sqlite3(&[
        fixture
            .restore_target
            .join("restored.db")
            .to_str()
            .expect("utf8"),
        "PRAGMA integrity_check;",
    ]);
    assert!(
        check.status.success(),
        "the restored copy passes integrity_check"
    );
}

/// Verification L5: the SQLite dump is restored to scratch and passes
/// integrity_check — the backup is proven restorable, not just created.
#[test]
fn verify_reaches_l5_for_sqlite() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = Fixture::new();
    let contents = std::fs::read_to_string(&fixture.config)
        .expect("config")
        .replace("level = 2", "level = 5");
    std::fs::write(&fixture.config, contents).expect("config");
    fixture.backup();

    vaultline()
        .args(["backup", "verify", "latest", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("reached L5"))
        .stdout(predicate::str::contains("integrity_check ok"));
}

/// inspect shows the snapshot record.
#[test]
fn inspect_shows_the_snapshot_record() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = Fixture::new();
    fixture.backup();
    let output = vaultline()
        .args(["backup", "inspect", "latest", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .output()
        .expect("inspect runs");
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert!(value["id"].as_str().unwrap().starts_with("thornwa-"));
    assert_eq!(value["application"], "thornwa");
}

/// PostgreSQL round trip (container-gated): dump → pg_restore into a
/// second database → the restored rows are queryable. This is the L5
/// rehearsal for server engines, executed through the restore command.
#[test]
fn postgresql_restore_round_trip() {
    if restic_bin().is_none() || docker_available().is_none() {
        eprintln!("skipping: restic or Docker not available (container-gated)");
        return;
    }
    use testcontainers::runners::SyncRunner as _;

    let fixture = Fixture::new();
    // A clean config: the files source only, a postgresql database, and a
    // restore procedure targeting a second database on the same server.
    let base = format!(
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
[[application.restore.steps]]
restore_files = {{ source = "uploads", target = "{restore_target}/uploads" }}
[[application.restore.steps]]
restore_database = {{ database = "main", target_database = "test2" }}
"#,
        uploads = toml_path(&fixture.uploads),
        repo_parent = toml_path(fixture.repo.parent().expect("repo parent")),
        restore_target = toml_path(&fixture.restore_target),
    );

    let postgres = testcontainers_modules::postgres::Postgres::default();
    let node = postgres.start().expect("start postgres");
    let port = node.get_host_port_ipv4(5432).expect("mapped port");

    // The target database for pg_restore.
    let create_db = Command::new("docker")
        .args([
            "exec",
            node.id(),
            "psql",
            "-U",
            "postgres",
            "-c",
            "CREATE DATABASE test2;",
        ])
        .output()
        .expect("docker exec");
    assert!(create_db.status.success(), "create test2 failed");

    // Seed the source database.
    let seed = Command::new("docker")
        .args([
            "exec",
            node.id(),
            "psql",
            "-U",
            "postgres",
            "-d",
            "postgres",
            "-c",
            "CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1),(2),(3);",
        ])
        .output()
        .expect("docker exec");
    assert!(
        seed.status.success(),
        "{:?}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let db_url = if cfg!(windows) {
        format!("postgresql://postgres:postgres@host.docker.internal:{port}/postgres")
    } else {
        // 127.0.0.1, never localhost: on the hosted runner localhost
        // resolves to ::1 first (pg) and the mysql client maps the
        // name to the unix socket — both miss the container's
        // published TCP port (first-hosted-run finding).
        format!("postgresql://postgres:postgres@127.0.0.1:{port}/postgres")
    };
    let contents = format!(
        "{base}\n[[application.databases]]\nname = \"main\"\nkind = \"postgresql\"\nurl_env = \"TEST_DATABASE_URL\"\nconsistency = {{ logical = {{ format = \"custom\" }} }}\n",
    );
    std::fs::write(&fixture.config, contents).expect("config");

    let shim = |name: &str, tool: &str| -> PathBuf {
        let path = fixture._dir.path().join(name);
        std::fs::write(
            &path,
            format!(
                "@echo off\r\nfor /f \"usebackq delims=\" %%a in (`powershell -NoProfile -Command \"(Get-Content -Raw $env:PGPASSFILE).Trim().Split(':')[4]\"`) do set PGPASSWORD=%%a\r\ndocker run --rm -i -e PGPASSWORD=%PGPASSWORD% postgres:16 {tool} %*\r\n"
            ),
        )
        .expect("shim");
        path
    };

    let mut backup_cmd = vaultline();
    backup_cmd
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .env("TEST_DATABASE_URL", &db_url);
    if cfg!(windows) {
        backup_cmd.env("VAULTLINE_PGDUMP", shim("pgdump.cmd", "pg_dump"));
    }
    backup_cmd.assert().success();

    let mut restore_cmd = vaultline();
    restore_cmd
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&fixture.restore_target)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .env("TEST_DATABASE_URL", &db_url);
    if cfg!(windows) {
        restore_cmd.env("VAULTLINE_PGRESTORE", shim("pgrestore.cmd", "pg_restore"));
    }
    restore_cmd
        .assert()
        .success()
        .stdout(predicate::str::contains("restore complete"));

    // The restored rows are queryable in test2.
    let count = Command::new("docker")
        .args([
            "exec",
            node.id(),
            "psql",
            "-U",
            "postgres",
            "-d",
            "test2",
            "-tAc",
            "SELECT COUNT(*) FROM t;",
        ])
        .output()
        .expect("docker exec");
    let out = String::from_utf8_lossy(&count.stdout);
    assert!(count.status.success(), "{out}");
    assert!(out.trim() == "3", "restored row count: {out}");
}

/// MySQL round trip (container-gated): dump → mysql client restore into a
/// second database → the restored rows are queryable. The first MySQL
/// pressure — it also pinned the `--databases` capture bug (recorded in
/// database.rs): a dump carrying CREATE DATABASE/USE would override the
/// declared restore target.
#[test]
fn mysql_restore_round_trip() {
    if restic_bin().is_none() || docker_available().is_none() {
        eprintln!("skipping: restic or Docker not available (container-gated)");
        return;
    }
    use testcontainers::runners::SyncRunner as _;

    let fixture = Fixture::new();
    let base = format!(
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
[[application.restore.steps]]
restore_files = {{ source = "uploads", target = "{restore_target}/uploads" }}
[[application.restore.steps]]
restore_database = {{ database = "main", target_database = "test2" }}
"#,
        uploads = toml_path(&fixture.uploads),
        repo_parent = toml_path(fixture.repo.parent().expect("repo parent")),
        restore_target = toml_path(&fixture.restore_target),
    );

    let mysql = testcontainers_modules::mysql::Mysql::default();
    let node = mysql.start().expect("start mysql");
    let port = node.get_host_port_ipv4(3306).expect("mapped port");

    let exec = |args: &[&str]| -> std::process::Output {
        Command::new("docker")
            .arg("exec")
            .arg(node.id())
            .args(args)
            .output()
            .expect("docker exec runs")
    };

    // The module's root has no password and a default database `test`.
    let seed = exec(&[
        "mysql",
        "-uroot",
        "test",
        "-e",
        "CREATE TABLE t(x INT); INSERT INTO t VALUES (1),(2),(3);",
    ]);
    assert!(
        seed.status.success(),
        "{:?}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let create_db = exec(&["mysql", "-uroot", "-e", "CREATE DATABASE test2;"]);
    assert!(
        create_db.status.success(),
        "{:?}",
        String::from_utf8_lossy(&create_db.stderr)
    );

    let db_url = if cfg!(windows) {
        format!("mysql://root@host.docker.internal:{port}/test")
    } else {
        format!("mysql://root@127.0.0.1:{port}/test")
    };
    let contents = format!(
        "{base}\n[[application.databases]]\nname = \"main\"\nkind = \"mysql\"\nurl_env = \"TEST_DATABASE_URL\"\nconsistency = {{ logical = {{ format = \"sql\" }} }}\n",
    );
    std::fs::write(&fixture.config, contents).expect("config");

    // On Windows there is no host mysqldump/mysql client; the shims run the
    // real tools inside the mysql image, reaching the host-mapped port
    // through host.docker.internal (MYSQL_PWD forwards the password env;
    // the module's root has none).
    let shim = |name: &str, tool: &str| -> PathBuf {
        let path = fixture._dir.path().join(name);
        std::fs::write(
            &path,
            format!(
                "@echo off\r\ndocker run --rm -i -e MYSQL_PWD=%MYSQL_PWD% mysql:8.1 {tool} %*\r\n"
            ),
        )
        .expect("shim");
        path
    };

    let mut backup_cmd = vaultline();
    backup_cmd
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .env("TEST_DATABASE_URL", &db_url);
    if cfg!(windows) {
        backup_cmd.env("VAULTLINE_MYSQLDUMP", shim("mysqldump.cmd", "mysqldump"));
    }
    backup_cmd.assert().success();

    let mut restore_cmd = vaultline();
    restore_cmd
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&fixture.restore_target)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .env("TEST_DATABASE_URL", &db_url);
    if cfg!(windows) {
        restore_cmd.env("VAULTLINE_MYSQL", shim("mysql.cmd", "mysql"));
    }
    restore_cmd
        .assert()
        .success()
        .stdout(predicate::str::contains("restore complete"));

    // The restored rows are queryable in test2 — and the SOURCE table is
    // untouched (the dump must not have recreated the source database).
    let count = exec(&[
        "mysql",
        "-uroot",
        "-N",
        "test2",
        "-e",
        "SELECT COUNT(*) FROM t;",
    ]);
    let out = String::from_utf8_lossy(&count.stdout);
    assert!(count.status.success(), "{out}");
    assert!(out.trim() == "3", "restored row count: {out}");

    let source_still = exec(&[
        "mysql",
        "-uroot",
        "-N",
        "test",
        "-e",
        "SELECT COUNT(*) FROM t;",
    ]);
    let out = String::from_utf8_lossy(&source_still.stdout);
    assert!(source_still.status.success(), "{out}");
    assert!(out.trim() == "3", "source rows untouched: {out}");
}

/// MariaDB round trip (container-gated): the same capture/restore path as
/// MySQL against the mariadb engine — closing the mysql/mariadb kind
/// pair (the Phase 7 candidate).
#[test]
fn mariadb_restore_round_trip() {
    if restic_bin().is_none() || docker_available().is_none() {
        eprintln!("skipping: restic or Docker not available (container-gated)");
        return;
    }
    use testcontainers::runners::SyncRunner as _;

    let fixture = Fixture::new();
    let base = format!(
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
[[application.restore.steps]]
restore_files = {{ source = "uploads", target = "{restore_target}/uploads" }}
[[application.restore.steps]]
restore_database = {{ database = "main", target_database = "test2" }}
"#,
        uploads = toml_path(&fixture.uploads),
        repo_parent = toml_path(fixture.repo.parent().expect("repo parent")),
        restore_target = toml_path(&fixture.restore_target),
    );

    let mariadb = testcontainers_modules::mariadb::Mariadb::default();
    let node = mariadb.start().expect("start mariadb");
    let port = node.get_host_port_ipv4(3306).expect("mapped port");

    let exec = |args: &[&str]| -> std::process::Output {
        Command::new("docker")
            .arg("exec")
            .arg(node.id())
            .args(args)
            .output()
            .expect("docker exec runs")
    };

    // The module's root has no password and a default database `test`.
    // The mariadb image ships the `mariadb` client (and `mysql` as its
    // alias); both accept the same flags.
    let seed = exec(&[
        "mariadb",
        "-uroot",
        "test",
        "-e",
        "CREATE TABLE t(x INT); INSERT INTO t VALUES (1),(2),(3);",
    ]);
    assert!(
        seed.status.success(),
        "{:?}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let create_db = exec(&["mariadb", "-uroot", "-e", "CREATE DATABASE test2;"]);
    assert!(
        create_db.status.success(),
        "{:?}",
        String::from_utf8_lossy(&create_db.stderr)
    );

    let db_url = if cfg!(windows) {
        format!("mysql://root@host.docker.internal:{port}/test")
    } else {
        // The tools run INSIDE the server container (the exec shim
        // below), where localhost is the server itself and the port is
        // the server's OWN 3306 — the host-mapped port belongs to the
        // Windows shim's host-side connection (unlike the mysql round
        // trip, whose native host-side client needs 127.0.0.1).
        "mysql://root@localhost:3306/test".to_string()
    };
    let contents = format!(
        "{base}\n[[application.databases]]\nname = \"main\"\nkind = \"mariadb\"\nurl_env = \"TEST_DATABASE_URL\"\nconsistency = {{ logical = {{ format = \"sql\" }} }}\n",
    );
    std::fs::write(&fixture.config, contents).expect("config");

    // The dump MUST come from the server's own tool: the first hosted
    // run showed the host-side MySQL-client mysqldump querying
    // information_schema.COLUMN_STATISTICS, which MariaDB does not have.
    // Windows shims docker-run the mariadb image; Unix shims docker-exec
    // the already-running server container (where the socket exists and
    // stdin flows through -i). The mariadb:11.3 image renamed its tools
    // (recorded: `mysqldump` is not found — the names are
    // `mariadb-dump` and `mariadb`).
    let shim = |name: &str, tool: &str| -> PathBuf {
        let path = fixture._dir.path().join(name);
        let script = if cfg!(windows) {
            format!(
                "@echo off\r\ndocker run --rm -i -e MYSQL_PWD=%MYSQL_PWD% mariadb:11.3 {tool} %*\r\n"
            )
        } else {
            format!(
                "#!/bin/sh\nexec docker exec -i {} {tool} \"$@\"\n",
                node.id()
            )
        };
        std::fs::write(&path, script).expect("shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod shim");
        }
        path
    };

    let mut backup_cmd = vaultline();
    backup_cmd
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .env("TEST_DATABASE_URL", &db_url)
        .env(
            "VAULTLINE_MYSQLDUMP",
            shim(
                if cfg!(windows) {
                    "mysqldump.cmd"
                } else {
                    "mysqldump"
                },
                "mariadb-dump",
            ),
        );
    backup_cmd.assert().success();

    let mut restore_cmd = vaultline();
    restore_cmd
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&fixture.restore_target)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .env("TEST_DATABASE_URL", &db_url)
        .env(
            "VAULTLINE_MYSQL",
            shim(if cfg!(windows) { "mysql.cmd" } else { "mysql" }, "mariadb"),
        );
    restore_cmd
        .assert()
        .success()
        .stdout(predicate::str::contains("restore complete"));

    let count = exec(&[
        "mariadb",
        "-uroot",
        "-N",
        "test2",
        "-e",
        "SELECT COUNT(*) FROM t;",
    ]);
    let out = String::from_utf8_lossy(&count.stdout);
    assert!(count.status.success(), "{out}");
    assert!(out.trim() == "3", "restored row count: {out}");

    let source_still = exec(&[
        "mariadb",
        "-uroot",
        "-N",
        "test",
        "-e",
        "SELECT COUNT(*) FROM t;",
    ]);
    let out = String::from_utf8_lossy(&source_still.stdout);
    assert!(source_still.status.success(), "{out}");
    assert!(out.trim() == "3", "source rows untouched: {out}");
}

/// Unit-level contract tests live with the module; here the snapshot
/// selector contract is exercised end-to-end: unique prefixes work and
/// ambiguous ones fail.
#[test]
fn snapshot_selector_prefixes() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = Fixture::new();
    fixture.backup();
    fixture.backup();

    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.state_dir().join("state.json")).expect("state file"),
    )
    .expect("state parses");
    let first_id = state["applications"]["thornwa"]["snapshots"][0]["id"]
        .as_str()
        .expect("id")
        .to_string();

    // A full id resolves; a unique prefix resolves; the shared prefix is
    // ambiguous.
    vaultline()
        .args(["backup", "inspect"])
        .arg(&first_id)
        .args(["--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success();
    // The id is `app-<timestamp>-<8 hex>`; the hex part begins at index
    // 19, so 20 chars are unique across snapshots taken in the same
    // second.
    let prefix: String = first_id.chars().take(20).collect();
    vaultline()
        .args(["backup", "inspect"])
        .arg(&prefix)
        .args(["--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success();
    vaultline()
        .args(["backup", "inspect"])
        .arg("thornwa-")
        .args(["--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .code(1)
        .stderr(predicate::str::contains("ambiguous"));
}

/// A concurrent restore is refused by the same state lock as a backup:
/// a live holder's lock makes `restore` fail before any snapshot work
/// (no snapshot needed — the lock precedes selection).
#[test]
fn concurrent_restore_is_refused_by_the_lock() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = Fixture::new();
    fixture.backup();
    let mut holder = if cfg!(windows) {
        Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 > nul"])
            .spawn()
            .expect("holder")
    } else {
        Command::new("sh")
            .args(["-c", "sleep 30"])
            .spawn()
            .expect("holder")
    };
    std::fs::create_dir_all(fixture.state_dir()).expect("state dir");
    std::fs::write(
        fixture.state_dir().join("lock"),
        format!("pid {}\n", holder.id()),
    )
    .expect("lock file");

    vaultline()
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("another vaultline process"));

    let _ = holder.kill();
    let _ = holder.wait();
}

/// A restore step that references a source the SNAPSHOT does not contain
/// (declared in the definition only after the snapshot was made) fails
/// at plan time with the snapshot's reconstructs list named.
#[test]
fn restore_step_referencing_an_uncaptured_source_is_named() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = Fixture::new();
    fixture.backup();

    // A second database + restore step declared AFTER the snapshot:
    // validation passes (the definition has it), the snapshot does not.
    let second_db = fixture._dir.path().join("appdb2.sqlite");
    let create = sqlite3(&[
        second_db.to_str().expect("utf8"),
        "CREATE TABLE t2(x INTEGER); INSERT INTO t2 VALUES (7);",
    ]);
    assert!(create.status.success(), "sqlite3 create failed");
    let mut config = std::fs::read_to_string(&fixture.config).expect("config");
    config.push_str(&format!(
        "\n[[application.databases]]\nname = \"appdb2\"\nkind = \"sqlite\"\npath = \"{db}\"\nconsistency = {{ logical = {{ format = \"sql\" }} }}\n[[application.restore.steps]]\nrestore_database = {{ database = \"appdb2\", target_database = \"{target}/restored2.db\" }}\n",
        db = toml_path(&second_db),
        target = toml_path(&fixture.restore_target),
    ));
    std::fs::write(&fixture.config, config).expect("config");

    vaultline()
        .args(["restore", "latest", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&fixture.restore_target)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("does not contain"))
        .stderr(predicate::str::contains("appdb2"));
}

/// Verification L3 fails when the engine no longer holds the recorded
/// snapshot (an old backup — forgotten or removed): the failure names
/// the level and the snapshot.
#[test]
fn verify_fails_at_l3_when_the_engine_lost_the_snapshot() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = Fixture::new();
    fixture.backup();
    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.state_dir().join("state.json")).expect("state file"),
    )
    .expect("state parses");
    let engine_id =
        state["applications"]["thornwa"]["snapshots"][0]["engine_snapshot"]["snapshot_id"]
            .as_str()
            .expect("engine id")
            .to_string();

    // Forget the snapshot from the engine (the product's own cross-check
    // then finds it gone).
    let forget = Command::new(restic_bin().expect("restic"))
        .args([
            "-r",
            fixture.repo.to_str().expect("utf8"),
            "forget",
            &engine_id,
        ])
        .env("RESTIC_PASSWORD", "test-password")
        .output()
        .expect("restic runs");
    assert!(
        forget.status.success(),
        "{}",
        String::from_utf8_lossy(&forget.stderr)
    );

    vaultline()
        .args(["backup", "verify", "latest", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("FAILED at L3"));
}

/// The raw-recovery path (--from-engine): with the STATE FILE gone (the
/// destroyed-host case), the engine's own snapshot list drives the
/// restore — the whole snapshot lands in the target without the
/// procedure (docs/disaster-recovery.md).
#[test]
fn from_engine_restores_without_the_state_file() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = Fixture::new();
    fixture.backup();

    // The state file dies with the host.
    std::fs::remove_dir_all(fixture.state_dir()).expect("state dir removed");

    let raw_target = fixture._dir.path().join("raw-recovery");
    vaultline()
        .args(["restore", "latest", "--from-engine", "--config"])
        .arg(&fixture.config)
        .arg("--target")
        .arg(&raw_target)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("raw recovery complete"));

    // The data is back: the files source content and the database dump.
    let hello = find_any(&raw_target, "hello.txt");
    assert_eq!(
        std::fs::read_to_string(hello.expect("hello.txt recovered")).expect("read"),
        "hello world"
    );
    let dump = find_any(&raw_target, "appdb.db");
    assert!(dump.is_some(), "the sqlite dump is recovered");
}

/// A recursive name search (restic stages the snapshot under the
/// platform's path shape; the raw recovery restores it verbatim).
fn find_any(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_any(&path, name) {
                return Some(found);
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
    }
    None
}

/// A single-FILE source restores correctly (the 1.0-finalization smoke
/// finding: file-shaped sources were reported as "does not exist" by
/// the directory-only copy).
#[test]
fn a_single_file_source_restores() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let payload = dir.path().join("hello.txt");
    std::fs::write(&payload, "solo file").expect("payload");
    let repo_parent = dir.path().join("repos");
    let target = dir.path().join("restored");
    let config = dir.path().join("vaultline.toml");
    std::fs::write(
        &config,
        format!(
            r#"
[application]
name = "thornwa"
[[application.sources.files]]
name = "uploads"
paths = ["{payload}"]
[application.storage]
kind = "local"
path = "{repo_parent}"
password_env = "VAULTLINE_TEST_PASSWORD"
[application.retention]
keep_last = 1
[application.verification]
level = 1
[[application.restore.steps]]
restore_files = {{ source = "uploads", target = "{target}/uploads" }}
"#,
            payload = toml_path(&payload),
            repo_parent = toml_path(&repo_parent),
            target = toml_path(&target),
        ),
    )
    .expect("config");

    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", dir.path().join("state"))
        .assert()
        .success();

    vaultline()
        .args(["restore", "latest", "--config"])
        .arg(&config)
        .arg("--target")
        .arg(dir.path().join("restore-root"))
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", dir.path().join("state"))
        .assert()
        .success()
        .stdout(predicate::str::contains("restore complete"));

    assert_eq!(
        std::fs::read_to_string(target.join("uploads/hello.txt")).expect("restored"),
        "solo file"
    );
}
