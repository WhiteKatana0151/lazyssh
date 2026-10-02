//! Portable backups of the whole LazySSH profile (servers, tags, pins,
//! forwards, launcher) and restoring them.
//!
//! A backup is the config wrapped in a small envelope that names the format
//! and when it was taken. Restores also accept a bare `servers.json`, so an
//! old copy of the config file works too. Backups hold no secrets: like the
//! config, they contain key *paths* only.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{now_unix_secs, Config};

pub const FORMAT: &str = "lazyssh-backup";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    format: String,
    format_version: u32,
    app_version: String,
    exported_at: u64,
    config: Config,
}

/// Serializes `config` as a backup taken at `at` (Unix seconds).
pub fn to_json(config: &Config, at: u64) -> Result<String> {
    let envelope = Envelope {
        format: FORMAT.to_string(),
        format_version: FORMAT_VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        exported_at: at,
        config: config.clone(),
    };
    Ok(serde_json::to_string_pretty(&envelope)?)
}

/// Reads a backup. Returns `Ok(None)` when `text` is not LazySSH JSON at
/// all — e.g. an OpenSSH config — so callers can fall back to that import.
pub fn parse(text: &str) -> Result<Option<Config>> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Ok(None);
    };
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if object.get("format").and_then(|f| f.as_str()) == Some(FORMAT) {
        let envelope: Envelope =
            serde_json::from_value(value).context("backup file is damaged or incomplete")?;
        if envelope.format_version > FORMAT_VERSION {
            bail!(
                "backup format v{} is newer than this LazySSH supports (v{FORMAT_VERSION}); \
                 upgrade LazySSH to restore it",
                envelope.format_version
            );
        }
        return Ok(Some(envelope.config));
    }
    if object.get("servers").is_some_and(|s| s.is_array()) {
        let config: Config = serde_json::from_value(value).context("servers.json is damaged")?;
        return Ok(Some(config));
    }
    Ok(None)
}

/// What a restore changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    pub added: usize,
    /// Names skipped because a server with that name already exists.
    pub skipped: Vec<String>,
}

/// Merges `incoming` into `current` — new names are added with all their
/// metadata, existing names are left alone — or, with `replace`, makes
/// `current` an exact copy of the backup, launcher included.
pub fn restore(current: &mut Config, incoming: Config, replace: bool) -> Restored {
    if replace {
        let added = incoming.servers.len();
        *current = incoming;
        return Restored {
            added,
            skipped: Vec::new(),
        };
    }
    let mut skipped = Vec::new();
    let mut added = 0;
    for server in incoming.servers {
        if current.index_of_name(&server.name).is_some() {
            skipped.push(server.name);
        } else {
            current.add(server);
            added += 1;
        }
    }
    Restored { added, skipped }
}

/// `~/.config/lazyssh/backups`, beside the config file.
pub fn default_dir() -> Result<PathBuf> {
    let config = Config::default_path()?;
    Ok(config
        .parent()
        .context("config path has no parent directory")?
        .join("backups"))
}

/// `lazyssh-<label>-YYYYMMDD-HHMMSS.json` in UTC.
pub fn file_name(label: &str, at: u64) -> String {
    format!("lazyssh-{label}-{}.json", timestamp(at))
}

/// Writes a backup of `config` to `path`.
pub fn write(config: &Config, path: &Path) -> Result<()> {
    crate::config::write_atomic(path, to_json(config, now_unix_secs())?.as_bytes())
}

/// Writes a backup into [`default_dir`], returning its path.
pub fn write_default(config: &Config, label: &str) -> Result<PathBuf> {
    let path = default_dir()?.join(file_name(label, now_unix_secs()));
    write(config, &path)?;
    Ok(path)
}

/// Reads and parses a backup file, failing if it is not one.
pub fn load(path: &Path) -> Result<Config> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    parse(&text)?.with_context(|| format!("{} is not a LazySSH backup", path.display()))
}

/// `YYYYMMDD-HHMMSS` (UTC) for Unix seconds, without a date crate.
pub fn timestamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Howard Hinnant's days-since-epoch to proleptic Gregorian date.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Server, SshLauncher};

    fn server(name: &str) -> Server {
        Server {
            name: name.into(),
            host: format!("{name}.example"),
            ..Default::default()
        }
    }

    fn rich_config() -> Config {
        let mut a = server("node-2");
        a.tags = vec!["homelab".into()];
        a.pinned = true;
        a.forwards = vec!["L3000:localhost:3000".into()];
        a.last_connected_at = Some(1_700_000_000);
        let mut b = server("db");
        b.jump_host = Some("node-2".into());
        Config {
            servers: vec![a, b],
            launcher: SshLauncher::Kitty,
        }
    }

    #[test]
    fn backup_round_trips_every_field() {
        let config = rich_config();
        let json = to_json(&config, 42).unwrap();
        assert!(json.contains("\"format\": \"lazyssh-backup\""));
        assert!(json.contains("\"exported_at\": 42"));
        assert_eq!(parse(&json).unwrap(), Some(config));
    }

    #[test]
    fn bare_servers_json_is_accepted() {
        let config = rich_config();
        let raw = serde_json::to_string(&config).unwrap();
        assert_eq!(parse(&raw).unwrap(), Some(config));
    }

    #[test]
    fn non_backups_are_recognised_as_such() {
        for text in [
            "Host a\n  HostName b\n",
            "",
            "[1,2]",
            "{\"other\": 1}",
            "\"str\"",
        ] {
            assert_eq!(parse(text).unwrap(), None, "{text:?}");
        }
    }

    #[test]
    fn damaged_or_future_backups_fail_loudly() {
        let damaged = r#"{"format":"lazyssh-backup","format_version":1}"#;
        assert!(parse(damaged).is_err());
        let future = to_json(&rich_config(), 1)
            .unwrap()
            .replace("\"format_version\": 1", "\"format_version\": 99");
        let err = parse(&future).unwrap_err().to_string();
        assert!(err.contains("newer"), "{err}");
    }

    #[test]
    fn merge_adds_new_names_and_keeps_existing_entries() {
        let mut current = Config::default();
        let mut mine = server("NODE-2");
        mine.host = "keep-me".into();
        current.add(mine);

        let report = restore(&mut current, rich_config(), false);
        assert_eq!(report.added, 1);
        assert_eq!(report.skipped, ["node-2"]);
        assert_eq!(current.servers[0].host, "keep-me");
        assert_eq!(current.servers[1].jump_host.as_deref(), Some("node-2"));
        assert_eq!(current.launcher, SshLauncher::Auto, "merge keeps settings");
    }

    #[test]
    fn replace_takes_the_backup_wholesale() {
        let mut current = Config::default();
        current.add(server("gone"));
        let report = restore(&mut current, rich_config(), true);
        assert_eq!(report.added, 2);
        assert_eq!(current, rich_config());
    }

    #[test]
    fn timestamps_are_utc_and_sortable() {
        assert_eq!(timestamp(0), "19700101-000000");
        assert_eq!(timestamp(951_782_400), "20000229-000000");
        assert_eq!(timestamp(1_790_946_000), "20261002-130000");
        assert_eq!(
            file_name("backup", 1_790_946_000),
            "lazyssh-backup-20261002-130000.json"
        );
    }

    #[test]
    fn write_and_load_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/backup.json");
        write(&rich_config(), &path).unwrap();
        assert_eq!(load(&path).unwrap(), rich_config());

        let not_backup = dir.path().join("cfg");
        fs::write(&not_backup, "Host a\n").unwrap();
        assert!(load(&not_backup).is_err());
    }
}
