# Vaultline

**Application-aware backup & disaster recovery for Linux VPSes.**

Vaultline turns one declarative *Application Recovery Definition* — everything
an application needs to be recreated, declared in a single file — into
scheduled, verified, restorable backups. It orchestrates the mature backup
engine [restic](https://restic.net) instead of reimplementing deduplication,
compression, or encryption.

## The problem

Modern VPS applications are compositions: git repos, configuration, a
PostgreSQL database, uploaded files, Docker volumes, credentials, deployment
metadata. Existing tools protect individual data sources extremely well; the
operator still carries the composition burden — what to back up, in what
order, how to get a consistent database dump, where each backup lives, how
retention works, how to verify it, and how to reconstruct the application.
**Application recovery is a composition problem, not a file-copy problem.**

## The idea

One engine-agnostic definition per application (`vaultline.toml`):

- **sources** — files, git remotes, and config *references* (recorded, never
  copied: secrets stay where they are)
- **databases** — captured through their engine's consistency mechanism,
  never by copying live database files
- **volumes** — Docker volumes with explicit capture semantics
- **storage** — where the repository lives; credentials are environment
  variable names, never literals
- **retention** — deterministic and explainable
- **verification** — a level from L1 (command succeeded) to L6 (full
  application recovery test), the distinction between "backup created" and
  "backup proven restorable"
- **restore procedure** — the ordered steps a fresh host follows after a
  disaster

Restore is first-class from day one: the disaster scenario — VPS destroyed,
new VPS, install, connect repository, restore, verify, operational — is the
defining end-to-end test.

## What it does today

- `vaultline init` — write a commented, valid configuration template
  (safe by default: never overwrites an existing file)
- `vaultline validate` — strict configuration checking: unknown fields are
  rejected, and **every** validation failure is reported at once
- `vaultline backup run` — execute the definition against restic: quiesce
  rules, git mirror staging, config references recorded but never copied,
  **per-engine database dumps** (PostgreSQL `pg_dump -Fc`, MySQL/MariaDB
  `mysqldump`, SQLite via the Online Backup API — credentials never in
  argv), volume capture (direct semantics), repository initialization on
  first use, the backup, and the inline L2 integrity check when the
  policy demands it; the snapshot (engine reference, database metadata,
  verification level actually reached, what it can reconstruct) is
  recorded in the state file
- `vaultline backup list` — the recorded snapshots per application
- storage backends: local (fully integration-tested), S3-compatible
  (proven against MinIO), SFTP (URL-mapped; agent/ssh-config auth)
- the canonical recovery model (ADR-003), typed TOML configuration
  (ADR-004), the error model with documented exit codes, structured
  logging, named metrics
- supply-chain verification from day one: cargo-vet (with public audit-store
  imports and certified direct dependencies) and cargo-deny

MySQL/MariaDB capture is implemented but not yet integration-proven;
volume sidecar/pause-first semantics, restore, prune, and verification
beyond L2 are declared but not executed — see
[docs/limitations.md](docs/limitations.md) for exactly what is missing and
what would remove each limitation.

## Quick start

```sh
cargo build --release

# create a template configuration
./target/release/vaultline init --name my-app

# fill in your application's paths, then check it
./target/release/vaultline validate

# back it up (restic on PATH, password in the referenced env var)
RESTIC_PASSWORD_MY_APP=... ./target/release/vaultline backup run
./target/release/vaultline backup list
```

Exit codes: `0` success · `1` operational failure · `2` usage/config error.
Logs are JSON lines on stderr; stdout carries machine-readable command output.

## Documentation

- [docs/index.md](docs/index.md) — the documentation tree
- [docs/adr/](docs/adr/) — architecture decision records
- [docs/architecture.md](docs/architecture.md) — design and crate layout
- [docs/config.md](docs/config.md) — the `vaultline.toml` reference
- [docs/cli.md](docs/cli.md) — commands, exit codes, output contracts
- [docs/security.md](docs/security.md) — the threat model
- [docs/testing.md](docs/testing.md) — the test strategy
- [docs/observability.md](docs/observability.md) — logging and metrics
- [docs/limitations.md](docs/limitations.md) — what is missing and why

## License

MIT — see [LICENSE](LICENSE).
