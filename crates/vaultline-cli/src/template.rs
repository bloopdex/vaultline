//! The configuration template written by `vaultline init`. It must always be
//! a VALID configuration for the declared name — there is a test enforcing
//! that invariant, and `init` re-validates the file it writes.

/// The template with `{name}` placeholders. Commented blocks (database,
/// volumes, further restore steps) show shapes without taking effect.
const TEMPLATE: &str = r#"# vaultline.toml — one Application Recovery Definition.
# What this file declares, engine-agnostically: everything the application
# needs to be recreated (sources, databases, volumes, storage, retention,
# verification, restore procedure). Vaultline orchestrates mature backup
# engines (restic) to execute it — it never reimplements deduplication,
# compression, or encryption.
#
# Check it anytime with: vaultline validate
# Regenerate with:       vaultline init --name <app>

[application]
name = "{name}"
description = "What this application is"
# schedule = "30 2 * * *"     # backup schedule: 5-field cron (minute hour
#                               day-of-month month day-of-week)

# Sources: files, git remotes, and config references.
# A config reference is RECORDED but never copied — secrets stay where they
# are; the restore procedure re-provisions them.
[[application.sources.files]]
name = "uploads"
paths = ["/srv/{name}/uploads"]
# excludes = ["**/*.tmp"]

[[application.sources.config_refs]]
name = "env"
path = "/srv/{name}/.env"
note = "secrets — reference only"

# Databases are captured through their engine's consistency mechanism —
# never by copying live database files. For a PostgreSQL database:
# [[application.databases]]
# name = "main"
# kind = "postgresql"           # postgresql | mysql | mariadb | sqlite
# url_env = "{NAME}_DATABASE_URL"   # the env var holding the connection string
# consistency = {{ logical = {{ format = "custom" }} }}   # custom (-Fc) | sql
# For a SQLite database use `path = "/srv/{name}/app.db"` instead of
# `url_env`, and `format = "sql"` (the backup API only produces plain SQL).

# Docker volumes:
# [[application.volumes]]
# name = "{name}_data"
# capture = "direct"            # direct | sidecar | pause-first

# WHERE the snapshot repository lives (ADR-V0-4). Credentials are env-var
# names, never literals. Other backends: kind = "s3" (endpoint, bucket,
# access_key_env, secret_key_env) or kind = "sftp" (host, port, user,
# path — the remote repository directory; key_file/known_hosts are
# declared but not wired yet: ssh-agent and ~/.ssh/config work today).
[application.storage]
kind = "local"
path = "/var/backups/{name}"
password_env = "RESTIC_PASSWORD_{NAME}"
# repository = "{name}"          # repository name (default: the application name)

# Retention is deterministic and explainable; at least one count must be
# non-zero (all-zero would delete everything on the first prune).
[application.retention]
keep_last = 14
keep_daily = 7
keep_weekly = 4
keep_monthly = 6
keep_yearly = 1

# Verification: the level a snapshot must reach before it counts as healthy
# (L1 command succeeded … L6 full recovery test). The schedule runs through
# `vaultline schedule run` or the generated systemd timer; app_checks are
# recorded for a later phase.
[application.verification]
level = 3
# schedule = "0 3 * * 7"
# app_checks = ["pg-integrity"]

# The ordered restore procedure — what a fresh host follows to reconstruct
# this application after a disaster.
[[application.restore.steps]]
restore_files = { source = "uploads", target = "/srv/{name}/uploads" }

# [[application.restore.steps]]
# restore_database = { database = "main", target_database = "{name}" }
# [[application.restore.steps]]
# restore_volume = { volume = "{name}_data" }
# [[application.restore.steps]]
# wait_healthy = { url = "http://localhost:3000/health" }
"#;

/// Render the template for an application name. The name must already have
/// passed [`vaultline_core::config::validate_application_name`].
pub fn render(name: &str) -> String {
    TEMPLATE
        .replace("{name}", name)
        .replace("{NAME}", &name.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vaultline_core::config::{parse_str, validate};

    /// The template must always be a valid configuration for any legal name —
    /// `init` promises to write a file that `validate` accepts.
    #[test]
    fn rendered_template_validates() {
        let contents = render("thornwa");
        let outcome = validate(parse_str(&contents).expect("template parses"));
        assert!(
            outcome.is_valid(),
            "template failed validation: {:?}",
            outcome.errors
        );
        assert!(
            outcome.warnings.is_empty(),
            "template should not warn: {:?}",
            outcome.warnings
        );
        let app = outcome.application.expect("application");
        assert_eq!(app.name, "thornwa");
        assert_eq!(app.sources.len(), 2);
        assert_eq!(app.restore.steps.len(), 1);
    }
}
