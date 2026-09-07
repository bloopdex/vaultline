//! SFTP backend end-to-end (the open proof from Phase 3): a real sshd
//! (atmoz/sftp) with key authentication through the SSH agent, the real
//! restic sftp backend, and the real known-hosts verification path.
//! Container-gated; the test manages one temporary line in the user's
//! known_hosts (appended for the container's ephemeral host key, removed
//! on completion — restic verifies host keys, and the repo configuration
//! cannot inject a private known-hosts file).

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

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

/// The known_hosts file restic will consult (Go's os.UserHomeDir: the
/// real profile on Windows, $HOME elsewhere).
fn known_hosts_path() -> PathBuf {
    let home = if cfg!(windows) {
        PathBuf::from(std::env::var_os("USERPROFILE").expect("USERPROFILE"))
    } else {
        PathBuf::from(std::env::var_os("HOME").expect("HOME"))
    };
    home.join(".ssh").join("known_hosts")
}

/// One temporary line in the real known_hosts, removed on drop.
struct KnownHostsLine {
    line: String,
}

impl KnownHostsLine {
    fn append(entry: &str) -> Option<Self> {
        let path = known_hosts_path();
        std::fs::create_dir_all(path.parent().expect("parent")).ok()?;
        let mut contents = std::fs::read_to_string(&path).unwrap_or_default();
        let line = format!("{entry}\n");
        contents.push_str(&line);
        std::fs::write(&path, contents).ok()?;
        Some(Self { line })
    }
}

impl Drop for KnownHostsLine {
    fn drop(&mut self) {
        let path = known_hosts_path();
        if let Ok(contents) = std::fs::read_to_string(&path) {
            let cleaned: String = contents
                .lines()
                .filter(|&l| l != self.line.trim_end())
                .collect::<Vec<_>>()
                .join("\n");
            let _ = std::fs::write(&path, format!("{cleaned}\n"));
        }
    }
}

/// An SSH agent the test controls: the ssh-agent service/process plus the
/// test key loaded into it. Killed on drop.
struct TestAgent {
    _child: Option<Child>,
}

impl TestAgent {
    fn start() -> Option<Self> {
        if cfg!(unix) {
            // ssh-agent -s prints the socket on stdout.
            let output = Command::new(ssh_tool("ssh-agent"))
                .arg("-s")
                .output()
                .ok()?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let sock = stdout
                .split_whitespace()
                .find(|token| token.contains("SSH_AUTH_SOCK"))
                .and_then(|token| token.split(';').next())
                .and_then(|assignment| assignment.split('=').nth(1))?;
            // SAFETY: single-threaded test process at setup time; no other
            // thread reads the environment concurrently.
            unsafe { std::env::set_var("SSH_AUTH_SOCK", sock) };
            Some(Self { _child: None })
        } else {
            // Windows OpenSSH: ssh-agent runs as a console process holding
            // the named pipe \\.\pipe\openssh-ssh-agent, which ssh-add and
            // (if supported) restic discover without SSH_AUTH_SOCK.
            let child = Command::new(ssh_tool("ssh-agent")).spawn().ok()?;
            std::thread::sleep(std::time::Duration::from_millis(500));
            Some(Self {
                _child: Some(child),
            })
        }
    }
}

impl Drop for TestAgent {
    fn drop(&mut self) {
        if let Some(mut child) = self._child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
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

    let Some(agent) = TestAgent::start() else {
        eprintln!("skipping: could not start an SSH agent for the test key");
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");

    // The test keypair.
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

    // Load the key into the agent.
    let add = match Command::new(ssh_tool("ssh-add")).arg(&key).output() {
        Ok(output) => output,
        Err(_) => {
            eprintln!("skipping: ssh-add unavailable");
            return;
        }
    };
    if !add.status.success() {
        eprintln!(
            "skipping: no usable SSH agent on this host ({}) — on Windows, enable the OpenSSH agent service first",
            String::from_utf8_lossy(&add.stderr).trim()
        );
        return;
    }

    // The sshd container: user `backup`, chrooted home, key auth. No
    // The server is a plain ubuntu container the test configures
    // itself — no image entrypoint magic. (The history that decided
    // this: the atmoz/sftp image failed the hosted runs three
    // different ways — a 60s startup timeout on its first-boot
    // keygen, the runner's docker-proxy accepting TCP before sshd
    // existed, and finally an opaque connection close with EMPTY
    // server logs. A boring, deterministic server wins.)
    let image = GenericImage::new("ubuntu", "24.04")
        .with_exposed_port(22.tcp())
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
         && useradd -m backup \
         && mkdir -p /home/backup/.ssh /home/backup/uploads \
         && cp /keys/authorized_keys /home/backup/.ssh/authorized_keys \
         && chown -R backup:backup /home/backup \
         && chmod 700 /home/backup/.ssh \
         && chmod 600 /home/backup/.ssh/authorized_keys \
         && ssh-keygen -A \
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

    // restic verifies host keys: register the container's ephemeral key
    // (added temporarily; removed on drop).
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
    let Some(_hosts_line) = KnownHostsLine::append(&format!("[127.0.0.1]:{port} {host_key}"))
    else {
        eprintln!("skipping: cannot write the known_hosts file");
        return;
    };

    // The image's entrypoint must have moved the test key from the
    // queued keys dir into the server's authorized_keys (chown + chmod
    // 600). Asserting it here turns any setup difference into a clear
    // failure instead of a connection reset later.
    let server_keys = Command::new("docker")
        .args([
            "exec",
            node.id(),
            "cat",
            "/home/backup/.ssh/authorized_keys",
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
user = "backup"
# The repository directory on the server (created in the setup step,
# owned by the backup user).
path = "/home/backup/uploads"
password_env = "VAULTLINE_TEST_PASSWORD"
[application.retention]
keep_last = 14
[application.verification]
level = 1
"#,
        uploads = toml_path(&uploads),
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

    // The repository genuinely lives on the sftp server: restic lists the
    // snapshot through the same sftp URL the product builds.
    let repo_url = format!("sftp://backup@127.0.0.1:{port}//home/backup/uploads/thornwa");
    let ls = Command::new(restic_bin().expect("restic"))
        .args(["-r", &repo_url, "--json", "snapshots"])
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

    drop(agent);
}
