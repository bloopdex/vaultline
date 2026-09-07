# Changelog

## 0.1.0 — 2026-09-07

The foundation release (no binaries published; the repository is the
artifact):

- **The canonical Application Recovery Model** (ADR-003): `Application`
  and `BackupSnapshot` types — engine-agnostic, serialization pinned —
  with the verification levels L1–L6, explainable retention, and the
  ordered restore procedure.
- **Typed TOML configuration** (ADR-004): strict parsing (unknown fields
  rejected with span), aggregated validation (every failure reported with
  its field path), the shared application-name rule.
- **CLI foundation**: `init` (safe-by-default — never overwrites), and
  `validate` (human + `--json`), `--version`, with the documented exit-code
  contract (0/1/2).
- **Error model and structured logging**: JSON lines on stderr,
  `RUST_LOG`, `--log-format`.
- **Supply chain from day one**: cargo-vet (public audit-store imports +
  18 certified direct production dependencies) and cargo-deny (advisories,
  licenses, bans, sources — all green), in CI with the workflow
  self-validation script.
- **Test infrastructure**: 36 tests (model/config/error units, CLI
  integration), the ignored testcontainers harness for later phases.
- **Architecture decision records 001–004** migrated into docs/adr/.
- The research record (the tool landscape survey, the gap analysis, and
  the evaluation that commissioned the project) lives in the project
  graph.
