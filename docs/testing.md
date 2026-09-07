# Testing strategy

Tests are part of implementation: a feature without tests is not done.

## Layers

| Layer | Where | What it pins |
|---|---|---|
| unit | `vaultline-core` (`#[cfg(test)]` modules) | the model's serialization contracts (application + snapshot JSON round-trips, RFC 3339 timestamps, level ordering); every validation rule (name rules, duplicates, dangling restore references, per-kind storage fields, retention sanity); the exit-code contract; state-file round-trips, version refusal, corrupt-state refusal, lock exclusivity |
| unit | `vaultline-cli` (engine + backup modules) | the failure classifier against **both** restic exit-code tables (the ADR-002 amendment), tolerant `--json` summary extraction (garbage tolerated, missing summary = failure), repository URL builders for all three storage kinds, sftp key-file deferral, password-env diagnostics |
| template | `vaultline-cli` (template tests) | the `init` template validates for any legal name, warning-free |
| integration | `vaultline-cli/tests/cli.rs` | the real binary against real files: `init` writes and refuses to overwrite, `validate` exit codes and output, `--json` payloads, `--version` |
| engine integration | `vaultline-cli/tests/backup.rs` | the real binary + **the real restic binary**: end-to-end backup (init-if-needed, files + excludes, quiesce, git mirror, config-ref exclusion proven by repository listing), state recording, `backup list`, `--json` payload, wrong-password mapping, missing-restic diagnostics, lock refusal |
| container | `vaultline-cli/tests/containers.rs` | the harness later phases build database/SFTP/MinIO integration on |

## The engine integration gate

`tests/backup.rs` runs only when restic is available: `VAULTLINE_RESTIC_BIN`
overrides the binary path, else `restic` on PATH; otherwise the tests skip
with a note. CI installs restic on the ubuntu job; the Windows job skips
the engine suite. Locally:

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

- No orchestration tests (no orchestration exists).
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
