//! End-to-end CLI tests: the real binary, real files, the documented exit
//! codes. These pin the contract in docs/cli.md.

use assert_cmd::Command;
use predicates::prelude::*;

/// The valid configuration the template promises to be (mirrors the core
/// crate's fixture).
const VALID: &str = r#"
[application]
name = "thornwa"
[[application.sources.files]]
name = "uploads"
paths = ["/srv/thornwa/uploads"]
[[application.sources.config_refs]]
name = "env"
path = "/srv/thornwa/.env"
[application.storage]
kind = "local"
path = "/var/backups/thornwa"
password_env = "RESTIC_PASSWORD_THORNWA"
[application.retention]
keep_last = 14
[application.verification]
level = 3
[[application.restore.steps]]
restore_files = { source = "uploads", target = "/srv/thornwa/uploads" }
"#;

fn vaultline() -> Command {
    Command::cargo_bin("vaultline").expect("binary built by cargo")
}

#[test]
fn version_reports_the_crate_version() {
    vaultline()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn init_writes_a_template_that_validate_accepts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("vaultline.toml");

    vaultline()
        .args(["init", "--name", "thornwa-test", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains("configuration valid"));

    vaultline()
        .args(["validate", "--config"])
        .arg(&config)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "configuration valid: thornwa-test",
        ));
}

#[test]
fn init_refuses_to_overwrite() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("vaultline.toml");
    std::fs::write(&config, VALID).expect("seed file");

    vaultline()
        .args(["init", "--name", "other-app", "--config"])
        .arg(&config)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("refusing to overwrite"));

    // The original file is untouched.
    assert_eq!(std::fs::read_to_string(&config).expect("read"), VALID);
}

#[test]
fn init_rejects_an_invalid_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("vaultline.toml");

    vaultline()
        .args(["init", "--name", "Bad Name!", "--config"])
        .arg(&config)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("application.name"));

    assert!(!config.exists(), "no file may be written for a bad name");
}

#[test]
fn validate_missing_file_is_operational() {
    let dir = tempfile::tempdir().expect("tempdir");
    vaultline()
        .args(["validate", "--config"])
        .arg(dir.path().join("nope.toml"))
        .assert()
        .code(1)
        .stderr(predicate::str::contains("cannot read configuration file"));
}

#[test]
fn validate_reports_every_error_not_just_the_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("bad.toml");
    std::fs::write(&config, VALID.replace("keep_last = 14", "keep_last = 0"))
        .expect("write fixture");

    vaultline()
        .args(["validate", "--config"])
        .arg(&config)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("delete everything"));
}

#[test]
fn validate_rejects_unknown_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("bad.toml");
    std::fs::write(
        &config,
        VALID.replace(
            "name = \"thornwa\"",
            "name = \"thornwa\"\ncolour = \"blue\"",
        ),
    )
    .expect("write fixture");

    vaultline()
        .args(["validate", "--config"])
        .arg(&config)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown field"));
}

#[test]
fn validate_json_output_is_machine_readable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("vaultline.toml");
    std::fs::write(&config, VALID).expect("write fixture");

    let output = vaultline()
        .args(["validate", "--config"])
        .arg(&config)
        .arg("--json")
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(0));

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(value["valid"], true);
    assert_eq!(value["application"], "thornwa");
    assert_eq!(value["summary"]["verification_level"], 3);
}

#[test]
fn validate_json_output_reports_invalid_with_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("bad.toml");
    std::fs::write(&config, VALID.replace("level = 3", "level = 9")).expect("write fixture");

    let output = vaultline()
        .args(["validate", "--config"])
        .arg(&config)
        .arg("--json")
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(2));

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(value["valid"], false);
    assert!(
        value["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty())
    );
}
