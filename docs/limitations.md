# Limitations

Each limitation in the four-part form: what is missing / why / what
happens instead / what would remove it.

## MySQL/MariaDB capture is implemented but not yet integration-proven

- **What is missing**: `mysqldump --single-transaction --routines
  --triggers --events` capture is implemented (MYSQL_PWD environment
  auth, argv never carries credentials) but has never run against a live
  MySQL or MariaDB server — no integration test proves it.
- **Why**: the verification vehicle (a MySQL testcontainer + a
  mysqldump binary on the test host) has not been stood up; PostgreSQL
  and SQLite were the phase's proven engines.
- **What happens instead**: a missing/invalid connection fails with a
  clear diagnostic; nothing pretends the mechanism is proven.
- **What would remove it**: a MySQL/MariaDB testcontainer suite.

## Volume sidecar / pause-first semantics are declared, not captured

- **What is missing**: volumes with `capture = "sidecar"` or
  `"pause-first"` are validated but not captured.
- **Why**: the semantics decision (read-only sidecar mount vs pause
  first) is an experiment against real Docker volumes; the direct path
  (host path or `docker volume inspect` mountpoint) is implemented and
  proven for host paths.
- **What happens instead**: the run warns and records such volumes as
  declared-but-not-captured in the snapshot metadata — never silently
  claimed.
- **What would remove it**: the sidecar and pause-first executors.

## Docker-volume direct capture is implemented but not yet proven

- **What is missing**: `capture = "direct"` with a docker volume name
  resolves the mountpoint via `docker volume inspect`, but no test has
  captured a real named Docker volume (the inspect path works; the
  cross-platform test needs a Docker-engine-specific fixture).
- **Why**: host-path volumes are fully proven; named-volume proof needs
  the volume to exist on the test host's engine.
- **What happens instead**: resolution failures are explicit operational
  errors naming the volume.
- **What would remove it**: a named-volume integration test.

## SFTP key_file / known_hosts are not wired

- **What is missing**: an sftp storage target works with ssh-agent and
  `~/.ssh/config`, but the `key_file` and `known_hosts` fields are
  rejected with an explicit error.
- **Why**: restic's mechanism for custom SSH commands (`-o
  sftp.command`) executes a string through a shell — that conflicts with
  the argv-only discipline (ADR-002). Wiring it safely needs a
  documented, injection-free approach.
- **What happens instead**: the validator accepts the fields; `backup
  run` refuses with a clear "not wired yet" error rather than building a
  shell string.
- **What would remove it**: an injection-free custom-SSH mechanism or
  restic gaining first-class key-file flags.

## S3/SFTP targets are declared and URL-mapped, not yet integration-tested

- **What is missing**: repository URL construction for s3 and sftp is
  implemented and unit-tested; no integration test has run against a
  real S3-compatible store or SFTP server.
- **Why**: those tests are container-gated (MinIO, SFTP server) and the
  container harness is being proven up.
- **What happens instead**: local storage is the fully integration-tested
  path; s3/sftp failures surface through restic's own error channel.
- **What would remove it**: MinIO and SFTP testcontainers in the
  container harness.

## Verification beyond L2 is not executed

- **What is missing**: the verification policy's schedule and app checks
  are recorded but not executed, and levels L3–L6 (metadata verification,
  file restore rehearsal, database restore rehearsal, the full recovery
  test) have no executor.
- **Why**: L4–L6 require restore machinery; scheduling requires the
  operations phase. L1–L2 run inline on every backup whose policy
  demands them.
- **What happens instead**: every backup records the level actually
  reached (L1 or L2) in the snapshot's integrity record — the
  created-vs-proven-restorable distinction is preserved honestly.
- **What would remove it**: the restore & verification phase (L3–L6) and
  the scheduling integration.

## Retention, prune, and restore are declared, not executed

- **What is missing**: the retention policy is validated but no `prune`
  command exists; the restore procedure is validated (references must
  resolve) but no `restore` command executes it.
- **Why**: retention explainability and restore-first design are
  downstream of the backup path, which this phase established; prune and
  restore are the operations and restore phases.
- **What happens instead**: nothing deletes snapshots (the safe default),
  and every snapshot carries its restore metadata so future restores
  inherit it.
- **What would remove it**: the `prune` command with per-snapshot
  retention explanations, and the `restore` command executing the
  procedure (restore-to-sandbox first).
