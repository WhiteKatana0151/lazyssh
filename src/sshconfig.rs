//! Read-only importer for OpenSSH client config files (`~/.ssh/config`).
//!
//! Only concrete `Host` aliases become entries: wildcard and negated
//! patterns (`*`, `?`, `!`) describe defaults rather than machines, and
//! `Match` blocks are conditional, so both are skipped. The parser never
//! writes the file and never follows `Include`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::Server;

/// Description stamped on imported entries so they are easy to find.
pub const IMPORTED_DESCRIPTION: &str = "imported from ssh config";

/// `~/.ssh/config`, if a home directory is known.
pub fn default_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".ssh").join("config"))
}

/// Reads and parses the config at `path`.
pub fn load(path: &Path) -> Result<Vec<Server>> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(parse(&text))
}

/// Settings collected for one `Host` block before it is turned into
/// servers, one per concrete alias on the `Host` line.
#[derive(Default)]
struct Block {
    aliases: Vec<String>,
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity: Option<String>,
    jump: Option<String>,
}

impl Block {
    fn finish(self, out: &mut Vec<Server>) {
        // A host or user starting with '-' would be read as an ssh option.
        let unsafe_arg = |v: &Option<String>| v.as_deref().is_some_and(|v| v.starts_with('-'));
        if unsafe_arg(&self.hostname) || unsafe_arg(&self.user) || unsafe_arg(&self.jump) {
            return;
        }
        for alias in self.aliases.iter().filter(|a| !a.starts_with('-')) {
            out.push(Server {
                name: alias.clone(),
                description: IMPORTED_DESCRIPTION.to_string(),
                host: self.hostname.clone().unwrap_or_else(|| alias.clone()),
                port: self.port,
                username: self.user.clone(),
                identity_file: self.identity.clone(),
                jump_host: self.jump.clone(),
                ..Default::default()
            });
        }
    }
}

/// Splits a config line into keyword and value. OpenSSH accepts both
/// `Key value` and `Key=value`, and a value may be double-quoted.
fn split_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let split = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let key = line[..split].to_ascii_lowercase();
    let value = line[split..]
        .trim_start_matches(|c: char| c.is_whitespace() || c == '=')
        .trim();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    Some((key, value.to_string()))
}

fn is_concrete(pattern: &str) -> bool {
    !pattern.is_empty() && !pattern.contains(['*', '?', '!'])
}

/// Parses config text into one server per concrete `Host` alias, in file
/// order. As in OpenSSH, the first value seen for a keyword wins.
pub fn parse(text: &str) -> Vec<Server> {
    let mut out = Vec::new();
    // `None` while outside any Host block, or inside a skipped Match block.
    let mut block: Option<Block> = None;

    for line in text.lines() {
        let Some((key, value)) = split_line(line) else {
            continue;
        };
        match key.as_str() {
            "host" => {
                if let Some(done) = block.take() {
                    done.finish(&mut out);
                }
                let aliases: Vec<String> = value
                    .split_whitespace()
                    .filter(|p| is_concrete(p))
                    .map(str::to_string)
                    .collect();
                block = Some(Block {
                    aliases,
                    ..Default::default()
                });
            }
            "match" => {
                if let Some(done) = block.take() {
                    done.finish(&mut out);
                }
            }
            _ => {
                let Some(current) = block.as_mut() else {
                    continue;
                };
                match key.as_str() {
                    "hostname" if current.hostname.is_none() => current.hostname = Some(value),
                    "user" if current.user.is_none() => current.user = Some(value),
                    "port" if current.port.is_none() => {
                        current.port = value.parse().ok().filter(|p| *p != 0)
                    }
                    "identityfile" if current.identity.is_none() => current.identity = Some(value),
                    "proxyjump"
                        if current.jump.is_none() && !value.eq_ignore_ascii_case("none") =>
                    {
                        current.jump = Some(value)
                    }
                    _ => {}
                }
            }
        }
    }
    if let Some(done) = block {
        done.finish(&mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# global defaults are not machines
Host *
    ServerAliveInterval 30
    User nobody

Host node-2 n2
    HostName 100.64.0.2
    User sam
    Port 2222
    IdentityFile ~/.ssh/id_ed25519
    IdentityFile ~/.ssh/ignored_second_key

host=gitea
  hostname = "gitea.example.ts.net"
  proxyjump node-2

Host *.internal !secret
    User ops

Match host foo
    User matched

Host bare
"#;

    #[test]
    fn imports_concrete_hosts_with_their_settings() {
        let servers = parse(SAMPLE);
        let names: Vec<_> = servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["node-2", "n2", "gitea", "bare"]);

        let node = &servers[0];
        assert_eq!(node.host, "100.64.0.2");
        assert_eq!(node.username.as_deref(), Some("sam"));
        assert_eq!(node.port, Some(2222));
        assert_eq!(node.identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
        assert_eq!(node.description, IMPORTED_DESCRIPTION);
        // Both aliases of one block share its settings.
        assert_eq!(servers[1].host, "100.64.0.2");

        let gitea = &servers[2];
        assert_eq!(gitea.host, "gitea.example.ts.net");
        assert_eq!(gitea.jump_host.as_deref(), Some("node-2"));
        assert_eq!(gitea.username, None, "Host * defaults are not copied");

        // A block with no HostName connects to the alias itself.
        assert_eq!(servers[3].host, "bare");
    }

    #[test]
    fn match_blocks_do_not_leak_into_the_previous_host() {
        let servers = parse("Host a\nMatch all\n  User leaked\n");
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].username, None);
    }

    #[test]
    fn option_lookalikes_are_never_imported() {
        let text = "Host evil\n HostName -oProxyCommand=x\nHost evil2\n User -x\n\
                    Host -dash ok\n HostName fine\n";
        let names: Vec<_> = parse(text).into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["ok"]);
    }

    #[test]
    fn junk_is_ignored() {
        assert!(parse("").is_empty());
        assert!(parse("User orphan\n# only comments\n").is_empty());
        let servers = parse("Host x\n  Port nope\n  ProxyJump none\n");
        assert_eq!(servers[0].port, None);
        assert_eq!(servers[0].jump_host, None);
    }

    #[test]
    fn load_reads_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        fs::write(&path, "Host a\n HostName 1.2.3.4\n").unwrap();
        assert_eq!(load(&path).unwrap()[0].host, "1.2.3.4");
        assert!(load(&dir.path().join("missing")).is_err());
    }
}
