# ADR-V0-5: Operations & Scheduling

- Status: accepted
- Date: 2026-09-07

## Context

The definition declares what to back up, how long to keep it, and how to
verify it. Until now, *when* — the backup rhythm, the verification
rhythm, and the retention enforcement — had no executor: schedules were
recorded-but-not-executed, nothing ever deleted snapshots, and the
deployment story (the design philosophy promises "single static binary +
config + systemd timer") did not exist. Three design promises mature
here:

1. **Explainable retention** — "retention decisions must state their
   reason" is a Phase 0 design-philosophy line. Deletion without
   explanation is how backup systems destroy trust.
2. **Safe-by-default destructive actions** — pruning forgets data; the
   default must be to change nothing.
3. **Boring technology** — systemd timers, not a custom daemon.

## Decision

**Schedules live in the definition.** `application.schedule` (backup
runs) joins `verification.schedule`; both are 5-field cron expressions
validated at configuration time. Six/seven-field expressions are
rejected — the accepted language must be expressible by every executor
(`schedule run` and the systemd timer) with identical semantics.

**Due-ness bookkeeping lives in the state file.** A job is due when its
cron has an occurrence strictly after the job's last run that has
already arrived ("never ran" is due immediately). The anchors: the
newest snapshot timestamp for backups; `operations.last_verify_at` for
verification. The verification attempt time is recorded *before* the
run — a failing verification must not hot-loop on every invocation;
the failure itself is the signal. Manual runs do not touch the
bookkeeping.

**Retention is a pure, deterministic plan over recorded snapshots.**
`keep_last N` (the N most recent), `keep_daily D` (the newest snapshot
of each of the D newest calendar days, UTC), and likewise weekly (ISO
weeks), monthly, yearly. A snapshot kept by ANY rule is kept with all
of its reasons; kept by none is forgotten with the reason it matched
none. The newest snapshot is always kept by any non-zero policy (the
all-zero policy is refused at configuration). The plan is computed in
`vaultline-core` and unit-pinned; the executor prints every decision
with its reasons.

**Prune is dry-run by default.** `--apply` forgets only recorded ids
the engine still holds (cross-checked against the engine's listing
first — never blind), then runs `restic prune` (the engine's own
defaults govern repack aggressiveness). The outcome is recorded in the
state file; snapshot records stay immutable — pruning never rewrites
history, it appends to the operations record.

**Two execution paths, one semantics.** `vaultline schedule run` runs
whatever is due (the portable, cron-driven executor); `vaultline timer
generate|install` emits systemd service+timer pairs with the cron
translated to `OnCalendar` and `Persistent=true` (install is Linux-only
and enables without starting; starting is an explicit `--now`).

**Cron evaluation wraps the audited `cron` crate** (chosen by evidence
over hand-rolling: correctness maturity of next-occurrence computation,
and over alternatives after cargo-vet review — the crate, its phf
family, rand, and siphasher are all certified). Three crate behaviors
were recorded empirically during integration and shape the wrapper:
6/7-field input only (the wrapper prepends the fixed second `0` for the
user-facing 5-field contract); day-of-week 1..=7 with Sunday=1 and 0
rejected; and **both-restricted day fields evaluate with AND semantics**
(verified: `0 9 13 * 6` next fires on a Friday-the-13th) while standard
cron and systemd OnCalendar OR them. Rather than ship an executor whose
timer and scheduler could disagree about the same definition,
both-restricted day fields are rejected at validation with an
explanation. A schedule the crate accepts but OnCalendar cannot express
(steps over single values) still runs under `schedule run`; timer
generation refuses it with a clear error, and validation warns.

## Consequences

- Retention is explainable and auditable: `prune` (dry-run by default)
  states per snapshot which rule kept or deleted it, and the applied
  outcome is recorded durably.
- Scheduled execution works on systemd hosts (units) and without
  systemd (`schedule run` under any scheduler) with one due-ness rule.
- The accepted cron language is narrowed: 5 fields, one restricted day
  field — deliberate, documented, and refusal is loud.
- `doctor`/`status` give the operator diagnosis and the retention
  projection without mutating anything.
- State-file schema stays v1: the operations records are additive and
  optional (older state files load; older binaries ignore the new
  fields).

## Alternatives considered

- **Hand-rolled cron evaluator** — full control of semantics and no
  supply-chain surface, but next-occurrence computation is error-prone
  and the crate's quirks were only found by testing real behavior; the
  boring, audited crate won.
- **OnCalendar-only scheduling (no cron in the definition)** — simpler,
  but couples the definition to systemd and leaves non-systemd hosts
  without the schedule at all.
- **Allowing both-restricted day fields with a documented "AND"** —
  rejected: the same definition would then mean different things to the
  timer and the scheduler; ambiguity in when backups run is worse than
  a narrower language.

## Revisit conditions

- If restic gains first-class retention semantics we prefer (or the
  `cron` crate changes its AND behavior), re-evaluate the both-restricted
  rejection and the forget/prune choreography.
- If a second executor target appears (e.g. cronie drop-ins), the
  OnCalendar translation table moves to per-target translators.
