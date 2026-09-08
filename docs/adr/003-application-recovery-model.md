# ADR-003 — The Application Recovery Model

*Status: accepted · recorded 2026-09-07 (ADR-V0-3 in the research record)*

## Context

The gap that motivates Vaultline (validated against 13 backup tools and 6
adjacent systems): no open tool bundles what-to-capture, database
consistency, volumes, retention, tiered verification, and an executable
restore procedure into one engine-agnostic per-application definition.
borgmatic and autorestic are engine-locked and restore-less; Velero is
Kubernetes-only with no data verification; the commercial panel products
hard-code per-app recipes.

## Decision

**One canonical, engine-agnostic Application Recovery Definition** per
application, implemented as the `Application` type in `vaultline-core`:

- **identity** — a validated machine name
- **sources** — files (paths + excludes + optional quiesce), git remotes
  (mirror vs reference), and config *references* (recorded in manifests,
  never copied — the secret stays where it is)
- **databases** — engine type + connection-by-environment-variable +
  consistency strategy (the MVP mechanism is a logical dump; physical/PITR
  strategies are recorded for later)
- **volumes** — Docker volumes with capture semantics (direct / sidecar /
  pause-first)
- **storage** — the repository target (ADR-004)
- **retention** — deterministic, explainable keep-counts
- **verification** — a level plus schedule plus named application-semantic
  checks
- **restore procedure** — ordered, target-aware steps (restore files /
  restore database / restore volume / wait-healthy)

Database consistency is per-engine and never `cp` of live files: PostgreSQL
via `pg_dump -Fc` (MVP), MySQL/MariaDB via `mysqldump --single-transaction`,
SQLite via its Online Backup API — with restore-side semantic checks
(`PRAGMA integrity_check`, row counts) as part of the verification policy.

### Verification levels

The product's central distinction between *backup created* and *backup
proven restorable*:

| Level | Meaning |
|---|---|
| L1 | the backup command succeeded |
| L2 | repository integrity verified (restic `check`) |
| L3 | snapshot metadata verified |
| L4 | selected files restorable (restore-to-scratch + compare) |
| L5 | databases restorable (scratch instance + integrity + row counts) |
| L6 | full application recovery test (the disaster-recovery end-to-end) |

A definition's verification policy names a level and a schedule;
`status` reports the latest snapshot's verification level (and
`backup inspect` any snapshot's full record — `doctor` covers the
environment, not per-snapshot levels).

### Retention explainability

`prune` must state, per snapshot, the policy rule that retained or deleted
it ("kept: daily.2 — weekly policy, newest snapshot not replacing it").
Never opaque.

### Validation

The model was validated at design level against three application shapes —
a docker-compose application with a PostgreSQL database and volumes, a
bare-files application, and a database-only application — and expresses all
three without per-shape special cases.

## Consequences

- `BackupSnapshot` is the recorded outcome of a run: snapshot id,
  application, timestamp, source manifest, database metadata (engine,
  mechanism, dump format), redacted configuration metadata, engine
  snapshot reference, integrity information, and what it can reconstruct.
  Its serialization is pinned from day one so manifests stay stable.
- The configuration format (ADR-004) is a projection of this model; the
  model is the single canonical representation shared by configuration,
  manifests, and later the state file.
- Local state: a plain state file + lockfile until evidence demands SQLite
  (the default is no embedded database).
