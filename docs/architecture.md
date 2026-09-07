# Architecture

## Shape

A Cargo workspace with two crates:

```
crates/vaultline-core   the canonical model (ADR-003) + typed configuration
                        (ADR-004) + the error model
crates/vaultline-cli    the binary: command surface, logging, dispatch
```

Dependency direction is inward: `vaultline-core` contains no engine
behavior and no process I/O beyond reading the configuration file;
`vaultline-cli` depends on `vaultline-core`, never the reverse. restic
orchestration (ADR-002) is a CLI-layer concern that does not exist yet —
when it lands, it lands behind the adapter boundary, not inside the model.

## The canonical model

One internal representation shared by every feature (configuration,
snapshot manifests, and later the state file): `Application` and
`BackupSnapshot` in `vaultline-core::model`. The TOML wire format
(`vaultline-core::config`) is a projection of the model: parse strict
(`deny_unknown_fields`), validate everything (cross-references, name
rules, per-backend required fields, retention sanity), convert. Validation
aggregates **all** failures — diagnostics explain, never first-error-only.

## Configuration as data

`vaultline.toml` is a validated, typed document with clear provenance:
unknown fields are rejected at parse time with the parser's line/column
span; semantic rules are rejected at validation time with the offending
field path. The same rules serve `validate` and `init` (the template is
re-validated at write time — an invalid template is a bug, not a user
problem).

## Error handling

One error type (`VaultlineError`) crossing the CLI boundary, classified
into families that map to the documented exit codes (docs/cli.md). The
error message is the diagnosis: file paths, field paths, reasons, and
suggested fixes where a fix exists.

## Logging & observability

Structured logging from day one (SOT Section 14): JSON lines on stderr by
default, `--log-format pretty` for humans, level via `RUST_LOG`. stdout is
reserved for machine-readable command output. Named metrics are designed
in docs/observability.md and emit once backup operations exist.

## Concurrency

None. Sequential processing is the design default; parallelism is added
only when measurement demonstrates a need.

## The engine boundary (implemented)

`vaultline-cli::engine` is the restic adapter (ADR-002): a subprocess
with argv-only arguments (never shell interpolation), the repository
password as an environment variable (never argv), `--json` output parsed
tolerantly, and failures classified by exit code + stderr message — the
amendment in ADR-002 records why (the exit-code tables differ between
restic versions; the messages are stable). The storage adapter builds
restic repository URLs for local / s3 / sftp targets (ADR-004) and
forwards credential environment variables; connectivity is carried by
restic's own backends.

`vaultline-cli::backup` is the orchestration: quiesce rules, git mirror
staging, config-reference recording (never copied), repository
initialization on first use, the backup, the inline L2 integrity check,
and the `BackupSnapshot` record persisted through
`vaultline-core::state` (atomic writes + a create-new lockfile).

## State

Plain files, per ADR-003: `state.json` (schema version 1) records
snapshots per application; a `lock` file (create-new semantics) refuses
concurrent runs. Embedded SQLite remains off the table until evidence
demands it.
