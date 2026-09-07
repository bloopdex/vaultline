# CLI reference

## Commands

```
vaultline init     write a commented vaultline.toml template
vaultline validate check a vaultline.toml and report every problem
vaultline --version
```

### `vaultline init`

```
vaultline init [--name <name>] [--config <path>]
```

- `--name` — the application name for the template (default
  `example-app`); must satisfy the application-name rule
  (docs/config.md)
- `--config` — where to write (default `./vaultline.toml`)

`init` is generative, not destructive: it **never overwrites** an existing
file. The written template is re-validated before the command reports
success — a template that does not validate is a bug.

### `vaultline validate`

```
vaultline validate [--config <path>] [--json]
```

- `--config` — the file to validate (default `./vaultline.toml`)
- `--json` — machine-readable result on stdout (exit code unchanged)

On success the summary line reports the application name and its counts.
On failure **every** validation error is printed, each with its field path
(e.g. `application.retention: at least one keep count must be non-zero
...`). Warnings (e.g. relative paths that resolve on the target host) do
not fail validation.

The `--json` payload:

```json
{ "valid": true,
  "application": "thornwa",
  "warnings": ["..."],
  "summary": { "sources": 2, "databases": 1, "volumes": 1,
               "verification_level": 3, "storage": "local",
               "restore_steps": 3 } }
```

Invalid configurations carry `"valid": false` and an `errors` array of
`{ "path", "message" }` objects; the exit code is still 2.

## Global flags

- `--log-format json|pretty` (default `json`, also `VAULTLINE_LOG_FORMAT`)
- `-h/--help`, `-V/--version`

## Exit codes

| Code | Family | Meaning |
|---|---|---|
| 0 | success | the command completed |
| 1 | operational | an operation started but could not complete (I/O, engine, network, internal invariant) |
| 2 | usage/config | the invocation or the configuration is wrong — fix it and retry |

## Output contract

- **stdout** — machine-readable command output (the validate summary, the
  `--json` payload).
- **stderr** — logs (JSON lines by default) and human diagnostics
  (errors, warnings).
- Log level via `RUST_LOG` (default `vaultline=info`).
