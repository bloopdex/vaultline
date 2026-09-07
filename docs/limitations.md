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

## L6 rehearsal automation and app checks are not executed

- **What is missing**: `verification.app_checks` is recorded but not
  executed, and the full L6 recovery test has no scheduled rehearsal —
  it is exercised manually through `vaultline restore --verify`, and
  the disaster scenario is pinned by the integration suite.
- **Why**: L6 rehearsal requires a scratch application environment
  (hosts, DNS, external dependencies) that a backup tool cannot assume;
  app checks are per-application semantics. Scheduling itself arrived
  in Phase 5 (`schedule run`, systemd timers) and executes L1–L5.
- **What happens instead**: every snapshot records the level actually
  reached; `backup verify` and `restore --verify` raise it durably;
  the verification schedule runs them on time.
- **What would remove it**: a rehearsal-environment contract (Phase 6)
  and the per-engine app-check executors.

## Cross-platform restore is not supported

- **What is missing**: a snapshot taken on one OS restores on the same
  OS only (path translation maps Windows drive paths to their stored
  `/C/...` form; Unix paths map 1:1).
- **Why**: the VPS story is Linux-to-Linux; cross-platform path mapping
  is a distinct problem with no current user.
- **What happens instead**: restored paths that do not exist in the
  snapshot fail with an explicit error naming the path.
- **What would remove it**: a path-mapping table per backup host.
