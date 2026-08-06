use std::process::Command;

use crate::config::Server;

/// Builds the `ssh` command that would be used to connect to `server`,
/// without actually running it. Kept separate from `connect` so the
/// argument construction can be tested.
pub fn build_command(server: &Server) -> Command {
    let mut cmd = Command::new("ssh");

    if let Some(identity) = &server.identity_file {
        if !identity.is_empty() {
            cmd.arg("-i").arg(identity);
        }
    }

    let target = match &server.username {
        Some(user) if !user.is_empty() => format!("{}@{}", user, server.host),
        _ => server.host.clone(),
    };
    cmd.arg(target);

    cmd
}

/// Replaces the current process with `ssh`, handing full control of the
/// terminal to it. On success this never returns.
#[cfg(unix)]
pub fn connect(server: &Server) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    build_command(server).exec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_server() -> Server {
        Server {
            name: "prod".to_string(),
            description: "production box".to_string(),
            host: "example.com".to_string(),
            username: None,
            identity_file: None,
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
    fn blank_username_and_identity_are_ignored() {
        let mut server = base_server();
        server.username = Some(String::new());
        server.identity_file = Some(String::new());
        let cmd = build_command(&server);
        assert_eq!(args(&cmd), vec!["example.com"]);
    }
}
