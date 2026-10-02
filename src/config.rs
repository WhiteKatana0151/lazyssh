use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A saved SSH server entry. No secrets are stored, only a path to a key file.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    pub name: String,
    pub description: String,
    pub host: String,
    /// `serde(default)` keeps configs written before this field existed loading.
    #[serde(default)]
    pub port: Option<u16>,
    pub username: Option<String>,
    pub identity_file: Option<String>,
    /// Extra arguments passed to `ssh` verbatim, split on whitespace.
    #[serde(default)]
    pub extra_args: Option<String>,
    /// Unix timestamp (seconds) of the most recent connection. `None` means
    /// never connected; `serde(default)` keeps older configs loading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_connected_at: Option<u64>,
    /// Free-form labels such as `homelab` or `prod`, searchable with `/#tag`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Pinned servers always sort above the recency-ranked rest.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// ProxyJump hop: either the name of another saved server, which is
    /// resolved to its `[user@]host[:port]` at connect time, or a literal
    /// `-J` destination such as `bastion@jump.example.com:2222`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump_host: Option<String>,
    /// Saved port forwards in `L8080:localhost:80`, `R9000:localhost:9000`,
    /// or `D1080` form; started and stopped from the forwards dialog.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forwards: Vec<String>,
}

/// Longest ProxyJump chain followed when resolving saved-server references,
/// which also stops reference cycles.
const MAX_JUMP_DEPTH: usize = 8;

impl Server {
    /// `[user@]host[:port]`, the destination syntax `ssh -J` accepts.
    pub fn jump_destination(&self) -> String {
        let mut spec = match &self.username {
            Some(user) if !user.is_empty() => format!("{user}@{}", self.host),
            _ => self.host.clone(),
        };
        if let Some(port) = self.port {
            spec.push_str(&format!(":{port}"));
        }
        spec
    }

    /// Port used to reach the server directly.
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(22)
    }

    /// Whether any tag starts with `prefix`, ignoring case.
    pub fn has_tag_prefix(&self, prefix: &str) -> bool {
        let prefix = prefix.to_lowercase();
        self.tags
            .iter()
            .any(|tag| tag.to_lowercase().starts_with(&prefix))
    }
}

/// Current Unix time in seconds, or 0 if the clock is before the epoch.
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Which program takes over the terminal when connecting interactively.
///
/// `Auto` is the safe default: it only reaches for Kitty when the app is
/// actually running inside Kitty *and* a Kitty launcher exists, otherwise it
/// falls back to plain OpenSSH.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SshLauncher {
    #[default]
    Auto,
    #[serde(rename = "openssh")]
    OpenSsh,
    Kitty,
}

impl SshLauncher {
    pub const ALL: [SshLauncher; 3] = [SshLauncher::Auto, SshLauncher::OpenSsh, SshLauncher::Kitty];

    pub fn label(self) -> &'static str {
        match self {
            SshLauncher::Auto => "Auto",
            SshLauncher::OpenSsh => "OpenSSH",
            SshLauncher::Kitty => "Kitty",
        }
    }

    /// One-line explanation shown next to each choice in the settings modal.
    pub fn description(self) -> &'static str {
        match self {
            SshLauncher::Auto => "Use Kitty when running in Kitty, else OpenSSH",
            SshLauncher::OpenSsh => "Always run plain `ssh`",
            SshLauncher::Kitty => "Always run `kitten ssh` (needs Kitty installed)",
        }
    }

    /// Index of this variant in [`SshLauncher::ALL`], used by the settings
    /// modal to drive keyboard navigation.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|l| *l == self).unwrap_or(0)
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or_default()
    }

    /// The next choice in [`SshLauncher::ALL`], clamped at the end like the
    /// rest of the app's list navigation.
    pub fn next(self) -> Self {
        Self::from_index((self.index() + 1).min(Self::ALL.len() - 1))
    }

    /// The previous choice, clamped at the start.
    pub fn prev(self) -> Self {
        Self::from_index(self.index().saturating_sub(1))
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub servers: Vec<Server>,
    /// `serde(default)` keeps configs written before this field existed
    /// loading, as [`SshLauncher::Auto`].
    #[serde(default)]
    pub launcher: SshLauncher,
}

impl Config {
    /// Returns `~/.config/lazyssh/servers.json` (or the platform equivalent).
    pub fn default_path() -> Result<PathBuf> {
        let dir = dirs::config_dir().context("could not determine config directory")?;
        Ok(dir.join("lazyssh").join("servers.json"))
    }

    /// Loads the config from the default path, returning an empty config if it
    /// doesn't exist yet.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::default_path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let data = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Config = serde_json::from_str(&data)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(config)
    }

    /// Saves the config to the default path, creating parent directories as needed.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::default_path()?)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let data = serde_json::to_string_pretty(self)?;
        fs::write(path, data).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    pub fn add(&mut self, server: Server) {
        self.servers.push(server);
    }

    /// Replaces the server at `index`, returning `false` if out of bounds.
    pub fn update(&mut self, index: usize, server: Server) -> bool {
        match self.servers.get_mut(index) {
            Some(slot) => {
                *slot = server;
                true
            }
            None => false,
        }
    }

    /// Replaces the server at `index` while keeping its connection history
    /// and pin, so editing an entry never resets how it ranks.
    pub fn update_preserving_recency(&mut self, index: usize, mut server: Server) -> bool {
        if let Some(existing) = self.servers.get(index) {
            server.last_connected_at = existing.last_connected_at;
            server.pinned = existing.pinned;
        }
        self.update(index, server)
    }

    /// Stamps the server at `index` as connected at `at` (Unix seconds),
    /// returning `false` if out of bounds.
    pub fn mark_connected(&mut self, index: usize, at: u64) -> bool {
        match self.servers.get_mut(index) {
            Some(server) => {
                server.last_connected_at = Some(at);
                true
            }
            None => false,
        }
    }

    /// Orders pinned servers first, then most recently connected first.
    /// Never-connected servers follow the connected ones within each group,
    /// keeping their existing relative order (the sort is stable).
    pub fn sort_by_recency(&mut self) {
        self.servers.sort_by_key(|server| {
            (
                std::cmp::Reverse(server.pinned),
                std::cmp::Reverse(server.last_connected_at),
            )
        });
    }

    /// Flips the pin on the server at `index`, returning the new state.
    pub fn toggle_pin(&mut self, index: usize) -> Option<bool> {
        let server = self.servers.get_mut(index)?;
        server.pinned = !server.pinned;
        Some(server.pinned)
    }

    /// Exact (case-insensitive) name lookup.
    pub fn index_of_name(&self, name: &str) -> Option<usize> {
        self.servers
            .iter()
            .position(|server| server.name.eq_ignore_ascii_case(name))
    }

    /// A copy of `server` ready to hand to ssh: its jump host, if it names
    /// saved servers, is replaced by the resolved `-J` chain.
    pub fn resolved(&self, server: &Server) -> Result<Server> {
        let mut out = server.clone();
        out.jump_host = self.resolve_jump(server)?;
        Ok(out)
    }

    /// Adds every server whose name is not already taken (ignoring case),
    /// returning how many were added. Names are also deduplicated within
    /// `incoming` itself, first one wins.
    pub fn import(&mut self, incoming: Vec<Server>) -> usize {
        let mut added = 0;
        for server in incoming {
            if self.index_of_name(&server.name).is_none() {
                self.add(server);
                added += 1;
            }
        }
        added
    }

    /// Resolves `server`'s jump host into a `-J` argument. A value naming a
    /// saved server becomes that server's destination, following its own jump
    /// host first so chains become `hop1,hop2`; anything else is passed on
    /// verbatim. Cycles and overly deep chains are an error.
    pub fn resolve_jump(&self, server: &Server) -> Result<Option<String>> {
        let mut hops = Vec::new();
        let mut current = match server.jump_host.as_deref().map(str::trim) {
            Some(jump) if !jump.is_empty() => jump.to_string(),
            _ => return Ok(None),
        };
        let mut seen = vec![server.name.to_lowercase()];
        loop {
            let Some(index) = self.index_of_name(&current) else {
                hops.push(current);
                break;
            };
            let hop = &self.servers[index];
            let key = hop.name.to_lowercase();
            if seen.contains(&key) || hops.len() >= MAX_JUMP_DEPTH {
                anyhow::bail!(
                    "jump host chain through `{}` loops or is too deep",
                    hop.name
                );
            }
            seen.push(key);
            hops.push(hop.jump_destination());
            match hop.jump_host.as_deref().map(str::trim) {
                Some(next) if !next.is_empty() => current = next.to_string(),
                _ => break,
            }
        }
        // `-J a,b` connects through a first: the outermost hop is the one
        // found last while walking inward from the target.
        hops.reverse();
        Ok(Some(hops.join(",")))
    }

    pub fn remove(&mut self, index: usize) -> Option<Server> {
        if index < self.servers.len() {
            Some(self.servers.remove(index))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_server(name: &str) -> Server {
        Server {
            name: name.to_string(),
            description: "test server".to_string(),
            host: "example.com".to_string(),
            port: Some(2222),
            username: Some("root".to_string()),
            identity_file: Some("/home/user/.ssh/id_ed25519".to_string()),
            extra_args: Some("-o ServerAliveInterval=30".to_string()),
            ..Default::default()
        }
    }

    fn named(name: &str, host: &str) -> Server {
        Server {
            name: name.into(),
            host: host.into(),
            ..Default::default()
        }
    }

    #[test]
    fn new_fields_round_trip_and_stay_out_of_old_shape_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.json");

        let mut config = Config::default();
        config.add(sample_server("plain"));
        let mut rich = sample_server("rich");
        rich.tags = vec!["prod".into(), "eu".into()];
        rich.pinned = true;
        rich.jump_host = Some("bastion".into());
        rich.forwards = vec!["L8080:localhost:80".into()];
        config.add(rich);
        config.save_to(&path).unwrap();

        assert_eq!(Config::load_from(&path).unwrap(), config);
        let raw = fs::read_to_string(&path).unwrap();
        // Defaults are skipped, so untouched entries keep their old shape.
        assert_eq!(raw.matches("\"pinned\"").count(), 1);
        assert_eq!(raw.matches("\"tags\"").count(), 1);
    }

    #[test]
    fn pinned_sort_above_recency() {
        let mut config = Config::default();
        for name in ["a", "b", "c", "d"] {
            config.add(sample_server(name));
        }
        config.mark_connected(0, 300);
        config.mark_connected(1, 100);
        assert_eq!(config.toggle_pin(2), Some(true));
        config.sort_by_recency();
        let names: Vec<_> = config.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["c", "a", "b", "d"]);
        assert_eq!(config.toggle_pin(0), Some(false));
        assert_eq!(config.toggle_pin(9), None);
    }

    #[test]
    fn jump_resolves_saved_names_into_chains_and_passes_literals_through() {
        let mut config = Config::default();
        let mut outer = named("edge", "edge.example.com");
        outer.username = Some("ops".into());
        outer.port = Some(2222);
        let mut inner = named("bastion", "10.0.0.1");
        inner.jump_host = Some("EDGE".into());
        let mut target = named("db", "10.0.0.5");
        target.jump_host = Some("bastion".into());
        config.add(outer);
        config.add(inner);
        config.add(target.clone());

        assert_eq!(
            config.resolve_jump(&target).unwrap().as_deref(),
            Some("ops@edge.example.com:2222,10.0.0.1")
        );

        target.jump_host = Some("me@literal:22".into());
        assert_eq!(
            config.resolve_jump(&target).unwrap().as_deref(),
            Some("me@literal:22")
        );

        target.jump_host = Some("   ".into());
        assert_eq!(config.resolve_jump(&target).unwrap(), None);
    }

    #[test]
    fn import_skips_taken_names_case_insensitively() {
        let mut config = Config::default();
        config.add(named("Node-2", "old"));
        let added = config.import(vec![
            named("node-2", "new"),
            named("gitea", "g"),
            named("GITEA", "dupe"),
        ]);
        assert_eq!(added, 1);
        let hosts: Vec<_> = config.servers.iter().map(|s| s.host.as_str()).collect();
        assert_eq!(hosts, ["old", "g"]);
    }

    #[test]
    fn edits_keep_the_pin() {
        let mut config = Config::default();
        config.add(named("a", "h"));
        config.toggle_pin(0);
        config.update_preserving_recency(0, named("a2", "h"));
        assert!(config.servers[0].pinned);
    }

    #[test]
    fn resolved_rewrites_only_the_jump_host() {
        let mut config = Config::default();
        config.add(named("bastion", "b.example"));
        let mut target = named("db", "10.0.0.5");
        target.jump_host = Some("bastion".into());
        let resolved = config.resolved(&target).unwrap();
        assert_eq!(resolved.jump_host.as_deref(), Some("b.example"));
        assert_eq!(resolved.host, target.host);
    }

    #[test]
    fn jump_cycles_are_rejected() {
        let mut config = Config::default();
        let mut a = named("a", "a.example");
        a.jump_host = Some("b".into());
        let mut b = named("b", "b.example");
        b.jump_host = Some("a".into());
        config.add(a.clone());
        config.add(b);
        assert!(config.resolve_jump(&a).is_err());

        let mut selfish = named("self", "s.example");
        selfish.jump_host = Some("self".into());
        config.add(selfish.clone());
        assert!(config.resolve_jump(&selfish).is_err());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("servers.json");

        let mut config = Config::default();
        config.add(sample_server("box1"));
        config.add(sample_server("box2"));
        config.save_to(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.json");

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded, Config::default());
    }

    #[test]
    fn add_and_remove() {
        let mut config = Config::default();
        config.add(sample_server("box1"));
        config.add(sample_server("box2"));
        assert_eq!(config.servers.len(), 2);

        let removed = config.remove(0).unwrap();
        assert_eq!(removed.name, "box1");
        assert_eq!(config.servers.len(), 1);
        assert_eq!(config.servers[0].name, "box2");
    }

    #[test]
    fn old_configs_without_new_fields_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.json");
        fs::write(
            &path,
            r#"{"servers":[{"name":"box1","description":"","host":"example.com","username":null,"identity_file":null}]}"#,
        )
        .unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.servers.len(), 1);
        assert_eq!(loaded.servers[0].port, None);
        assert_eq!(loaded.servers[0].extra_args, None);
        assert_eq!(loaded.servers[0].last_connected_at, None);
    }

    #[test]
    fn launcher_defaults_to_auto() {
        assert_eq!(Config::default().launcher, SshLauncher::Auto);
        assert_eq!(SshLauncher::default(), SshLauncher::Auto);
    }

    #[test]
    fn old_configs_without_a_launcher_load_as_auto() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.json");
        fs::write(
            &path,
            r#"{"servers":[{"name":"box1","description":"","host":"example.com","username":null,"identity_file":null}]}"#,
        )
        .unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.launcher, SshLauncher::Auto);
    }

    #[test]
    fn launcher_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.json");

        for launcher in [SshLauncher::Auto, SshLauncher::OpenSsh, SshLauncher::Kitty] {
            let config = Config {
                servers: vec![sample_server("box1")],
                launcher,
            };
            config.save_to(&path).unwrap();
            assert_eq!(Config::load_from(&path).unwrap(), config);
        }
    }

    #[test]
    fn launcher_labels_are_stable() {
        assert_eq!(SshLauncher::ALL.len(), 3);
        assert_eq!(SshLauncher::Auto.label(), "Auto");
        assert_eq!(SshLauncher::OpenSsh.label(), "OpenSSH");
        assert_eq!(SshLauncher::Kitty.label(), "Kitty");
    }

    #[test]
    fn recency_survives_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.json");

        let mut config = Config::default();
        config.add(sample_server("box1"));
        config.mark_connected(0, 1_700_000_000);
        config.save_to(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.servers[0].last_connected_at, Some(1_700_000_000));
    }

    #[test]
    fn mark_connected_stamps_only_in_bounds() {
        let mut config = Config::default();
        config.add(sample_server("box1"));

        assert!(config.mark_connected(0, 1700));
        assert_eq!(config.servers[0].last_connected_at, Some(1700));
        assert!(!config.mark_connected(5, 1700));
    }

    #[test]
    fn sort_by_recency_puts_recent_first_and_never_last() {
        let mut config = Config::default();
        for name in ["a", "b", "c", "d"] {
            config.add(sample_server(name));
        }
        config.mark_connected(1, 100);
        config.mark_connected(3, 200);

        config.sort_by_recency();
        let names: Vec<_> = config.servers.iter().map(|s| s.name.as_str()).collect();
        // Most recent first; never-connected keep their relative order.
        assert_eq!(names, ["d", "b", "a", "c"]);
    }

    #[test]
    fn update_preserving_recency_keeps_timestamp() {
        let mut config = Config::default();
        config.add(sample_server("box1"));
        config.mark_connected(0, 1234);

        assert!(config.update_preserving_recency(0, sample_server("box1-renamed")));
        assert_eq!(config.servers[0].name, "box1-renamed");
        assert_eq!(config.servers[0].last_connected_at, Some(1234));

        assert!(!config.update_preserving_recency(5, sample_server("nope")));
    }

    #[test]
    fn update_replaces_in_place() {
        let mut config = Config::default();
        config.add(sample_server("box1"));

        let mut replacement = sample_server("box1-renamed");
        replacement.port = Some(2200);
        assert!(config.update(0, replacement));
        assert_eq!(config.servers[0].name, "box1-renamed");
        assert_eq!(config.servers[0].port, Some(2200));

        assert!(!config.update(5, sample_server("nope")));
    }

    #[test]
    fn remove_out_of_bounds_is_noop() {
        let mut config = Config::default();
        config.add(sample_server("box1"));
        assert!(config.remove(5).is_none());
        assert_eq!(config.servers.len(), 1);
    }
}
