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

**Part 2 (2026-09-07, the first hosted run):** a THIRD table surfaced —
restic 0.16.4 (ubuntu-latest's apt package, the CI job's engine; probed
against a container) exits **1** for a wrong password, with the same
stable message the older tables used 11/12 for. The messages are stable
across all three generations while the codes drift, so the classification
became **message-first**: the stable messages decide before any code
table, unlisted codes with unlisted messages stay generic — still never
guessed. The classifier is unit-tested against all three tables.

A second empirical finding from the same integration pass: probing a
missing **s3 repository hangs** — restic retries a missing bucket
indefinitely ("Stat(<config/>) returned error, retrying after …") instead
of returning the missing-repository exit code. Repository setup is
therefore **init-first**: `init` creates the repository (and the bucket)
when absent, and fails with "already exists" when present — that message
is treated as success. There is no probe step.

**Part 3 (2026-09-08, the fix round):** the sftp `key_file`/`known_hosts`
deferral is discharged. The original objection — restic's custom-SSH
mechanism (`-o sftp.command`) re-runs a string through a shell — was
checked against restic's own source instead of assumed: the sftp backend
**always** execs the native ssh client and hands it the tokens from
`-o sftp.args=` (registered since restic 0.16.1 — apt's 0.16.4 included;
verified in v0.16.4 and current sources) directly to `exec.Command` as
argv. `SplitShellStrings` (internal/backend/shell_split.go) tokenizes the
string — whitespace and backslash split outside quotes, single/double
quotes open and close, no escapes inside quotes. One subtlety its
first local Windows execution exposed (via an argv-capturing ssh shim,
2026-09-08): the tokenizer's quotes are SEPARATORS, not concatenators —
`Key='v'` splits into `Key=` and `v`, so an option whose value carries a
path must be quoted whole. The product therefore builds ONE argv token
`-o sftp.args=-o BatchMode=yes -i '<key>' -o 'UserKnownHostsFile=<hosts>'`
(`BatchMode` turns prompts into clean failures — restic's stdin is the
sftp pipe); rejects quote/newline characters in either path at
validation (whole-token quoting is then unambiguous); and stat-checks
the declared files with a named error. No shell executes anywhere in
the chain — the argv-only discipline holds.
