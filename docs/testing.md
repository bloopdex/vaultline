# Testing strategy

Tests are part of implementation: a feature without tests is not done.

## Layers

| Layer | Where | What it pins |
|---|---|---|
| unit | `vaultline-core` (`#[cfg(test)]` modules) | the model's serialization contracts (application + snapshot JSON round-trips, RFC 3339 timestamps, level ordering); every validation rule (name rules, duplicates, dangling restore references, per-kind storage fields, retention sanity); the exit-code contract; state-file round-trips, version refusal, corrupt-state refusal, lock exclusivity |
| unit | `vaultline-cli` (engine + backup + database modules) | the failure classifier against **both** restic exit-code tables (the ADR-002 amendment), tolerant `--json` summary extraction (garbage tolerated, missing summary = failure), repository URL builders for all three storage kinds, sftp key-file deferral, password-env diagnostics, connection-string parsing (URI + key=value) with sanitization that never leaks passwords into argv, the pgpass file format |
| template | `vaultline-cli` (template tests) | the `init` template validates for any legal name, warning-free |
| integration | `vaultline-cli/tests/cli.rs` | the real binary against real files: `init` writes and refuses to overwrite, `validate` exit codes and output, `--json` payloads, `--version` |
| engine integration | `vaultline-cli/tests/backup.rs` | the real binary + **the real restic binary**: end-to-end backup (init-if-needed, files + excludes, quiesce, git mirror, config-ref exclusion proven by repository listing), state recording, `backup list`, `--json` payload, wrong-password mapping, missing-restic diagnostics, lock refusal |
| database integration | `vaultline-cli/tests/databases.rs` | **SQLite end-to-end without containers** (dump captured via the backup API, recorded metadata, restore-side `integrity_check` + row counts on the restored copy — the L5-lite proof); **PostgreSQL end-to-end** (testcontainers PostgreSQL + real pg_dump — on Windows through a container shim, on Unix from PATH; the dump is proven to be a valid `-Fc` archive by its PGDMP signature); **MinIO S3 end-to-end** (testcontainers MinIO + restic's s3 backend listing the bucket); volume direct capture (host path); the sidecar honesty contract |
| restore & verification integration | `vaultline-cli/tests/restore.rs` | the sandbox-then-promote executor (files promoted, nested content, `--dry-run` writes nothing), the no-overwrite contract, **the disaster end-to-end (backup → destroy → restore → data + database intact)**, the health-endpoint poll, verification L3 (engine cross-check), L4 (recursive content comparison, tamper → failure), L5 (SQLite rehearsal, durable state record), the **PostgreSQL pg_restore round trip** (dump → restore into a second database → row count), snapshot selectors |
| operations integration | `vaultline-cli/tests/operations.rs` | **prune** (dry-run changes nothing and explains every decision; `--apply` forgets, prunes, records, and re-runs idempotently; `--json` carries the plan), **schedule run** (due backup + verification execute once, bookkeeping recorded, second run finds nothing due), **doctor** (healthy all-ok; a broken restic override fails the check with exit 1; structured `--json`), **status** (summary + retention projection), **timer generate** (unit content incl. the translated OnCalendar, refusal without schedules, install refusal off Linux), **validate** (invalid crons and both-restricted day fields rejected) |
| container | `vaultline-cli/tests/containers.rs` | the harness itself: a PostgreSQL container comes up and accepts connections |

## The integration gates

`tests/backup.rs` and `tests/databases.rs` skip with a note when a
requirement is absent: restic (`VAULTLINE_RESTIC_BIN` or PATH), sqlite3
(`VAULTLINE_SQLITE3` or PATH), pg_dump (`VAULTLINE_PGDUMP` or PATH), or a
running Docker engine. CI installs restic, sqlite3, and postgresql-client
on the ubuntu job (Docker is present on the runner, so the container-gated
tests run hosted); the Windows job skips whatever is missing. Locally:

```sh
VAULTLINE_RESTIC_BIN=A:\BloopLab\tools\restic.exe cargo test --workspace
```

## The container harness

`tests/containers.rs` uses testcontainers (PostgreSQL today; SFTP and
MinIO arrive with their consumers). It is `#[ignore]`d by default because
it needs a running Docker engine:

```sh
cargo test --test containers -- --ignored
```

Recorded 2026-09-07: with the Docker engine stopped, the harness fails
fast with a clear connection error — that is the designed behavior, and
the test is skipped in CI by design.

## What is deliberately not tested yet

- The systemd `install` path itself runs nowhere in CI — unit-content
  generation is fully tested; the install command's systemd interaction
  is thin (write + daemon-reload + enable) and Linux-privilege-gated.
- No fuzz targets — the configuration parser is the first fuzz candidate
  when hardening begins.
- No benchmarks — measurement starts when backup operations exist
  (SOT Section 12: baseline before target).

## Running everything

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
cargo vet check --store-path supply-chain
cargo deny check
python scripts/check-workflows.py
```

(The maintainer checklist in CONTRIBUTING.md is the same list, in the
order to run before every push.)
