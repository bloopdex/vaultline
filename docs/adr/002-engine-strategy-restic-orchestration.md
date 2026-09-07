# ADR-002 — Backup engine strategy: orchestrate restic

*Status: accepted · recorded 2026-09-07 (ADR-V0-2 in the research record)*

## Context

The backup-engine core — content-addressed deduplication, compression,
encryption, repository integrity — is a solved, hardened problem with
multiple mature implementations. Rebuilding it would add a decade of
correctness risk for zero differentiation. The alternatives:

- **A — implement our own engine**: rejected. Reinvents the solved core;
  violates the project's own anti-clone discipline.
- **B — orchestrate restic via its `--json` CLI contract**: the precedent
  is autorestic, a YAML wrapper over restic. restic documents machine-
  readable `--json` output and a stable exit-code contract (0 success; 1
  generic; 3 repository not found; 10 lock failure; 11 wrong password; 12
  unknown command; 130 interrupted).
- **C — embed Kopia as a library**: rejected. Kopia's blob interfaces are
  self-described as unstable and not externally implementable; its
  repository format strands the restic ecosystem; it forces the language
  question (ADR-001).
- **D — embed rustic's crates**: rejected for now. The only intended library
  API in the restic ecosystem, but self-described as beta and missing
  regression tests.

## Decision

**Orchestrate restic as a subprocess through its `--json` CLI contract.**
Never reimplement deduplication, compression, or encryption.

Engineering discipline for the orchestration layer:

- subprocess arguments are argv arrays — never shell interpolation
- the exit-code contract above is mapped to typed outcomes; any unknown
  exit code is treated as failure
- `--json` output is parsed tolerantly (unknown fields ignored, schema
  mismatches surfaced as operational failures, never crashes)
- restic's format has two independent implementations (restic, rustic) —
  a hedge against single-implementation lock-in

## Consequences

- The core domain types stay engine-agnostic (ADR-003); restic specifics
  (repository passwords, snapshot ids, backend flags) are adapter-layer
  concerns.
- Backend coverage comes from restic's own backends (local, SFTP, S3,
  MinIO, B2, Azure, GCS, and the rclone bridge) — Vaultline's storage
  abstraction (ADR-004) declares them, restic carries them.
- A restic binary on the target host is a deployment requirement.

## Revisit conditions

- If rustic's crates mature into a stable, regression-tested library, an
  in-process option exists and should be re-evaluated.
- If a verification capability restic lacks (e.g. sampling-verified restore
  rehearsal) becomes a hard requirement, re-open the engine question with
  that requirement on the table.

## Amendment 2026-09-07 — failure classification is exit code + message

The original decision recorded restic's documented exit-code table
(0/1/3/10/11/12/130). First-run integration testing against restic 0.19.1
found the table inaccurate: a missing repository exits **10** (not 3), a
wrong password exits **12** (not 11), and lock contention blocks until the
lock frees rather than erroring. Older restic releases follow the published
table (3 missing / 10 lock / 11 wrong password / 12 unknown command).

The stderr messages ("repository does not exist", "wrong password or no key
found", "unknown command") are stable across both tables. The orchestration
layer therefore classifies failures by **exit code + stderr message**, with
unrecognized combinations treated as generic failures — never guessed. The
classifier is unit-tested against both tables (engine.rs).
