//! vaultline-core: the canonical, engine-agnostic application-recovery model
//! (ADR-V0-3) and its strongly typed TOML configuration layer.
//!
//! Dependency direction (SOT Section 10.3): this crate contains no engine
//! behavior and no process I/O beyond reading the configuration file — restic
//! orchestration (ADR-V0-2) is a CLI-layer concern, and the CLI crate depends
//! on this one, never the reverse.

pub mod config;
pub mod cron;
pub mod error;
pub mod model;
pub mod retention;
pub mod state;
