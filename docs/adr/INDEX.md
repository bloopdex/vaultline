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
