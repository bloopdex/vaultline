# Changelog

## 0.3.0 — 2026-09-07

Database capture and storage backends:

- **Per-engine database dumps** (ADR-V0-3 consistency mechanisms):
  PostgreSQL via `pg_dump -Fc` with the connection string sanitized on
  argv and authentication through a temporary PGPASSFILE; MySQL/MariaDB
  via `mysqldump --single-transaction --routines --triggers --events`
  with the password in `MYSQL_PWD`; SQLite via the CLI's `.backup`
  (Online Backup API — never `cp` of a WAL-mode database). Dumps stage
  into the same restic run; snapshot `database_metadata` records engine,
  mechanism, and format; a failed dump aborts the backup.
- **Volume direct capture**: host paths captured as-is; Docker volume
  names resolved via `docker volume inspect`; sidecar/pause-first stay
  declared-but-not-captured (recorded honestly).
- **Integration proof**: SQLite end-to-end including the restore-side
  check (integrity_check + row counts on the restored dump); PostgreSQL
  end-to-end against a testcontainers server (the dump proven a valid
  `-Fc` archive); S3-compatible end-to-end into MinIO; the Phase 1
  container harness is now proven (its ignored test runs green).
- Tool requirements documented (cli.md): restic/pg_dump/mysqldump/
  sqlite3 on PATH with `VAULTLINE_*` overrides; CI installs them on the
  ubuntu job.
- Docs updated to the implemented state; limitations narrowed to what
  remains unproven or deferred.

## 0.2.0 — 2026-09-07

Backup execution — the engine boundary is real:

- **`vaultline backup run`**: executes the recovery definition against
  restic — quiesce rules (shell-free argv), git mirror staging, config
  references recorded but never copied (proven by test), repository
  initialization on first use, the backup, and the inline L2 integrity
  check when the verification policy demands it. Sources declared but not
  captured (databases, volumes) are warned and recorded in the snapshot's
  configuration metadata — never silently claimed.
- **`vaultline backup list`**: the recorded snapshots per application.
- **The restic engine layer** (ADR-002 + amendment): argv-only
  invocation, password via environment, tolerant `--json` parsing, and
  failure classification by exit code + stderr message — the amendment
  records the empirically verified restic 0.19.1 exit-code behavior
  (missing repo → 10, wrong password → 12; the old table differs) and the
  classifier is tested against both tables.
- **Storage URL mapping** (ADR-004): local / s3 / sftp repository URLs
  built from the definition with credential environment forwarding; sftp
  gains the remote `path` field (key_file/known_hosts remain deferred
  rather than reintroducing shell parsing).
- **State persistence** (ADR-003): `state.json` (schema v1, atomic
  writes) + a create-new lockfile refusing concurrent runs; default state
  dir per platform, `VAULTLINE_STATE_DIR` override.
- **Named metrics emitted**: backups_run, backup_duration_ms,
  verification_level_reached, backup_files_new, backup_bytes_processed.
- **69 tests passing** (engine classifier + URL builders, state/lock
  units, CLI integration, and the real-restic end-to-end suite), CI
  installs restic on the ubuntu job.
- Documentation: ADR-002 amendment, cli/architecture/config/testing/
  observability/security/limitations updated to the implemented state.

## 0.1.0 — 2026-09-07

The foundation release (no binaries published; the repository is the
artifact):

- **The canonical Application Recovery Model** (ADR-003): `Application`
  and `BackupSnapshot` types — engine-agnostic, serialization pinned —
  with the verification levels L1–L6, explainable retention, and the
  ordered restore procedure.
- **Typed TOML configuration** (ADR-004): strict parsing (unknown fields
  rejected with span), aggregated validation (every failure reported with
  its field path), the shared application-name rule.
- **CLI foundation**: `init` (safe-by-default — never overwrites), and
  `validate` (human + `--json`), `--version`, with the documented exit-code
  contract (0/1/2).
- **Error model and structured logging**: JSON lines on stderr,
  `RUST_LOG`, `--log-format`.
- **Supply chain from day one**: cargo-vet (public audit-store imports +
  18 certified direct production dependencies) and cargo-deny (advisories,
  licenses, bans, sources — all green), in CI with the workflow
  self-validation script.
- **Test infrastructure**: 36 tests (model/config/error units, CLI
  integration), the ignored testcontainers harness for later phases.
- **Architecture decision records 001–004** migrated into docs/adr/.
- The research record (the tool landscape survey, the gap analysis, and
  the evaluation that commissioned the project) lives in the project
  graph.
