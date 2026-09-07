# ADR-001 — Implementation language: Rust

*Status: accepted · recorded 2026-09-07 (ADR-V0-1 in the research record)*

## Context

Vaultline is an orchestration layer over backup engines, not a storage or
deduplication engine itself. The choice of language therefore depends on:

1. whether any language gains an in-process engine advantage, and
2. which language satisfies the project's two hard constraints — a
   supply-chain verification culture (dependency audits, vulnerability
   scanning in CI) and VPS-appropriate distribution (a single small static
   binary for systemd timers on small VPSes).

The engine research (ADR-002) settled the first point: restic is explicitly
CLI-only by maintainer policy (its issue tracker records that no stable
library API is offered; the `--json` CLI contract is the machine interface),
and the remaining candidates (Kopia's library, rustic's crates) were rejected
in ADR-002. **Every language is a subprocess orchestrator** — the embeddability
axis favors none.

## Decision

**Rust.** The project is a Cargo workspace of two crates: `vaultline-core`
(the engine-agnostic model and configuration) and `vaultline-cli` (the binary).

## Why Rust over the alternatives

- **Go** — restic's own ecosystem and the widest cloud SDK coverage. Its
  only advantage, in-process engine embedding, does not exist for restic.
  Its SFTP library (`pkg/sftp`) has been frozen since 2023 with a security
  fix taking 8+ weeks to land. Its supply-chain story (no equivalent of
  cargo-vet's shared audit ecosystem) is the weakest of the three.
- **Kotlin/JVM** — matches the strict Gradle verification standard used
  elsewhere, but fails distribution: ~45 MB jlink images, 200–400 ms startup,
  120 MB+ resident memory per timer invocation.
- **Rust** — the unique point satisfying both constraints: cargo-vet /
  cargo-deny replicate the strict-verification philosophy with a shared
  cross-organization audit pool, and a static musl binary (1–8 MB, ~1 ms
  startup) fits a systemd timer on a small VPS. `russh-sftp` is actively
  maintained where Go's equivalent is frozen.

## Consequences

- The whole dependency closure is vetted from day one: cargo-vet with public
  audit-store imports (mozilla, google, isrg, bytecode-alliance,
  embark-studios) plus certifications for the direct production dependencies,
  and cargo-deny (advisories, licenses, bans, sources) in CI.
- Residual risk recorded: the Azure SDK for Rust reached GA only recently —
  the Azure storage adapter is deferred, and restic's own Azure backend
  covers the repository side until then.
- Slower iteration than Go is accepted; the model-first design (ADR-003)
  keeps the hot paths in data, not plumbing.

## Revisit conditions

- None active. If a future engine requirement forces in-process embedding
  into a Go library (e.g. Kopia's interfaces stabilize), reopen the
  language question with that new evidence.
