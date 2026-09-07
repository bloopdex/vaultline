# Limitations

Each limitation in the four-part form: what is missing / why / what
happens instead / what would remove it.

## MariaDB and MySQL are both proven by round trips

## The sidecar capture depends on the alpine image

- **What is missing**: `capture = "sidecar"` runs a throwaway `alpine`
  container, so the image must be present locally or pullable at
  backup time.
- **Why**: the sidecar's job is to reach volumes the host cannot
  (Docker Desktop keeps mountpoints inside its VM); an image is the
  vehicle. alpine is the smallest standard one.
- **What happens instead**: the capture fails with the docker stderr
  and the requirement named ("the alpine image must be present or
  pullable"); the run is aborted, never silently incomplete.
- **What would remove it**: a user-declared sidecar image in the
  volume definition (the ADR-008 revisit condition).

## Docker-volume direct capture is proven on Unix, not on Desktop VMs

- **What is missing**: the named docker-volume proof exists
  (`named_docker_volume_direct_capture` — a real volume, data written
  through a throwaway container, captured via the resolved mountpoint
  and verified in the repository) but is Unix-gated: on Windows/macOS
  Desktop the volume's mountpoint lives inside the Docker VM, not on
  the host, so the test cannot run locally.
- **Why**: the proof executes on the hosted ubuntu job (native Linux
  Docker keeps volumes on the host). The sidecar and pause-first
  semantics ARE proven on Desktop (host-path volumes through real
  containers — they are exactly the remedy for the mountpoint gap).
- **What happens instead**: direct/pause-first captures whose
  docker-reported mountpoint the host cannot reach abort with an error
  naming the sidecar remedy (the pre-flight's Desktop signature);
  resolution failures are explicit operational errors naming the
  volume.
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

## Unmapped cross-platform targets follow the OS's interpretation

- **What is missing**: a restore target that matches no path-map entry
  resolves per the operating system's own path rules — a Unix target
  like `/srv/app` on Windows means the current drive's root
  (`C:\srv\app`).
- **Why**: the target set is per-host; the map is the operator's
  control, and the OS interpretation is the standard behavior of every
  path-based tool.
- **What happens instead**: the declared path map (configuration +
  `--path-map`) translates declared production prefixes into this
  host's layout — longest prefix wins, CLI entries break ties; the
  dry-run plan shows each mapping explicitly. Content location is
  platform-independent (backslash/drive normalization), so a snapshot
  made on either OS is found on either OS.
- **What would remove it**: nothing to remove — the map IS the
  cross-platform restore (ADR-008).

## Pre-Phase-8 snapshots have no recorded volume capture path

- **What is missing**: snapshots recorded before v0.8.0 carry no
  `path` on their volume manifest entries; their restores resolve the
  volume on the restore host (which may not be the capture host).
- **Why**: the recorded capture path (ADR-008) is a new additive state
  field; old state files are honored as-is, not rewritten.
- **What happens instead**: the restore falls back to the restore
  host's resolution of the volume name — correct whenever the hosts
  resolve it identically (the same-host case, and host-path volumes).
- **What would remove it**: a fresh backup records the path; the
  fallback stays for the old records.
