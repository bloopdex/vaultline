# Architecture

## Shape

A Cargo workspace with two crates:

```
crates/vaultline-core   the canonical model (ADR-003) + typed configuration
                        (ADR-004) + the error model
crates/vaultline-cli    the binary: command surface, logging, dispatch
```

Dependency direction is inward: `vaultline-core` contains no engine
behavior and no process I/O beyond reading the configuration file;
`vaultline-cli` depends on `vaultline-core`, never the reverse. restic
orchestration (ADR-002) lives behind the engine adapter boundary in the
CLI layer, never inside the model.

## The canonical model

One internal representation shared by every feature (configuration,
snapshot manifests, and later the state file): `Application` and
`BackupSnapshot` in `vaultline-core::model`. The TOML wire format
(`vaultline-core::config`) is a projection of the model: parse strict
(`deny_unknown_fields`), validate everything (cross-references, name
rules, per-backend required fields, retention sanity), convert. Validation
aggregates **all** failures — diagnostics explain, never first-error-only.

## Configuration as data

`vaultline.toml` is a validated, typed document with clear provenance:
unknown fields are rejected at parse time with the parser's line/column
span; semantic rules are rejected at validation time with the offending
field path. The same rules serve `validate` and `init` (the template is
re-validated at write time — an invalid template is a bug, not a user
problem).

## Error handling

One error type (`VaultlineError`) crossing the CLI boundary, classified
into families that map to the documented exit codes (docs/cli.md). The
error message is the diagnosis: file paths, field paths, reasons, and
suggested fixes where a fix exists.

## Logging & observability

Structured logging from day one (SOT Section 14): JSON lines on stderr by
default, `--log-format pretty` for humans, level via `RUST_LOG`. stdout is
reserved for machine-readable command output. Named metrics are designed
in docs/observability.md and emit once backup operations exist.

## Concurrency

None. Sequential processing is the design default; parallelism is added
only when measurement demonstrates a need.

## The engine boundary (implemented)

`vaultline-cli::engine` is the restic adapter (ADR-002): a subprocess
with argv-only arguments (never shell interpolation), the repository
password as an environment variable (never argv), `--json` output parsed
tolerantly, and failures classified by exit code + stderr message — the
amendment in ADR-002 records why (the exit-code tables differ between
restic versions; the messages are stable). The storage adapter builds
restic repository URLs for local / s3 / sftp targets (ADR-004) and
forwards credential environment variables; connectivity is carried by
restic's own backends.

`vaultline-cli::backup` is the orchestration: quiesce rules, git mirror
staging, config-reference recording (never copied), per-engine database
dumps into staging (below), volume path resolution, repository
initialization on first use, the backup, the inline L2 integrity check,
and the `BackupSnapshot` record persisted through
`vaultline-core::state` (atomic writes + a create-new lockfile).

`vaultline-cli::database` is the consistency layer (ADR-V0-3): each
database is captured through its engine's own tooling into a staging
dump that joins the same restic run — `pg_dump -Fc` (connection string
sanitized on argv, authentication via a temporary PGPASSFILE),
`mysqldump --single-transaction --routines --triggers --events`
(password via `MYSQL_PWD`), and `sqlite3 .backup` (the Online Backup
API — never `cp` of a WAL-mode database). A failed dump aborts the
backup: a snapshot never claims database coverage a dump did not
produce.

`vaultline-cli::restore` is the recovery executor: restore-first by
construction. The whole snapshot is restored by restic into a staging
area under the target, then the procedure's steps promote content into
their declared destinations (never overwriting), databases are restored
through their engine's tool (pg_restore / mysql, both fed by stdin —
no dump path on argv), volumes are copied, health endpoints polled. The
verification executor (`backup verify`) proves snapshots against the
policy — L3 engine cross-check, L4 recursive content comparison against
live files, L5 SQLite rehearsal, and L6 the full recovery rehearsal
(ADR-006): the snapshot restored into
`<rehearsal.target>/.vaultline-rehearsal/<snapshot-id>/` with the
procedure's path targets mirrored under the rehearsal root, then the
app checks — and records the level actually reached durably in the
state file (L3-L5 are recorded before the rehearsal attempt; a failing
rehearsal never erases the level proven). Promotion decides on
`symlink_metadata`: symlinks are recreated as links, never followed.

## The operations executors (implemented)

`vaultline-core::cron` wraps the audited `cron` crate (ADR-005): the
5-field schedule contract, validation, next-occurrence computation, and
OnCalendar translation. `vaultline-core::retention` computes the
deterministic keep/forget plan over recorded snapshots — every decision
carries its reasons. `vaultline-cli::prune` applies the plan (dry-run by
default; engine cross-check before any `forget`; outcome recorded).
`vaultline-cli::schedule` is the due-ness executor (anchors in the
operations record). `vaultline-cli::ops` is `doctor`/`status` (read-only
diagnosis). `vaultline-cli::timer` generates the systemd units
(service+timer per declared schedule, cron → OnCalendar, Persistent).

The reliability layer (ADR-007): the state lock gains liveness-based
stale-lock recovery (dead holder → reclaim with a warning; alive →
refused; the probe is injected CLI-side so core stays process-I/O-free);
the backup pre-flights every declared capture path (a vanished source
aborts — restic skips missing paths silently, verified empirically);
the rehearsal's staging and promotion both live under the per-snapshot
rehearsal directory; tree removal is robust on Windows (ACL reset +
attribute clear + retry — restic-restored directories carry
mode-derived restrictive ACLs, the same root cause as its
restore-timestamp quirk). The benchmark baseline lives in
examples/bench.rs with docs/benchmarks/baseline.json and
scripts/bench-check.py.

## State

Plain files, per ADR-003: `state.json` (schema version 1) records
snapshots per application plus the operations bookkeeping (verification
schedule runs, applied prunes — additive optional fields, so the
version stays 1); a `lock` file (create-new semantics) refuses
concurrent runs. Embedded SQLite remains off the table until evidence
demands it.
