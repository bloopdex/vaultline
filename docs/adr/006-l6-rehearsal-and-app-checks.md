# ADR-V0-6: L6 Rehearsal Automation and Executable App Checks

- Status: accepted
- Date: 2026-09-07

## Context

The verification ladder ends at L6 — "full application recovery test" —
the one level with no executor. `app_checks` were recorded names, not
executable checks. Phase 0's promise ("scheduled verification levels up
to a full disaster-recovery rehearsal") was therefore half-delivered:
the levels up to L5 execute, the top level did not. Meanwhile the
hardening layer owed tests from the testing strategy: hostile
configuration input, malicious snapshot archives, corrupted
repositories, and credential redaction under failure.

## Decision

**The definition declares the rehearsal root.** `application.rehearsal.target`
joins the model, and validation REQUIRES it when the verification level
is L6 — a definition that promises the full recovery test must say
where the scratch rehearsal lives.

**The rehearsal mirrors the layout, never touches the live paths.**
The L6 executor restores the snapshot into
`<target>/.vaultline-rehearsal/<snapshot-id>/` and executes the restore
procedure with its PATH targets remapped under the rehearsal root
(absolute targets mirrored — a scratch clone of the layout; leading
slashes and a Windows drive prefix are stripped so the root stays the
prefix). Database names and health URLs are not paths and run as
declared. Re-rehearsing a snapshot clears only that snapshot's own
rehearsal directory — it holds no user data.

**App checks are executable, shell-free.** The wire format changes from
name strings to check tables (`name`, `command`, `args` — argv, never a
shell string, per the security model). The environment contract: CWD is
the rehearsal root (or the `restore --verify` target), and
`VAULTLINE_REHEARSAL_DIR` names it. Exit 0 passes; anything else fails
the rehearsal with the stderr tail as the diagnosis. Checks are the
application's own semantics — vaultline only provides the argv boundary
and the environment.

**L6 is reached through the existing ladder.** `backup verify` runs
L3–L5 first, records the reached level durably, THEN attempts the
rehearsal — a failing rehearsal must not erase the L5 actually proven.
On success the snapshot's `highest_verified_level` rises to L6 with
`verified_at`. `schedule run` inherits the semantics: a due
verification at policy level L6 IS the rehearsal.

**The hardening layer is tested, not asserted:** a 10 MiB configuration
size limit with a clear error; a deterministic mutation harness (fixed
seed, byte flips/insertions/deletions) proving the parser and validator
never panic on hostile input; the malicious-archive defense — symlink
decisions are made on `symlink_metadata`, symlinks are recreated AS
links and never followed (a host that cannot create links fails loudly,
never silently follows them); the corrupt-repository fixture (a
truncated pack makes the inline L2 check fail and the snapshot records
L1 — the level actually reached); and the redaction failure test (a
failed dump with an embedded password prints the password nowhere).

## Consequences

- The verification ladder is complete: every level L1–L6 executes, on
  demand and on schedule, and the reached level is recorded durably.
- A rehearsal never touches live paths; the operator's only obligation
  is declaring a scratch root.
- The `app_checks` wire format is a breaking change (names → check
  tables). Nothing external depends on it; the change is documented in
  the CHANGELOG.
- Hostile input, malicious archives, corrupted repositories, and
  credential failures are pinned by tests rather than promised.

## Alternatives considered

- **Checks as a separate command** (`vaultline check <app>` with the
  checks against the live application) — rejected: verification is
  about the SNAPSHOT; running checks against live data proves nothing
  about restorability.
- **Rehearsal as a dedicated restore-procedure variant declared in the
  definition** — more explicit, but duplicates the procedure; the
  remap-under-root rule is smaller and deterministic.
- **Following symlinks during promotion with a "sandbox" claim** —
  rejected outright: following is exactly the behavior a malicious
  archive exploits; the defense is not to follow.

## Revisit conditions

- If rehearsals need per-run cleanup semantics beyond per-snapshot
  directories (prune cleaning forgotten snapshots' rehearsal trees is a
  Phase 7 candidate), revisit the directory layout.
- If app checks grow environment needs (service endpoints, ports), the
  environment contract (CWD + VAULTLINE_REHEARSAL_DIR) may gain a
  documented port-assignment hook.
