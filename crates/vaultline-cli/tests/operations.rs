//! Operations integration tests (Phase 5): prune, schedule due-ness,
//! doctor, status, and the systemd timer generation — against the real
//! restic binary. Gated on restic availability exactly like tests/backup.rs.

use std::path::{Path, PathBuf};
use std::process::Command;

use predicates::prelude::*;

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

fn op_cmd() -> Option<assert_cmd::Command> {
    restic_bin().map(|_| vaultline())
}

/// The fixture: a config with a daily backup schedule, a weekday
/// verification schedule, keep_last 2 retention, one files source.
const CONFIG: &str = r#"
[application]
name = "thornwa"
schedule = "0 3 * * *"
[[application.sources.files]]
name = "uploads"
paths = ["{uploads}"]
[application.storage]
kind = "local"
path = "{repo_parent}"
password_env = "VAULTLINE_TEST_PASSWORD"
[application.retention]
keep_last = 2
[application.verification]
level = 3
schedule = "0 3 * * 1"
[[application.restore.steps]]
restore_files = { source = "uploads", target = "/srv/thornwa/uploads" }
"#;

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
        let contents = CONFIG
            .replace("{uploads}", &toml_path(&uploads))
            .replace("{repo_parent}", &toml_path(&repo_parent));
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

    fn envs<'a>(&self, cmd: &'a mut assert_cmd::Command) -> &'a mut assert_cmd::Command {
        cmd.env("VAULTLINE_TEST_PASSWORD", "test-password")
            .env("VAULTLINE_STATE_DIR", self.state_dir())
    }

    fn backup(&self) {
        let mut cmd = vaultline();
        self.envs(&mut cmd);
        cmd.args(["backup", "run", "--config"])
            .arg(&self.config)
            .assert()
            .success();
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

    fn engine_snapshot_count(&self) -> usize {
        let out = self.restic(&["--json", "snapshots"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let values: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
            .flat_map(|value| match value {
                serde_json::Value::Array(items) => items,
                other => vec![other],
            })
            .collect();
        values.len()
    }
}

/// Dry-run explains every decision and changes nothing.
#[test]
fn prune_dry_run_explains_and_changes_nothing() {
    let Some(mut cmd) = op_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();
    for _ in 0..3 {
        fixture.backup();
    }
    assert_eq!(fixture.engine_snapshot_count(), 3);

    cmd.args(["backup", "prune", "--config"])
        .arg(&fixture.config);
    fixture.envs(&mut cmd);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("KEEP"))
        .stdout(predicate::str::contains("FORGET"))
        .stdout(predicate::str::contains("keep_last"))
        .stdout(predicate::str::contains("dry run — nothing changed"));

    // Nothing changed: the engine still holds all three snapshots, and no
    // prune is recorded in state.
    assert_eq!(fixture.engine_snapshot_count(), 3);
    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.state_dir().join("state.json")).expect("state"),
    )
    .expect("json");
    assert_eq!(
        state["applications"]["thornwa"]["operations"]["prunes"]
            .as_array()
            .expect("prunes array")
            .len(),
        0
    );
}

/// --json carries the full plan with reasons.
#[test]
fn prune_json_payload_carries_decisions() {
    let Some(mut cmd) = op_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();
    fixture.backup();

    cmd.args(["backup", "prune", "--config"])
        .arg(&fixture.config)
        .arg("--json");
    fixture.envs(&mut cmd);
    let out = cmd.output().expect("runs");
    assert!(out.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(payload["kept"], 1);
    assert_eq!(payload["to_forget"], 0);
    assert_eq!(payload["applied"], false);
    assert_eq!(payload["plan"][0]["action"], "keep");
    assert!(
        payload["plan"][0]["reasons"][0]
            .as_str()
            .expect("reason")
            .contains("keep_last")
    );
}

/// --apply forgets what the policy does not keep, prunes the repository,
/// records the outcome — and is idempotent on re-run.
#[test]
fn prune_apply_forgets_records_and_is_idempotent() {
    let Some(mut cmd) = op_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();
    for _ in 0..3 {
        fixture.backup();
    }
    assert_eq!(fixture.engine_snapshot_count(), 3);

    cmd.args(["backup", "prune", "--config"])
        .arg(&fixture.config)
        .arg("--apply");
    fixture.envs(&mut cmd);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("prune applied"))
        .stdout(predicate::str::contains("1 snapshot(s) forgotten, 2 kept"));

    // keep_last 2 over three snapshots: two engine snapshots remain.
    assert_eq!(fixture.engine_snapshot_count(), 2);

    let state_path = fixture.state_dir().join("state.json");
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).expect("state")).expect("json");
    let prunes = state["applications"]["thornwa"]["operations"]["prunes"]
        .as_array()
        .expect("prunes array");
    assert_eq!(prunes.len(), 1, "one applied prune recorded");
    assert_eq!(prunes[0]["forgotten"].as_array().expect("ids").len(), 1);
    assert_eq!(prunes[0]["kept"], 2);
    assert!(
        state["applications"]["thornwa"]["operations"]["last_prune_at"]
            .as_str()
            .is_some()
    );

    // Idempotent: the forgotten ids no longer exist in the engine, so a
    // second apply forgets nothing (but still records the run honestly).
    let mut again = vaultline();
    fixture.envs(&mut again);
    again
        .args(["backup", "prune", "--config"])
        .arg(&fixture.config)
        .arg("--apply")
        .assert()
        .success()
        .stdout(predicate::str::contains("0 snapshot(s) forgotten, 2 kept"));
    assert_eq!(fixture.engine_snapshot_count(), 2);
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).expect("state")).expect("json");
    assert_eq!(
        state["applications"]["thornwa"]["operations"]["prunes"]
            .as_array()
            .expect("prunes")
            .len(),
        2
    );
}

/// schedule run executes the due backup + verification, records the
/// bookkeeping, and a second immediate run finds nothing due.
#[test]
fn schedule_run_executes_due_jobs_once() {
    let Some(mut cmd) = op_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();

    // Never ran: both jobs are due immediately.
    cmd.args(["schedule", "run", "--config"])
        .arg(&fixture.config);
    fixture.envs(&mut cmd);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("backup due"))
        .stdout(predicate::str::contains("verification due"))
        .stdout(predicate::str::contains("verifying latest"));

    // The backup landed a snapshot; the verification bookkeeping recorded
    // the attempt; verify raised the recorded level to L3.
    let state_path = fixture.state_dir().join("state.json");
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).expect("state")).expect("json");
    assert_eq!(
        state["applications"]["thornwa"]["snapshots"]
            .as_array()
            .expect("snapshots")
            .len(),
        1
    );
    assert!(
        state["applications"]["thornwa"]["operations"]["last_verify_at"]
            .as_str()
            .is_some()
    );
    assert_eq!(
        state["applications"]["thornwa"]["snapshots"][0]["integrity"]["highest_verified_level"],
        "L3"
    );

    // The next occurrences are in the future — nothing due now.
    let mut again = vaultline();
    fixture.envs(&mut again);
    again
        .args(["schedule", "run", "--config"])
        .arg(&fixture.config)
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing due"));
    assert_eq!(
        state["applications"]["thornwa"]["snapshots"]
            .as_array()
            .expect("snapshots")
            .len(),
        1
    );
}

/// doctor reports a healthy environment as all-ok (exit 0) and a broken
/// one with a failing check (exit 1).
#[test]
fn doctor_reports_health_and_failures() {
    let Some(mut cmd) = op_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();
    fixture.backup();

    // Healthy: config ok, restic ok, state ok, storage ok (password set,
    // the repository exists after the backup).
    cmd.args(["doctor", "--config"]).arg(&fixture.config);
    fixture.envs(&mut cmd);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("healthy"))
        .stdout(predicate::str::contains("ok\tconfig"))
        .stdout(predicate::str::contains("ok\trestic"))
        .stdout(predicate::str::contains("ok\tstorage"));

    // Broken restic override: the restic check fails and the exit code is 1.
    let mut broken = vaultline();
    fixture.envs(&mut broken);
    broken
        .args(["doctor", "--config"])
        .arg(&fixture.config)
        .env(
            "VAULTLINE_RESTIC_BIN",
            fixture._dir.path().join("no-such-restic"),
        )
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("error\trestic"));
}

/// doctor --json carries the per-check statuses.
#[test]
fn doctor_json_payload_is_structured() {
    let Some(mut cmd) = op_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();
    fixture.backup();

    cmd.args(["doctor", "--config"])
        .arg(&fixture.config)
        .arg("--json");
    fixture.envs(&mut cmd);
    let out = cmd.output().expect("runs");
    assert!(out.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(payload["healthy"], true);
    let checks = payload["checks"].as_array().expect("checks");
    assert!(
        checks
            .iter()
            .any(|c| c["name"] == "config" && c["status"] == "ok")
    );
    assert!(
        checks
            .iter()
            .any(|c| c["name"] == "storage" && c["status"] == "ok")
    );
}

/// status summarizes records, schedules, and the retention projection.
#[test]
fn status_summarizes_the_application() {
    let Some(mut cmd) = op_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();
    fixture.backup();

    cmd.args(["status", "--config"]).arg(&fixture.config);
    fixture.envs(&mut cmd);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("application: thornwa"))
        .stdout(predicate::str::contains("1 recorded, 0 pruned"))
        .stdout(predicate::str::contains("last backup:"))
        .stdout(predicate::str::contains("\"0 3 * * *\" — next occurrence"))
        .stdout(predicate::str::contains(
            "policy keeps 1, 0 would be forgotten",
        ));

    let mut json_cmd = vaultline();
    fixture.envs(&mut json_cmd);
    json_cmd
        .args(["status", "--config"])
        .arg(&fixture.config)
        .arg("--json")
        .assert()
        .success();
    let payload: serde_json::Value =
        serde_json::from_slice(&json_cmd.output().expect("runs").stdout).expect("json");
    assert_eq!(payload["snapshots"]["recorded"], 1);
    assert_eq!(payload["retention"]["projected_kept"], 1);
}

/// timer generate writes the service/timer pairs with translated
/// OnCalendar values; without schedules it refuses.
#[test]
fn timer_generate_writes_unit_files() {
    let Some(mut cmd) = op_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();
    let out_dir = fixture._dir.path().join("units");

    cmd.args(["timer", "generate", "--config"])
        .arg(&fixture.config)
        .arg("--out")
        .arg(&out_dir);
    fixture.envs(&mut cmd);
    cmd.assert().success();

    let service =
        std::fs::read_to_string(out_dir.join("vaultline-thornwa.service")).expect("service");
    let timer = std::fs::read_to_string(out_dir.join("vaultline-thornwa.timer")).expect("timer");
    let verify_service = std::fs::read_to_string(out_dir.join("vaultline-thornwa-verify.service"))
        .expect("verify service");
    let verify_timer = std::fs::read_to_string(out_dir.join("vaultline-thornwa-verify.timer"))
        .expect("verify timer");

    assert!(service.contains("ExecStart="), "{service}");
    assert!(service.contains("backup run --config"), "{service}");
    assert!(service.contains("[Service]\nType=oneshot"), "{service}");
    assert!(timer.contains("OnCalendar=*-*-* 3:0:00"), "{timer}");
    assert!(timer.contains("Persistent=true"), "{timer}");
    assert!(timer.contains("Unit=vaultline-thornwa.service"), "{timer}");
    assert!(timer.contains("WantedBy=timers.target"), "{timer}");
    assert!(
        verify_service.contains("backup verify latest"),
        "{verify_service}"
    );
    // "0 3 * * 1" is Sunday in the crate's day-of-week range (1..=7,
    // Sunday=1) — the translation carries that.
    assert!(
        verify_timer.contains("OnCalendar=Sun *-*-* 3:0:00"),
        "{verify_timer}"
    );

    // A definition without schedules has nothing to generate.
    let no_schedule_dir = fixture._dir.path().join("no-schedule");
    std::fs::create_dir_all(&no_schedule_dir).expect("dir");
    let mut config = std::fs::read_to_string(&fixture.config).expect("config");
    config = config.replace("schedule = \"0 3 * * *\"\n", "");
    config = config.replace("schedule = \"0 3 * * 1\"\n", "");
    let no_schedule_config = no_schedule_dir.join("vaultline.toml");
    std::fs::write(&no_schedule_config, config).expect("write");
    let mut refused = vaultline();
    fixture.envs(&mut refused);
    refused
        .args(["timer", "generate", "--config"])
        .arg(&no_schedule_config)
        .arg("--out")
        .arg(no_schedule_dir.join("units"))
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("no schedules declared"));
}

/// timer install is a Linux-only command; elsewhere it refuses clearly.
#[cfg(not(target_os = "linux"))]
#[test]
fn timer_install_refuses_off_linux() {
    let fixture = Fixture::new();
    let mut cmd = vaultline();
    fixture.envs(&mut cmd);
    cmd.args(["timer", "install", "--config"])
        .arg(&fixture.config)
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("Linux-only"));
}

/// The stale-lock recovery (ADR-007): a lock whose recorded holder pid
/// is ALIVE refuses the run; the same lock, once the holder died, is
/// reclaimed with a warning and the run proceeds — a crashed backup no
/// longer wedges every future run.
#[test]
fn stale_lock_is_refused_while_alive_and_reclaimed_when_dead() {
    let Some(mut cmd) = op_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();

    // A live holder: a sleeping child process whose pid the lock records.
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
        format!(
            "pid {}
",
            holder.id()
        ),
    )
    .expect("lock file");

    // The holder is alive: the run refuses.
    cmd.args(["backup", "run", "--config"]).arg(&fixture.config);
    fixture.envs(&mut cmd);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("another vaultline process"));

    // The holder dies: the next run reclaims the stale lock and proceeds.
    let _ = holder.kill();
    let _ = holder.wait();
    let mut recovered = vaultline();
    fixture.envs(&mut recovered);
    recovered
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .assert()
        .success()
        .stderr(predicate::str::contains("reclaiming the stale lock"))
        .stdout(predicate::str::contains("backup complete"));
}

/// The pid-reuse defense (ADR-007, the fix-round hardening): a lock whose
/// recorded pid is ALIVE but whose recorded start time differs from the
/// current process at that pid is a REUSED pid — the crashed holder is
/// gone and an unrelated process inherited its pid. Reclaimed by the same
/// evidence path; the matching-start-time case stays refused.
#[test]
fn a_reused_pid_is_reclaimed_and_the_matching_holder_is_refused() {
    let Some(mut cmd) = op_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();

    // A live unrelated process standing in for the pid-reuse victim.
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

    // The lock records the LIVE holder's pid with a start time that is
    // not the real one (no live process started at 0): a reused pid —
    // reclaimed, and the backup proceeds.
    std::fs::write(
        fixture.state_dir().join("lock"),
        format!("pid {}\nstarted 0\n", holder.id()),
    )
    .expect("lock file");
    cmd.args(["backup", "run", "--config"]).arg(&fixture.config);
    fixture.envs(&mut cmd);
    cmd.assert()
        .success()
        .stderr(predicate::str::contains("reclaiming the stale lock"))
        .stdout(predicate::str::contains("backup complete"));

    // The same live pid with its REAL start time: the same process the
    // lock recorded — refused exactly like any held lock.
    let started = vaultline_cli::backup::process_started(holder.id())
        .expect("the start-time probe answers on this host");
    std::fs::write(
        fixture.state_dir().join("lock"),
        format!("pid {}\nstarted {started}\n", holder.id()),
    )
    .expect("lock file");
    let mut refused = vaultline();
    fixture.envs(&mut refused);
    refused
        .args(["backup", "run", "--config"])
        .arg(&fixture.config)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("another vaultline process"));

    let _ = holder.kill();
    let _ = holder.wait();
}

/// The vanished-source defense (ADR-007): restic skips missing paths
/// silently (verified empirically), so a declared source that vanished
/// must abort the backup naming the path — never a silently-incomplete
/// snapshot.
#[test]
fn vanished_source_aborts_the_backup() {
    let Some(mut cmd) = op_cmd() else {
        eprintln!("skipping: restic not available (VAULTLINE_RESTIC_BIN or PATH)");
        return;
    };
    let fixture = Fixture::new();
    // Append a second files source that does not exist.
    let mut config = std::fs::read_to_string(&fixture.config).expect("config");
    config.push_str(
        "
[[application.sources.files]]
name = \"vanished\"
paths = [\"/definitely/not/here-vaultline\"]
",
    );
    std::fs::write(&fixture.config, config).expect("config");

    cmd.args(["backup", "run", "--config"]).arg(&fixture.config);
    fixture.envs(&mut cmd);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("vanished"));

    // No snapshot was recorded — the failed run left nothing behind.
    let state_path = fixture.state_dir().join("state.json");
    if state_path.exists() {
        let state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state_path).expect("state"))
                .expect("json");
        assert_eq!(
            state["applications"]["thornwa"]["snapshots"]
                .as_array()
                .expect("snapshots")
                .len(),
            0
        );
    }
}

/// validate rejects invalid crons and both-restricted day fields.
#[test]
fn validate_rejects_bad_schedules() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("vaultline.toml");
    let write = |contents: &str| std::fs::write(&config, contents).expect("write");

    let base = |schedule: &str| {
        format!(
            r#"
[application]
name = "sched-app"
schedule = "{schedule}"
[[application.sources.files]]
name = "uploads"
paths = ["/srv/uploads"]
[application.storage]
kind = "local"
path = "/var/backups/sched-app"
password_env = "RESTIC_PASSWORD"
[application.retention]
keep_last = 7
[application.verification]
level = 1
"#
        )
    };

    write(&base("not a cron"));
    vaultline()
        .args(["validate", "--config"])
        .arg(&config)
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("application.schedule"));

    // Both day fields restricted: ambiguous semantics, refused.
    write(&base("0 9 13 * 6"));
    vaultline()
        .args(["validate", "--config"])
        .arg(&config)
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "both day-of-month and day-of-week",
        ));

    write(&base("30 2 * * *"));
    vaultline()
        .args(["validate", "--config"])
        .arg(&config)
        .assert()
        .success();
}

/// The release surfaces: `version` reports the binary version, the
/// state-file schema version, and the orchestrated engine — human and
/// machine forms (the release-checklist contract).
#[test]
fn version_reports_the_versioned_surfaces() {
    vaultline()
        .args(["version"])
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")))
        .stdout(predicate::str::contains("state schema: v1"))
        .stdout(predicate::str::contains("restic"));

    vaultline()
        .args(["version", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "\"version\":\"{}\"",
            env!("CARGO_PKG_VERSION")
        )))
        .stdout(predicate::str::contains("\"state_schema\":1"))
        .stdout(predicate::str::contains("\"engine\":\"restic\""));
}
