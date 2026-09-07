# Security

The threat model lives in [docs/security.md](docs/security.md) — each
threat with its boundary, defense, and implementation status.

## Reporting a vulnerability

Report security issues privately to the repository owner (contact in the
commit history / hosting profile). Please include:

- the affected command and configuration shape
- steps to reproduce
- the impact you believe the issue has

The model-level controls already enforced today: configuration files can
never carry literal credentials, unknown configuration fields are
rejected, and the manifest format structurally excludes secret values
(the redaction contract).
