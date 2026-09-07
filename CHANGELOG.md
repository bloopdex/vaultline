# Changelog

## 0.6.0 — 2026-09-07

Hardening & security — the verification ladder is complete and the
boundaries are pinned by tests:

- **The L6 recovery rehearsal executes** (ADR-006): `backup verify` at
  policy level L6 restores the snapshot into
  `<rehearsal.target>/.vaultline-rehearsal/<snapshot-id>/` with the
  procedure's path targets mirrored under the rehearsal root (a scratch
  clone of the layout — never the live paths), runs the app checks, and
  records L6 durably. `schedule run` inherits it: a due verification at
  L6 IS the rehearsal. L3–L5 are recorded BEFORE the rehearsal attempt —
  a failing rehearsal does not erase the level actually proven.
- **App checks are executable** (`name`, `command`, shell-free `args`):
  CWD is the rehearsal root, `VAULTLINE_REHEARSAL_DIR` names it; a
  failing check fails the rehearsal with the stderr tail as the
  diagnosis. `restore --verify` runs them against its target. The wire
  format changes from name strings to check tables (nothing external
  depends on it).
- **`application.rehearsal.target`** joins the definition and is
  REQUIRED when the verification level is L6.
- **The hardening layer, tested not asserted**: a 10 MiB configuration
  size limit; a deterministic mutation harness proving the parser and
  validator never panic on hostile input (2000 seeded rounds); the
  malicious-archive defense — symlinks are recreated as links, never
  followed (a host that cannot create links fails loudly), pinned by
  unit + integration tests; the corrupt-repository fixture (a truncated
  pack fails the inline L2 check and the snapshot records L1 — the
  level actually reached); the redaction failure test (a failed dump
  with an embedded password prints the password nowhere).
- Follow-up proofs closed: the **MySQL restore round trip** (dump →
  mysql client into a second database → rows verified, source untouched)
  which pinned the `--databases` capture bug (the flag embedded CREATE
  DATABASE/USE, overriding declared restore targets); the **SFTP
  backend proof** and the **named docker-volume capture proof** written
  (agent- and Unix-gated; they execute on the hosted ubuntu job). CI
  installs the mysql client tools and the OpenSSH client.
- **152 tests passing**, 0 failures, all gates green. Docs updated;
  limitations narrowed to L6-rehearsal cleanup, app-check environment
  extensions, cross-platform restore, and the remaining backend proofs.

## 0.5.0 — 2026-09-07

Operations and scheduling — the backup runs itself, and retention
explains itself:

- **`vaultline backup prune`** enforces the retention policy with
  **per-snapshot explanations**: every decision prints with its reasons
  ("among the 14 most recent snapshots (keep_last)", "newest snapshot of
  2026-09-07 (keep_daily)", …). Dry-run by default — without `--apply`
  nothing changes. `--apply` forgets only recorded ids the engine still
  holds (cross-checked first, never blind), runs `restic prune`, and
  records the outcome in the state file (snapshots stay immutable; the
  operations record notes what was forgotten and when). Idempotent by
  construction and by test.
- **`vaultline schedule run`** — the due-ness executor: runs the backup
  and/or verification the definition's crons make due and explains what
  ran and why. Bookkeeping is recorded (a failing verification does not
  hot-loop; the attempt time is recorded before the run).
- **`vaultline doctor`** — environment checks (config, restic, dump
  tools, state file, storage connectivity) as ok/warning/error with
  details; exit 0 only when nothing errored.
- **`vaultline status`** — the application summary: snapshots, levels
  reached vs policy, next schedule occurrences, retention projection,
  prune history.
- **`vaultline timer generate|install`** — the systemd story: one
  service+timer pair per declared schedule with the cron translated to
  `OnCalendar` (Persistent=true). `install` is Linux-only, daemon-reloads,
  enables, and never starts (starting is an explicit `--now`).
- **Schedules are part of the definition**: `application.schedule`
  (backups) joins `verification.schedule`; both are 5-field cron,
  validated at configuration time.
- **The cron dependency decision by evidence** (ADR-005): the audited
  `cron` crate, with three recorded behavioral findings — it requires
  6/7 fields (5-field input is rejected outright; the wrapper prepends
  the fixed second), its day-of-week range is 1..=7 with Sunday=1 (0 is
  rejected), and both-restricted day fields AND rather than OR (standard
  cron ORs; systemd ORs) — so both-restricted day fields are rejected at
  validation rather than scheduled ambiguously. Crate + phf/rand/
  siphasher dependencies audited (94 crates certified).
- Metrics: prune_runs, prune_snapshots_kept, prune_snapshots_forgotten,
  schedule_runs, schedule_backups_due_run, schedule_verifications_due_run.
- **138 tests passing** (retention-plan fixtures, cron evaluation and
  OnCalendar translation, due-ness, unit-content generation, and 10
  real-restic operations integration tests including prune dry-run /
  apply / idempotence and the schedule executor).
- Docs updated; limitations narrowed (the scheduling and prune
  limitations are delivered; L6 rehearsal automation and app_checks
  remain).

## 0.4.0 — 2026-09-07

Restore and verification — the restore-first promise is executable:

- **`vaultline restore`** executes the definition's restore procedure,
  sandbox-then-promote: the snapshot is restored by restic into a
  staging area under the target, then each step promotes content into
  its declared destination — files copied with directories created and
  **existing files never overwritten** (a collision is an error naming
  the file), databases restored through their engine's tool (pg_restore
  / mysql, both fed by stdin — the dump never appears on argv), volumes
  copied, health endpoints polled. `--dry-run` prints the plan and
  writes nothing; `--verify` adds the restore-side SQLite checks.
- **`vaultline backup verify`** proves a snapshot against the policy:
  L3 (the engine still holds the snapshot — short-id aware), L4
  (recursive content comparison of restored files against the live
  files; any mismatch fails), L5 (SQLite restore rehearsal with
  integrity_check) — and records the level actually reached durably in
  the state file.
- **`vaultline backup inspect`** shows a snapshot's full record.
- Snapshot selectors: full id, unique prefix, or `latest`.
- Metrics: restore_duration_ms, restore_rehearsals_run,
  restore_rehearsal_failures.
- Integration proof: the disaster end-to-end (backup → destroy →
  restore → data and database intact), the PostgreSQL pg_restore round
  trip (dump restored into a second database, row count verified), L4
  tamper detection, L5 SQLite rehearsal, the no-overwrite contract.
- Docs updated; limitations narrowed (scheduling, prune, and
  cross-platform restore remain).

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
