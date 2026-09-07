# Limitations

Each limitation in the four-part form: what is missing / why / what
happens instead / what would remove it.

## Databases are declared, not captured

- **What is missing**: `vaultline backup run` does not dump databases. A
  declared `[[application.databases]]` entry is validated but its
  consistency mechanism (pg_dump etc.) is not executed.
- **Why**: a database dump must be proven against a live server — the
  container-gated integration path (PostgreSQL testcontainers) is the
  verification vehicle, and it is being built up deliberately.
- **What happens instead**: the run warns on stderr and the snapshot
  records "databases declared but not captured (names)" in its
  configuration metadata; the database never appears in the snapshot's
  reconstructs list. A snapshot never claims database coverage it does
  not have.
- **What would remove it**: the database-capture step with per-engine
  consistency execution, proven against a real PostgreSQL instance.

## Volumes are declared, not captured

- **What is missing**: declared volumes (direct / sidecar / pause-first
  capture semantics) are validated but not captured.
- **Why**: the capture-semantics decision (read-only mount vs sidecar
  reader vs pause-first) is an experiment against real Docker volumes;
  the direct-semantics path is next.
- **What happens instead**: the run warns and records the volumes as
  declared-but-not-captured, exactly like databases.
- **What would remove it**: the volume-capture step (direct semantics
  first).

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
