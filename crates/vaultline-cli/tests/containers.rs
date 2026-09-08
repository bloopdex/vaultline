//! The container test harness — the infrastructure the database, SFTP, and
//! MinIO integration suites run on (docs/testing.md).
//!
//! These tests are `#[ignore]`d by default: they require a running Docker
//! engine and pull images. Run them explicitly:
//!
//! ```text
//! cargo test --test containers -- --ignored
//! ```
//!
//! What is proven here is the HARNESS itself (container start, port
//! mapping, reachability) — the per-backend interactions live in the
//! suites that consume it (tests/storage.rs, tests/databases.rs).

use std::net::TcpStream;
use std::time::Duration;

use testcontainers::runners::SyncRunner as _;

/// A PostgreSQL container comes up and accepts TCP connections on its mapped
/// port. This is the first consumer of the harness; the database-backup phase
/// (pg_dump orchestration) builds its integration suite on the same pattern.
#[test]
#[ignore = "requires Docker; run with `cargo test --test containers -- --ignored`"]
fn postgres_container_is_reachable() {
    let container = testcontainers_modules::postgres::Postgres::default();
    let node = container.start().expect("start postgres container");
    let port = node.get_host_port_ipv4(5432).expect("mapped port");

    let connected = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().expect("socket addr"),
        Duration::from_secs(10),
    );
    assert!(
        connected.is_ok(),
        "postgres container mapped port {port} not reachable: {:?}",
        connected.err()
    );
}
