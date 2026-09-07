# Security model

The threat model was recorded before implementation (ADR-003). Each entry:
threat → boundary → defense → status.

## Encryption at rest and in transit

- **Threat**: an attacker reads the backup repository (stolen disk, exposed
  bucket).
- **Boundary**: the repository.
- **Defense**: reuse restic's audited cryptography — content-defined
  chunking, authenticated encryption, integrity-checked metadata.
  Vaultline never invents cryptography.
- **Status**: the model declares the repository-password environment
  variable; execution (and thus the encryption itself) arrives with backup
  execution.

## Key and credential management

- **Threat**: secrets leak through configuration files, backups of the
  backup tooling, or repository access.
- **Boundary**: the configuration file and the process environment.
- **Defense**: credentials are environment-variable *names* in
  `vaultline.toml`, never literals; the validator enforces it. Config
  references (`.env` files) are recorded in manifests as pointers, never
  copied into backups.
- **Status**: enforced at configuration time today.

## Malicious backup archives on restore

- **Threat**: a compromised or poisoned repository returns archives
  containing path traversal, symlinks, hardlinks, or setuid files that
  escape or weaponize the restore target.
- **Boundary**: the restore operation.
- **Defense**: restore-to-sandbox first, explicit promotion second — the
  restore procedure (ADR-003) is target-aware by design, and destructive
  actions are explicit, never implicit.
- **Status**: designed; enforced when restore execution lands.

## Subprocess safety

- **Threat**: command injection through source paths, environment values,
  or repository fields reaching a shell.
- **Boundary**: every restic invocation.
- **Defense**: argv arrays only — no shell interpolation, ever
  (ADR-002). Paths and values travel as arguments, not as strings to be
  parsed.
- **Status**: the discipline is recorded and will be test-enforced with
  the orchestration layer.

## Secrets in logs and manifests

- **Threat**: connection strings, repository passwords, or environment
  values appear in logs or snapshot manifests.
- **Boundary**: the logging and manifest-writing code.
- **Defense**: the redaction contract — manifests carry shapes and names,
  never values; structured logging carries field paths and counts, never
  environment contents.
- **Status**: the manifest types enforce it structurally (no field for
  values exists); log hygiene is test-enforced once orchestration emits
  events.

## Corrupted backups

- **Threat**: silent corruption (FAST '08 measured it at material rates)
  turns "backup created" into false confidence.
- **Boundary**: the verification policy.
- **Defense**: the tiered verification levels L1–L6 (ADR-003) — engine
  integrity checks, restore rehearsal, and the full disaster-recovery
  test — with `doctor` reporting the last-known level per snapshot.
- **Status**: the levels are the product's core model; execution arrives
  with verification.
