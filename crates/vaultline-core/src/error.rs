//! The single error type crossing the CLI boundary, with the documented
//! exit-code contract (docs/cli.md):
//!
//! | code | family | meaning |
//! |---|---|---|
//! | 0 | success | the command completed |
//! | 1 | operational | an operation started but could not complete (I/O, engine, network, internal invariant) |
//! | 2 | usage/config | the invocation or the configuration is wrong — fix it and retry |
//!
//! Diagnostics explain, they do not just fail (SOT Section 10.8): validation
//! failures are aggregated into [`ValidationError`] lists, never reported
//! first-error-only.

use std::fmt;

/// The failure family; determines the process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Configuration is missing, unparsable, or fails validation.
    Config,
    /// The command line was used incorrectly.
    Usage,
    /// A required resource (file, directory) could not be read or written.
    Io,
    /// An operation started but could not complete (engine, network).
    Operational,
    /// A feature that exists in the model but is not implemented yet.
    Unsupported,
    /// An invariant was violated — a bug, not an input problem.
    Internal,
}

/// The error type crossing the CLI boundary. Carries the exit-code family, a
/// human message, and an optional underlying source error.
#[derive(Debug)]
pub struct VaultlineError {
    pub kind: ErrorKind,
    pub message: String,
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl VaultlineError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    pub fn with_source(
        kind: ErrorKind,
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    /// The documented exit-code contract.
    pub fn exit_code(&self) -> i32 {
        match self.kind {
            ErrorKind::Io | ErrorKind::Operational | ErrorKind::Internal => 1,
            ErrorKind::Config | ErrorKind::Usage | ErrorKind::Unsupported => 2,
        }
    }
}

impl fmt::Display for VaultlineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(source) = &self.source {
            write!(f, ": {source}")?;
        }
        Ok(())
    }
}

impl std::error::Error for VaultlineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|s| s as &(dyn std::error::Error + 'static))
    }
}

/// One aggregated validation failure, carrying the configuration path that
/// caused it (e.g. `application.name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

impl ValidationError {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// A non-blocking diagnostic: the configuration is valid, but something is
/// worth the operator's attention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub message: String,
}

impl Warning {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

pub type Result<T> = std::result::Result<T, VaultlineError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;
    use std::io;

    #[test]
    fn exit_codes_follow_the_contract() {
        assert_eq!(VaultlineError::new(ErrorKind::Config, "bad").exit_code(), 2);
        assert_eq!(VaultlineError::new(ErrorKind::Usage, "bad").exit_code(), 2);
        assert_eq!(
            VaultlineError::new(ErrorKind::Unsupported, "bad").exit_code(),
            2
        );
        assert_eq!(VaultlineError::new(ErrorKind::Io, "bad").exit_code(), 1);
        assert_eq!(
            VaultlineError::new(ErrorKind::Operational, "bad").exit_code(),
            1
        );
        assert_eq!(
            VaultlineError::new(ErrorKind::Internal, "bad").exit_code(),
            1
        );
    }

    #[test]
    fn display_includes_source_chain() {
        let err = VaultlineError::with_source(
            ErrorKind::Io,
            "cannot read configuration",
            io::Error::new(io::ErrorKind::NotFound, "no such file"),
        );
        assert_eq!(err.to_string(), "cannot read configuration: no such file");
        assert!(err.source().is_some());
    }

    #[test]
    fn validation_errors_format_with_path() {
        let err = ValidationError::new("application.name", "must be lowercase");
        assert_eq!(err.to_string(), "application.name: must be lowercase");
    }
}
