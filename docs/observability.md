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

The metric set is designed now and emits once backup operations exist
(emitting numbers for operations that do not run would be fiction). The
names are part of the machine contract from day one:

| Metric | Meaning |
|---|---|
| `backups_run` | backup commands completed successfully |
| `backups_failed` | backup commands that failed |
| `backup_duration_ms` | wall time of a backup run |
| `verification_level_reached` | highest level actually reached for a snapshot |
| `restore_rehearsals_run` | restore-rehearsal runs completed |
| `restore_rehearsal_failures` | rehearsals that did not prove restorability |
| `restore_duration_ms` | wall time of a restore/rehearsal |
| `prune_kept` / `prune_deleted` | snapshots retained / deleted by a prune, with the per-snapshot reason recorded in the log |

Delivery mechanism (structured stderr events vs a metrics endpoint) is
decided with the backup phase; the names are stable.
