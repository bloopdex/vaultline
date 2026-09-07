//! Deterministic, explainable retention (the design philosophy, Phase 5):
//! `prune` must state, per snapshot, which rule kept or deleted it. This
//! module computes the decision over the recorded snapshots; the prune
//! executor applies it to the engine.
//!
//! The rules (applied to snapshots sorted by timestamp):
//! - `keep_last N` — the N most recent snapshots
//! - `keep_daily D` — the newest snapshot of each of the D newest calendar
//!   days (UTC)
//! - `keep_weekly W` — the newest snapshot of each of the W newest ISO
//!   weeks
//! - `keep_monthly M` — the newest snapshot of each of the M newest
//!   calendar months
//! - `keep_yearly Y` — the newest snapshot of each of the Y newest years
//!
//! A snapshot kept by ANY rule is kept (with all of its reasons); a
//! snapshot kept by none is forgotten. Because the newest snapshot always
//! lies in the newest bucket of every rule, any non-zero policy keeps the
//! newest snapshot — an all-zero policy is refused at configuration time.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{BackupSnapshot, RetentionPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RetentionAction {
    Keep,
    Forget,
}

/// One snapshot's fate under the policy, with its reasons.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetentionDecision {
    pub snapshot_id: String,
    pub timestamp: DateTime<Utc>,
    pub action: RetentionAction,
    /// Every rule that kept the snapshot, or the single reason it matched
    /// none. Rule reasons are ordered keep_last, daily, weekly, monthly,
    /// yearly.
    pub reasons: Vec<String>,
}

impl RetentionDecision {
    pub fn is_kept(&self) -> bool {
        self.action == RetentionAction::Keep
    }
}

/// Compute the retention plan for the recorded snapshots. The output is
/// ordered by timestamp (ascending); ties on the timestamp are broken by
/// snapshot id so the plan is byte-for-byte deterministic.
pub fn plan(snapshots: &[BackupSnapshot], policy: &RetentionPolicy) -> Vec<RetentionDecision> {
    if snapshots.is_empty() {
        return Vec::new();
    }

    let mut ordered: Vec<&BackupSnapshot> = snapshots.iter().collect();
    ordered.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then_with(|| a.id.cmp(&b.id)));

    let mut reasons: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for snapshot in &ordered {
        reasons.insert(&snapshot.id, Vec::new());
    }

    // keep_last: the N most recent.
    let keep_last = policy.keep_last as usize;
    if keep_last > 0 {
        let start = ordered.len().saturating_sub(keep_last);
        for snapshot in &ordered[start..] {
            reasons
                .get_mut(snapshot.id.as_str())
                .expect("inserted")
                .push(format!(
                    "among the {keep_last} most recent snapshots (keep_last)"
                ));
        }
    }

    // keep_daily: newest snapshot per calendar day; the D newest days.
    let keep_daily = policy.keep_daily as usize;
    if keep_daily > 0 {
        let mut days: Vec<(DateTime<Utc>, &BackupSnapshot)> =
            newest_per(&ordered, |ts| (ts.year(), ts.month(), ts.day()));
        sort_newest_groups(&mut days);
        for (_, snapshot) in days.into_iter().take(keep_daily) {
            let date = snapshot
                .timestamp
                .date_naive()
                .format("%Y-%m-%d")
                .to_string();
            reasons
                .get_mut(snapshot.id.as_str())
                .expect("inserted")
                .push(format!("newest snapshot of {date} (keep_daily)"));
        }
    }

    // keep_weekly: newest snapshot per ISO week; the W newest weeks.
    let keep_weekly = policy.keep_weekly as usize;
    if keep_weekly > 0 {
        let mut weeks: Vec<(DateTime<Utc>, &BackupSnapshot)> = newest_per(&ordered, |ts| {
            let week = ts.date_naive().iso_week();
            (week.year(), week.week())
        });
        sort_newest_groups(&mut weeks);
        for (_, snapshot) in weeks.into_iter().take(keep_weekly) {
            let week = snapshot.timestamp.date_naive().iso_week();
            reasons
                .get_mut(snapshot.id.as_str())
                .expect("inserted")
                .push(format!(
                    "newest snapshot of week {}-W{:02} (keep_weekly)",
                    week.year(),
                    week.week()
                ));
        }
    }

    // keep_monthly: newest snapshot per calendar month; the M newest months.
    let keep_monthly = policy.keep_monthly as usize;
    if keep_monthly > 0 {
        let mut months: Vec<(DateTime<Utc>, &BackupSnapshot)> =
            newest_per(&ordered, |ts| (ts.year(), ts.month()));
        sort_newest_groups(&mut months);
        for (_, snapshot) in months.into_iter().take(keep_monthly) {
            reasons
                .get_mut(snapshot.id.as_str())
                .expect("inserted")
                .push(format!(
                    "newest snapshot of {}-{:02} (keep_monthly)",
                    snapshot.timestamp.year(),
                    snapshot.timestamp.month()
                ));
        }
    }

    // keep_yearly: newest snapshot per year; the Y newest years.
    let keep_yearly = policy.keep_yearly as usize;
    if keep_yearly > 0 {
        let mut years: Vec<(DateTime<Utc>, &BackupSnapshot)> =
            newest_per(&ordered, |ts| (ts.year(),));
        sort_newest_groups(&mut years);
        for (_, snapshot) in years.into_iter().take(keep_yearly) {
            reasons
                .get_mut(snapshot.id.as_str())
                .expect("inserted")
                .push(format!(
                    "newest snapshot of {} (keep_yearly)",
                    snapshot.timestamp.year()
                ));
        }
    }

    ordered
        .into_iter()
        .map(|snapshot| {
            let mut snapshot_reasons = reasons.remove(snapshot.id.as_str()).expect("inserted");
            let action = if snapshot_reasons.is_empty() {
                snapshot_reasons
                    .push("matched no retention rule — the policy does not keep it".to_string());
                RetentionAction::Forget
            } else {
                RetentionAction::Keep
            };
            RetentionDecision {
                snapshot_id: snapshot.id.clone(),
                timestamp: snapshot.timestamp,
                action,
                reasons: snapshot_reasons,
            }
        })
        .collect()
}

/// Group snapshots by a bucket key; return the newest snapshot per bucket
/// plus the bucket's newest timestamp (for ordering buckets by age).
fn newest_per<'a, K: Ord + Clone>(
    ordered: &[&'a BackupSnapshot],
    key: impl Fn(DateTime<Utc>) -> K,
) -> Vec<(DateTime<Utc>, &'a BackupSnapshot)> {
    let mut newest: BTreeMap<K, (DateTime<Utc>, &'a BackupSnapshot)> = BTreeMap::new();
    for snapshot in ordered {
        let bucket = key(snapshot.timestamp);
        newest
            .entry(bucket)
            .and_modify(|(ts, existing)| {
                // Input is ascending; later timestamps always win, with the
                // id as the tie-breaker for same-instant snapshots.
                if snapshot.timestamp >= *ts {
                    *ts = snapshot.timestamp;
                    *existing = snapshot;
                }
            })
            .or_insert((snapshot.timestamp, snapshot));
    }
    newest.into_values().collect()
}

/// Order groups newest-first by their newest timestamp (ties: later item
/// wins is fine — the take() boundary is only reached on timestamp ties
/// at the bucket boundary, which real timestamps do not produce).
fn sort_newest_groups(groups: &mut [(DateTime<Utc>, &BackupSnapshot)]) {
    groups.sort_by_key(|(ts, _)| std::cmp::Reverse(*ts));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ConfigMetadata, DatabaseSnapshotMeta, EngineSnapshotRef, IntegrityInfo, RestoreMetadata,
        SourceManifest, VerificationLevel,
    };

    fn snapshot(id: &str, timestamp: &str) -> BackupSnapshot {
        BackupSnapshot {
            id: id.to_string(),
            application: "thornwa".to_string(),
            timestamp: DateTime::parse_from_rfc3339(timestamp)
                .expect("rfc3339")
                .to_utc(),
            source_manifest: SourceManifest {
                entries: Vec::new(),
            },
            database_metadata: Vec::<DatabaseSnapshotMeta>::new(),
            configuration_metadata: ConfigMetadata {
                note: "test".to_string(),
            },
            engine_snapshot: EngineSnapshotRef {
                engine: "restic".to_string(),
                snapshot_id: "engine".to_string(),
            },
            integrity: IntegrityInfo {
                engine_verified: false,
                highest_verified_level: VerificationLevel::L1,
                verified_at: None,
            },
            restore_metadata: RestoreMetadata {
                reconstructs: Vec::new(),
            },
        }
    }

    fn policy(
        keep_last: u32,
        keep_daily: u32,
        keep_weekly: u32,
        keep_monthly: u32,
        keep_yearly: u32,
    ) -> RetentionPolicy {
        RetentionPolicy {
            keep_last,
            keep_daily,
            keep_weekly,
            keep_monthly,
            keep_yearly,
        }
    }

    fn kept_ids(decisions: &[RetentionDecision]) -> Vec<&str> {
        decisions
            .iter()
            .filter(|d| d.is_kept())
            .map(|d| d.snapshot_id.as_str())
            .collect()
    }

    #[test]
    fn empty_input_plans_empty() {
        assert!(plan(&[], &policy(7, 0, 0, 0, 0)).is_empty());
    }

    #[test]
    fn keep_last_keeps_the_newest_n() {
        let snapshots = vec![
            snapshot("a", "2026-09-01T10:00:00Z"),
            snapshot("b", "2026-09-02T10:00:00Z"),
            snapshot("c", "2026-09-03T10:00:00Z"),
        ];
        let decisions = plan(&snapshots, &policy(2, 0, 0, 0, 0));
        assert_eq!(kept_ids(&decisions), vec!["b", "c"]);
        assert_eq!(decisions[0].action, RetentionAction::Forget);
        assert!(decisions[0].reasons[0].contains("no retention rule"));
        assert!(decisions[1].reasons[0].contains("keep_last"));
    }

    #[test]
    fn daily_keeps_newest_per_day_up_to_d_days() {
        let snapshots = vec![
            snapshot("d1-old", "2026-09-01T08:00:00Z"),
            snapshot("d1-new", "2026-09-01T20:00:00Z"),
            snapshot("d2", "2026-09-02T10:00:00Z"),
            snapshot("d3", "2026-09-03T10:00:00Z"),
        ];
        // keep_daily 2: the 2 newest days are 09-02 and 09-03 — each day's
        // newest survives; day 1 (both snapshots) is outside the window.
        let decisions = plan(&snapshots, &policy(0, 2, 0, 0, 0));
        assert_eq!(kept_ids(&decisions), vec!["d2", "d3"]);
        assert!(
            decisions
                .iter()
                .find(|d| d.snapshot_id == "d2")
                .expect("kept")
                .reasons[0]
                .contains("2026-09-02")
        );
    }

    #[test]
    fn daily_three_days_keeps_newest_per_day() {
        let snapshots = vec![
            snapshot("d1-old", "2026-09-01T08:00:00Z"),
            snapshot("d1-new", "2026-09-01T20:00:00Z"),
            snapshot("d2", "2026-09-02T10:00:00Z"),
            snapshot("d3", "2026-09-03T10:00:00Z"),
        ];
        // keep_daily 3 covers all three days; day 1 contributes only its
        // newest snapshot.
        let decisions = plan(&snapshots, &policy(0, 3, 0, 0, 0));
        assert_eq!(kept_ids(&decisions), vec!["d1-new", "d2", "d3"]);
    }

    #[test]
    fn weekly_keeps_newest_per_iso_week() {
        // 2026-09-01 is a Tuesday; 2026-09-07 is a Monday (ISO week starts
        // Monday) — so 09-01..09-06 is one ISO week, 09-07 the next.
        let snapshots = vec![
            snapshot("w1-a", "2026-09-01T10:00:00Z"),
            snapshot("w1-b", "2026-09-03T10:00:00Z"),
            snapshot("w2", "2026-09-07T10:00:00Z"),
        ];
        // keep_weekly 1: only the newest week (w2).
        let decisions = plan(&snapshots, &policy(0, 0, 1, 0, 0));
        assert_eq!(kept_ids(&decisions), vec!["w2"]);
        // keep_weekly 2: newest per week for both weeks.
        let decisions = plan(&snapshots, &policy(0, 0, 2, 0, 0));
        assert_eq!(kept_ids(&decisions), vec!["w1-b", "w2"]);
    }

    #[test]
    fn monthly_and_yearly_buckets_keep_the_newest() {
        let snapshots = vec![
            snapshot("y1-m1-a", "2025-01-05T10:00:00Z"),
            snapshot("y1-m1-b", "2025-01-25T10:00:00Z"),
            snapshot("y1-m2", "2025-02-10T10:00:00Z"),
            snapshot("y2", "2026-01-10T10:00:00Z"),
        ];
        // keep_monthly 1: newest month is 2026-01 → y2 only.
        let decisions = plan(&snapshots, &policy(0, 0, 0, 1, 0));
        assert_eq!(kept_ids(&decisions), vec!["y2"]);
        // keep_yearly 2: the newest snapshot of each year — 2025's newest
        // is y1-m2 (February), not y1-m1-b.
        let decisions = plan(&snapshots, &policy(0, 0, 0, 0, 2));
        assert_eq!(kept_ids(&decisions), vec!["y1-m2", "y2"]);
    }

    #[test]
    fn rules_union_with_all_reasons() {
        let snapshots = vec![
            snapshot("recent", "2026-09-07T10:00:00Z"),
            snapshot("old", "2026-01-01T10:00:00Z"),
        ];
        // keep_last 1 AND keep_yearly 1: both keep "recent" (newest + newest
        // of its year); "old" is the newest of 2026-01? No — 2026-01-01 and
        // 2026-09-07 are the SAME year, so yearly keeps only "recent".
        let decisions = plan(&snapshots, &policy(1, 0, 0, 0, 1));
        let recent = decisions
            .iter()
            .find(|d| d.snapshot_id == "recent")
            .expect("kept");
        assert_eq!(
            recent.reasons.len(),
            2,
            "both rules keep it: {:?}",
            recent.reasons
        );
        assert!(
            decisions
                .iter()
                .find(|d| d.snapshot_id == "old")
                .expect("old")
                .reasons[0]
                .contains("no retention rule")
        );
    }

    #[test]
    fn newest_snapshot_is_always_kept_by_any_nonzero_policy() {
        let snapshots = vec![
            snapshot("a", "2026-09-01T10:00:00Z"),
            snapshot("b", "2026-09-07T10:00:00Z"),
        ];
        for policy in [
            policy(0, 1, 0, 0, 0),
            policy(0, 0, 1, 0, 0),
            policy(0, 0, 0, 1, 0),
            policy(0, 0, 0, 0, 1),
            policy(1, 0, 0, 0, 0),
        ] {
            let decisions = plan(&snapshots, &policy);
            assert!(
                decisions
                    .iter()
                    .find(|d| d.snapshot_id == "b")
                    .expect("newest present")
                    .is_kept(),
                "newest must survive"
            );
        }
    }

    #[test]
    fn output_is_ordered_by_timestamp() {
        let snapshots = vec![
            snapshot("later", "2026-09-03T10:00:00Z"),
            snapshot("earlier", "2026-09-01T10:00:00Z"),
        ];
        let decisions = plan(&snapshots, &policy(7, 0, 0, 0, 0));
        assert_eq!(decisions[0].snapshot_id, "earlier");
        assert_eq!(decisions[1].snapshot_id, "later");
    }
}
