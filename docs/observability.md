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
Emitted today, by `backup run`:

| Metric | Meaning |
|---|---|
| `backups_run` | 1 on a successful backup run |
| `backups_failed` | 1 on a failed backup run |
| `backup_duration_ms` | wall time of a backup run (quiesce + staging + restic + check) |
| `verification_level_reached` | highest level actually reached for the snapshot |
| `backup_files_new` | files newly added to the repository (from restic's summary) |
| `backup_bytes_processed` | bytes processed (from restic's summary) |

Designed for the phases that produce them (names stable, not yet
emitted):

| Metric | Meaning |
|---|---|
| `restore_rehearsals_run` | restore-rehearsal runs completed |
| `restore_rehearsal_failures` | rehearsals that did not prove restorability |
| `restore_duration_ms` | wall time of a restore/rehearsal |
| `prune_kept` / `prune_deleted` | snapshots retained / deleted by a prune, with the per-snapshot reason recorded in the log |
