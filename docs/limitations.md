# Limitations

Each limitation in the four-part form: what is missing / why / what
happens instead / what would remove it.

The 0.9.0 fix round closed five recorded entries (SFTP
`key_file`/`known_hosts`, the agent-gated SFTP proof, the sidecar's
hardcoded alpine image, the lock's pid-reuse window, and the
named-volume proof's CI skip — see the ADR-002 part 3, ADR-007 and
ADR-008 amendments). What remains:

## Docker-volume direct capture is proven on Unix, not on Desktop VMs

- **What is missing**: the named docker-volume proof exists
  (`named_docker_volume_direct_capture` — a real volume, data written
  through a throwaway container, captured via the resolved mountpoint
  and verified in the repository). It is Unix-gated AND gated on the
  mountpoint being reachable from the test process: on Windows/macOS
  Desktop the mountpoint lives inside the Docker VM. On the hosted CI
  job the proof EXECUTES (the job grants docker-data traversal; the
  first hosted run had recorded the skip — fixed 2026-09-08).
- **Why**: the proof runs wherever the mountpoint is visible; the
  sidecar and pause-first semantics ARE proven everywhere else
  (host-path volumes through real containers — they are exactly the
  remedy for the mountpoint gap).
- **What happens instead**: on Desktop VMs the proof skips with the
  reason named; direct/pause-first captures whose docker-reported
  mountpoint the host cannot reach abort with an error naming the
  sidecar remedy (the pre-flight's Desktop signature); resolution
  failures are explicit operational errors naming the volume.
- **What would remove it**: nothing local — the Desktop-VM gate is
  environmental.

## Lock start-time verification degrades on non-Linux unix

- **What is missing**: the pid-reuse defense (ADR-007) records and
  compares the holder's process start time. Linux reads
  `/proc/<pid>/stat`; Windows asks PowerShell for `StartTime`. On
  other unix (macOS, BSD) there is no probe, so the lock degrades to
  the pre-0.9.0 liveness-only behavior: a reused pid there would read
  as a live holder until manual removal.
- **Why**: the product targets Linux VPSes; adding per-BSD OS APIs for
  a fail-safe degradation path is not worth the surface.
- **What happens instead**: liveness-only verification (the previous,
  accepted contract); the fail-safe discipline is unchanged (an
  unanswerable probe never reclaims).
- **What would remove it**: a probe for the target unix (e.g. `ps -o
  lstart=` with format normalization).

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
