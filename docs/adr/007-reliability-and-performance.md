# ADR-V0-7: Reliability & Performance

- Status: accepted
- Date: 2026-09-07

## Context

Three reliability gaps were recorded before this phase, two of them
with fresh empirical evidence:

1. **A crashed run wedges every future run.** The state lock was
   create-new only: a `SIGKILL`ed backup leaves the lock file forever,
   and every later command errors until a human removes it.
2. **A vanished source produces a silently-incomplete snapshot.**
   Verified empirically (2026-09-07): restic prints "does not exist,
   skipping" for a missing backup path and SUCCEEDS — a backup that
   claims the source but never captured it.
3. **Rehearsals of different snapshots collide.** The Phase 6 remap
   mirrored the procedure's targets under the SHARED rehearsal root; two
   rehearsals promote into the same mirrored layout, and the
   never-overwrite contract turns the second one into an error.

Plus the measurement debt: SOT Section 12 says "baseline before
target", and backup operations have existed since Phase 2 with no
baseline.

## Decision

**Stale locks are recovered by evidence, only.** The lock file records
the holder's pid. On contention, a liveness probe decides: a DEAD
holder (a crashed run) is reclaimed with a warning and the acquisition
retries once; an ALIVE holder errors exactly as before; an unreadable
or unparsable lock file is never reclaimed — without evidence of
death, the strict contract stands. The probe is CLI-side (`kill -0` /
`tasklist`), injected into the core lock so vaultline-core stays
process-I/O-free, and is fail-safe: an unanswerable probe counts as
"alive". PID-reuse risk is documented as the accepted trade-off of
plain-file locking.

**Every declared capture path is existence-checked before the engine
runs.** A vanished source aborts the backup naming the path — the
failure-first honesty contract over a "successful" incomplete backup.
(The check is cheap and local; the engine remains the authority on
everything after the pre-flight.)

**The rehearsal remap root is the per-snapshot rehearsal directory.**
`<target>/.vaultline-rehearsal/<snapshot-id>/` holds BOTH the staging
and the promoted mirrored layout; the app checks run with that
directory as CWD and `VAULTLINE_REHEARSAL_DIR`. Rehearsals of
different snapshots can never collide, and re-rehearsing a snapshot
clears only its own directory.

**Prune removes the forgotten snapshots' rehearsal directories** —
they hold only vaultline's own rehearsal artifacts, so they die with
their snapshot. Removal is robust on Windows: restic-restored
directories carry mode-derived restrictive ACLs (the same root cause
as its restore-timestamp "Access is denied" quirk — verified: not even
the read-only attribute can be changed on them without an `icacls
/reset` by the owner), so the removal resets ACLs, clears read-only
attributes, and retries briefly against antivirus's transient locks.

**A partial restore explains itself.** Promotion stopped part-way
leaves already-promoted content in place (never-overwrite forbids
clobbering); the error now says so and names the retry path.

**The benchmark baseline lands.** A deterministic std-only example
measures the hot paths (config load+validate, retention plan over 1000
snapshots, cron next-occurrence, state round trip) as medians;
`docs/benchmarks/baseline.json` commits the measured numbers and
`scripts/bench-check.py` fails any metric above 3x its baseline.

## Consequences

- A crashed run no longer wedges the host: the next run reclaims the
  lock with a warning, and a genuinely concurrent run is still refused.
- Incomplete backups cannot be silent; the operator learns the exact
  vanished path.
- Rehearsals are isolated per snapshot and their directories have a
  defined lifecycle (removed with the snapshot).
- The recorded restic-on-Windows findings grow: ACL-locked restored
  directories join the exit-code and timestamp quirks.
- Performance is baselined with a regression gate; targets (SOT
  Section 12) can now be set against evidence.

## Alternatives considered

- **Stale-lock recovery by age** (remove locks older than N minutes) —
  rejected: time is a heuristic, liveness is evidence; a slow legit
  run could lose its lock.
- **Shell-level `--databases`-style pre-checks in the engine** — the
  engine is restic; the pre-flight is ours by construction.
- **Rehearsal layout with content-addressed subdirectories** — more
  machinery; per-snapshot directories are already unique.
- **criterion for benchmarks** — rejected for now: a new dependency
  for measurements the std-only harness covers; revisit when variance
  analysis is needed.

## Revisit conditions

- If restic gains a missing-path error mode, the pre-flight becomes a
  defense-in-depth check rather than the only defense.
- ~~If lock contention becomes a real problem on multi-admin hosts,
  revisit PID liveness (process start times) or move to a
  lock-file-with-token scheme.~~ **Discharged (2026-09-08, the fix
  round):** the lock now records the holder's process start time at
  acquisition (Linux `/proc/<pid>/stat`; Windows PowerShell
  `StartTime`; other unix degrades to liveness, recorded honestly) and
  the contention check compares it — a live pid whose start time
  differs from the record is a REUSED pid and is reclaimed with the
  same warning. Pid-only locks from earlier builds fall back to
  liveness alone. The remaining theoretical gap (a pid reused by a
  process that started at the same jiffie) is negligible; a
  lock-file-with-token scheme stays a future option if evidence ever
  demands it.
