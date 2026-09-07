# Testing strategy

Tests are part of implementation: a feature without tests is not done.

## Layers

| Layer | Where | What it pins |
|---|---|---|
| unit | `vaultline-core` (`#[cfg(test)]` modules) | the model's serialization contracts (application + snapshot JSON round-trips, RFC 3339 timestamps, level ordering); every validation rule (name rules, duplicates, dangling restore references, per-kind storage fields, retention sanity); the exit-code contract |
| template | `vaultline-cli` (template tests) | the `init` template validates for any legal name, warning-free |
| integration | `vaultline-cli/tests/cli.rs` | the real binary against real files: `init` writes and refuses to overwrite, `validate` exit codes and output, `--json` payloads, `--version` |
| container | `vaultline-cli/tests/containers.rs` | the harness later phases build database/SFTP/MinIO integration on |

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
