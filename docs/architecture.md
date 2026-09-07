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

## The engine boundary (declared, not yet built)

When backup execution lands: restic as a subprocess (argv only, never
shell interpolation), `--json` output parsed tolerantly, the documented
exit-code contract mapped to typed outcomes (ADR-002). The storage
adapter delegates to restic's own backends (ADR-004). All of it sits in
the CLI layer, behind interfaces the model does not see.
