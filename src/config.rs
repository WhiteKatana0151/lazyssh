use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A saved SSH server entry. No secrets are stored, only a path to a key file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    pub name: String,
    pub description: String,
    pub host: String,
    pub username: Option<String>,
    pub identity_file: Option<String>,
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
            username: Some("root".to_string()),
            identity_file: Some("/home/user/.ssh/id_ed25519".to_string()),
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
    fn remove_out_of_bounds_is_noop() {
        let mut config = Config::default();
        config.add(sample_server("box1"));
        assert!(config.remove(5).is_none());
        assert_eq!(config.servers.len(), 1);
    }
}
