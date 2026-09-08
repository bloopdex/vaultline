//! The benchmark baseline (ADR-007 / SOT Section 12: baseline before
//! target). Deterministic std-only measurements of the hot paths:
//! configuration load+validate, the retention plan over a large snapshot
//! history, cron next-occurrence evaluation, the state-file round trip,
//! and the CLI startup (`vaultline version` spawn-to-exit — Section 12's
//! startup-time check for CLI projects). Run with `cargo run --release
//! --example bench`; the JSON on stdout is compared against
//! docs/benchmarks/baseline.json by scripts/bench-check.py.
//!
//! Medians, not means: one slow scheduler slice must not move a baseline.

use std::time::Instant;

use chrono::{DateTime, Utc};
use vaultline_core::model::{
    BackupSnapshot, ConfigMetadata, EngineSnapshotRef, IntegrityInfo, RestoreMetadata,
    SourceManifest, VerificationLevel,
};
use vaultline_core::retention::plan;
use vaultline_core::state::State;

/// A realistic full-shape configuration string (mirrors the validated
/// shape the template and the tests carry).
const CONFIG: &str = r#"
[application]
name = "thornwa"
description = "ThornWA compose stack"
schedule = "30 2 * * *"
[[application.sources.files]]
name = "uploads"
paths = ["/srv/thornwa/uploads", "/srv/thornwa/assets"]
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
level = 6
schedule = "0 3 * * 1"
[[application.verification.app_checks]]
name = "pg-integrity"
command = "psql"
args = ["-c", "SELECT 1"]
[application.rehearsal]
target = "/srv/rehearsal/thornwa"
[[application.restore.steps]]
restore_files = { source = "uploads", target = "/srv/thornwa/uploads" }
[[application.restore.steps]]
restore_database = { database = "main", target_database = "thornwa" }
[[application.restore.steps]]
wait_healthy = { url = "http://localhost:3000/health" }
"#;

fn median_ms(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
    samples[samples.len() / 2]
}

fn measure(n: usize, mut f: impl FnMut()) -> serde_json::Value {
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        let start = Instant::now();
        f();
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    serde_json::json!({ "median_ms": median_ms(samples), "n": n })
}

fn sample_snapshot(id: &str, timestamp: DateTime<Utc>) -> BackupSnapshot {
    BackupSnapshot {
        id: id.to_string(),
        application: "thornwa".to_string(),
        timestamp,
        source_manifest: SourceManifest {
            entries: Vec::new(),
        },
        database_metadata: Vec::new(),
        configuration_metadata: ConfigMetadata {
            note: "benchmark".to_string(),
        },
        engine_snapshot: EngineSnapshotRef {
            engine: "restic".to_string(),
            snapshot_id: "engine".to_string(),
        },
        integrity: IntegrityInfo {
            engine_verified: true,
            highest_verified_level: VerificationLevel::L1,
            verified_at: None,
        },
        restore_metadata: RestoreMetadata {
            reconstructs: vec!["uploads".to_string()],
        },
    }
}

/// A snapshot history: 1000 snapshots spread over ~3 years (several per
/// week) — the retention plan's realistic worst case.
fn snapshot_history(count: usize) -> Vec<BackupSnapshot> {
    let base = DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
        .expect("base")
        .to_utc();
    (0..count)
        .map(|i| {
            let hours = (i as i64) * 26; // roughly daily-ish cadence
            sample_snapshot(
                &format!("snap-{i:04}"),
                base + chrono::Duration::hours(hours),
            )
        })
        .collect()
}

/// The sibling release binary (cargo sets CARGO_BIN_EXE_vaultline at
/// runtime; the fallback covers a direct invocation of the example).
fn vaultline_bin() -> std::path::PathBuf {
    if let Ok(bin) = std::env::var("CARGO_BIN_EXE_vaultline") {
        return std::path::PathBuf::from(bin);
    }
    let mut bin = std::env::current_exe()
        .expect("current exe")
        .parent()
        .and_then(|p| p.parent())
        .expect("bin dir")
        .to_path_buf();
    bin.push(if cfg!(windows) {
        "vaultline.exe"
    } else {
        "vaultline"
    });
    bin
}

fn main() {
    let policy = vaultline_core::model::RetentionPolicy {
        keep_last: 14,
        keep_daily: 7,
        keep_weekly: 4,
        keep_monthly: 6,
        keep_yearly: 1,
    };
    let history = snapshot_history(1000);
    let schedule = vaultline_core::cron::parse_schedule("0 3 * * 1").expect("schedule");
    let anchor = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("anchor")
        .to_utc();
    let state_dir = tempfile::tempdir().expect("tempdir");
    let state_path = state_dir.path().join("state.json");
    let mut state = State::new();
    for snapshot in &history {
        state.record_snapshot("thornwa", snapshot.clone());
    }

    let mut cursor = anchor;
    let report = serde_json::json!({
        "config_load_validate": measure(200, || {
            let parsed = vaultline_core::config::parse_str(CONFIG).expect("parses");
            let outcome = vaultline_core::config::validate(parsed);
            assert!(outcome.is_valid(), "{:?}", outcome.errors);
        }),
        "retention_plan_1000": measure(100, || {
            let decisions = plan(&history, &policy);
            assert_eq!(decisions.len(), 1000);
        }),
        "cron_next_after": measure(1000, || {
            cursor = vaultline_core::cron::next_after(&schedule, cursor).expect("next");
        }),
        "state_roundtrip_1000": measure(50, || {
            state.save(&state_path).expect("save");
            let loaded = State::load(&state_path).expect("load");
            assert_eq!(
                loaded.applications["thornwa"].snapshots.len(),
                1000
            );
        }),
        // CLI startup: the same package's release binary, spawn-to-exit
        // for the `version` command (std-only).
        "startup_version": measure(50, || {
            let out = std::process::Command::new(vaultline_bin())
                .arg("version")
                .output()
                .expect("spawns");
            assert!(out.status.success(), "version exits 0");
        }),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("serializes")
    );
}
