# Contributing

## Before you push

Run every gate locally, in order, before pushing (all must be green):

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
cargo vet check --store-path supply-chain
cargo deny check
python scripts/check-workflows.py
git diff --check
```

The container harness (`cargo test --test containers -- --ignored`)
requires a running Docker engine and is not part of the pre-push gates.

## Dependency policy

Adding or upgrading a dependency is a deliberate act:

- `cargo vet` must stay green. New crates either come with imported
  public audits or get certified (`cargo vet certify <crate> <version>
  --criteria safe-to-deploy`) with notes recording what was actually
  reviewed. The exemptions in `supply-chain/config.toml` are the explicit
  review backlog — a new exemption is debt you are deliberately taking on.
- `cargo deny` must stay green across advisories, licenses, bans, and
  sources. A new license in the tree requires updating the allow list in
  `deny.toml` — a reviewable diff, not an accident.
- Prefer upgrading deliberately over merging red dependency-update PRs:
  bump locally, re-run every gate, then commit the lockfile with the
  upgrade.

## Commit conventions

- Commit messages describe the change and why; reference the ADR where
  the change rests on one.
- No automated co-author trailers.

## Documentation

The documentation tree (docs/index.md) is part of the code review:
behavior changes ship with their documentation changes, and every
limitation lives in docs/limitations.md in the four-part form.
