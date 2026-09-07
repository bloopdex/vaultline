# Limitations

Each limitation in the four-part form: what is missing / why / what
happens instead / what would remove it.

## MySQL/MariaDB is proven by the round trip; MariaDB itself is not

- **What is missing**: the MySQL restore round trip is proven (dump →
  mysql client into a second database → rows verified, source
  untouched); MariaDB — the other member of the `mysql`/`mariadb` kind
  pair — has never been run against its own server.
- **Why**: the engines share the dump tool and the wire protocol; a
  MariaDB testcontainer is one module swap away, and the round trip
  already pinned one real bug (the `--databases` flag embedded CREATE
  DATABASE/USE in dumps, overriding declared restore targets).
- **What happens instead**: MariaDB definitions run through the same
  mysqldump path as MySQL.
- **What would remove it**: a MariaDB testcontainer round trip.

## Volume sidecar / pause-first semantics are declared, not captured

- **What is missing**: volumes with `capture = "sidecar"` or
  `"pause-first"` are validated but not captured.
- **Why**: the semantics decision (read-only sidecar mount vs pause
  first) is an experiment against real Docker volumes; the direct path
  (host path or `docker volume inspect` mountpoint) is implemented and
  proven for host paths.
- **What happens instead**: the run warns and records such volumes as
  declared-but-not-captured in the snapshot metadata — never silently
  claimed.
- **What would remove it**: the sidecar and pause-first executors.

## Docker-volume direct capture is proven on Unix, not on Desktop VMs

- **What is missing**: the named docker-volume proof exists
  (`named_docker_volume_direct_capture` — a real volume, data written
  through a throwaway container, captured via the resolved mountpoint
  and verified in the repository) but is Unix-gated: on Windows/macOS
  Desktop the volume's mountpoint lives inside the Docker VM, not on
  the host, so the test cannot run locally.
- **Why**: the proof executes on the hosted ubuntu job (native Linux
  Docker keeps volumes on the host).
- **What happens instead**: resolution failures are explicit operational
  errors naming the volume.
- **What would remove it**: nothing local — the gate is environmental.

## SFTP key_file / known_hosts are not wired

- **What is missing**: an sftp storage target works with ssh-agent and
  `~/.ssh/config`, but the `key_file` and `known_hosts` fields are
  rejected with an explicit error.
- **Why**: restic's mechanism for custom SSH commands (`-o
  sftp.command`) executes a string through a shell — that conflicts with
  the argv-only discipline (ADR-002). Wiring it safely needs a
  documented, injection-free approach.
- **What happens instead**: the validator accepts the fields; `backup
  run` refuses with a clear "not wired yet" error rather than building a
  shell string.
- **What would remove it**: an injection-free custom-SSH mechanism or
  restic gaining first-class key-file flags.

## The SFTP proof is agent-gated; locally it needs the OpenSSH agent

- **What is missing**: the SFTP end-to-end proof exists (`atmoz/sftp`
  sshd + key authentication through the SSH agent + the real known-hosts
  verification path) but requires a running SSH agent — on the dev
  machine the Windows OpenSSH agent service is disabled (error 1058),
  so the test skips with a note.
- **Why**: the proof executes on the hosted ubuntu job (openssh-client
  installed); locally it runs once the agent service is enabled.
- **What happens instead**: the suite skips with the enabling
  instructions; S3 (MinIO) and local storage remain fully proven.
- **What would remove it**: `Start-Service ssh-agent` on the dev
  machine (user decision).

## Rehearsal directories of forgotten snapshots are not cleaned

- **What is missing**: `backup prune` forgets engine snapshots but does
  not remove their `<rehearsal.target>/.vaultline-rehearsal/<id>/`
  directories — old rehearsal layouts accumulate on the rehearsal host.
- **Why**: the per-snapshot rehearsal directory holds only vaultline's
  own rehearsal artifacts (never user data), so cleanup is hygiene, not
  safety; the executor already clears a directory before re-rehearsing
  the same snapshot.
- **What happens instead**: each snapshot's rehearsal is cleared on its
  own next rehearsal; forgotten snapshots keep their last layout.
- **What would remove it**: pruning the rehearsal directories of
  forgotten snapshots in `backup prune --apply` (Phase 7 candidate).

## Cross-platform restore is not supported

- **What is missing**: a snapshot taken on one OS restores on the same
  OS only (path translation maps Windows drive paths to their stored
  `/C/...` form; Unix paths map 1:1).
- **Why**: the VPS story is Linux-to-Linux; cross-platform path mapping
  is a distinct problem with no current user.
- **What happens instead**: restored paths that do not exist in the
  snapshot fail with an explicit error naming the path.
- **What would remove it**: a path-mapping table per backup host.
