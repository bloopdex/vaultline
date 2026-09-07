# Observability

Structured logging from day one (SOT Section 14): the questions are *what
happened, why did it happen, what is happening now, is performance
degrading*.

## Logging

- **Format**: JSON lines by default (the machine-readable contract),
  `--log-format pretty` for humans (also `VAULTLINE_LOG_FORMAT`).
- **Destination**: stderr. stdout carries machine-readable command output
  only.
- **Level**: `RUST_LOG` (default `vaultline=info`).
- **Content**: field paths, counts, and outcomes — never environment
  values or credential material (the redaction contract, docs/security.md).

## Named metrics

One structured stderr event per metric (target `vaultline.metrics`).
Emitted today:

| Metric | Meaning |
|---|---|
| `backups_run` | 1 on a successful backup run |
| `backups_failed` | 1 on a failed backup run |
| `backup_duration_ms` | wall time of a backup run (quiesce + staging + restic + check) |
| `verification_level_reached` | highest level actually reached for the snapshot |
| `backup_files_new` | files newly added to the repository (from restic's summary) |
| `backup_bytes_processed` | bytes processed (from restic's summary) |
| `restore_rehearsals_run` | 1 per completed restore or verification rehearsal |
| `restore_rehearsal_failures` | 1 when a rehearsal's verification fails |
| `restore_duration_ms` | wall time of a restore |
| `prune_runs` | 1 per prune invocation (dry-run included) |
| `prune_snapshots_kept` | snapshots the retention plan kept |
| `prune_snapshots_forgotten` | snapshots the retention plan would forget (the plan count; the applied count is in the state record) |
| `schedule_runs` | 1 per `schedule run` invocation that found work |
| `schedule_backups_due_run` | 1 per backup launched by the schedule executor |
| `schedule_verifications_due_run` | 1 per verification launched by the schedule executor |

The per-snapshot retention reasons themselves are command output and the
state record, not metrics — explanations are data, not counters.
