//! The metrics channel (docs/observability.md): one structured stderr event
//! per named metric. Values are numbers or short strings; secrets never
//! appear here (the redaction contract).

use tracing::info;

/// Emit one named metric as a structured log event
/// (target `vaultline.metrics`).
pub fn emit(name: &str, value: serde_json::Value) {
    info!(target: "vaultline.metrics", metric = name, value = %value, "metric");
}

pub fn emit_u64(name: &str, value: u64) {
    emit(name, serde_json::Value::from(value));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract: emitting is just structured logging — no panics for any
    /// value shape.
    #[test]
    fn emit_accepts_numbers_and_strings() {
        emit_u64("backups_run", 1);
        emit("backup_status", serde_json::Value::String("ok".to_string()));
    }
}
