use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::config::{Server, SshLauncher};

/// The fixed command run on the remote host during bootstrap. It appends
/// whatever arrives on stdin to `authorized_keys`; the public key travels
/// over that pipe, so nothing user-controlled is ever interpolated into a
/// shell string. The `tail` guard adds a newline first if the existing file
/// doesn't end with one, and `umask 077` gives fresh files/dirs safe modes.
const BOOTSTRAP_REMOTE_SCRIPT: &str = "exec sh -c 'umask 077; mkdir -p ~/.ssh && \
     { [ -z \"$(tail -c 1 ~/.ssh/authorized_keys 2>/dev/null)\" ] || \
     echo >> ~/.ssh/authorized_keys; } && cat >> ~/.ssh/authorized_keys'";

/// A concrete program to hand the terminal to, once the preference in the
/// config has been reconciled with what is actually installed and running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launcher {
    /// Plain `ssh`.
    OpenSsh,
    /// Standalone `kitten ssh`.
    Kitten(PathBuf),
    /// `kitty +kitten ssh`, for installs that ship no standalone `kitten`.
    KittyKitten(PathBuf),
}

/// The environment facts that decide whether we are running inside Kitty.
/// Captured as plain data so the decision below stays pure and testable —
/// tests build one directly instead of mutating the process environment,
/// which is global state shared by every test thread.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LauncherEnv {
    pub term: Option<OsString>,
    pub kitty_window_id: Option<OsString>,
}

impl LauncherEnv {
    pub fn from_env() -> Self {
        Self {
            term: std::env::var_os("TERM"),
            kitty_window_id: std::env::var_os("KITTY_WINDOW_ID"),
        }
    }

    /// Whether this environment identifies a direct Kitty terminal. An
    /// explicit non-Kitty `TERM` wins over `KITTY_WINDOW_ID`, which can remain
    /// set inside tmux or screen even when Kitty passthrough is unavailable.
    pub fn is_kitty(&self) -> bool {
        match self.term.as_deref() {
            Some(term) if term == OsStr::new("xterm-kitty") => true,
            Some(_) => false,
            None => self
                .kitty_window_id
                .as_deref()
                .is_some_and(|id| !id.is_empty()),
        }
    }
}

/// The Kitty launcher to use, if one is installed. Prefers the standalone
/// `kitten` binary and falls back to `kitty +kitten`.
fn kitty_launcher(find: &impl Fn(&str) -> Option<PathBuf>) -> Option<Launcher> {
    if let Some(path) = find("kitten") {
        Some(Launcher::Kitten(path))
    } else {
        find("kitty").map(Launcher::KittyKitten)
    }
}

/// Reconciles the configured preference with the environment and what is
/// installed. `find` returns the exact path of a discovered program so the
/// command does not search `PATH` again after resolution.
///
/// `Auto` never takes a risk: it uses Kitty only when the terminal really is
/// Kitty *and* a launcher exists, so a config copied to another machine keeps
/// working. `Kitty` is an explicit choice, so a missing launcher is an error
/// rather than a silent downgrade.
pub fn resolve_launcher(
    preference: SshLauncher,
    env: &LauncherEnv,
    find: impl Fn(&str) -> Option<PathBuf>,
) -> Result<Launcher> {
    match preference {
        SshLauncher::OpenSsh => Ok(Launcher::OpenSsh),
        SshLauncher::Auto => {
            if env.is_kitty() {
                Ok(kitty_launcher(&find).unwrap_or(Launcher::OpenSsh))
            } else {
                Ok(Launcher::OpenSsh)
            }
        }
        SshLauncher::Kitty => kitty_launcher(&find).context(
            "launcher is set to Kitty but neither `kitten` nor `kitty` is on PATH \
             (install Kitty, or switch the launcher to Auto or OpenSSH with `s`)",
        ),
    }
}

/// [`resolve_launcher`] against this process's real environment and `PATH`.
pub fn resolve_launcher_from_env(preference: SshLauncher) -> Result<Launcher> {
    resolve_launcher(preference, &LauncherEnv::from_env(), find_executable)
}

/// Extensions tried on Windows when `PATHEXT` is unset.
#[cfg(windows)]
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// Whether `path` is a file this process could execute.
#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Looks `name` up in a `PATH`-style variable. Both the search path and the
/// Windows `PATHEXT` list are arguments rather than reads of the process
/// environment, so tests can exercise discovery against a temp directory
/// without mutating global state that other threads share.
fn find_executable_in(
    name: &str,
    path_var: Option<&OsStr>,
    #[cfg_attr(not(windows), allow(unused_variables))] pathext: Option<&OsStr>,
) -> Option<PathBuf> {
    let path_var = path_var?;
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exts = pathext
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| DEFAULT_PATHEXT.to_string());
            for ext in exts.split(';').filter(|e| !e.is_empty()) {
                let candidate = dir.join(format!("{name}{ext}"));
                if is_executable_file(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Looks `name` up on this process's `PATH`.
pub fn find_executable(name: &str) -> Option<PathBuf> {
    find_executable_in(
        name,
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("PATHEXT").as_deref(),
    )
}

/// Appends the ssh options for `server` — identity, port, extra args — to
/// `cmd`, without the destination. Shared by every launcher so the arguments
/// never depend on which program ends up running ssh.
fn push_ssh_options(cmd: &mut Command, server: &Server) {
    if let Some(identity) = &server.identity_file {
        if !identity.is_empty() {
            cmd.arg("-i").arg(identity);
        }
    }

    if let Some(port) = server.port {
        cmd.arg("-p").arg(port.to_string());
    }

    if let Some(jump) = jump_arg(server) {
        cmd.arg("-J").arg(jump);
    }

    if let Some(extra) = &server.extra_args {
        cmd.args(extra.split_whitespace());
    }
}

/// The `-J` value for `server`. Callers hand in a server from
/// [`crate::config::Config::resolved`], whose jump host has already been
/// turned from a saved-server name into a real `[user@]host[:port]` chain.
fn jump_arg(server: &Server) -> Option<&str> {
    server
        .jump_host
        .as_deref()
        .map(str::trim)
        .filter(|jump| !jump.is_empty())
}

/// How to open a session with a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// An interactive shell through the configured launcher.
    Ssh,
    /// An interactive `sftp` session.
    Sftp,
    /// `mosh`, tunnelling its bootstrap through ssh with the same options.
    Mosh,
}

impl LaunchMode {
    pub const ALL: [LaunchMode; 3] = [LaunchMode::Ssh, LaunchMode::Sftp, LaunchMode::Mosh];

    pub fn label(self) -> &'static str {
        match self {
            LaunchMode::Ssh => "SSH",
            LaunchMode::Sftp => "SFTP",
            LaunchMode::Mosh => "Mosh",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            LaunchMode::Ssh => "Interactive shell via the configured launcher",
            LaunchMode::Sftp => "File transfer session (sftp)",
            LaunchMode::Mosh => "Roaming, latency-tolerant shell (mosh)",
        }
    }

    /// The program this mode needs on `PATH`.
    pub fn program(self) -> &'static str {
        match self {
            LaunchMode::Ssh => "ssh",
            LaunchMode::Sftp => "sftp",
            LaunchMode::Mosh => "mosh",
        }
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(i + 1).min(Self::ALL.len() - 1)]
    }

    pub fn prev(self) -> Self {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[i.saturating_sub(1)]
    }
}

/// Only `-o Key=value` options survive from extra args when the command is
/// not `ssh` itself: flags such as `-A` or `-t` mean something else (or
/// nothing) to `sftp`, while `-o` is understood by both.
fn ssh_o_options(extra: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let mut tokens = extra.unwrap_or_default().split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "-o" {
            if let Some(value) = tokens.next() {
                out.push("-o".to_string());
                out.push(value.to_string());
            }
        } else if token.starts_with("-o") {
            out.push(token.to_string());
        }
    }
    out
}

/// `sftp` to `server`. sftp spells the port flag `-P`.
pub fn build_sftp_command(server: &Server) -> Command {
    let mut cmd = Command::new("sftp");
    if let Some(identity) = server.identity_file.as_deref().filter(|i| !i.is_empty()) {
        cmd.arg("-i").arg(identity);
    }
    if let Some(port) = server.port {
        cmd.arg("-P").arg(port.to_string());
    }
    if let Some(jump) = jump_arg(server) {
        cmd.arg("-J").arg(jump);
    }
    cmd.args(ssh_o_options(server.extra_args.as_deref()));
    cmd.arg(target(server));
    cmd
}

/// `mosh` to `server`. mosh parses `--ssh` with shell-style word
/// splitting, so every ssh argument is single-quoted into that one value;
/// no shell is spawned by LazySSH itself.
pub fn build_mosh_command(server: &Server) -> Command {
    let mut cmd = Command::new("mosh");
    let ssh = base_command(server);
    let options: Vec<String> = ssh
        .get_args()
        .map(|arg| shell_quote(&arg.to_string_lossy()))
        .collect();
    if !options.is_empty() {
        cmd.arg(format!("--ssh=ssh {}", options.join(" ")));
    }
    // Ends option parsing so the destination is never read as a flag.
    cmd.arg("--");
    cmd.arg(target(server));
    cmd
}

/// The interactive command for `mode`.
pub fn build_launch_command(server: &Server, mode: LaunchMode, launcher: Launcher) -> Command {
    match mode {
        LaunchMode::Ssh => build_interactive_command(server, launcher),
        LaunchMode::Sftp => build_sftp_command(server),
        LaunchMode::Mosh => build_mosh_command(server),
    }
}

/// POSIX single-quoting, leaving obviously safe words bare for readability.
pub fn shell_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@=,+%~".contains(c));
    if safe {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// A copy-pasteable shell rendering of `cmd`.
pub fn command_line(cmd: &Command) -> String {
    std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|part| shell_quote(&part.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A validated, saved port forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardSpec {
    /// `'L'`, `'R'`, or `'D'`.
    pub kind: char,
    /// What follows the flag, e.g. `8080:localhost:80` or `1080`.
    pub spec: String,
}

impl ForwardSpec {
    /// Parses `L8080:localhost:80`, `-L 8080:localhost:80`, `r9000:host:22`,
    /// or `D1080` into a forward.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        let body = raw.strip_prefix('-').unwrap_or(raw);
        let mut chars = body.chars();
        let kind = chars
            .next()
            .map(|c| c.to_ascii_uppercase())
            .filter(|c| matches!(c, 'L' | 'R' | 'D'))
            .ok_or_else(|| format!("Forward `{raw}` must start with L, R, or D"))?;
        let spec = chars.as_str().trim();
        let invalid = || format!("Invalid forward `{raw}`");
        if spec.is_empty() || spec.starts_with('-') || spec.contains(char::is_whitespace) {
            return Err(invalid());
        }
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.iter().any(|part| part.is_empty()) {
            return Err(invalid());
        }
        // The listen port is the last part for D and the second-to-last
        // part is the remote port for L/R; checking every numeric slot keeps
        // obvious typos out without reimplementing ssh's full grammar.
        let ok = match kind {
            'D' => parts.len() <= 2 && parts.last().unwrap().parse::<u16>().is_ok(),
            _ => {
                (3..=4).contains(&parts.len())
                    && parts[parts.len() - 3].parse::<u16>().is_ok()
                    && parts[parts.len() - 1].parse::<u16>().is_ok()
            }
        };
        if !ok {
            return Err(invalid());
        }
        Ok(Self {
            kind,
            spec: spec.to_string(),
        })
    }

    /// The canonical saved form, e.g. `L8080:localhost:80`.
    pub fn canonical(&self) -> String {
        format!("{}{}", self.kind, self.spec)
    }
}

/// A background, non-interactive ssh holding `forward` open. `BatchMode`
/// guarantees ssh never prompts (which would scribble over the TUI), so
/// forwards need key or agent authentication; `ExitOnForwardFailure` makes
/// a busy local port an error instead of a silently useless session.
pub fn build_forward_command(server: &Server, forward: &ForwardSpec) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args([
        "-N",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "BatchMode=yes",
    ]);
    push_ssh_options(&mut cmd, server);
    cmd.arg(format!("-{}", forward.kind)).arg(&forward.spec);
    cmd.arg(target(server));
    cmd
}

/// The common `ssh` invocation for `server`: identity, port, and extra args,
/// without the destination.
fn base_command(server: &Server) -> Command {
    let mut cmd = Command::new("ssh");
    push_ssh_options(&mut cmd, server);
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

/// Builds the command that hands the terminal to `launcher` for an
/// interactive session with `server`.
///
/// Every launcher runs the same ssh options and destination after its own
/// fixed prefix, so switching launchers never changes how the connection is
/// made. Nothing is passed through a shell and no environment variable is
/// set — notably not `TERM`, which Kitty's own `kitten ssh` handles by
/// shipping its terminfo to the remote host.
pub fn build_interactive_command(server: &Server, launcher: Launcher) -> Command {
    let mut cmd = match launcher {
        // Plain ssh is exactly the command used everywhere else.
        Launcher::OpenSsh => return build_command(server),
        Launcher::Kitten(path) => {
            let mut cmd = Command::new(path);
            cmd.arg("ssh");
            cmd
        }
        Launcher::KittyKitten(path) => {
            let mut cmd = Command::new(path);
            cmd.args(["+kitten", "ssh"]);
            cmd
        }
    };
    push_ssh_options(&mut cmd, server);
    cmd.arg(target(server));
    cmd
}

/// Builds the `ssh` command that installs the public key on `server`. The
/// remote script is a fixed string and the key is piped over stdin, so no
/// user input reaches a shell. Key installation always runs over native
/// `ssh`, whatever launcher is configured for interactive sessions: it is a
/// non-interactive pipe, and terminal integration has nothing to add to it.
/// stdout/stderr stay inherited so ssh can
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

/// Hands the terminal to `cmd` and returns its exit code.
///
/// With `replace` on Unix the lazyssh process becomes `cmd` via `exec`, so
/// this only returns on failure. Otherwise — on Windows, or when lazyssh
/// must outlive the session to tear down port forwards — it runs `cmd` as a
/// child and waits.
pub fn run_interactive(mut cmd: Command, replace: bool) -> std::io::Result<i32> {
    #[cfg(unix)]
    if replace {
        use std::os::unix::process::CommandExt;
        return Err(cmd.exec());
    }
    let _ = replace;
    let status = cmd.status()?;
    Ok(status.code().unwrap_or(1))
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
            ..Default::default()
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

    /// Creates an executable file at `path` (mode 755 on unix, `.exe` name
    /// on windows), so discovery can be exercised against a real directory.
    fn make_executable(dir: &std::path::Path, name: &str) -> PathBuf {
        let path = if cfg!(windows) {
            dir.join(format!("{name}.exe"))
        } else {
            dir.join(name)
        };
        fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn path_var(dirs: &[&std::path::Path]) -> std::ffi::OsString {
        std::env::join_paths(dirs.iter().map(|d| d.to_path_buf())).unwrap()
    }

    #[test]
    fn find_executable_in_scans_path_entries_in_order() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let wanted = make_executable(first.path(), "kitten");
        make_executable(second.path(), "kitten");

        let found = find_executable_in(
            "kitten",
            Some(&path_var(&[first.path(), second.path()])),
            None,
        );
        assert_eq!(found, Some(wanted));
    }

    #[test]
    fn find_executable_in_skips_missing_dirs_and_finds_later_entries() {
        let missing = tempfile::tempdir().unwrap();
        let real = tempfile::tempdir().unwrap();
        let wanted = make_executable(real.path(), "kitty");
        let missing_path = missing.path().join("nope");

        let found = find_executable_in(
            "kitty",
            Some(&path_var(&[&missing_path, real.path()])),
            None,
        );
        assert_eq!(found, Some(wanted));
    }

    #[test]
    #[cfg(unix)]
    fn find_executable_in_ignores_files_without_the_exec_bit() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("kitten"), "not executable").unwrap();

        assert_eq!(
            find_executable_in("kitten", Some(&path_var(&[dir.path()])), None),
            None
        );
    }

    #[test]
    fn find_executable_in_returns_none_without_a_path_or_a_match() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_executable_in("kitten", None, None), None);
        assert_eq!(
            find_executable_in("kitten", Some(&path_var(&[dir.path()])), None),
            None
        );
    }

    fn kitty_env() -> LauncherEnv {
        LauncherEnv {
            term: Some("xterm-kitty".into()),
            kitty_window_id: None,
        }
    }

    fn plain_env() -> LauncherEnv {
        LauncherEnv {
            term: Some("xterm-256color".into()),
            kitty_window_id: None,
        }
    }

    /// A discovery probe that returns a stable concrete path for listed programs.
    fn only(available: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        move |name: &str| available.contains(&name).then(|| PathBuf::from(name))
    }

    #[test]
    fn kitty_env_is_detected_by_term_or_window_id_without_explicit_non_kitty_term() {
        assert!(kitty_env().is_kitty());
        assert!(LauncherEnv {
            term: None,
            kitty_window_id: Some("1".into()),
        }
        .is_kitty());
        assert!(!plain_env().is_kitty());
        // KITTY_WINDOW_ID can leak through tmux/screen, where passthrough may fail.
        assert!(!LauncherEnv {
            term: Some("tmux-256color".into()),
            kitty_window_id: Some("1".into()),
        }
        .is_kitty());
        assert!(!LauncherEnv::default().is_kitty());
        // An empty KITTY_WINDOW_ID is not a Kitty session.
        assert!(!LauncherEnv {
            term: None,
            kitty_window_id: Some(OsString::new()),
        }
        .is_kitty());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_term_is_present_and_blocks_kitty_window_id_fallback() {
        use std::os::unix::ffi::OsStringExt;

        let env = LauncherEnv {
            term: Some(std::ffi::OsString::from_vec(vec![0xff])),
            kitty_window_id: Some("1".into()),
        };

        assert!(!env.is_kitty());
    }

    #[test]
    fn auto_picks_kitty_only_inside_kitty_with_a_launcher() {
        assert_eq!(
            resolve_launcher(SshLauncher::Auto, &kitty_env(), only(&["kitten", "kitty"])).unwrap(),
            Launcher::Kitten(PathBuf::from("kitten"))
        );
        // Standalone `kitten` missing: fall back to `kitty +kitten ssh`.
        assert_eq!(
            resolve_launcher(SshLauncher::Auto, &kitty_env(), only(&["kitty"])).unwrap(),
            Launcher::KittyKitten(PathBuf::from("kitty"))
        );
    }

    #[test]
    fn auto_falls_back_to_openssh_when_kitty_is_absent() {
        // Inside Kitty, but no launcher on PATH.
        assert_eq!(
            resolve_launcher(SshLauncher::Auto, &kitty_env(), only(&[])).unwrap(),
            Launcher::OpenSsh
        );
        // Launcher present, but not running inside Kitty.
        assert_eq!(
            resolve_launcher(SshLauncher::Auto, &plain_env(), only(&["kitten", "kitty"])).unwrap(),
            Launcher::OpenSsh
        );
    }

    #[test]
    fn forced_openssh_ignores_the_environment() {
        assert_eq!(
            resolve_launcher(
                SshLauncher::OpenSsh,
                &kitty_env(),
                only(&["kitten", "kitty"])
            )
            .unwrap(),
            Launcher::OpenSsh
        );
    }

    #[test]
    fn forced_kitty_prefers_standalone_kitten_then_falls_back() {
        assert_eq!(
            resolve_launcher(SshLauncher::Kitty, &plain_env(), only(&["kitten", "kitty"])).unwrap(),
            Launcher::Kitten(PathBuf::from("kitten"))
        );
        assert_eq!(
            resolve_launcher(SshLauncher::Kitty, &plain_env(), only(&["kitty"])).unwrap(),
            Launcher::KittyKitten(PathBuf::from("kitty"))
        );
    }

    #[test]
    fn forced_kitty_errors_instead_of_silently_using_openssh() {
        let err = resolve_launcher(SshLauncher::Kitty, &kitty_env(), only(&[])).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("kitten"), "{message}");
        assert!(message.contains("kitty"), "{message}");
        assert!(
            message.contains("Auto") || message.contains("OpenSSH"),
            "error should point at a way out: {message}"
        );
    }

    #[test]
    fn resolved_kitten_path_is_the_program_used_by_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let wanted = make_executable(dir.path(), "kitten");
        let launcher = resolve_launcher(SshLauncher::Kitty, &plain_env(), |name| {
            (name == "kitten").then(|| wanted.clone())
        })
        .unwrap();

        let cmd = build_interactive_command(&base_server(), launcher);

        assert_eq!(cmd.get_program(), wanted.as_os_str());
    }

    #[test]
    fn resolved_kitty_fallback_path_is_the_program_used_by_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let wanted = make_executable(dir.path(), "kitty");
        let launcher = resolve_launcher(SshLauncher::Auto, &kitty_env(), |name| {
            (name == "kitty").then(|| wanted.clone())
        })
        .unwrap();

        let cmd = build_interactive_command(&base_server(), launcher);

        assert_eq!(cmd.get_program(), wanted.as_os_str());
        assert_eq!(args(&cmd)[..2], ["+kitten", "ssh"]);
    }

    #[test]
    fn jump_host_becomes_a_single_j_argument() {
        let mut server = base_server();
        server.jump_host = Some(" ops@edge:2222,10.0.0.1 ".into());
        assert_eq!(
            args(&build_command(&server)),
            ["-J", "ops@edge:2222,10.0.0.1", "example.com"]
        );
        server.jump_host = Some("  ".into());
        assert_eq!(args(&build_command(&server)), ["example.com"]);
    }

    #[test]
    fn sftp_uses_capital_p_and_keeps_only_o_options() {
        let mut server = full_server();
        server.jump_host = Some("bastion".into());
        server.extra_args = Some("-A -o ServerAliveInterval=30 -oCompression=yes -t".into());
        let cmd = build_sftp_command(&server);
        assert_eq!(cmd.get_program(), "sftp");
        assert_eq!(
            args(&cmd),
            [
                "-i",
                "/home/user/.ssh/id_ed25519",
                "-P",
                "2222",
                "-J",
                "bastion",
                "-o",
                "ServerAliveInterval=30",
                "-oCompression=yes",
                "deploy@example.com"
            ]
        );
    }

    #[test]
    fn mosh_quotes_ssh_options_into_one_argument() {
        let mut server = full_server();
        server.identity_file = Some("/keys/my key".into());
        let cmd = build_mosh_command(&server);
        assert_eq!(cmd.get_program(), "mosh");
        assert_eq!(
            args(&cmd),
            [
                "--ssh=ssh -i '/keys/my key' -p 2222 -A -o ServerAliveInterval=30",
                "--",
                "deploy@example.com"
            ]
        );
        let bare = build_mosh_command(&base_server());
        assert_eq!(args(&bare), ["--", "example.com"]);
    }

    #[test]
    fn launch_mode_picks_the_matching_builder() {
        let server = base_server();
        for (mode, program) in [
            (LaunchMode::Ssh, "ssh"),
            (LaunchMode::Sftp, "sftp"),
            (LaunchMode::Mosh, "mosh"),
        ] {
            let cmd = build_launch_command(&server, mode, Launcher::OpenSsh);
            assert_eq!(cmd.get_program(), program);
            assert_eq!(mode.program(), program);
        }
        assert_eq!(LaunchMode::Ssh.prev(), LaunchMode::Ssh);
        assert_eq!(LaunchMode::Mosh.next(), LaunchMode::Mosh);
        assert_eq!(LaunchMode::Ssh.next().next(), LaunchMode::Mosh);
    }

    #[test]
    fn command_line_is_copy_pasteable() {
        let mut server = full_server();
        server.identity_file = Some("~/.ssh/it's".into());
        assert_eq!(
            command_line(&build_command(&server)),
            "ssh -i '~/.ssh/it'\\''s' -p 2222 -A -o ServerAliveInterval=30 deploy@example.com"
        );
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("$(rm)"), "'$(rm)'");
    }

    #[test]
    fn forward_specs_parse_and_canonicalize() {
        for (raw, kind, spec) in [
            ("L8080:localhost:80", 'L', "8080:localhost:80"),
            ("-L 8080:localhost:80", 'L', "8080:localhost:80"),
            ("l127.0.0.1:8080:db:5432", 'L', "127.0.0.1:8080:db:5432"),
            ("R9000:localhost:9000", 'R', "9000:localhost:9000"),
            ("D1080", 'D', "1080"),
            ("-D 127.0.0.1:1080", 'D', "127.0.0.1:1080"),
        ] {
            let parsed = ForwardSpec::parse(raw).unwrap();
            assert_eq!((parsed.kind, parsed.spec.as_str()), (kind, spec), "{raw}");
        }
        assert_eq!(
            ForwardSpec::parse("-L 8080:localhost:80")
                .unwrap()
                .canonical(),
            "L8080:localhost:80"
        );
        for bad in [
            "",
            "X8080",
            "L",
            "L8080",
            "Lfoo:localhost:80",
            "L8080:localhost:http",
            "L8080::80",
            "D-oProxyCommand=x",
            "L8080:a b:80",
            "Dnope",
            "D1:2:3",
        ] {
            assert!(ForwardSpec::parse(bad).is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn forward_command_is_batch_mode_and_fails_on_busy_ports() {
        let mut server = full_server();
        server.jump_host = Some("bastion".into());
        let forward = ForwardSpec::parse("L8080:localhost:80").unwrap();
        let cmd = build_forward_command(&server, &forward);
        let got = args(&cmd);
        assert_eq!(cmd.get_program(), "ssh");
        assert_eq!(
            got[..5],
            [
                "-N",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "BatchMode=yes"
            ]
        );
        assert!(got.windows(2).any(|w| w == ["-J", "bastion"]));
        assert_eq!(
            got[got.len() - 3..],
            ["-L", "8080:localhost:80", "deploy@example.com"]
        );
    }

    fn full_server() -> Server {
        let mut server = base_server();
        server.username = Some("deploy".to_string());
        server.identity_file = Some("/home/user/.ssh/id_ed25519".to_string());
        server.port = Some(2222);
        server.extra_args = Some("-A -o ServerAliveInterval=30".to_string());
        server
    }

    /// The ssh options and destination, in the order every launcher must
    /// preserve after its own prefix.
    const SSH_TAIL: [&str; 8] = [
        "-i",
        "/home/user/.ssh/id_ed25519",
        "-p",
        "2222",
        "-A",
        "-o",
        "ServerAliveInterval=30",
        "deploy@example.com",
    ];

    #[test]
    fn interactive_command_preserves_argv_after_each_launcher_prefix() {
        let server = full_server();

        let cmd = build_interactive_command(&server, Launcher::OpenSsh);
        assert_eq!(cmd.get_program(), "ssh");
        assert_eq!(args(&cmd), SSH_TAIL);

        let cmd = build_interactive_command(&server, Launcher::Kitten(PathBuf::from("kitten")));
        assert_eq!(cmd.get_program(), "kitten");
        let expected: Vec<&str> = std::iter::once("ssh").chain(SSH_TAIL).collect();
        assert_eq!(args(&cmd), expected);

        let cmd = build_interactive_command(&server, Launcher::KittyKitten(PathBuf::from("kitty")));
        assert_eq!(cmd.get_program(), "kitty");
        let expected: Vec<&str> = ["+kitten", "ssh"].into_iter().chain(SSH_TAIL).collect();
        assert_eq!(args(&cmd), expected);
    }

    #[test]
    fn openssh_launcher_is_the_plain_ssh_command() {
        let server = full_server();
        let plain = build_command(&server);
        let launched = build_interactive_command(&server, Launcher::OpenSsh);
        assert_eq!(plain.get_program(), launched.get_program());
        assert_eq!(args(&plain), args(&launched));
    }

    #[test]
    fn bootstrap_stays_on_native_openssh() {
        let mut server = full_server();
        server.extra_args = None;
        // Key installation runs over plain ssh no matter which launcher the
        // user picked for interactive sessions.
        let cmd = build_bootstrap_command(&server);
        assert_eq!(cmd.get_program(), "ssh");
        assert_eq!(
            args(&cmd).last().map(String::as_str),
            Some(BOOTSTRAP_REMOTE_SCRIPT)
        );
    }

    #[test]
    fn commands_never_set_environment_variables() {
        let server = full_server();
        // Notably TERM: kitty ships its own terminfo, and overriding TERM
        // here would break the remote session rather than fix it.
        for cmd in [
            build_command(&server),
            build_bootstrap_command(&server),
            build_interactive_command(&server, Launcher::Kitten(PathBuf::from("kitten"))),
            build_interactive_command(&server, Launcher::KittyKitten(PathBuf::from("kitty"))),
        ] {
            assert_eq!(cmd.get_envs().count(), 0);
        }
    }

    #[test]
    fn launchers_never_invoke_a_shell() {
        let server = full_server();
        for launcher in [
            Launcher::OpenSsh,
            Launcher::Kitten(PathBuf::from("kitten")),
            Launcher::KittyKitten(PathBuf::from("kitty")),
        ] {
            let cmd = build_interactive_command(&server, launcher);
            let program = cmd.get_program().to_string_lossy().to_string();
            assert!(
                ["ssh", "kitten", "kitty"].contains(&program.as_str()),
                "unexpected program {program:?}"
            );
        }
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
