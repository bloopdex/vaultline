# Limitations

Each limitation in the four-part form: what is missing / why / what
happens instead / what would remove it.

## Backup execution does not exist

- **What is missing**: `vaultline` cannot create backups. The declared
  command surface (backup, restore, verify, prune, doctor, status) exists
  only as the model behind it.
- **Why**: the foundation deliberately precedes execution — the model,
  configuration, and diagnostics are the load-bearing decisions every
  later capability consumes (the project's build order).
- **What happens instead**: `init` and `validate` are fully implemented;
  nothing pretends a backup ran.
- **What would remove it**: the backup phase — restic orchestration
  (ADR-002) behind the storage abstraction (ADR-004).

## Storage backends are declared, not connected

- **What is missing**: no repository is ever opened; `kind = "local"`,
  `"s3"`, and `"sftp"` are validated configurations only.
- **Why**: restic's own backends carry the connectivity (ADR-004); wiring
  them is part of backup execution.
- **What happens instead**: the validator enforces each backend's required
  fields, so a definition written today is executable-shaped tomorrow.
- **What would remove it**: the backup phase's repository-open step.

## Verification, scheduling, and app checks are recorded, not executed

- **What is missing**: `verification.schedule` and `verification.app_checks`
  are accepted, validated, and reported as warnings — nothing runs on the
  schedule, and no app-semantic check executes.
- **Why**: a schedule that silently does nothing would be worse than a
  warning; execution needs the backup and restore phases first.
- **What happens instead**: `validate` warns explicitly that these fields
  are recorded but not yet executed.
- **What would remove it**: the verification phase (levels L2–L6) and the
  scheduling integration (systemd timer).

## No local state

- **What is missing**: nothing is remembered between runs — no snapshot
  records, no verification history, no lockfile.
- **Why**: the state model was decided in ADR-003 (plain files first,
  embedded SQLite only on evidence) and state has no producer yet.
- **What happens instead**: each invocation is stateless; `BackupSnapshot`
  is a serializable type with no persistence.
- **What would remove it**: the backup phase, which writes the state file
  the type was pinned for.

## The restore procedure is declared, not executable

- **What is missing**: `application.restore.steps` is validated
  (references must resolve) but nothing executes the steps.
- **Why**: restore-first design means the disaster-recovery end-to-end
  test (L6) is the defining test — it requires backups to exist first.
- **What happens instead**: definitions carry the procedure so that every
  future backup inherits it.
- **What would remove it**: the restore phase, built against the
  disaster scenario the project is defined by.
