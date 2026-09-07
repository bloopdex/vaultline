//! Hardening & security integration tests (Phase 6): the L6 recovery
//! rehearsal with executable app checks (ADR-006), the malicious-archive
//! defense, the corrupt-repository honesty fixture, and the redaction
//! failure path. Gated per test (restic, sqlite3, Docker).

use std::path::{Path, PathBuf};
use std::process::Command;

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

fn file_contains(dir: &Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            // Never follow links during the scan — that is the point.
            continue;
        }
        if path.is_dir() {
            if file_contains(&path, needle) {
                return true;
            }
        } else if std::fs::read_to_string(&path)
            .map(|contents| contents.contains(needle))
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// The L6 fixture: a files source + a SQLite database + restore steps, a
/// rehearsal root, level 6, and one app check that verifies the
/// rehearsed uploads file (the path the procedure remaps under the
/// rehearsal root).
struct L6Fixture {
    _dir: tempfile::TempDir,
    config: PathBuf,
    restore_target: PathBuf,
    rehearsal_root: PathBuf,
}

/// The executor's remap, mirrored in the fixture: absolute targets are
/// mirrored under the root with slashes and a Windows drive prefix
/// stripped.
fn remapped_under(root: &Path, absolute: &Path) -> PathBuf {
    let mut trimmed = absolute
        .display()
        .to_string()
        .trim_start_matches(['/', '\\'])
        .to_string();
    if trimmed.len() >= 2
        && trimmed.as_bytes()[1] == b':'
        && trimmed.as_bytes()[0].is_ascii_alphabetic()
    {
        trimmed = trimmed[2..].trim_start_matches(['/', '\\']).to_string();
    }
    root.join(trimmed)
}

impl L6Fixture {
    /// `passing`: the check asserts the rehearsed uploads file exists and
    /// exits 0; otherwise it exits 1.
    fn new(passing: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let uploads = dir.path().join("uploads");
        std::fs::create_dir_all(&uploads).expect("uploads dir");
        std::fs::write(uploads.join("hello.txt"), "hello world").expect("file");
        let db_path = dir.path().join("app.sqlite");
        let create = sqlite3(&[
            db_path.to_str().expect("utf8"),
            "CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1),(2),(3);",
        ]);
        assert!(create.status.success(), "sqlite3 create failed");
        let repo_parent = dir.path().join("repos");
        let restore_target = dir.path().join("restored");
        let rehearsal_root = dir.path().join("rehearsal");
        // The rehearsed file: {restore_target}/uploads/hello.txt mirrored
        // under the rehearsal root.
        let rehearsed_file =
            remapped_under(&rehearsal_root, &restore_target).join("uploads/hello.txt");
        // Windows executes only files it recognizes — the check script
        // needs the .cmd extension there.
        let check = dir
            .path()
            .join(if cfg!(windows) { "check.cmd" } else { "check" });
        let check_script = if passing {
            let rehearsed_file = rehearsed_file.display().to_string().replace('\\', "/");
            if cfg!(unix) {
                format!("#!/bin/sh\ntest -f \"{rehearsed_file}\" || exit 1\nexit 0\n")
            } else {
                format!(
                    "@echo off\r\nif exist \"{rehearsed_file}\" (exit /b 0) else (exit /b 1)\r\n"
                )
            }
        } else if cfg!(unix) {
            "#!/bin/sh\necho rehearsal check failed\nexit 1\n".to_string()
        } else {
            "@echo off\r\necho rehearsal check failed\r\nexit /b 1\r\n".to_string()
        };
        std::fs::write(&check, check_script).expect("check script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&check, std::fs::Permissions::from_mode(0o755))
                .expect("chmod check");
        }
        let check_command = check.display().to_string().replace('\\', "/");

        // The rehearsed path: {restore_target}/uploads/hello.txt remapped
        // under the rehearsal root — the check asserts it (relative to its
        // CWD, which is the rehearsal root).
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
level = 6
[[application.verification.app_checks]]
name = "uploads-present"
command = "{check_command}"
[application.rehearsal]
target = "{rehearsal_root}"
[[application.restore.steps]]
restore_files = {{ source = "uploads", target = "{restore_target}/uploads" }}
[[application.restore.steps]]
restore_database = {{ database = "appdb", target_database = "{restore_target}/restored.db" }}
"#,
            uploads = toml_path(&uploads),
            db = toml_path(&db_path),
            repo_parent = toml_path(&repo_parent),
            restore_target = toml_path(&restore_target),
            rehearsal_root = toml_path(&rehearsal_root),
            check_command = check_command,
        );
        std::fs::write(&config, contents).expect("config");
        Self {
            _dir: dir,
            config,
            restore_target,
            rehearsal_root,
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

    fn snapshot_level(&self) -> String {
        let state: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(self.state_dir().join("state.json")).expect("state file"),
        )
        .expect("state parses");
        state["applications"]["thornwa"]["snapshots"][0]["integrity"]["highest_verified_level"]
            .as_str()
            .expect("level")
            .to_string()
    }
}

/// The L6 rehearsal executes through `backup verify`, runs the app
/// checks, and records L6 durably.
#[test]
fn l6_rehearsal_verifies_checks_and_records() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = L6Fixture::new(true);
    fixture.backup();

    vaultline()
        .args(["backup", "verify", "latest", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("verification reached L6"))
        .stdout(predicate::str::contains("L6: rehearsed"))
        .stdout(predicate::str::contains(
            "app check \"uploads-present\" passed",
        ));

    assert_eq!(fixture.snapshot_level(), "L6");
    // The rehearsed layout exists under the root: the procedure's target
    // was mirrored, not the live path.
    let rehearsed =
        remapped_under(&fixture.rehearsal_root, &fixture.restore_target).join("uploads/hello.txt");
    assert!(
        rehearsed.exists(),
        "rehearsed file missing at {}",
        rehearsed.display()
    );
}

/// A failing app check fails the rehearsal — but the L5 that was proven
/// first stays recorded.
#[test]
fn l6_failing_check_fails_but_l5_is_recorded() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = L6Fixture::new(false);
    fixture.backup();

    vaultline()
        .args(["backup", "verify", "latest", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "app check \"uploads-present\" FAILED",
        ));

    // L5 was proven before the rehearsal attempt — it is the honest record.
    assert_eq!(fixture.snapshot_level(), "L5");
}

/// `schedule run` at level 6: the due verification IS the rehearsal.
#[test]
fn schedule_run_at_l6_rehearses() {
    if restic_bin().is_none() || sqlite3_bin().is_none() {
        eprintln!("skipping: restic or sqlite3 not available");
        return;
    }
    let fixture = L6Fixture::new(true);
    // Add the schedules the fixture config lacks (level 6 requires the
    // rehearsal; the schedules make the jobs due).
    let mut config = std::fs::read_to_string(&fixture.config).expect("config");
    config = config.replace(
        "name = \"thornwa\"\n",
        "name = \"thornwa\"\nschedule = \"0 3 * * *\"\n",
    );
    config = config.replace("level = 6\n", "level = 6\nschedule = \"0 3 * * 1\"\n");
    std::fs::write(&fixture.config, config).expect("config");

    vaultline()
        .args(["schedule", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture.state_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("verification due"))
        .stdout(predicate::str::contains("verification reached L6"));

    assert_eq!(fixture.snapshot_level(), "L6");
}

/// The malicious-archive defense: a symlink inside the snapshot pointing
/// at a file OUTSIDE the captured tree is never followed — the secret's
/// content never lands in the restored tree.
#[test]
fn malicious_symlink_does_not_escape() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let uploads = dir.path().join("uploads");
    std::fs::create_dir_all(&uploads).expect("uploads dir");
    std::fs::write(uploads.join("hello.txt"), "hello world").expect("file");

    // The secret lives OUTSIDE the captured tree.
    let secret = dir.path().join("outside-secret.txt");
    std::fs::write(&secret, "TOP-SECRET-CONTENT-42").expect("secret");
    let link = uploads.join("leak-link");
    #[cfg(unix)]
    let link_result = std::os::unix::fs::symlink(&secret, &link);
    #[cfg(windows)]
    let link_result = std::os::windows::fs::symlink_file(&secret, &link);
    if let Err(e) = link_result {
        eprintln!(
            "skipping: this host cannot create symlinks ({e}) — the defense is proven on the hosted job"
        );
        return;
    }

    let repo_parent = dir.path().join("repos");
    let restore_target = dir.path().join("restored");
    let config = dir.path().join("vaultline.toml");
    std::fs::write(
        &config,
        format!(
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
level = 1
[[application.restore.steps]]
restore_files = {{ source = "uploads", target = "{restore_target}/uploads" }}
"#,
            uploads = toml_path(&uploads),
            repo_parent = toml_path(&repo_parent),
            restore_target = toml_path(&restore_target),
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

    // restic stores the link itself; restore may fail to recreate it on
    // hosts without link privileges — either way, the secret content must
    // never appear in the restored tree, and the outside file is intact.
    let _ = vaultline()
        .args(["restore", "latest", "--config"])
        .arg(&config)
        .arg("--target")
        .arg(&restore_target)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", dir.path().join("state"))
        .output()
        .expect("restore runs");

    assert!(
        !file_contains(&restore_target, "TOP-SECRET-CONTENT-42"),
        "the secret content leaked into the restored tree"
    );
    assert_eq!(
        std::fs::read_to_string(&secret).expect("secret intact"),
        "TOP-SECRET-CONTENT-42"
    );
}

/// The corrupt-repository honesty fixture: a truncated pack makes the
/// inline L2 check fail, and the next snapshot records L1 — the level
/// actually reached — instead of claiming L2.
#[test]
fn corrupt_repository_records_l1_honestly() {
    if restic_bin().is_none() {
        eprintln!("skipping: restic not available");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let uploads = dir.path().join("uploads");
    std::fs::create_dir_all(&uploads).expect("uploads dir");
    std::fs::write(uploads.join("hello.txt"), "hello world").expect("file");
    let repo_parent = dir.path().join("repos");
    let config = dir.path().join("vaultline.toml");
    std::fs::write(
        &config,
        format!(
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
        ),
    )
    .expect("config");
    let repo = repo_parent.join("thornwa");

    // First backup: healthy repository, L2 recorded.
    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", dir.path().join("state"))
        .assert()
        .success()
        .stdout(predicate::str::contains("verification L2"));

    // Corrupt the repository: truncate one pack file under data/.
    let data_dir = repo.join("data");
    let pack = first_file_under(&data_dir).expect("a pack file exists");
    let full = std::fs::read(&pack).expect("pack bytes");
    std::fs::write(&pack, &full[..full.len() / 2]).expect("truncate the pack");

    // The next backup succeeds but its L2 check fails — the snapshot is
    // recorded at L1, the level actually reached.
    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", dir.path().join("state"))
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "repository integrity check FAILED",
        ));

    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("state/state.json")).expect("state file"),
    )
    .expect("state parses");
    let snapshots = state["applications"]["thornwa"]["snapshots"]
        .as_array()
        .expect("snapshots");
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[1]["integrity"]["highest_verified_level"], "L1");
    assert_eq!(snapshots[0]["integrity"]["highest_verified_level"], "L2");
}

fn first_file_under(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = first_file_under(&path) {
                return Some(found);
            }
        } else {
            return Some(path);
        }
    }
    None
}

/// The redaction failure path: a failed dump with an embedded password
/// prints the password NOWHERE (argv sanitized, env never logged, the
/// engine's stderr does not echo credentials). Container-gated.
#[test]
fn failed_dump_never_prints_the_password() {
    if restic_bin().is_none() || docker_available().is_none() {
        eprintln!("skipping: restic or Docker not available (container-gated)");
        return;
    }
    use testcontainers::runners::SyncRunner as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let uploads = dir.path().join("uploads");
    std::fs::create_dir_all(&uploads).expect("uploads dir");
    std::fs::write(uploads.join("hello.txt"), "hello world").expect("file");
    let repo_parent = dir.path().join("repos");
    let config = dir.path().join("vaultline.toml");
    std::fs::write(
        &config,
        format!(
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
level = 1
"#,
            uploads = toml_path(&uploads),
            repo_parent = toml_path(&repo_parent),
        ),
    )
    .expect("config");

    let postgres = testcontainers_modules::postgres::Postgres::default();
    let node = postgres.start().expect("start postgres");
    let port = node.get_host_port_ipv4(5432).expect("mapped port");
    let host = if cfg!(windows) {
        "host.docker.internal".to_string()
    } else {
        "localhost".to_string()
    };

    // A WRONG password inside the URL: pg_dump must fail at
    // authentication, and the failure output must not echo it.
    let secret_password = "sup3rs3cr3t-pw-XYZ";
    let db_url = format!("postgresql://postgres:{secret_password}@{host}:{port}/postgres");

    let mut config_contents = std::fs::read_to_string(&config).expect("config");
    config_contents.push_str(
        "\n[[application.databases]]\nname = \"main\"\nkind = \"postgresql\"\nurl_env = \"TEST_DATABASE_URL\"\nconsistency = { logical = { format = \"custom\" } }\n",
    );
    std::fs::write(&config, config_contents).expect("config");

    let mut command = vaultline();
    command
        .args(["backup", "run", "--config"])
        .arg(&config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", dir.path().join("state"))
        .env("TEST_DATABASE_URL", &db_url);
    if cfg!(windows) {
        let shim = dir.path().join("pgdump.cmd");
        std::fs::write(
            &shim,
            "@echo off\r\nfor /f \"usebackq delims=\" %%a in (`powershell -NoProfile -Command \"(Get-Content -Raw $env:PGPASSFILE).Trim().Split(':')[4]\"`) do set PGPASSWORD=%%a\r\ndocker run --rm -i -e PGPASSWORD=%PGPASSWORD% postgres:16 pg_dump %*\r\n",
        )
        .expect("shim");
        command.env("VAULTLINE_PGDUMP", &shim);
    }
    let output = command.output().expect("runs");
    assert!(
        !output.status.success(),
        "the dump must fail with the wrong password"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains(secret_password),
        "the password leaked into the failure output: {combined}"
    );
    // The failure is a clean diagnostic, not a crash.
    assert!(combined.contains("pg_dump failed"), "{combined}");
}
