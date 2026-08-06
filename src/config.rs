use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A saved SSH server entry. No secrets are stored, only a path to a key file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// Current Unix time in seconds, or 0 if the clock is before the epoch.
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub servers: Vec<Server>,
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

    /// Replaces the server at `index` while keeping its connection history,
    /// so editing an entry never resets its recency.
    pub fn update_preserving_recency(&mut self, index: usize, mut server: Server) -> bool {
        if let Some(existing) = self.servers.get(index) {
            server.last_connected_at = existing.last_connected_at;
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

    /// Orders servers most recently connected first. Never-connected servers
    /// follow the connected ones, keeping their existing relative order.
    pub fn sort_by_recency(&mut self) {
        self.servers
            .sort_by_key(|server| std::cmp::Reverse(server.last_connected_at));
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
            last_connected_at: None,
        }
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
