# Configuration reference — `vaultline.toml`

One file declares **one application** (the Application Recovery Definition,
ADR-003). Unknown fields are rejected with the parser's line/column span;
semantic rules are checked together and all failures reported at once.

The annotated example (also what `vaultline init` writes):

```toml
[application]
name = "thornwa"                # the application-name rule, below
description = "What this application is"
# schedule = "30 2 * * *"       # backup schedule: 5-field cron
#                                (minute hour day-of-month month day-of-week)

# --- sources -----------------------------------------------------------
[[application.sources.files]]
name = "uploads"                # unique among all sources
paths = ["/srv/thornwa/uploads"]
excludes = ["**/*.tmp"]
# quiesce = { command = "sync", args = [] }   # optional, shell-free argv

[[application.sources.git]]
name = "code"
remote = "https://example.com/thornwa.git"
reference = "main"
capture = "mirror"              # mirror | reference

[[application.sources.config_refs]]
name = "env"                    # recorded, NEVER copied
path = "/srv/thornwa/.env"
note = "secrets — reference only"

# --- databases ---------------------------------------------------------
[[application.databases]]
name = "main"                   # unique among databases
kind = "postgresql"             # postgresql | mysql | mariadb | sqlite
url_env = "THORNWA_DATABASE_URL"   # the env var holding the connection string
consistency = { logical = { format = "custom" } }   # custom (-Fc) | sql
# sqlite databases use `path = "/srv/app/app.db"` instead of `url_env`
# and require `format = "sql"` (the backup API produces plain SQL).

# --- volumes -----------------------------------------------------------
[[application.volumes]]
name = "thornwa_pgdata"
capture = "direct"              # direct | sidecar | pause-first
# container = "app"             # pause-first only: the writer to pause
# image = "alpine:3.20"         # sidecar only: the capture container
#                               # (defaults to `alpine`; pin it on
#                               # air-gapped hosts)

# capture semantics:
#   direct       capture the volume's path (host path or docker mountpoint)
#   sidecar      copy it out through a read-only sidecar container
#                (works where the host cannot reach docker mountpoints,
#                e.g. Docker Desktop; the image is pulled on first use)
#   pause-first  pause `container` first, capture directly, ALWAYS unpause
#                (a stopped writer is already quiescent - it proceeds with
#                a note)

# --- storage (ADR-004) -------------------------------------------------
[application.storage]
kind = "local"                  # local | s3 | sftp
path = "/var/backups/thornwa"   # local only
password_env = "RESTIC_PASSWORD_THORNWA"   # never a literal
# repository = "thornwa"        # defaults to the application name

# s3 fields:  endpoint, bucket, region (optional),
#             access_key_env, secret_key_env
# sftp fields: host, port, user, path (remote directory),
#              key_file (optional), known_hosts (optional)
#   key_file:     the private key for authentication (instead of the
#                 agent / ~/.ssh/config)
#   known_hosts:  the known-hosts file restic verifies the server key
#                 against (replaces the user's ~/.ssh/known_hosts)
#   Both are passed to the native ssh client through restic's own argv
#   pipeline (no shell anywhere); paths must not contain quote or
#   newline characters, and must exist as files at backup time.

# --- retention ---------------------------------------------------------
[application.retention]
keep_last = 14                  # at least one count must be non-zero
keep_daily = 7
keep_weekly = 4
keep_monthly = 6
keep_yearly = 1

# --- verification (ADR-003) ---------------------------------------------
[application.verification]
level = 3                       # 1..=6, the levels L1..L6
# schedule = "0 3 * * 1"        # verification schedule: 5-field cron

# [[application.verification.app_checks]]   # executable checks (ADR-006)
# name = "pg-integrity"
# command = "psql"              # shell-free argv — never a shell string
# args = ["-c", "SELECT 1"]

# [application.rehearsal]       # REQUIRED when level = 6 (the L6
# target = "/srv/rehearsal"     # recovery rehearsal's scratch root)

# --- the ordered restore procedure --------------------------------------
[[application.restore.steps]]
restore_files = { source = "uploads", target = "/srv/thornwa/uploads" }
[[application.restore.steps]]
restore_database = { database = "main", target_database = "thornwa" }
[[application.restore.steps]]
restore_volume = { volume = "thornwa_pgdata" }
[[application.restore.steps]]
wait_healthy = { url = "http://localhost:3000/health" }

# --- cross-platform restore (ADR-008) -----------------------------------
# [[application.restore.path_map]]  # declared (production) target
# from = "/srv"                    # prefixes translated to THIS host's
# to = "C:/srv"                    # layout; longest prefix wins
```

## The application-name rule

1–63 characters, lowercase alphanumerics and dashes, starting and ending
with an alphanumeric (`thornwa`, `thorn-wa2`; not `Thornwa`, `-thornwa`,
`thornwa-`, `thorn_wa`).

## Validation rules (all applied; all reported)

- at least one capture target exists (sources, databases, or volumes)
- source names unique across all source kinds; database names unique;
  volume names unique (their identity is the name)
- server databases require `url_env` (and reject `path`); SQLite requires
  `path` (and rejects `url_env`); SQLite rejects `format = "custom"`
- storage: `password_env` required for every kind; per-kind required
  fields (local: `path`; s3: `endpoint`, `bucket`, `access_key_env`,
  `secret_key_env`; sftp: `host`, `user`, `path` (the remote repository
  directory), `port` 1–65535)
- retention: at least one keep count non-zero (an all-zero policy would
  delete everything on the first prune)
- verification level 1–6
- schedules: `application.schedule` and `application.verification.schedule`
  are 5-field cron expressions (minute hour day-of-month month
  day-of-week). Rejected: a different field count, invalid syntax, and
  both day fields restricted at once — the evaluation semantics differ
  between cron implementations (the engine ANDs them; systemd timers OR
  them), so the same definition could fire at different times (ADR-005)
- app checks: unique names, non-empty `command` (args are argv)
- L6 requires `application.rehearsal.target` (non-empty)
- volumes: `pause-first` requires `container` (the writer to pause);
  `container` with any other capture is rejected (a meaningless field
  is never silently ignored)
- path map entries: non-empty `from` and `to`, unique `from`
  prefixes (duplicates would make the tie-break order-dependent)
- the configuration file may not exceed 10 MiB (the hardening limit)
- every restore-step reference resolves to a declared source / database /
  volume name
- warnings (non-blocking): relative source paths (they resolve on the
  target host, not locally); a schedule the cron engine accepts but
  OnCalendar cannot express (`vaultline timer generate` will refuse it —
  `vaultline schedule run` still works)
