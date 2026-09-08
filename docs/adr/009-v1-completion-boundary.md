# ADR-V0-9: The 1.0 completion boundary

- Status: accepted
- Date: 2026-09-08

## Context

After Phase 9 (the fix round, v0.9.0), ten items remained recorded as
open across the project's history. The final-completion review
classified every one of them — the rule: a feature is implemented for
1.0 only when the promised scope requires it; otherwise it is closed
with an explicit trigger. Two more genuine defects were found by the
finalization audits themselves (a docs audit and a failure-mode
coverage matrix) and fixed as part of 1.0.

## The classifications

**CONDITIONAL — NOT CURRENTLY TRIGGERED** (recorded, no ambiguous
"maybe later"):

- *Cross-project signal edges* (DeployScore backup-health; EnvFP
  restored-environment verification). SOT Section 7: an edge activates
  when BOTH sides define the signal contract. The consumers are
  research-only projects with no interface; inventing one here would be
  an invented transport (ADR-008's standing decision). Trigger: either
  side publishes a signal schema; the edge then activates on both
  sides' integration phases.
- *ADR-008's remaining revisit conditions*: the multi-writer container
  list, and pause-window splitting. The dogfooding rounds produced
  neither a multi-writer volume nor a pause-window complaint. Triggers
  unchanged: a real volume with several writers; a real application for
  which the whole-run pause window is a problem.

**OPTIONAL EVOLUTION** (explicitly not 1.0, triggers recorded):

- *PostgreSQL physical/PITR* (ADR-003 "recorded for later"): the MVP
  mechanism is the logical dump — proven end-to-end (capture, PGDMP
  validation, pg_restore round trip, dogfooding). Trigger: a real
  application whose dump exceeds a feasible dump window, or whose
  recovery tolerance demands point-in-time restore.
- *GCS adapter* (ADR-004 "later, isolated adapters"): restic's own
  backend covers the repository side meanwhile. Trigger: a deployment
  whose only available object storage is GCS.
- *Azure adapter*: same shape; ADR-001's residual risk (young Azure
  SDK) is the standing note. Trigger: an Azure-bound deployment.
- *Sanitizer fuzzing beyond the parser*: one target is implemented (see
  the fuzzing decision below); more targets arrive only when a new
  untrusted-input surface of comparable size appears.

**BY DESIGN — verified, kept**:

- *FTP/FTPS*: excluded with evidence (ADR-004: insecure by default, no
  resumability guarantees, weak metadata). SFTP covers the
  transfer-over-network need with real security. No trigger — the
  exclusion stands unless the evidence changes.
- *The pre-0.8.0 capture-path fallback*: compatibility for real
  historical state files; an additive serde default. Removing it would
  break old records for zero gain.
- *The unmapped-target OS interpretation*: the path map IS the
  cross-platform restore (ADR-008); unmapped targets follow the OS per
  standard behavior.
- *The Desktop-VM mountpoint gate*: an environmental fact of Docker
  Desktop; the product aborts honestly naming the sidecar remedy (the
  CI-side half was fixed in Phase 9).

## The two finalization findings, fixed in 1.0

1. **`restore --verify` checked the LIVE SQLite path** (the
   definition's `path`, not the restored copy placed at the step's
   target). Fixed: `execute_steps` now returns each restored SQLite
   file's target, and the checks run against it. Regression test:
   `restore_verify_checks_the_restored_sqlite_copy` (corrupt the live
   file — the verify still passes on the restored copy).
2. **A pg_dump that exits 0 with garbage output produced a
   silently-useless snapshot.** Fixed: the capture validates the PGDMP
   signature of a custom-format dump and aborts naming the dump.
   Regression test: `garbage_pgdump_output_fails_the_capture`.
3. **`restore_volume` on hosts where the docker-reported mountpoint is
   unreachable or unwritable** (the DR proof's own finding): on Docker
   Desktop the old direct copy wrote the bytes to a PHANTOM host path
   (the VM-internal mountpoint string resolved on the current drive);
   on the hosted runner it hit PermissionDenied (root-owned docker
   data). Fixed: docker volumes ALWAYS restore through a throwaway
   container (the reverse of the sidecar capture), with the
   never-overwrite contract preserved by a read-only listing pass
   first. Regression tests:
   `docker_volume_restores_through_the_reverse_sidecar` (proven on
   both Desktop and the hosted runner) and the DR proof's own two-path
   cycle.
4. **Single-FILE sources could not restore** (the clean-checkout
   smoke's finding): the promotion copy handled directories only, so a
   file-shaped source was reported as "does not exist in the
   snapshot". Fixed: the copy handles both shapes — a single file
   lands as `target/<file-name>` under the same never-overwrite
   contract. Regression test: `a_single_file_source_restores`.

## The raw-recovery path (--from-engine)

The disaster runbook audit found the one honest hole in the defining
scenario: on a fresh host the state file — the record of snapshots —
died with the VPS, so `restore` could not discover from the engine
alone. The fix, bounded: `vaultline restore --from-engine` restores the
WHOLE engine snapshot into the target without the state record or the
procedure (the engine's own snapshot list drives the selection; the
manifest names live in the state record, so the operator picks the
pieces). The full procedure-driven restore works once the state file is
recovered from its offsite copy — docs/disaster-recovery.md documents
both paths and the state-file backup step. Regression test:
`from_engine_restores_without_the_state_file`.

## The fuzzing decision

Targeted, not blanket: ONE cargo-fuzz target — the configuration
parser, the largest untrusted-input surface (configs may be shared,
generated, or tampered; parse + validate must never panic) — runs as a
60-second libFuzzer+ASan smoke on a nightly CI job (weekly schedule +
manual dispatch, .github/workflows/fuzz.yml). The stable-rust
2000-round mutation harness remains the every-push proof. The fuzz
crate is excluded from the workspace (nightly-only). Locally the target
compiles; the smoke executes on the ubuntu job (Windows lacks the ASan
runtime DLL — recorded).

## The 1.0 acceptance criteria

v1.0.0 ships when ALL of the following hold, each by evidence:

1. the six Phase 9 fixes stand (SFTP key/known-hosts wiring, the
   agent-free proof, the sidecar image, lock start-time verification,
   the hosted named-volume proof, the stale-text cleanup)
2. the two finalization defects are fixed with regression tests
3. the raw-recovery path exists, is tested, and is documented in the
   DR runbook
4. the failure-mode matrix is closed: every case detected/reported/
   tested, the untestable-portable cases documented
5. the fuzz smoke job exists and its target builds
6. the benchmark baseline covers CLI startup and the bench check gates
   it
7. the documentation answers the ten user-scope questions and matches
   the implementation (the docs audit's findings all fixed)
8. the disaster-recovery proof executes: backup → destroy → clean
   environment → restore → every artifact independently verified
9. all local gates green; hosted master + tag CI green
10. the clean-checkout release validation passes and the published
    v1.0.0 artifacts verify (checksum + version)

## Consequences

- The remaining-items list is fully classified: nothing is left in an
  ambiguous state; every deferred item carries its trigger.
- The documentation's by-design records (docs/limitations.md) and this
  ADR are the single source for "what Vaultline deliberately is not".
- Vaultline enters the MAINTENANCE lifecycle stage (SOT Section 15)
  after the 1.0 release; evolution items activate on their recorded
  triggers.
