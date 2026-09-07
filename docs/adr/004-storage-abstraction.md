# ADR-004 — Storage abstraction: WHAT vs WHERE

*Status: accepted · recorded 2026-09-07 (ADR-V0-4 in the research record)*

## Context

The recovery definition (ADR-003) declares *what* to back up. Where the
repository lives is a separate concern: the same application definition
must target a local directory today and an object store tomorrow without
changing its shape. The candidate backends were compared on security,
reliability, resumability, metadata, atomicity, operational complexity,
cost, and maturity: local, SFTP, S3-compatible, GCS, Azure, FTP/FTPS.

## Decision

**Separate WHAT (the recovery definition) from WHERE (the repository
adapter).** The `StorageTarget` type declares the backend and its
credentials as environment-variable names; execution is delegated to
restic's own backends (ADR-002) wherever restic already covers the target —
Vaultline's adapter layer only exists where restic does not cover a need.

**MVP backend order: Local → S3-compatible → SFTP.**

- **Local** first: zero dependencies, the disaster-recovery rehearsal needs
  it anyway.
- **S3-compatible** second: the object-store case (MinIO included) with the
  most mature Rust SDK.
- **SFTP** third: the classic VPS offsite target.
- **GCS / Azure** later, as isolated adapters. The Azure SDK for Rust is
  recent GA (recorded in ADR-001); restic's own Azure backend covers the
  repository side meanwhile.

**FTP/FTPS is excluded as a preferred backend** — insecure by default, no
resumability guarantees, weak metadata. A compatibility adapter would be
added only if a concrete requirement appeared.

## Consequences

- `StorageTarget` in the model carries the kind (local path / S3-compatible
  endpoint+bucket / SFTP host+port+user+key), the repository name, and the
  repository-password environment variable — never a literal credential.
- The configuration validator enforces per-kind required fields (for
  example: S3 requires endpoint, bucket, and both key environment variables;
  SFTP requires host and user and a valid port).
- Adding a backend changes the model enum and the validator — a deliberate,
  reviewable diff — not the recovery definitions that use existing backends.
