# CLI reference

## Commands

```
vaultline init          write a commented vaultline.toml template
vaultline validate      check a vaultline.toml and report every problem
vaultline backup run    execute the recovery definition (restic, local/s3/sftp)
vaultline backup list   list the snapshots recorded for this application
vaultline backup verify prove a snapshot against the policy (L3/L4/L5) and
                        record the level actually reached
vaultline backup inspect show a snapshot's full record
vaultline restore       execute the definition's restore procedure
                        (sandbox-then-promote, never overwrites)
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

### `vaultline backup run`

```
vaultline backup run [--config <path>] [--json]
```

Executes the definition: runs each file source's quiesce rule (if
declared), stages git mirror sources, records config references (never
copies them), initializes the repository on first use, runs the restic
backup, and — when the verification policy demands L2 or higher — runs the
repository integrity check inline. The resulting snapshot (engine
reference, verification level actually reached, what it can reconstruct)
is recorded in the state file and reported on stdout (`--json` for the
machine payload).

Sources declared but not captured yet (databases, volumes) are warned on
stderr and recorded in the snapshot's configuration metadata — a snapshot
never claims coverage it does not have.

The state directory is `VAULTLINE_STATE_DIR`, else the platform data
directory (`~/.local/state/vaultline` on Linux). A lockfile refuses
concurrent runs (see `vaultline backup list`'s state below).

### `vaultline backup list`

```
vaultline backup list [--config <path>] [--json]
```

Lists the snapshots recorded for the application: id, timestamp, highest
verification level reached, and what each snapshot can reconstruct.

### `vaultline backup verify`

```
vaultline backup verify <snapshot> [--config <path>] [--json]
```

Proves a snapshot against the definition's verification policy and
records the level actually reached in the state file (durable — `backup
list` shows it):

- **L3** — the engine still holds the recorded snapshot (cross-check).
- **L4** — the sources are restored to scratch and every file's content
  is compared against the live files; any mismatch fails verification.
- **L5** — SQLite databases are restored to scratch and pass
  `integrity_check`; server engines get an honest note (their rehearsal
  is `vaultline restore --verify` against a scratch instance).

`<snapshot>` is the full id, a unique prefix, or `latest`.

### `vaultline backup inspect`

```
vaultline backup inspect <snapshot> [--config <path>] [--json]
```

Prints the snapshot's full record (manifest, database metadata, engine
reference, integrity, reconstructs).

### `vaultline restore`

```
vaultline restore <snapshot> [--config <path>] [--target DIR] [--dry-run] [--verify] [--json]
```

Executes the definition's ordered restore procedure for the snapshot:

1. the snapshot is restored by restic into a staging area under the
   target (`.vaultline/<id>/`),
2. each procedure step promotes content into its declared destination:
   `restore_files` copies (directories created, **existing files never
   overwritten** — a collision is an error naming the file),
   `restore_database` runs the engine's restore tool (pg_restore /
   mysql / a SQLite file placement), `restore_volume` copies the
   volume's bytes, `wait_healthy` polls the health endpoint,
3. `--verify` then runs the restore-side checks (SQLite
   integrity_check on the restored database).

The default target is `./vaultline-restore` — never the live paths
unless the procedure declares them. `--dry-run` prints the plan and
writes nothing.

## Global flags

- `--log-format json|pretty` (default `json`, also `VAULTLINE_LOG_FORMAT`)
- `-h/--help`, `-V/--version`

## Engine and tool requirements

`backup run` invokes external tools, each located via an environment
override or PATH:

| Tool | Override | Used for |
|---|---|---|
| restic | `VAULTLINE_RESTIC_BIN` | the backup engine (ADR-002) |
| pg_dump | `VAULTLINE_PGDUMP` | PostgreSQL custom-format dumps |
| mysqldump | `VAULTLINE_MYSQLDUMP` | MySQL/MariaDB dumps |
| sqlite3 | `VAULTLINE_SQLITE3` | SQLite backups (the Online Backup API) |
| docker | — | docker-volume mountpoint resolution |

The repository password is read from the environment variable named in
the configuration (`application.storage.password_env`), the database
password from the database's connection environment variable — never
from the file, and never on a command line (PostgreSQL authentication
travels through a temporary PGPASSFILE; MySQL through `MYSQL_PWD`).

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
