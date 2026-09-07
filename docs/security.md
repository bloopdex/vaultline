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
  copied into backups. Database credentials never reach argv: PostgreSQL
  dumps authenticate through a temporary PGPASSFILE (0600 on Unix,
  removed after the dump); MySQL/MariaDB through the `MYSQL_PWD`
  environment variable; the connection string passed to `pg_dump` is
  sanitized (password stripped) by a unit-tested parser.
- **Status**: enforced — connection-string sanitization and the pgpass
  format are unit-tested; the capture integration tests run against real
  servers.

## Malicious backup archives on restore

- **Threat**: a compromised or poisoned repository returns archives
  containing path traversal, symlinks, hardlinks, or setuid files that
  escape or weaponize the restore target.
- **Boundary**: the restore operation.
- **Defense**: restore-to-sandbox first, explicit promotion second — the
  restore procedure (ADR-003) is target-aware by design, and destructive
  actions are explicit, never implicit.
- **Status**: enforced — the executor stages the whole snapshot under
  `.vaultline/<id>/`, promotes only what the procedure declares, and
  **never overwrites an existing file** (a collision is an error naming
  the file; the contract is integration-tested).

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
  content never enters the repository, and a failure-path test proves a
  failed dump with an embedded password prints the password NOWHERE
  (argv sanitized, env never logged, engine stderr does not echo it).

## Symlinks in malicious archives

- **Threat**: a hostile (or compromised) snapshot whose entries are
  symlinks pointing outside the captured tree — following them during
  promotion would exfiltrate the link targets into the restore, or
  recurse outside the sandbox.
- **Boundary**: the promotion copy (`copy_tree_into`).
- **Defense**: every entry is decided on `symlink_metadata`, never on
  the followed type; symlinks are recreated AS symlinks pointing where
  the archive declared — never followed, never written through. A host
  that cannot create the link (Windows without link privileges) fails
  loudly naming the file rather than silently following it.
- **Status**: enforced — unit + integration tests pin that the link
  target's content never lands in the restored tree.

## Hostile configuration input

- **Threat**: malformed or adversarial configuration files crashing the
  parser or validator (panics are not diagnostics).
- **Boundary**: `config::parse_str` / `config::validate`.
- **Defense**: a 10 MiB file-size limit at load with a clear error, and
  a deterministic mutation harness (seeded byte flips/insertions/
  deletions over the valid template) asserting the parser and validator
  never panic — errors are the contract.
- **Status**: enforced — 2000 seeded mutation rounds in the unit suite.

## Corrupted backups

- **Threat**: silent corruption (FAST '08 measured it at material rates)
  turns "backup created" into false confidence.
- **Boundary**: the verification policy.
- **Defense**: the tiered verification levels L1–L6 (ADR-003) — engine
  integrity checks, restore rehearsal, and the full disaster-recovery
  test — with `doctor`/`status` reporting the last-known level per
  snapshot and the verification schedule executing it.
- **Status**: enforced — L1–L6 execute on demand and on schedule (L6
  is the full recovery rehearsal, ADR-006), the reached level is
  recorded durably, and the disaster scenario is pinned by tests. The
  vanished-source defense (ADR-007) guarantees a backup never claims a
  source it did not capture: missing paths abort before the engine
  runs.

## Reliability of destructive actions (ADR-007)

- **Threat**: a crashed run wedging every future run (stale lock), a
  vanished source producing a silently-incomplete snapshot, or a
  rehearsal colliding with another snapshot's layout.
- **Boundary**: the state lock, the backup pre-flight, the rehearsal
  executor.
- **Defense**: locks are reclaimed only on evidence of the holder's
  death (liveness probe; unanswerable counts as alive); capture paths
  are existence-checked before the engine runs; staging and promotion
  both live under the per-snapshot rehearsal directory.
- **Status**: enforced and integration-tested (live-holder refusal,
  dead-holder reclaim, vanished-source abort, rehearsal isolation).

## Destructive actions (prune)

- **Threat**: retention deletes the wrong snapshots (misconfigured
  policy, a bug in the bucket computation) and the data is gone.
- **Boundary**: `vaultline backup prune`.
- **Defense**: safe by default — without `--apply` nothing changes, and
  every decision prints with its reasons for human review first. With
  `--apply`: only snapshots the state file records are ever considered;
  the engine listing is cross-checked before any `forget` (an id the
  engine no longer holds is noted, never blindly sent); snapshot
  records are never rewritten — the outcome is appended to the
  operations record, so what was deleted and why stays auditable.
- **Status**: enforced and integration-tested (dry-run purity, applied
  idempotence, the recorded outcome).
