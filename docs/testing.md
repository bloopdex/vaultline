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
| database integration | `vaultline-cli/tests/databases.rs` | **SQLite end-to-end without containers** (dump captured via the backup API, recorded metadata, restore-side `integrity_check` + row counts on the restored copy — the L5-lite proof); **PostgreSQL end-to-end** (testcontainers PostgreSQL + real pg_dump — on Windows through a container shim, on Unix from PATH; the dump is proven to be a valid `-Fc` archive by its PGDMP signature); **MinIO S3 end-to-end** (testcontainers MinIO + restic's s3 backend listing the bucket); volume direct capture (host path); the **sidecar capture round trip** (real docker volume semantics: copy-out through the sidecar container, the recorded capture path, wipe → restore → bytes intact) and the **pause-first lifecycle** (the writer is paused and unpaused again — verified by the engine’s state; the guard unpauses even when the capture fails; a stopped writer proceeds with a note) |
| restore & verification integration | `vaultline-cli/tests/restore.rs` | the sandbox-then-promote executor (files promoted, nested content, `--dry-run` writes nothing), the no-overwrite contract, **the disaster end-to-end (backup → destroy → restore → data + database intact)**, the health-endpoint poll, verification L3 (engine cross-check), L4 (recursive content comparison, tamper → failure), L5 (SQLite rehearsal, durable state record), the **PostgreSQL pg_restore round trip** (dump → restore into a second database → row count), snapshot selectors |
| hardening integration | `vaultline-cli/tests/hardening.rs` | **the L6 rehearsal** (restores into the declared root with mirrored targets, runs the app checks — passing and failing — and records L6/L5 durably; `schedule run` at L6 rehearses), **the malicious-archive defense** (a symlink to an outside secret never leaks its content), **the corrupt-repository honesty fixture** (a truncated pack fails L2 and the next snapshot records L1), **the redaction failure test** (a failed dump with an embedded password prints the password nowhere), the **MySQL restore round trip** (dump → mysql client into a second database → rows verified, source untouched) |
| reliability integration | `vaultline-cli/tests/operations.rs` + `tests/hardening.rs` | the **stale-lock lifecycle** (a live holder's lock refuses the run; once the holder dies the lock is reclaimed with a warning and the run proceeds — the crashed-run case), the **vanished-source defense** (a missing declared path aborts the backup naming it, no snapshot recorded), **rehearsal-directory pruning** (the forgotten snapshot's rehearsal dir is removed, the kept one remains), the **partial-restore guidance** (the no-overwrite error carries the stopped-part-way retry path), the **MariaDB restore round trip** (dump → client into a second database → rows verified), the **cross-platform restore** (a declared Unix target translates through the configuration’s path map, the dry-run plan shows the mapping, and a CLI `--path-map` entry overrides the configuration on a tie), and the platform-independent snapshot-path translation |
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
- The SFTP proof is docker-gated and runs everywhere Docker does —
  it authenticates with a test-owned key file through the product's
  own `key_file`/`known_hosts` wiring (agent-free since 0.9.0: no
  ssh-agent, no mutation of the user's `~/.ssh/known_hosts`), so it
  executes locally on Windows and on the hosted ubuntu job. The named
  docker-volume proof is Unix-gated AND mountpoint-reachability-gated:
  on Desktop VMs the mountpoint lives inside the Docker VM, so the
  proof skips there with the reason named; on the hosted job CI
  grants docker-data traversal and the proof executes. The sidecar
  and pause-first proofs are docker-gated but NOT Unix-gated:
  host-path volumes + real containers run on Desktop and native
  Linux alike.
- No sanitizer fuzzing (cargo-fuzz needs the nightly toolchain) — the
  deterministic mutation harness is the stable-rust stand-in until
  then.
- Benchmarks: the std-only example (examples/bench.rs) measures the
  hot paths as medians; the baseline is committed and
  scripts/bench-check.py gates regressions at 3x. No sanitizer fuzzing
  yet (nightly toolchain); the mutation harness stands in.

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
