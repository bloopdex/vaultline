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
- **Status**: active — every repository initialized by `backup run` is a
  restic repository with its own password; wrong passwords surface as
  configuration errors, never fallbacks.

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
  parsed. This is why the sftp `key_file` shortcut is refused: restic's
  mechanism for it would reintroduce shell parsing.
- **Status**: enforced in the engine layer; integration-tested end to
  end (paths with spaces and odd characters are valid arguments).

## Secrets in logs and manifests

- **Threat**: connection strings, repository passwords, or environment
  values appear in logs or snapshot manifests.
- **Boundary**: the logging and manifest-writing code.
- **Defense**: the redaction contract — manifests carry shapes and names,
  never values; structured logging carries field paths and counts, never
  environment contents. Config references (`.env` files) are recorded as
  pointers and are structurally excluded from the backup path list.
- **Status**: enforced — an integration test proves the secret file's
  content never enters the repository.

## Corrupted backups

- **Threat**: silent corruption (FAST '08 measured it at material rates)
  turns "backup created" into false confidence.
- **Boundary**: the verification policy.
- **Defense**: the tiered verification levels L1–L6 (ADR-003) — engine
  integrity checks, restore rehearsal, and the full disaster-recovery
  test — with `doctor` reporting the last-known level per snapshot.
- **Status**: the levels are the product's core model; execution arrives
  with verification.
