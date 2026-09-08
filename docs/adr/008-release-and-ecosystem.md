# ADR-V0-8: Release & Ecosystem

- Status: accepted
- Date: 2026-09-07

## Context

Phase 8 closes the three recorded gaps between v0.7.0 and a releasable
tool: the two capture semantics that were still declared-but-not-captured
(sidecar, pause-first), the cross-platform restore limitation (a snapshot
restored only on its own OS), and the missing release machinery (no
version surface, no installers, no tag-driven publishing). Plus the
ecosystem edges the SOT records as planned (backup-health →
DeployScore, restored-environment verification ↔ EnvFP).

Empirical facts that shaped the decisions:

1. **The mountpoint problem.** `docker volume inspect` reports a
   volume's mountpoint, but on Docker Desktop (Windows/macOS) that path
   lives inside the Docker VM — the host cannot reach it. Direct
   capture is impossible there; a host-path volume works everywhere.
2. **The pause contract.** `docker pause` fails on a stopped container,
   but a stopped container is already quiescent — pausing it is
   unnecessary. Distinguishing "running but pause failed" from "not
   running" by stderr-message matching would couple to Docker's wording;
   `docker inspect --format {{.State.Running}}` is the evidence.
3. **The staging path problem.** A sidecar capture stages the volume's
   bytes in a temporary directory — a path that exists only during the
   backup. A restore that re-resolves the volume on the restore host
   would look for the bytes under the wrong path; the snapshot must
   record where the bytes were captured.
4. **The stored-path form.** restic stores Windows paths with forward
   slashes and the drive letter as a bare first component (`C:/x` →
   `C/x`); Unix paths pass through. Restoring a Windows-made snapshot on
   Linux therefore stages a plain `C/...` directory. Locating content
   across platforms needs one platform-independent translation, not two
   platform-specific ones.

## Decision

**Sidecar capture** (`capture = "sidecar"`): the volume is mounted
read-only into a throwaway `alpine` container that copies it into a
host staging directory (bind-mounted into the container) — argv-only,
no shell, no new dependencies. The image is pulled on first use. This
is the semantics that works where the host cannot reach the volume's
mountpoint; it works everywhere else too.

**Pause-first capture** (`capture = "pause-first"`): the volume
declares its writer (`container`); the executor pauses it, captures
through the direct mechanism, and unpauses ALWAYS — a drop guard, so
even a failed capture unpauses (a backup error must never leave a
production container paused; an unpause failure is logged as CRITICAL
with the manual fix). A stopped container is already quiescent: the
capture proceeds with a note instead of failing on the un-pausable
container. A running container whose pause fails aborts — never
capture live writes under a pause that never happened.

**Snapshots record their capture paths.** Each volume manifest entry
carries the path the bytes were captured from (`volume-direct`,
`volume-pause-first`, `volume-sidecar`). Restores locate the content
through the record, never through the restore host's own resolution
(hosts differ; the volume may not even exist on the restore host).
Entries without a path (snapshots recorded before this field) fall
back to the restore host's resolution.

**Cross-platform restore through a declared path map.** The restore
procedure gains `path_map` entries (`from` → `to`) and the CLI gains
repeatable `--path-map from=to`. The longest matching prefix wins;
ties go to the later entry (CLI entries come last, so they override
the configuration). The map applies to the live restore's path targets
(files, SQLite targets, volume destinations); the rehearsal mirrors
never get mapped — they represent the production layout. The
snapshot-location translation (`snapshot_path_of`) becomes
platform-independent: backslashes to forward slashes and the drive
prefix to the bare leading component, so content captured on either OS
is located on either OS. An unmapped target resolves per the OS's own
interpretation (a Unix target on Windows means the current drive's
root) — the map exists for operators who need control.

**The release machinery mirrors the established precedent.** A
`version` subcommand reports the versioned surfaces (binary, state
schema, engine) in human and `--json` forms; installers for both
platforms download the binary and verify it against the published
`SHA256SUMS` before installing; the release workflow is tag-driven,
verifies the tag against the single-sourced workspace version, builds
the static musl Linux binary (ADR-V0-1's distribution story) and the
Windows binary, smoke-tests each, and publishes through the gh CLI;
`docs/release/RELEASE-CHECKLIST.md` is the reproducible procedure.
Publication executes once the repository is hosted.

**The ecosystem edges stay declared, not implemented.** The SOT
Section 7 rule: a signal edge activates when BOTH sides define the
contract. DeployScore (Phase 0) and EnvFP (Phase 0) have no interface
to feed; inventing one here would be an invented transport. The
activation condition is recorded on both sides' future integration
phases.

## Consequences

- The declared-but-not-captured honesty notes are gone: every capture
  semantics in the definition executes. The honesty contract now lives
  in the pre-flight (vanished sources) and the manifest (capture
  paths).
- Volumes are restorable on a different host than the backup host —
  a real cross-host property, not just a cross-OS one.
- Pause windows span the backup run (the writer stays paused from the
  pause to the end of the capture). Fine for the target scale; noted
  as a revisit condition.
- The sidecar executor needs the `alpine` image present or pullable —
  recorded in the template and the doctor-relevant error message. Since
  the fix round (2026-09-08) the volume can declare its own `image`
  (sidecar-only), so air-gapped hosts can pin a pre-loaded image.
- Windows→Linux restore works through the platform-independent
  translation + the path map; the Linux-restore engine shape (a `C/`
  directory staged at the root) is probed against real restic and
  pinned by the translation unit tests.
- Release artifacts are generated, never committed; `dist/` is
  gitignored.

## Alternatives considered

- **tar-stream sidecar** (container tar → host extraction) — rejected:
  needs a host tar step for bytes that end up on disk anyway; the
  bind-mounted `cp -a` is argv-only and has no extraction step.
- **Pause semantics per volume list of containers** — rejected for now:
  one writer per volume covers the declared shapes; a `containers`
  list is additive if a multi-writer volume shows up.
- **Detecting the Desktop-VM mountpoint gap at validation time** —
  rejected: it is an environmental fact of the backup host, not a
  configuration property; the pre-flight error names the sidecar
  remedy instead.
- **Rejecting unmapped cross-platform targets at validation** —
  rejected: the target set is per-host; the map is the operator's
  tool, and the OS's interpretation of an unmapped path is standard
  behavior, documented.
- **Inventing the ecosystem signal now** — rejected per SOT Section 7:
  both sides must define the contract; the activation condition is
  recorded instead.

## Revisit conditions

- If multi-writer volumes appear (several containers writing one
  volume), extend pause-first to a container list.
- If the pause window (whole backup run) becomes a problem for a
  real application, split the run: pause → capture the volume → unpause
  → capture the rest.
- ~~If the alpine dependency is unacceptable on air-gapped hosts,
  revisit the sidecar image contract (a user-declared image).~~
  **Discharged (2026-09-08, the fix round):** the volume's `image`
  field declares the sidecar image (default `alpine`; sidecar-only,
  validated like the container rule).
