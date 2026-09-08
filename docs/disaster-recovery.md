# Disaster recovery — the VPS-disappears runbook

The defining scenario: the VPS dies completely. What survives is the
restic repository (off-host storage) and the recovery definition (the
`vaultline.toml` lives with the application's own repository — that is
the point of the Application Recovery Definition). What does NOT
survive is the state directory, which holds vaultline's record of
snapshots (ids, manifests, verification levels). This runbook is the
complete honest path from nothing to a running application.

## What survives where

| Artifact | Where it lives | Survives the VPS? |
|---|---|---|
| the snapshot data (files, dumps, volumes) | the restic repository (local dir on another disk / S3 / SFTP) | yes — the repository IS the disaster copy |
| the recovery definition (`vaultline.toml`) | the application's own repo, checked in | yes — copy it from git |
| credentials (repo password env, database URLs) | environment variables | you re-set them on the new host |
| the state directory (`state.json` + the lock) | the dead VPS | **only if you copied it offsite** — see below |

## Step 0 (once, before the disaster): back up the state file

The state file is one small JSON document (`$VAULTLINE_STATE_DIR/
state.json`). Copy it offsite with the definition — e.g. check it into
the app repo, or let another tool back it up. It is what turns
`vaultline restore` into the full, procedure-driven recovery.

## The runbook

### 1. Install

```sh
sh -c "$(curl -fsSL https://github.com/bloopdex/vaultline/releases/download/v1.0.0/install.sh)"
```

Install restic and the dump tools the definition needs (docs/cli.md's
tool table). Copy `vaultline.toml` from the app repo.

### 2. Set the environment

Every credential the definition names: the repository password env,
the database connection env vars, the s3 env vars, `VAULTLINE_STATE_DIR`
if you override it.

### 3. Discover

With the state file restored to the state directory:

```sh
vaultline backup list --config vaultline.toml
vaultline backup inspect latest --config vaultline.toml
vaultline doctor --config vaultline.toml     # environment + storage check
```

### 4. Restore

```sh
vaultline restore latest --config vaultline.toml --target /srv --verify
```

`--path-map from=to` adapts the definition's production targets to the
new host's layout; `--dry-run` shows the plan first. Promotion never
overwrites an existing file; a partial restore explains itself and
names the retry path.

### 5. Verify

`--verify` checks the restored SQLite databases (integrity_check on the
restored copy) and runs the definition's app checks. For server
databases, `restore_database` already ran pg_restore / mysql against
the declared target — check the row counts and start the application;
`wait_healthy` steps poll it if declared.

## The raw-recovery fallback (state file lost)

If the state file did not survive, the engine's own snapshot list is
still the record of what exists:

```sh
# plan: which engine snapshot would land where
vaultline restore latest --from-engine --config vaultline.toml --target /srv --dry-run

# recover: the WHOLE engine snapshot lands in the target
vaultline restore latest --from-engine --config vaultline.toml --target /srv
```

This restores everything — the files, the dumps (`<name>.dump` for
PostgreSQL, `<name>.db` for SQLite), the captured volumes — but NOT
through the restore procedure: the manifest names live in the state
record, so the operator picks the pieces (feed the dump to pg_restore,
move the volume bytes). This is the honest degraded path — a reason to
keep Step 0's copy.

## What the dogfooding proved

The real ThornWA-shaped stack ran exactly this cycle (2026-09-08,
recorded on the Phase 10 page) — backup → destroy → clean environment
→ restore → every artifact independently verified:

- **Backup**: 1592 files / 72.57 MB in 20.8 s (the dump + two sidecar
  volume captures + restic), L2 verified.
- **Destroy**: `docker compose down -v` — the postgres container AND
  its data volume gone; the OpenWA volume deleted.
- **Restore path 1 (the definition)**: files (8 migrations), the
  database (pg_restore into `thornwa_restored` — the 2 users rows
  back, byte-identical), and the OpenWA volume (4 files) in 4.5 s.
- **Restore path 2 (the raw volume)**: the entire postgres data volume
  restored into a never-booted empty volume (1579 files in 6.5 s) —
  postgres then BOOTED HEALTHY from the restored data with the
  database intact (`1|rami`, `2|demo`).
- The proof caught and fixed a real defect: on hosts where the
  docker-reported mountpoint is unreachable (Docker Desktop),
  `restore_volume` originally wrote the bytes to a phantom host path —
  it now restores through a reverse-sidecar container with the
  never-overwrite contract preserved by a listing check (regression
  test: `docker_volume_restores_through_the_reverse_sidecar`).
- Operational note the proof surfaced: the volume step lands its bytes
  wherever the volume is resolved — stop the writer container first,
  or restore a database volume into a never-booted empty volume and
  start the container after.
