//! restic orchestration (ADR-V0-2): a subprocess speaking the documented
//! `--json` contract. Discipline enforced here:
//!
//! - arguments are argv arrays — never shell interpolation
//! - the repository password travels via the `RESTIC_PASSWORD` environment
//!   variable, never argv (argv is visible in the process table)
//! - failures are classified by **exit code + stderr message** (see
//!   [`classify_failure`]): the exit-code tables differ between restic
//!   versions (empirically verified on 0.19.1, 2026-09-07: missing
//!   repository → 10, wrong password → 12; older restic used 3/10/11/12),
//!   and the messages are stable across both. Anything unrecognized is a
//!   failure, never a guess.
//! - `--json` output is parsed tolerantly (unrecognized lines and fields are
//!   ignored); a missing or malformed *summary* line is an operational
//!   failure, never a crash

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use vaultline_core::error::{ErrorKind, Result, VaultlineError};

/// restic failure classes, derived from exit code + stderr message
/// (ADR-V0-2 amendment 2026-09-07 — see [`classify_failure`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResticFailure {
    /// The repository does not exist (probe before init).
    RepoMissing,
    /// Another process holds the repository lock.
    Locked,
    /// The repository password (or key) was rejected.
    WrongPassword,
    /// restic does not know the command we issued.
    UnknownCommand,
    /// The run was interrupted.
    Interrupted,
    /// Any other failure.
    Generic,
}

/// A located restic binary and everything it needs to run.
pub struct Restic {
    bin: PathBuf,
}

/// The result of a successful backup run: the engine's snapshot reference
/// and the recorded file statistics.
#[derive(Debug, Clone)]
pub struct BackupSummary {
    pub snapshot_id: String,
    pub files_new: u64,
    pub total_bytes_processed: u64,
}

impl Restic {
    /// Locate the restic binary: `VAULTLINE_RESTIC_BIN` overrides, else
    /// `restic` on PATH.
    pub fn locate() -> Result<Self> {
        if let Some(env_bin) = std::env::var_os("VAULTLINE_RESTIC_BIN") {
            let bin = PathBuf::from(&env_bin);
            if !bin.is_file() {
                return Err(VaultlineError::new(
                    ErrorKind::Operational,
                    format!(
                        "VAULTLINE_RESTIC_BIN points to {}, which does not exist",
                        bin.display()
                    ),
                ));
            }
            return Ok(Self { bin });
        }
        Ok(Self {
            bin: PathBuf::from("restic"),
        })
    }

    /// Initialize the repository if it does not exist yet. `init` creates
    /// the repository (and the bucket, for s3 backends); an existing
    /// repository makes init fail with "already exists", which is treated
    /// as success.
    ///
    /// Why init-first instead of probe-then-init (ADR-002 amendment):
    /// probing a missing s3 repository hangs — restic retries a missing
    /// bucket indefinitely ("Stat(<config/>) returned error, retrying"),
    /// never returning the missing-repository exit code.
    pub fn init_if_needed(
        &self,
        repo: &str,
        password: &str,
        extra_envs: &[(String, String)],
    ) -> Result<()> {
        let output = self
            .command(password, extra_envs)
            .arg("-r")
            .arg(repo)
            .arg("init")
            .output()
            .map_err(|e| self.spawn_error(e))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr_text = String::from_utf8_lossy(&output.stderr);
        if stderr_text.contains("already exists") {
            return Ok(());
        }
        match output.status.code() {
            Some(code) => {
                Err(classify_failure(code, &output.stderr).into_error(password, &output.stderr))
            }
            None => Err(VaultlineError::new(
                ErrorKind::Operational,
                "restic was terminated by a signal",
            )),
        }
    }

    /// Run an arbitrary restic command with fixed arguments (argv only —
    /// the caller passes complete arguments, never fragments of a shell
    /// command).
    pub fn run(
        &self,
        fixed: &[&str],
        values: &[String],
        repo: &str,
        password: &str,
        extra_envs: &[(String, String)],
    ) -> Result<std::process::Output> {
        let mut command = self.command(password, extra_envs);
        command.arg("-r").arg(repo);
        command.args(fixed);
        command.args(values);
        command.output().map_err(|e| self.spawn_error(e))
    }

    /// List the engine's snapshots (`snapshots --json`). The output is a
    /// single JSON array on one line; each element is one snapshot.
    pub fn list_snapshots(
        &self,
        repo: &str,
        password: &str,
        extra_envs: &[(String, String)],
    ) -> Result<Vec<Value>> {
        let output = self
            .command(password, extra_envs)
            .arg("-r")
            .arg(repo)
            .args(["--json", "snapshots"])
            .output()
            .map_err(|e| self.spawn_error(e))?;
        self.ensure_success(&output, password)?;
        let mut snapshots = Vec::new();
        for line in parse_json_lines(&output.stdout) {
            if let Value::Array(items) = line {
                snapshots.extend(items);
            } else {
                snapshots.push(line);
            }
        }
        Ok(snapshots)
    }

    /// Run a backup: the given paths enter the repository, the given glob
    /// patterns are excluded. Returns the engine's summary.
    pub fn backup(
        &self,
        repo: &str,
        password: &str,
        paths: &[&Path],
        excludes: &[String],
        extra_envs: &[(String, String)],
    ) -> Result<BackupSummary> {
        let mut command = self.command(password, extra_envs);
        command.arg("-r").arg(repo).args(["--json", "backup"]);
        for exclude in excludes {
            command.arg("--exclude").arg(exclude);
        }
        command.arg("--").args(paths);
        let output = command.output().map_err(|e| self.spawn_error(e))?;
        self.ensure_success(&output, password)?;
        parse_backup_summary(&output.stdout)
    }

    /// Verify repository integrity (the quick structural check — the
    /// verification-level L2 gate; `--read-data` sampling is a later level).
    pub fn check(&self, repo: &str, password: &str, extra_envs: &[(String, String)]) -> Result<()> {
        let output = self
            .command(password, extra_envs)
            .arg("-r")
            .arg(repo)
            .arg("check")
            .output()
            .map_err(|e| self.spawn_error(e))?;
        self.ensure_success(&output, password)
    }

    fn command(&self, password: &str, extra_envs: &[(String, String)]) -> Command {
        let mut command = Command::new(&self.bin);
        // The password is an environment variable for restic, not argv.
        command.env("RESTIC_PASSWORD", password);
        for (key, value) in extra_envs {
            command.env(key, value);
        }
        command
    }

    fn spawn_error(&self, e: std::io::Error) -> VaultlineError {
        if e.kind() == std::io::ErrorKind::NotFound {
            VaultlineError::new(
                ErrorKind::Operational,
                format!(
                    "restic not found (looked for {}): install restic or point VAULTLINE_RESTIC_BIN at the binary",
                    self.bin.display()
                ),
            )
        } else {
            VaultlineError::with_source(ErrorKind::Operational, "cannot start restic", e)
        }
    }

    fn ensure_success(&self, output: &std::process::Output, password: &str) -> Result<()> {
        match output.status.code() {
            Some(0) => Ok(()),
            Some(code) => {
                Err(classify_failure(code, &output.stderr).into_error(password, &output.stderr))
            }
            None => Err(VaultlineError::new(
                ErrorKind::Operational,
                "restic was terminated by a signal",
            )),
        }
    }
}

/// Classify a restic failure by exit code + stderr message.
///
/// Why messages matter (ADR-V0-2 amendment 2026-09-07): the exit-code
/// tables differ between restic versions. Empirically verified on restic
/// 0.19.1: a missing repository exits **10** ("repository does not exist"),
/// a wrong password exits **12** ("wrong password or no key found"), and
/// lock contention blocks until the lock frees rather than erroring. Older
/// restic releases used 3 (missing repo), 10 (lock), 11 (wrong password),
/// 12 (unknown command). The stderr messages are stable across both tables,
/// so the classification is code + message, with unknown combinations
/// treated as generic failures — never guessed.
pub fn classify_failure(code: i32, stderr: &[u8]) -> ResticFailure {
    let text = String::from_utf8_lossy(stderr);
    match code {
        // 3: historical "repository does not exist" (pre-0.19 restic).
        3 => ResticFailure::RepoMissing,
        // 10: restic 0.19.x "repository does not exist"; historically the
        // lock code. The message disambiguates.
        10 => {
            if text.contains("lock") {
                ResticFailure::Locked
            } else {
                ResticFailure::RepoMissing
            }
        }
        // 11: historical wrong-password; 12: restic 0.19.x wrong-password
        // and the historical unknown-command. The message disambiguates.
        11 | 12 => {
            if text.contains("unknown command") {
                ResticFailure::UnknownCommand
            } else {
                ResticFailure::WrongPassword
            }
        }
        130 => ResticFailure::Interrupted,
        _ => ResticFailure::Generic,
    }
}

impl ResticFailure {
    /// Convert to a typed vaultline error. `password` names the environment
    /// variable for diagnostics; `stderr` supplies detail for the generic
    /// case.
    fn into_error(self, password: &str, stderr: &[u8]) -> VaultlineError {
        match self {
            ResticFailure::RepoMissing => VaultlineError::new(
                ErrorKind::Operational,
                "repository does not exist".to_string(),
            ),
            ResticFailure::Locked => VaultlineError::new(
                ErrorKind::Operational,
                "the repository is locked by another process; retry later (restic lock held)".to_string(),
            ),
            ResticFailure::WrongPassword => VaultlineError::new(
                ErrorKind::Config,
                format!(
                    "the repository password was rejected — check the {password} environment variable"
                ),
            ),
            ResticFailure::UnknownCommand => VaultlineError::new(
                ErrorKind::Internal,
                "restic does not know the command we issued — this is a vaultline bug (check the restic version)".to_string(),
            ),
            ResticFailure::Interrupted => VaultlineError::new(
                ErrorKind::Operational,
                "restic was interrupted".to_string(),
            ),
            ResticFailure::Generic => {
                let tail = stderr_tail(stderr);
                if tail.is_empty() {
                    VaultlineError::new(ErrorKind::Operational, "restic failed".to_string())
                } else {
                    VaultlineError::new(ErrorKind::Operational, format!("restic failed: {tail}"))
                }
            }
        }
    }
}

/// The last line(s) of restic's stderr, for diagnostics (JSON on stdout
/// carries data; stderr carries restic's human errors).
fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines().rev().take(3).collect::<Vec<_>>().join(" | ")
}

/// Parse restic's `--json` line stream tolerantly: every line is a JSON
/// object; lines that do not parse are ignored (unknown message types).
fn parse_json_lines(stdout: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .collect()
}

/// Find the `message_type: "summary"` line of a backup run and extract the
/// engine snapshot reference. Missing or malformed summary = the contract
/// was violated; we cannot record the snapshot, so the run is a failure.
fn parse_backup_summary(stdout: &[u8]) -> Result<BackupSummary> {
    let lines = parse_json_lines(stdout);
    for line in lines {
        if line.get("message_type").and_then(Value::as_str) == Some("summary") {
            let snapshot_id = line
                .get("snapshot_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    VaultlineError::new(
                        ErrorKind::Operational,
                        "restic reported a backup summary without a snapshot_id — cannot record the snapshot".to_string(),
                    )
                })?;
            return Ok(BackupSummary {
                snapshot_id: snapshot_id.to_string(),
                files_new: line.get("files_new").and_then(Value::as_u64).unwrap_or(0),
                total_bytes_processed: line
                    .get("total_bytes_processed")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            });
        }
    }
    Err(VaultlineError::new(
        ErrorKind::Operational,
        "restic reported success without a summary line — cannot record the snapshot".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restic_019_missing_repo_is_classified() {
        // Empirically verified on restic 0.19.1 (2026-09-07).
        assert_eq!(
            classify_failure(
                10,
                b"Fatal: repository does not exist: unable to open config file\n"
            ),
            ResticFailure::RepoMissing
        );
    }

    #[test]
    fn old_restic_exit_codes_are_classified_by_message() {
        // The historical table (restic < 0.19): 3 missing, 10 lock,
        // 11 wrong password, 12 unknown command.
        assert_eq!(
            classify_failure(3, b"Fatal: repository does not exist\n"),
            ResticFailure::RepoMissing
        );
        assert_eq!(
            classify_failure(10, b"unable to create lock in backend: already locked\n"),
            ResticFailure::Locked
        );
        assert_eq!(
            classify_failure(11, b"Fatal: wrong password or no key found\n"),
            ResticFailure::WrongPassword
        );
        assert_eq!(
            classify_failure(12, b"unknown command \"nope\"\n"),
            ResticFailure::UnknownCommand
        );
    }

    #[test]
    fn restic_019_wrong_password_is_classified() {
        assert_eq!(
            classify_failure(12, b"Fatal: wrong password or no key found\n"),
            ResticFailure::WrongPassword
        );
    }

    #[test]
    fn unknown_exit_codes_are_generic_never_guessed() {
        assert_eq!(classify_failure(77, b""), ResticFailure::Generic);
        assert_eq!(
            classify_failure(1, b"Fatal: something broke\n"),
            ResticFailure::Generic
        );
        assert_eq!(classify_failure(130, b""), ResticFailure::Interrupted);
    }

    #[test]
    fn error_kinds_follow_the_exit_code_contract() {
        assert_eq!(
            ResticFailure::RepoMissing.into_error("PW", b"").exit_code(),
            1
        );
        assert_eq!(ResticFailure::Locked.into_error("PW", b"").exit_code(), 1);
        assert_eq!(
            ResticFailure::WrongPassword
                .into_error("PW", b"")
                .exit_code(),
            2
        );
        assert_eq!(
            ResticFailure::UnknownCommand
                .into_error("PW", b"")
                .exit_code(),
            1
        );
        assert_eq!(ResticFailure::Generic.into_error("PW", b"").exit_code(), 1);
    }

    #[test]
    fn wrong_password_error_names_the_environment_variable() {
        let err = ResticFailure::WrongPassword.into_error("RESTIC_PASSWORD_THORNWA", b"");
        assert!(err.to_string().contains("RESTIC_PASSWORD_THORNWA"));
    }

    #[test]
    fn lock_error_explains_retry() {
        let err = ResticFailure::Locked.into_error("PW", b"");
        assert!(err.to_string().contains("locked"));
    }

    #[test]
    fn stderr_tail_is_included_in_generic_failures() {
        let err =
            ResticFailure::Generic.into_error("PW", b"Fatal: unable to open repo\nMore detail\n");
        assert!(err.to_string().contains("unable to open repo"));
    }

    #[test]
    fn summary_line_is_extracted_tolerantly() {
        // A realistic stream: status lines, an unknown line, garbage, then
        // the summary. Garbage must be ignored; the summary must be found.
        let stdout = concat!(
            "{\"message_type\":\"status\",\"percent_done\":0.5}\n",
            "{\"message_type\":\"unknown_future_type\",\"x\":1}\n",
            "not json at all\n",
            "{\"message_type\":\"summary\",\"files_new\":3,\"total_bytes_processed\":42,\"snapshot_id\":\"abc123\"}\n",
        );
        let summary = parse_backup_summary(stdout.as_bytes()).expect("summary found");
        assert_eq!(summary.snapshot_id, "abc123");
        assert_eq!(summary.files_new, 3);
        assert_eq!(summary.total_bytes_processed, 42);
    }

    #[test]
    fn missing_summary_is_a_failure() {
        let stdout = b"{\"message_type\":\"status\",\"percent_done\":1.0}\n";
        let err = parse_backup_summary(stdout).expect_err("no summary");
        assert!(err.to_string().contains("summary"));
    }

    #[test]
    fn malformed_summary_is_a_failure() {
        let stdout = b"{\"message_type\":\"summary\",\"files_new\":1}\n";
        let err = parse_backup_summary(stdout).expect_err("no snapshot_id");
        assert!(err.to_string().contains("snapshot_id"));
    }
}
