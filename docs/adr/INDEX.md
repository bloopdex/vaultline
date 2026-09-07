# Architecture Decision Records

Each record: Context → Decision → Consequences, plus the alternatives
considered and the explicit revisit conditions. The numbering continues the
`ADR-V0-n` identifiers used during the research phase (the research record
lives in the project graph; the repository is the durable home).

| ADR | Subject | Status |
|---|---|---|
| [001](001-rust-implementation-language.md) | Implementation language: Rust (ADR-V0-1) | accepted |
| [002](002-engine-strategy-restic-orchestration.md) | Backup engine strategy: orchestrate restic via its `--json` contract (ADR-V0-2) | accepted |
| [003](003-application-recovery-model.md) | The Application Recovery Model and verification levels L1–L6 (ADR-V0-3) | accepted |
| [004](004-storage-abstraction.md) | Storage abstraction: WHAT vs WHERE, backend order (ADR-V0-4) | accepted |
| [005](005-operations-scheduling.md) | Operations & scheduling: cron execution, explainable retention, prune safety, systemd timers (ADR-V0-5) | accepted |
| [006](006-l6-rehearsal-and-app-checks.md) | L6 rehearsal automation, executable app checks, and the hardening layer (ADR-V0-6) | accepted |
| [007](007-reliability-and-performance.md) | Reliability & performance: stale-lock recovery, vanished-source defense, rehearsal isolation, the benchmark baseline (ADR-V0-7) | accepted |
| [008](008-release-and-ecosystem.md) | Release & ecosystem: sidecar and pause-first volume capture, recorded capture paths, the cross-platform path map, the release machinery, the deferred signal edges (ADR-V0-8) | accepted |
