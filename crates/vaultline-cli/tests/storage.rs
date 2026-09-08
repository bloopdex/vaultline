//! SFTP backend end-to-end (the open proof from Phase 3): a real sshd
//! (a plain ubuntu server the test configures itself) with key-file
//! authentication THROUGH THE PRODUCT — the config declares `key_file`
//! and `known_hosts`, which the product passes to restic via its
//! `sftp.args` option (restic tokenizes the string and hands the tokens
//! to the native ssh client as argv — no shell executes anywhere,
//! ADR-002 amendment part 3). The proof runs AGENT-FREE: no ssh-agent,
//! no mutation of the user's real ~/.ssh/known_hosts — the test owns a
//! temporary keypair and known_hosts file in its tempdir. That is also
//! why the proof executes locally on Windows (the OpenSSH agent service
//! is not involved) and on the hosted ubuntu job alike. Container-gated.

use std::path::{Path, PathBuf};
use std::process::Command;

fn restic_bin() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("VAULTLINE_RESTIC_BIN") {
        let path = PathBuf::from(bin);
        return path.is_file().then_some(path);
    }
    let output = Command::new("restic").arg("version").output().ok()?;
    output.status.success().then(|| PathBuf::from("restic"))
}

fn docker_available() -> Option<()> {
    let output = Command::new("docker").arg("info").output().ok()?;
    output.status.success().then_some(())
}

fn vaultline() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("vaultline").expect("binary built by cargo")
}

/// The OpenSSH tool for this platform (Windows ships it under System32).
fn ssh_tool(name: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\Windows\System32\OpenSSH").join(format!("{name}.exe"))
    } else {
        PathBuf::from(name)
    }
}

#[test]
fn sftp_storage_backs_up_over_ssh() {
    if restic_bin().is_none() || docker_available().is_none() {
        eprintln!("skipping: restic or Docker not available (container-gated)");
        return;
    }
    use testcontainers::GenericImage;
    use testcontainers::core::{ImageExt, IntoContainerPort};
    use testcontainers::runners::SyncRunner as _;

    let dir = tempfile::tempdir().expect("tempdir");

    // The test keypair (owned by the test, passed to the product as
    // key_file — no agent involved).
    let key = dir.path().join("id_test");
    let keygen = Command::new(ssh_tool("ssh-keygen"))
        .args(["-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .arg("-q")
        .output()
        .expect("ssh-keygen runs");
    assert!(
        keygen.status.success(),
        "{}",
        String::from_utf8_lossy(&keygen.stderr)
    );
    let pubkey = std::fs::read_to_string(format!("{}.pub", key.display())).expect("pubkey");

    // The server is a plain ubuntu container the test configures
    // itself — no image entrypoint magic. (The history that decided
    // this: the atmoz/sftp image failed the hosted runs three
    // different ways — a 60s startup timeout on its first-boot
    // keygen, the runner's docker-proxy accepting TCP before sshd
    // existed, and finally an opaque connection close with EMPTY
    // server logs. A boring, deterministic server wins.)
    let image = GenericImage::new("ubuntu", "24.04")
        .with_exposed_port(22.tcp())
        // The image's default `bash` exits immediately without a TTY —
        // the container must stay up for the test to configure it.
        .with_cmd(vec!["tail", "-f", "/dev/null"])
        .with_copy_to("/keys/authorized_keys", pubkey.clone().into_bytes());
    let node = image.start().expect("start the server container");
    let port = node.get_host_port_ipv4(22).expect("mapped port");
    let exec = |args: &[&str]| -> std::process::Output {
        Command::new("docker")
            .arg("exec")
            .arg(node.id())
            .args(args)
            .output()
            .expect("docker exec runs")
    };

    // Configure sshd: the backup user with the test key, the host
    // keys, and a writable uploads directory for repositories.
    let setup = exec(&[
        "sh",
        "-c",
        "apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq openssh-server >/dev/null 2>&1 \
         && useradd -m vaultline-backup \
         && mkdir -p /home/vaultline-backup/.ssh /home/vaultline-backup/uploads \
         && cp /keys/authorized_keys /home/vaultline-backup/.ssh/authorized_keys \
         && chown -R vaultline-backup:vaultline-backup /home/vaultline-backup \
         && chmod 700 /home/vaultline-backup/.ssh \
         && chmod 600 /home/vaultline-backup/.ssh/authorized_keys \
         && ssh-keygen -A \
         && mkdir -p /run/sshd \
         && echo SERVER-CONFIGURED",
    ]);
    assert!(
        setup.status.success()
            && String::from_utf8_lossy(&setup.stdout).contains("SERVER-CONFIGURED"),
        "the server configuration failed: {} {}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr)
    );

    // Start sshd detached (the container's own bash stays PID 1).
    let sshd = exec(&[
        "sh",
        "-c",
        "nohup /usr/sbin/sshd -D -e >/var/log/sshd.log 2>&1 &",
    ]);
    assert!(
        sshd.status.success(),
        "cannot start sshd: {}",
        String::from_utf8_lossy(&sshd.stderr)
    );

    // Readiness: the sshd PROCESS, not the TCP port (the runner's
    // docker-proxy accepts connections before any listener exists).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    loop {
        let ready = exec(&["sh", "-c", "pgrep -x sshd >/dev/null"]);
        if ready.status.success() {
            break;
        }
        if std::time::Instant::now() > deadline {
            let log = exec(&["sh", "-c", "cat /var/log/sshd.log 2>/dev/null"]);
            panic!(
                "sshd never started in the server container: {}",
                String::from_utf8_lossy(&log.stdout)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }

    // The test owns its known_hosts file: the container's ephemeral
    // host key is registered there (declared to the product as
    // known_hosts — the user's real file is never touched).
    let host_key = Command::new("docker")
        .args([
            "exec",
            node.id(),
            "cat",
            "/etc/ssh/ssh_host_ed25519_key.pub",
        ])
        .output()
        .expect("docker exec");
    assert!(
        host_key.status.success(),
        "{}",
        String::from_utf8_lossy(&host_key.stderr)
    );
    let host_key = String::from_utf8_lossy(&host_key.stdout).trim().to_string();
    let known_hosts = dir.path().join("known_hosts");
    std::fs::write(&known_hosts, format!("[127.0.0.1]:{port} {host_key}\n")).expect("known_hosts");

    // The setup moved the test key from the queued keys dir into the
    // server's authorized_keys (chown + chmod 600). Asserting it here
    // turns any setup difference into a clear failure instead of a
    // connection reset later.
    let server_keys = Command::new("docker")
        .args([
            "exec",
            node.id(),
            "cat",
            "/home/vaultline-backup/.ssh/authorized_keys",
        ])
        .output()
        .expect("docker exec");
    let server_keys = String::from_utf8_lossy(&server_keys.stdout);
    assert!(
        server_keys.contains(pubkey.trim()),
        "the server's authorized_keys does not hold the test key: {server_keys}"
    );

    let uploads = dir.path().join("uploads");
    std::fs::create_dir_all(&uploads).expect("uploads");
    std::fs::write(uploads.join("payload.txt"), "over ssh").expect("payload");

    let config_path = dir.path().join("vaultline.toml");
    let toml_path = |p: &Path| p.display().to_string().replace('\\', "/");
    let config = format!(
        r#"
[application]
name = "thornwa"
[[application.sources.files]]
name = "uploads"
paths = ["{uploads}"]
[application.storage]
kind = "sftp"
host = "127.0.0.1"
port = {port}
user = "vaultline-backup"
# The repository directory on the server (created in the setup step,
# owned by the backup user).
path = "/home/vaultline-backup/uploads"
key_file = "{key}"
known_hosts = "{known_hosts}"
password_env = "VAULTLINE_TEST_PASSWORD"
[application.retention]
keep_last = 14
[application.verification]
level = 1
"#,
        uploads = toml_path(&uploads),
        key = toml_path(&key),
        known_hosts = toml_path(&known_hosts),
    );
    std::fs::write(&config_path, config).expect("config");

    // On failure, dump the server's own view before asserting — a
    // connection close during auth is otherwise opaque.
    let backup = vaultline()
        .args(["backup", "run", "--config"])
        .arg(&config_path)
        .env("VAULTLINE_TEST_PASSWORD", "test-password")
        .env("VAULTLINE_STATE_DIR", dir.path().join("state"))
        .output()
        .expect("vaultline runs");
    if !backup.status.success() {
        let logs = exec(&["sh", "-c", "cat /var/log/sshd.log 2>/dev/null"]);
        panic!(
            "backup failed: {}\n--- sshd log (tail):\n{}",
            String::from_utf8_lossy(&backup.stderr),
            String::from_utf8_lossy(&logs.stdout)
        );
    }
    assert!(
        String::from_utf8_lossy(&backup.stdout).contains("backup complete"),
        "{}",
        String::from_utf8_lossy(&backup.stdout)
    );

    // The repository genuinely lives on the sftp server: restic lists
    // the snapshot through the same sftp URL AND the same sftp.args the
    // product builds — the -o mechanism proven against the raw binary.
    let repo_url =
        format!("sftp://vaultline-backup@127.0.0.1:{port}//home/vaultline-backup/uploads/thornwa");
    let sftp_args = format!(
        "sftp.args=-o BatchMode=yes -i '{}' -o UserKnownHostsFile='{}'",
        toml_path(&key),
        toml_path(&known_hosts),
    );
    let ls = Command::new(restic_bin().expect("restic"))
        .args(["-o", &sftp_args, "-r", &repo_url, "--json", "snapshots"])
        .env("RESTIC_PASSWORD", "test-password")
        .output()
        .expect("restic runs");
    assert!(
        ls.status.success(),
        "{}",
        String::from_utf8_lossy(&ls.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&ls.stdout).expect("snapshot list parses");
    assert_eq!(payload.as_array().expect("array").len(), 1);
}
