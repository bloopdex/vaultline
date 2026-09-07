//! `vaultline backup prune` — retention enforcement with per-snapshot
//! explanations (the design philosophy: retention decisions state their
//! reason). Safe by default: without `--apply` nothing changes — the plan
//! prints, every decision with its reasons. With `--apply`, the recorded
//! snapshots the plan forgets are forgotten from the engine
//! (cross-checked against the engine's listing first — never blind),
//! `restic prune` reclaims space, and the outcome is recorded in the
//! state file.

use std::path::PathBuf;

use chrono::Utc;
use tracing::warn;

use vaultline_core::config;
use vaultline_core::error::{ErrorKind, Result, VaultlineError};
use vaultline_core::retention::{RetentionAction, plan};
use vaultline_core::state::{PruneRecord, State, StateLock};

use crate::backup::{repo_url, resolve_password, state_dir, storage_envs};
use crate::engine::Restic;
use crate::metrics;

#[derive(clap::Args)]
pub struct BackupPruneArgs {
    #[arg(long, default_value = "vaultline.toml")]
    pub config: PathBuf,

    /// Apply the plan: forget the snapshots the policy does not keep and
    /// run `restic prune`. Without this flag nothing changes.
    #[arg(long)]
    pub apply: bool,

    /// Machine-readable result on stdout (exit code unchanged).
    #[arg(long)]
    pub json: bool,
}

pub fn run_prune(args: BackupPruneArgs) -> Result<()> {
    let app = config::load(&args.config)?;
    let state_dir = state_dir()?;
    let _lock = StateLock::acquire(&state_dir)?;
    let state_path = state_dir.join("state.json");
    let state = State::load(&state_path)?;
    let snapshots = state
        .applications
        .get(&app.name)
        .map(|s| s.snapshots.as_slice())
        .unwrap_or(&[]);

    if snapshots.is_empty() {
        println!("no snapshots recorded for {} — nothing to prune", app.name);
        return Ok(());
    }

    let decisions = plan(snapshots, &app.retention);
    let forget_count = decisions
        .iter()
        .filter(|d| d.action == RetentionAction::Forget)
        .count();
    let kept_count = decisions.len() - forget_count;

    if args.json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "plan": decisions,
                "kept": kept_count,
                "to_forget": forget_count,
                "applied": args.apply,
            }))
            .expect("decisions serialize")
        );
    } else {
        for decision in &decisions {
            let verb = if decision.is_kept() { "KEEP" } else { "FORGET" };
            println!(
                "{verb}\t{}\t{}\t{}",
                decision.snapshot_id,
                decision.timestamp.to_rfc3339(),
                decision.reasons.join("; "),
            );
        }
        println!(
            "policy keeps {kept_count} of {} recorded snapshot(s)",
            decisions.len()
        );
    }

    metrics::emit_u64("prune_runs", 1);
    metrics::emit_u64("prune_snapshots_kept", kept_count as u64);
    metrics::emit_u64("prune_snapshots_forgotten", forget_count as u64);

    if !args.apply {
        println!(
            "dry run — nothing changed. Re-run with --apply to forget {forget_count} snapshot(s) and reclaim repository space."
        );
        return Ok(());
    }

    // Apply: cross-check the engine first — forget only ids the engine
    // still holds (an id already gone is noted, not sent to `forget`).
    let restic = Restic::locate()?;
    let password = resolve_password(&app)?;
    let repo = repo_url(&app)?;
    let extra_envs = storage_envs(&app)?;
    let engine_snapshots = restic.list_snapshots(&repo, &password, &extra_envs)?;
    let engine_has = |full_id: &str| {
        engine_snapshots.iter().any(|line| {
            line.get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|listed| listed == full_id || full_id.starts_with(listed))
        })
    };

    let mut forgotten: Vec<String> = Vec::new();
    let mut engine_ids: Vec<String> = Vec::new();
    for decision in &decisions {
        if decision.action != RetentionAction::Forget {
            continue;
        }
        let snapshot = snapshots
            .iter()
            .find(|s| s.id == decision.snapshot_id)
            .ok_or_else(|| {
                VaultlineError::new(
                    ErrorKind::Internal,
                    "a retention decision references a snapshot that is not recorded — this is a bug",
                )
            })?;
        let engine_id = &snapshot.engine_snapshot.snapshot_id;
        if engine_has(engine_id) {
            engine_ids.push(engine_id.clone());
            forgotten.push(decision.snapshot_id.clone());
        } else {
            warn!(
                snapshot = snapshot.id,
                engine_id,
                "the engine no longer holds this snapshot (already forgotten or removed); skipping"
            );
        }
    }

    if !engine_ids.is_empty() {
        restic.forget(&engine_ids, &repo, &password, &extra_envs)?;
        restic.prune(&repo, &password, &extra_envs)?;
    }

    // Record the outcome: snapshots stay immutable; the operations record
    // notes what was forgotten and when.
    let now = Utc::now();
    let mut state = State::load(&state_path)?;
    let entry = state.applications.entry(app.name.clone()).or_default();
    entry.operations.last_prune_at = Some(now);
    entry.operations.prunes.push(PruneRecord {
        at: now,
        forgotten,
        kept: kept_count,
    });
    state.save(&state_path)?;

    println!(
        "prune applied: {} snapshot(s) forgotten, {} kept; repository space reclaimed",
        engine_ids.len(),
        kept_count
    );
    Ok(())
}
