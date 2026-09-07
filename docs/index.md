# Vaultline documentation

The documentation tree, organized by concern.

## Design & decisions

- [architecture.md](architecture.md) — the design, crate layout, and the
  engine-agnostic boundary
- [adr/](adr/) — architecture decision records (language, engine strategy,
  the recovery model, the storage abstraction)

## Reference

- [config.md](config.md) — the `vaultline.toml` reference: every field,
  the validation rules, and the annotated example
- [cli.md](cli.md) — commands, the exit-code contract, logging and output
  contracts

## Quality & operations

- [security.md](security.md) — the threat model (threat → boundary →
  defense)
- [testing.md](testing.md) — the test strategy and how to run each layer
- [observability.md](observability.md) — structured logging and the named
  metrics
- [limitations.md](limitations.md) — what is missing, why, what happens
  instead, and what would remove each limitation
