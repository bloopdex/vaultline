//! End-to-end backup tests against the real restic binary. These run only
//! when restic is available: `VAULTLINE_RESTIC_BIN` overrides the binary
//! path, else `restic` on PATH — otherwise each test skips with a note
//! (docs/testing.md documents the gating).

use std::path::{Path, PathBuf};
use std::process::Command;

use predicates::prelude::*;

/// Locate a restic binary for the tests, or `None` (tests skip).
fn restic_bin() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("VAULTLINE_RESTIC_BIN") {
        let path = PathBuf::from(bin);
        return path.is_file().then_some(path);
    }
    let output = Command::new("restic").arg("version").output().ok()?;
    output.status.success().then(|| PathBuf::from("restic"))
}

fn vaultline() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("vaultline").expect("binary built by cargo")
}

fn backup_cmd() -> Option<assert_cmd::Command> {
    restic_bin().map(|_| vaultline())
}

const CONFIG: &str = r#"
[application]
name = "thornwa"
[[application.sources.files]]
name = "uploads"
paths = ["{uploads}"]
excludes = ["**/*.tmp"]
[[application.sources.config_refs]]
name = "env"
path = "{secret_file}"
note = "secrets — reference only"
[application.storage]
kind = "local"
path = "{repo_parent}"
password_env = "VAULTLINE_TEST_PASSWORD"
[application.retention]
keep_last = 14
[application.verification]
level = 2
[[application.restore.steps]]
restore_files = { source = "uploads", target = "/srv/thornwa/uploads" }
"#;

struct Fixture {
    _dir: tempfile::TempDir,
    config: PathBuf,
    uploads: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let uploads = dir.path().join("uploads");
        std::fs::create_dir_all(&uploads).expect("uploads dir");
        std::fs::write(uploads.join("hello.txt"), "hello world").expect("file");
        std::fs::write(uploads.join("noise.tmp"), "temporary").expect("file");
        let secret_file = dir.path().join(".env");
        std::fs::write(&secret_file, "DATABASE_URL=super-secret").expect("env file");
        let repo_parent = dir.path().join("repos");
        let config = dir.path().join("vaultline.toml");
        // Forward slashes: backslashes are TOML escape characters, and
        // restic accepts forward-slash paths on Windows.
        let toml_path = |p: &Path| p.display().to_string().replace('\\', "/");
        let contents = CONFIG
            .replace("{uploads}", &toml_path(&uploads))
            .replace("{secret_file}", &toml_path(&secret_file))
            .replace("{repo_parent}", &toml_path(&repo_parent));
        std::fs::write(&config, contents).expect("config");
        Self {
            _dir: dir,
            config,
            uploads,
            repo: repo_parent.join("thornwa"),
        }
    }

    fn restic(&self, args: &[&str]) -> std::process::Output {
        let bin = restic_bin().expect("restic present (test is gated)");
        Command::new(bin)
            .arg("-r")
            .arg(&self.repo)
            .args(args)
            .env("RESTIC_PASSWORD", "test-password")
            .output()
            .expect("restic runs")
    }
}

/// The core scenario: a fresh definition backs up files, excludes the glob,
/// records the config reference without copying it, verifies L2, and lands
/// a snapshot in the state file that `backup list` shows.
#[test]
fn backup_run_records_a_verified_snapshot() {
    let Some(mut cmd) = backup_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();

    cmd.args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture._dir.path().join("state"))
        .assert()
        .success()
        .stdout(predicate::str::contains("backup complete"))
        .stdout(predicate::str::contains("verification L2"));

    // The repository exists and holds the files — but not the excluded glob
    // and not the secret config reference.
    let ls = fixture.restic(&["ls", "latest"]);
    assert!(
        ls.status.success(),
        "{:?}",
        String::from_utf8_lossy(&ls.stderr)
    );
    let listing = String::from_utf8_lossy(&ls.stdout);
    assert!(listing.contains("hello.txt"), "{listing}");
    assert!(
        !listing.contains("noise.tmp"),
        "excluded glob captured: {listing}"
    );
    assert!(
        !listing.contains("super-secret") && !listing.contains(".env"),
        "config reference was copied into the backup: {listing}"
    );

    // The snapshot is recorded and listed.
    let list = vaultline()
        .args(["backup", "list", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_STATE_DIR", fixture._dir.path().join("state"))
        .output()
        .expect("list runs");
    assert!(list.status.success());
    let text = String::from_utf8_lossy(&list.stdout);
    assert!(text.contains("thornwa-"), "{text}");
    assert!(text.contains("L2"), "{text}");

    // The state file itself records the honest manifest.
    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture._dir.path().join("state/state.json")).expect("state file"),
    )
    .expect("state parses");
    let snapshot = &state["applications"]["thornwa"]["snapshots"][0];
    assert_eq!(snapshot["engine_snapshot"]["engine"], "restic");
    assert_eq!(snapshot["integrity"]["highest_verified_level"], "L2");
    assert_eq!(snapshot["integrity"]["engine_verified"], true);
    assert_eq!(
        snapshot["source_manifest"]["entries"][1]["kind"], "config_ref",
        "the config reference is recorded, not captured"
    );
}

/// `--json` produces the snapshot payload on stdout.
#[test]
fn backup_run_json_output_contains_the_snapshot() {
    let Some(mut cmd) = backup_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();

    let output = cmd
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .arg("--json")
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture._dir.path().join("state"))
        .output()
        .expect("runs");
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(value["application"], "thornwa");
    assert!(value["id"].as_str().unwrap().starts_with("thornwa-"));
}

/// A missing password environment variable is a configuration error (2),
/// before restic ever runs.
#[test]
fn missing_password_env_fails_fast_with_exit_2() {
    let Some(mut cmd) = backup_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();

    cmd.args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env_remove("VAULTLINE_TEST_PASSWORD")
        .env("VAULTLINE_STATE_DIR", fixture._dir.path().join("state"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("VAULTLINE_TEST_PASSWORD"));
}

/// A wrong password surfaces restic's exit 11 as a clear configuration
/// error naming the variable.
#[test]
fn wrong_password_maps_restic_exit_11() {
    let Some(mut cmd) = backup_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();

    // First run: create the repository with the real password.
    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture._dir.path().join("state"))
        .assert()
        .success();

    // Second run: wrong password.
    cmd.args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "wrong-password")
        .env("VAULTLINE_STATE_DIR", fixture._dir.path().join("state"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("password was rejected"));
}

/// A broken restic override fails with a clear operational error.
#[test]
fn missing_restic_binary_fails_clearly() {
    let fixture = Fixture::new();
    vaultline()
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env(
            "VAULTLINE_RESTIC_BIN",
            fixture._dir.path().join("no-such-restic"),
        )
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture._dir.path().join("state"))
        .assert()
        .code(1)
        .stderr(predicate::str::contains("does not exist"));
}

/// Two vaultline processes cannot mutate state concurrently: the lockfile
/// refuses the second run.
#[test]
fn concurrent_backup_is_refused_by_the_lock() {
    let Some(mut cmd) = backup_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();
    let state_dir = fixture._dir.path().join("state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    std::fs::write(state_dir.join("lock"), "pid 99999").expect("lock");

    cmd.args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", &state_dir)
        .assert()
        .code(1)
        .stderr(predicate::str::contains("another vaultline process"));
}

/// A git mirror source is cloned and enters the backup.
#[test]
fn git_mirror_source_is_captured() {
    let Some(mut cmd) = backup_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();

    // A local git remote with one commit.
    let remote = fixture._dir.path().join("remote.git");
    std::fs::create_dir_all(&remote).expect("remote");
    let git = |args: &[&str], dir: &Path| {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"], &remote);
    git(&["config", "user.email", "test@example.com"], &remote);
    git(&["config", "user.name", "Test"], &remote);
    std::fs::write(remote.join("code.txt"), "fn main() {}").expect("file");
    git(&["add", "code.txt"], &remote);
    git(&["commit", "-q", "-m", "initial"], &remote);

    let contents = std::fs::read_to_string(&fixture.config).expect("config")
        + &format!(
            r#"
[[application.sources.git]]
name = "code"
remote = "{}"
reference = "master"
capture = "mirror"
"#,
            remote.display().to_string().replace('\\', "/")
        );
    std::fs::write(&fixture.config, contents).expect("config");

    cmd.args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture._dir.path().join("state"))
        .assert()
        .success()
        .stdout(predicate::str::contains("backup complete"));

    // The mirrored repository (its HEAD file) is inside the restic repo.
    let ls = fixture.restic(&["ls", "latest"]);
    let listing = String::from_utf8_lossy(&ls.stdout);
    assert!(listing.contains("HEAD"), "{listing}");
}

/// A quiesce rule runs before the backup and its effect is observable.
#[test]
fn quiesce_rule_runs_before_capture() {
    let Some(mut cmd) = backup_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();
    let marker = fixture.uploads.join("quiesced.txt");

    // Cross-platform marker-writing command (shell-free argv).
    let (command, args): (&str, Vec<String>) = if cfg!(windows) {
        (
            "cmd",
            vec![
                "/C".to_string(),
                "echo quiesced >".to_string(),
                marker.to_str().unwrap().to_string(),
            ],
        )
    } else {
        let script = format!("echo quiesced > '{}'", marker.display());
        ("sh", vec!["-c".to_string(), script])
    };
    let quiesce_block = format!(
        "quiesce = {{ command = \"{command}\", args = [{args}] }}",
        args = args
            .iter()
            .map(|a| format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(", "),
    );
    let contents = std::fs::read_to_string(&fixture.config)
        .expect("config")
        .replace("excludes = [\"**/*.tmp\"]", &quiesce_block);
    std::fs::write(&fixture.config, contents).expect("config");

    cmd.args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", fixture._dir.path().join("state"))
        .assert()
        .success();

    assert!(
        marker.exists(),
        "the quiesce rule must have produced its marker before capture"
    );
}
