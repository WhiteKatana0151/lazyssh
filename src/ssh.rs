use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::config::Server;

/// The fixed command run on the remote host during bootstrap. It appends
/// whatever arrives on stdin to `authorized_keys`; the public key travels
/// over that pipe, so nothing user-controlled is ever interpolated into a
/// shell string. The `tail` guard adds a newline first if the existing file
/// doesn't end with one, and `umask 077` gives fresh files/dirs safe modes.
const BOOTSTRAP_REMOTE_SCRIPT: &str = "exec sh -c 'umask 077; mkdir -p ~/.ssh && \
     { [ -z \"$(tail -c 1 ~/.ssh/authorized_keys 2>/dev/null)\" ] || \
     echo >> ~/.ssh/authorized_keys; } && cat >> ~/.ssh/authorized_keys'";

/// The common `ssh` invocation for `server`: identity, port, and extra args,
/// without the destination.
fn base_command(server: &Server) -> Command {
    let mut cmd = Command::new("ssh");

    if let Some(identity) = &server.identity_file {
        if !identity.is_empty() {
            cmd.arg("-i").arg(identity);
        }
    }

    if let Some(port) = server.port {
        cmd.arg("-p").arg(port.to_string());
    }

    if let Some(extra) = &server.extra_args {
        cmd.args(extra.split_whitespace());
    }

    cmd
}

/// The `[user@]host` ssh destination for `server`.
fn target(server: &Server) -> String {
    match &server.username {
        Some(user) if !user.is_empty() => format!("{}@{}", user, server.host),
        _ => server.host.clone(),
    }
}

/// Builds the `ssh` command that would be used to connect to `server`,
/// without actually running it. Kept separate from `connect` so the
/// argument construction can be tested.
pub fn build_command(server: &Server) -> Command {
    let mut cmd = base_command(server);
    cmd.arg(target(server));
    cmd
}

/// Builds the `ssh` command that installs the public key on `server`. The
/// remote script is a fixed string and the key is piped over stdin, so no
/// user input reaches a shell. stdout/stderr stay inherited so ssh can
/// prompt for the remote password on the terminal itself — LazySSH never
/// sees or handles that password.
pub fn build_bootstrap_command(server: &Server) -> Command {
    let mut cmd = base_command(server);
    cmd.arg(target(server));
    cmd.arg(BOOTSTRAP_REMOTE_SCRIPT);
    cmd.stdin(Stdio::piped());
    cmd
}

/// Expands a leading `~`/`~/` in `path` to the home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// Path of the public half of `identity_file`: the same path with `.pub`
/// appended, as `ssh-keygen` writes it.
pub fn public_key_path(identity_file: &str) -> PathBuf {
    let mut path = expand_tilde(identity_file).into_os_string();
    path.push(".pub");
    PathBuf::from(path)
}

/// Reads the public key next to the private key at `identity_file`.
pub fn read_public_key(identity_file: &str) -> Result<String> {
    let path = public_key_path(identity_file);
    if !path.exists() {
        bail!(
            "public key not found at {} (expected next to the private key; \
             generate one with ssh-keygen if needed)",
            path.display()
        );
    }
    let key =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let key = key.trim();
    if key.is_empty() {
        bail!("public key file {} is empty", path.display());
    }
    Ok(key.to_string())
}

/// Installs `server`'s public key on the remote host by piping it to the
/// fixed append script over ssh. Any password prompt comes from ssh itself;
/// LazySSH never collects or forwards credentials.
pub fn bootstrap(server: &Server) -> Result<()> {
    let identity = server
        .identity_file
        .as_deref()
        .context("bootstrap requires an SSH key path")?;
    let key = read_public_key(identity)?;

    let mut child = build_bootstrap_command(server)
        .spawn()
        .context("failed to start ssh (is it installed and on PATH?)")?;
    {
        let mut stdin = child.stdin.take().context("failed to open ssh stdin")?;
        writeln!(stdin, "{key}").context("failed to send public key to ssh")?;
        // Dropping stdin closes the pipe so the remote `cat` finishes.
    }
    let status = child.wait().context("failed to wait for ssh")?;
    if !status.success() {
        bail!("ssh exited with {status}");
    }
    Ok(())
}

/// Connects to `server`, handing control of the terminal to `ssh`.
///
/// On Unix this replaces the lazyssh process with ssh. On Windows there is no
/// direct `exec`, so it starts ssh and waits for it to exit.
#[cfg(unix)]
pub fn connect(server: &Server) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    Err(build_command(server).exec())
}

#[cfg(not(unix))]
pub fn connect(server: &Server) -> std::io::Result<()> {
    let status = build_command(server).status()?;
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_server() -> Server {
        Server {
            name: "prod".to_string(),
            description: "production box".to_string(),
            host: "example.com".to_string(),
            port: None,
            username: None,
            identity_file: None,
            extra_args: None,
            last_connected_at: None,
        }
    }

    fn args(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn host_only() {
        let server = base_server();
        let cmd = build_command(&server);
        assert_eq!(cmd.get_program(), "ssh");
        assert_eq!(args(&cmd), vec!["example.com"]);
    }

    #[test]
    fn with_username() {
        let mut server = base_server();
        server.username = Some("deploy".to_string());
        let cmd = build_command(&server);
        assert_eq!(args(&cmd), vec!["deploy@example.com"]);
    }

    #[test]
    fn with_identity_file() {
        let mut server = base_server();
        server.identity_file = Some("/home/user/.ssh/id_ed25519".to_string());
        let cmd = build_command(&server);
        assert_eq!(
            args(&cmd),
            vec!["-i", "/home/user/.ssh/id_ed25519", "example.com"]
        );
    }

    #[test]
    fn with_username_and_identity_file() {
        let mut server = base_server();
        server.username = Some("deploy".to_string());
        server.identity_file = Some("/home/user/.ssh/id_ed25519".to_string());
        let cmd = build_command(&server);
        assert_eq!(
            args(&cmd),
            vec!["-i", "/home/user/.ssh/id_ed25519", "deploy@example.com"]
        );
    }

    #[test]
    fn with_port() {
        let mut server = base_server();
        server.port = Some(2222);
        let cmd = build_command(&server);
        assert_eq!(args(&cmd), vec!["-p", "2222", "example.com"]);
    }

    #[test]
    fn with_extra_args() {
        let mut server = base_server();
        server.extra_args = Some("-A -o ServerAliveInterval=30".to_string());
        let cmd = build_command(&server);
        assert_eq!(
            args(&cmd),
            vec!["-A", "-o", "ServerAliveInterval=30", "example.com"]
        );
    }

    #[test]
    fn blank_username_and_identity_are_ignored() {
        let mut server = base_server();
        server.username = Some(String::new());
        server.identity_file = Some(String::new());
        let cmd = build_command(&server);
        assert_eq!(args(&cmd), vec!["example.com"]);
    }

    #[test]
    fn bootstrap_command_pipes_key_into_fixed_remote_script() {
        let mut server = base_server();
        server.username = Some("deploy".to_string());
        server.identity_file = Some("/home/user/.ssh/id_ed25519".to_string());
        server.port = Some(2222);

        let cmd = build_bootstrap_command(&server);
        assert_eq!(cmd.get_program(), "ssh");
        assert_eq!(
            args(&cmd),
            vec![
                "-i",
                "/home/user/.ssh/id_ed25519",
                "-p",
                "2222",
                "deploy@example.com",
                BOOTSTRAP_REMOTE_SCRIPT,
            ]
        );
    }

    #[test]
    fn bootstrap_remote_script_is_a_single_fixed_argument() {
        let server = base_server();
        let cmd = build_bootstrap_command(&server);
        // The remote script must arrive as one argv entry, never assembled
        // from user input.
        assert_eq!(
            args(&cmd).last().map(String::as_str),
            Some(BOOTSTRAP_REMOTE_SCRIPT)
        );
        assert!(BOOTSTRAP_REMOTE_SCRIPT.contains("cat >> ~/.ssh/authorized_keys"));
    }

    #[test]
    fn public_key_path_appends_pub_suffix() {
        assert_eq!(
            public_key_path("/home/user/.ssh/id_ed25519"),
            PathBuf::from("/home/user/.ssh/id_ed25519.pub")
        );
    }

    #[test]
    fn expand_tilde_resolves_home_prefix() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(
            expand_tilde("~/.ssh/id_ed25519"),
            home.join(".ssh/id_ed25519")
        );
        // Paths without the prefix pass through untouched.
        assert_eq!(expand_tilde("/etc/key"), PathBuf::from("/etc/key"));
        assert_eq!(expand_tilde("relative/key"), PathBuf::from("relative/key"));
    }

    #[test]
    fn read_public_key_reads_and_trims_the_pub_file() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("id_ed25519");
        fs::write(
            dir.path().join("id_ed25519.pub"),
            "ssh-ed25519 AAAAC3Nza key-comment\n",
        )
        .unwrap();

        let key = read_public_key(private.to_str().unwrap()).unwrap();
        assert_eq!(key, "ssh-ed25519 AAAAC3Nza key-comment");
    }

    #[test]
    fn read_public_key_fails_clearly_when_missing_or_empty() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("id_ed25519");

        let err = read_public_key(private.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("id_ed25519.pub"), "{err}");

        fs::write(dir.path().join("id_ed25519.pub"), "  \n").unwrap();
        let err = read_public_key(private.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
    }
}
