# Limitations

Each limitation in the four-part form: what is missing / why / what
happens instead / what would remove it.

## MariaDB and MySQL are both proven by round trips

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

## Lock liveness uses pid reuse's accepted trade-off

- **What is missing**: stale-lock recovery (ADR-007) decides "dead" by
  pid liveness; a reused pid (a new unrelated process that inherited
  the crashed process's pid) would read as a live holder and refuse the
  run until manual removal.
- **Why**: plain-file locking with pid records is the boring,
  dependency-free contract; process start times would need per-platform
  OS APIs.
- **What happens instead**: the refusal names the lock file and the
  manual fix; the liveness probe is fail-safe (unanswerable = alive).
- **What would remove it**: a lock scheme with per-holder tokens or
  start-time verification (revisit condition in ADR-007).

## Cross-platform restore is not supported

- **What is missing**: a snapshot taken on one OS restores on the same
  OS only (path translation maps Windows drive paths to their stored
  `/C/...` form; Unix paths map 1:1).
- **Why**: the VPS story is Linux-to-Linux; cross-platform path mapping
  is a distinct problem with no current user.
- **What happens instead**: restored paths that do not exist in the
  snapshot fail with an explicit error naming the path.
- **What would remove it**: a path-mapping table per backup host.
