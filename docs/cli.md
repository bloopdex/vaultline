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
vaultline backup prune  enforce retention: per-snapshot keep/forget decisions
                        with reasons; dry-run unless --apply
vaultline restore       execute the definition's restore procedure
                        (sandbox-then-promote, never overwrites)
vaultline schedule run  run whatever the definition's schedules make due
vaultline doctor        check the environment (tools, state, storage)
vaultline status        summarize one application's records and schedules
vaultline timer generate write the systemd service/timer unit files
vaultline timer install   install the units into /etc/systemd/system
                          (Linux only; enables without starting)
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
- **L6** — the full recovery rehearsal (ADR-006): the snapshot is
  restored into `<rehearsal.target>/.vaultline-rehearsal/<snapshot-id>/`
  with the procedure's path targets mirrored under the rehearsal root
  (never the live paths), then the app checks run (CWD is the rehearsal
  root, `VAULTLINE_REHEARSAL_DIR` names it). L3–L5 are recorded before
  the rehearsal attempt — a failing rehearsal does not erase the level
  actually proven.

`<snapshot>` is the full id, a unique prefix, or `latest`. The
definition must declare `application.rehearsal.target` when the policy
level is L6.

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
writes nothing. `--verify` also runs the definition's app checks
against the target (same environment contract as L6).

### App checks (ADR-006)

Declared per definition:

```toml
[[application.verification.app_checks]]
name = "pg-integrity"
command = "psql"
args = ["-c", "SELECT 1"]
```

Shell-free argv: `command` is a path or tool name, `args` are argv —
never a shell string. The environment contract: CWD is the rehearsal
root (L6) or the restore target (`restore --verify`), and
`VAULTLINE_REHEARSAL_DIR` names it. Exit 0 passes; anything else fails
the verification with the stderr tail as the diagnosis.

### `vaultline backup prune`

```
vaultline backup prune [--config <path>] [--apply] [--json]
```

Enforces the retention policy with per-snapshot explanations
(ADR-005): every recorded snapshot prints KEEP or FORGET with its
reasons ("among the N most recent snapshots (keep_last)", "newest
snapshot of YYYY-MM-DD (keep_daily)", …, or "matched no retention
rule"). **Dry-run by default** — without `--apply` nothing changes.

`--apply` cross-checks the engine first (only ids the engine still
holds are forgotten — never blind), runs `restic forget` for them and
then `restic prune` (the engine's own defaults govern repack), and
records the outcome in the state file. Re-running after an applied
prune forgets nothing more (idempotent). Snapshot records are never
rewritten — pruning appends to the operations record.

### `vaultline schedule run`

```
vaultline schedule run [--config <path>] [--json]
```

The portable due-ness executor (ADR-005) — for hosts without systemd,
or for exercising the same logic the timers express. A job is due when
its cron has an occurrence strictly after the job's last run that has
already arrived ("never ran" is due immediately); the anchors are the
newest snapshot timestamp (backups) and `operations.last_verify_at`
(verification). Runs the due jobs and explains what ran and why. The
verification attempt time is recorded before the run — a failing
verification does not hot-loop on every invocation.

### `vaultline doctor` / `vaultline status`

```
vaultline doctor [--config <path>] [--json]
vaultline status [--config <path>] [--json]
```

`doctor` checks the environment — configuration, restic, the dump tools
the definition needs (one check per engine kind), the state file, and
storage connectivity (a warning when the password environment variable
is unset) — each as ok/warning/error with details. Exit 0 only when
nothing errored. `status` summarizes one application: recorded/pruned
snapshots, levels reached vs policy, next schedule occurrences, the
retention projection, and the prune history. Neither mutates anything.

### `vaultline timer generate` / `vaultline timer install`

```
vaultline timer generate [--config <path>] [--out DIR]
vaultline timer install [--config <path>] [--now]
```

`generate` writes the unit files (any platform; default output
directory `./vaultline-systemd`, regeneration overwrites — they are
derived artifacts): one service+timer pair per declared schedule —
`vaultline-<app>.timer` runs `backup run` from `application.schedule`,
`vaultline-<app>-verify.timer` runs `backup verify latest` from
`verification.schedule` — each with the cron translated to
`OnCalendar` and `Persistent=true` (a run missed while the host was
down is caught up). A definition with no schedules generates nothing
and errors. A schedule the cron engine accepts but OnCalendar cannot
express is refused with a clear error (validation warns about it up
front).

`install` writes the units into `/etc/systemd/system`, daemon-reloads,
and enables the timers — Linux only, and it never starts them; start
is an explicit `--now`.

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
