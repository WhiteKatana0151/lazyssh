//! Application state and key handling. Rendering lives in [`crate::ui`];
//! persistence in [`crate::config`]; the ssh handoff in [`crate::ssh`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::config::{Config, Server, SshLauncher};
use crate::forwards::{ForwardKey, Forwards};
use crate::probe::{probe_key, Prober, Reach};
use crate::ssh::{ForwardSpec, LaunchMode, TerminfoScope};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Description,
    Host,
    Port,
    Username,
    IdentityFile,
    JumpHost,
    ExtraArgs,
    Tags,
    Forwards,
}

impl Field {
    pub const ALL: [Field; 10] = [
        Field::Name,
        Field::Description,
        Field::Host,
        Field::Port,
        Field::Username,
        Field::IdentityFile,
        Field::JumpHost,
        Field::ExtraArgs,
        Field::Tags,
        Field::Forwards,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Field::Name => "Name",
            Field::Description => "Description",
            Field::Host => "Host / IP",
            Field::Port => "Port (optional)",
            Field::Username => "Username (optional)",
            Field::IdentityFile => "SSH key path (optional)",
            Field::JumpHost => "Jump host (name or host)",
            Field::ExtraArgs => "Extra ssh args (optional)",
            Field::Tags => "Tags (space separated)",
            Field::Forwards => "Forwards (L/R/D specs)",
        }
    }

    /// Label in the bootstrap form, where the target user and key are
    /// required rather than optional.
    pub fn bootstrap_label(self) -> &'static str {
        match self {
            Field::Username => "Username (required)",
            Field::IdentityFile => "SSH key path (required)",
            other => other.label(),
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|field| *field == self).unwrap();
        Self::ALL[(index + 1).min(Self::ALL.len() - 1)]
    }

    pub fn prev(self) -> Self {
        let index = Self::ALL.iter().position(|field| *field == self).unwrap();
        Self::ALL[index.saturating_sub(1)]
    }

    pub fn is_last(self) -> bool {
        self == *Self::ALL.last().unwrap()
    }
}

#[derive(Debug, Default)]
pub struct DraftServer {
    pub name: String,
    pub description: String,
    pub host: String,
    pub port: String,
    pub username: String,
    pub identity_file: String,
    pub jump_host: String,
    pub extra_args: String,
    pub tags: String,
    pub forwards: String,
}

impl DraftServer {
    pub fn from_server(server: &Server) -> Self {
        Self {
            name: server.name.clone(),
            description: server.description.clone(),
            host: server.host.clone(),
            port: server.port.map(|p| p.to_string()).unwrap_or_default(),
            username: server.username.clone().unwrap_or_default(),
            identity_file: server.identity_file.clone().unwrap_or_default(),
            jump_host: server.jump_host.clone().unwrap_or_default(),
            extra_args: server.extra_args.clone().unwrap_or_default(),
            tags: server.tags.join(" "),
            forwards: server.forwards.join(", "),
        }
    }

    /// A draft for a new entry copied from `server`, with a fresh name so
    /// it never collides with the original.
    pub fn duplicate_of(server: &Server, taken: impl Fn(&str) -> bool) -> Self {
        let mut draft = Self::from_server(server);
        let base = format!("{}-copy", server.name);
        draft.name = std::iter::once(base.clone())
            .chain((2..).map(|n| format!("{base}-{n}")))
            .find(|name| !taken(name))
            .unwrap_or(base);
        draft
    }

    pub fn current_value_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::Name => &mut self.name,
            Field::Description => &mut self.description,
            Field::Host => &mut self.host,
            Field::Port => &mut self.port,
            Field::Username => &mut self.username,
            Field::IdentityFile => &mut self.identity_file,
            Field::JumpHost => &mut self.jump_host,
            Field::ExtraArgs => &mut self.extra_args,
            Field::Tags => &mut self.tags,
            Field::Forwards => &mut self.forwards,
        }
    }

    pub fn current_value(&self, field: Field) -> &str {
        match field {
            Field::Name => &self.name,
            Field::Description => &self.description,
            Field::Host => &self.host,
            Field::Port => &self.port,
            Field::Username => &self.username,
            Field::IdentityFile => &self.identity_file,
            Field::JumpHost => &self.jump_host,
            Field::ExtraArgs => &self.extra_args,
            Field::Tags => &self.tags,
            Field::Forwards => &self.forwards,
        }
    }

    /// Validates the draft into a [`Server`] without consuming it, so a
    /// failed save can leave the dialog open with the input intact.
    pub fn to_server(&self) -> Result<Server, &'static str> {
        let name = self.name.trim();
        let host = self.host.trim();
        if name.is_empty() || host.is_empty() {
            return Err("Name and host are required");
        }

        let port = match self.port.trim() {
            "" => None,
            raw => Some(
                raw.parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0)
                    .ok_or("Port must be a number between 1 and 65535")?,
            ),
        };

        // The host, user, and jump host all end up as ssh destinations; a
        // leading '-' would let one be parsed as an ssh option instead.
        let jump_host = optional_trimmed(&self.jump_host);
        if host.starts_with('-')
            || self.username.trim().starts_with('-')
            || jump_host.as_deref().is_some_and(|j| j.starts_with('-'))
        {
            return Err("Host, username, and jump host must not start with '-'");
        }
        if jump_host
            .as_deref()
            .is_some_and(|j| j.contains(char::is_whitespace))
        {
            return Err("Jump host must not contain spaces");
        }
        if jump_host
            .as_deref()
            .is_some_and(|j| j.eq_ignore_ascii_case(name))
        {
            return Err("A server cannot be its own jump host");
        }

        let mut forwards: Vec<String> = Vec::new();
        for raw in self.forwards.split(',').filter(|f| !f.trim().is_empty()) {
            let canonical = ForwardSpec::parse(raw)
                .map_err(|_| "Forwards look like L8080:localhost:80, R9000:host:22, or D1080")?
                .canonical();
            if !forwards.contains(&canonical) {
                forwards.push(canonical);
            }
        }

        Ok(Server {
            name: name.to_string(),
            description: self.description.trim().to_string(),
            host: host.to_string(),
            port,
            username: optional_trimmed(&self.username),
            identity_file: optional_trimmed(&self.identity_file),
            extra_args: optional_trimmed(&self.extra_args),
            jump_host,
            tags: parse_tags(&self.tags),
            forwards,
            ..Default::default()
        })
    }

    /// Validates the draft for the bootstrap flow, which additionally needs a
    /// target user and a key whose `.pub` half will be installed remotely.
    pub fn to_bootstrap_server(&self) -> Result<Server, &'static str> {
        let server = self.to_server()?;
        if server.username.is_none() {
            return Err("Username is required for bootstrap");
        }
        if server.identity_file.is_none() {
            return Err("SSH key path is required for bootstrap");
        }
        Ok(server)
    }
}

/// Splits on spaces and commas, drops `#` prefixes, and removes duplicates
/// (ignoring case, first spelling wins).
pub fn parse_tags(raw: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for tag in raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(|t| t.trim_start_matches('#'))
        .filter(|t| !t.is_empty())
    {
        if !tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
            tags.push(tag.to_string());
        }
    }
    tags
}

fn optional_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// What submitting the form dialog should do with the draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormPurpose {
    /// Save as a new server entry.
    Add,
    /// Replace the server at this index.
    Edit(usize),
    /// Exit the TUI and install the public key on the drafted server.
    Bootstrap,
}

#[derive(Debug)]
pub enum Mode {
    Normal,
    /// Inline live filtering in the server card header.
    Search,
    /// The add/edit/bootstrap dialog.
    Form {
        draft: Box<DraftServer>,
        field: Field,
        purpose: FormPurpose,
    },
    /// The delete confirmation dialog for the selected server.
    ConfirmDelete,
    /// The settings dialog. `launcher` is a draft: it follows the highlighted
    /// choice and only reaches the config when the dialog is confirmed, so
    /// cancelling leaves the saved preference untouched.
    Settings {
        launcher: SshLauncher,
    },
    /// The key reference overlay.
    Help,
    /// Picking which `~/.ssh/config` hosts to import.
    Import {
        candidates: Vec<ImportCandidate>,
        cursor: usize,
    },
    /// Picking SSH, SFTP, or mosh for the selected server.
    Launch {
        mode: LaunchMode,
    },
    /// Starting and stopping the selected server's saved port forwards.
    Forwards {
        cursor: usize,
    },
    /// Creating backups and restoring them. Row 0 is "new backup"; row
    /// `n` is `entries[n - 1]`.
    Backups {
        dir: PathBuf,
        entries: Vec<BackupEntry>,
        cursor: usize,
    },
    /// Choosing where to install the local terminfo on the selected server.
    Terminfo {
        scope: TerminfoScope,
    },
}

/// One backup file in the backups dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    pub path: PathBuf,
    pub file_name: String,
    /// Server count, or `None` when the file could not be read as a backup.
    pub servers: Option<usize>,
}

/// The `YYYYMMDD-HHMMSS` suffix of a backup file name.
fn backup_stamp(name: &str) -> &str {
    let stem = name.strip_suffix(".json").unwrap_or(name);
    stem.get(stem.len().saturating_sub(15)..).unwrap_or(stem)
}

/// LazySSH backups in `dir`, newest first (names embed a sortable UTC
/// timestamp). A missing directory simply has no backups.
pub fn list_backups(dir: &Path) -> Vec<BackupEntry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<BackupEntry> = read
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("lazyssh-") && n.ends_with(".json"))
        })
        .map(|path| BackupEntry {
            file_name: path.file_name().unwrap().to_string_lossy().into_owned(),
            servers: crate::backup::load(&path).ok().map(|c| c.servers.len()),
            path,
        })
        .collect();
    // Sort by the trailing `YYYYMMDD-HHMMSS`, so `before-restore` and
    // `backup` files interleave chronologically rather than by label.
    entries.sort_by(|a, b| backup_stamp(&b.file_name).cmp(backup_stamp(&a.file_name)));
    entries
}

/// One importable host in the import dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportCandidate {
    pub server: Server,
    /// Already saved under this name; shown but never imported.
    pub exists: bool,
    pub chosen: bool,
}

/// How a status message should be rendered: `Hint` shows the contextual
/// keybinding badges instead of literal text, the rest color a short message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Hint,
    Success,
    Warn,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppExit {
    Quit,
    /// Open the selected server in this mode.
    Connect(LaunchMode),
    /// Leave the TUI and install this (not yet saved) server's public key on
    /// the remote host; the user is asked afterwards whether to save it.
    Bootstrap(Box<Server>),
    /// Leave the TUI and install the local terminfo on the selected server.
    Terminfo(TerminfoScope),
}

#[derive(Debug)]
pub struct App {
    pub config: Config,
    pub selected: usize,
    pub filter: String,
    pub mode: Mode,
    pub status: String,
    pub status_kind: StatusKind,
    pub tick: u64,
    /// Latest reachability per `host:port`, see [`crate::probe`].
    pub reach: HashMap<String, Reach>,
    pub prober: Prober,
    /// Running port forwards; they live exactly as long as the app.
    pub forwards: Forwards,
}

impl App {
    pub fn new(mut config: Config) -> Self {
        // Pinned first, then most recently connected, so the last-used
        // server is already selected on launch.
        config.sort_by_recency();
        Self {
            config,
            selected: 0,
            filter: String::new(),
            mode: Mode::Normal,
            status: String::new(),
            status_kind: StatusKind::Hint,
            tick: 0,
            reach: HashMap::new(),
            prober: Prober::default(),
            forwards: Forwards::default(),
        }
    }

    /// Reachability of `server`, if it has been probed.
    pub fn reach_of(&self, server: &Server) -> Option<Reach> {
        self.reach.get(&probe_key(server)).copied()
    }

    /// Probes every saved server in the background.
    pub fn refresh_reachability(&mut self) {
        self.prober.spawn(&self.config.servers, &mut self.reach);
    }

    /// Per-tick housekeeping: collects finished probes and reports forwards
    /// that died on their own.
    pub fn poll_background(&mut self) {
        self.prober.drain(&mut self.reach);
        if let Some(dead) = self.forwards.poll().into_iter().next() {
            self.set_status(
                StatusKind::Warn,
                format!(
                    "Forward {} on {} stopped: {}",
                    dead.key.spec, dead.key.server, dead.reason
                ),
            );
        }
    }

    pub fn set_status(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.status_kind = kind;
        self.status = message.into();
    }

    /// Returns the selected server only while it matches the live query.
    pub fn selected_server(&self) -> Option<&Server> {
        self.config
            .servers
            .get(self.selected)
            .filter(|server| matches_filter(server, &self.filter))
    }

    /// Full config indices of servers matching the live query, in list order.
    pub fn visible_indices(&self) -> Vec<usize> {
        self.config
            .servers
            .iter()
            .enumerate()
            .filter_map(|(index, server)| matches_filter(server, &self.filter).then_some(index))
            .collect()
    }

    /// Keeps selection on a matching server after filtering or mutation.
    fn ensure_selection_visible(&mut self) {
        let indices = self.visible_indices();
        if !indices.contains(&self.selected) {
            self.selected = indices.first().copied().unwrap_or(0);
        }
    }

    /// Moves forward through matching servers, clamping at the end.
    pub fn select_next(&mut self) {
        let indices = self.visible_indices();
        let position = indices.iter().position(|&index| index == self.selected);
        self.selected = indices
            .get(position.map_or(0, |p| (p + 1).min(indices.len() - 1)))
            .copied()
            .unwrap_or(0);
    }

    /// Moves backward through matching servers, clamping at the start.
    pub fn select_prev(&mut self) {
        let indices = self.visible_indices();
        let position = indices
            .iter()
            .position(|&index| index == self.selected)
            .unwrap_or(0);
        self.selected = indices
            .get(position.saturating_sub(1))
            .copied()
            .unwrap_or(0);
    }

    pub fn open_add(&mut self) {
        self.mode = Mode::Form {
            draft: Box::default(),
            field: Field::Name,
            purpose: FormPurpose::Add,
        };
        self.status_kind = StatusKind::Hint;
    }

    pub fn open_edit(&mut self) {
        let Some(server) = self.selected_server() else {
            self.set_status(StatusKind::Warn, "No server to edit");
            return;
        };
        let draft = DraftServer::from_server(server);
        self.mode = Mode::Form {
            draft: Box::new(draft),
            field: Field::Name,
            purpose: FormPurpose::Edit(self.selected),
        };
        self.status_kind = StatusKind::Hint;
    }

    /// Opens the add form prefilled from the selected server.
    pub fn open_duplicate(&mut self) {
        let Some(server) = self.selected_server() else {
            self.set_status(StatusKind::Warn, "No server to duplicate");
            return;
        };
        let draft =
            DraftServer::duplicate_of(server, |name| self.config.index_of_name(name).is_some());
        self.mode = Mode::Form {
            draft: Box::new(draft),
            field: Field::Name,
            purpose: FormPurpose::Add,
        };
        self.status_kind = StatusKind::Hint;
    }

    pub fn open_help(&mut self) {
        self.mode = Mode::Help;
        self.status_kind = StatusKind::Hint;
    }

    pub fn open_launch(&mut self) {
        if self.selected_server().is_none() {
            self.set_status(StatusKind::Warn, "No server selected");
            return;
        }
        self.mode = Mode::Launch {
            mode: LaunchMode::Ssh,
        };
        self.status_kind = StatusKind::Hint;
    }

    pub fn open_forwards(&mut self) {
        match self.selected_server() {
            None => self.set_status(StatusKind::Warn, "No server selected"),
            Some(server) if server.forwards.is_empty() => self.set_status(
                StatusKind::Info,
                "No saved forwards; add some with E → Forwards (e.g. L8080:localhost:80)",
            ),
            Some(_) => {
                self.mode = Mode::Forwards { cursor: 0 };
                self.status_kind = StatusKind::Hint;
            }
        }
    }

    /// Starts or stops forward `cursor` of the selected server.
    pub fn toggle_forward(&mut self, cursor: usize) {
        let Some(server) = self.selected_server().cloned() else {
            return;
        };
        let Some(raw) = server.forwards.get(cursor) else {
            return;
        };
        let key = ForwardKey::new(&server.name, raw);
        if self.forwards.stop(&key) {
            self.set_status(StatusKind::Info, format!("Stopped {raw}"));
            return;
        }
        let started = ForwardSpec::parse(raw)
            .map_err(anyhow::Error::msg)
            .and_then(|spec| {
                let resolved = self.config.resolved(&server)?;
                self.forwards
                    .start(key, crate::ssh::build_forward_command(&resolved, &spec))
            });
        match started {
            Ok(()) => self.set_status(StatusKind::Success, format!("Started {raw}")),
            Err(err) => self.set_status(StatusKind::Warn, format!("Forward failed: {err}")),
        }
    }

    /// Toggles the pin on the selected server, re-sorts, and keeps it
    /// selected at its new position.
    pub fn toggle_pin(&mut self) -> Result<()> {
        self.toggle_pin_with(Config::save)
    }

    fn toggle_pin_with(&mut self, save: impl FnOnce(&Config) -> Result<()>) -> Result<()> {
        if self.selected_server().is_none() {
            self.set_status(StatusKind::Warn, "No server to pin");
            return Ok(());
        }
        let Some(pinned) = self.config.toggle_pin(self.selected) else {
            return Ok(());
        };
        let name = self.config.servers[self.selected].name.clone();
        self.config.sort_by_recency();
        self.selected = self.config.index_of_name(&name).unwrap_or(0);
        save(&self.config)?;
        let verb = if pinned { "Pinned" } else { "Unpinned" };
        self.set_status(StatusKind::Success, format!("{verb} {name}"));
        Ok(())
    }

    /// The plain OpenSSH command line for the selected server.
    pub fn selected_command_line(&self) -> Option<Result<String>> {
        let server = self.selected_server()?;
        Some(
            self.config
                .resolved(server)
                .map(|resolved| crate::ssh::command_line(&crate::ssh::build_command(&resolved))),
        )
    }

    pub fn copy_command(&mut self) {
        self.copy_command_with(crate::clipboard::copy);
    }

    fn copy_command_with(&mut self, copy: impl FnOnce(&str) -> Result<&'static str>) {
        match self.selected_command_line() {
            None => self.set_status(StatusKind::Warn, "No server selected"),
            Some(Err(err)) => self.set_status(StatusKind::Warn, err.to_string()),
            Some(Ok(line)) => match copy(&line) {
                Ok(via) => self.set_status(StatusKind::Success, format!("Copied to {via}: {line}")),
                Err(err) => self.set_status(StatusKind::Warn, format!("Copy failed: {err}")),
            },
        }
    }

    pub fn open_terminfo(&mut self) {
        if self.selected_server().is_none() {
            self.set_status(StatusKind::Warn, "No server selected");
            return;
        }
        self.mode = Mode::Terminfo {
            scope: TerminfoScope::System,
        };
        self.status_kind = StatusKind::Hint;
    }

    /// Opens the backups dialog on the default backups directory.
    pub fn open_backups(&mut self) {
        match crate::backup::default_dir() {
            Ok(dir) => self.open_backups_in(dir),
            Err(err) => self.set_status(StatusKind::Warn, err.to_string()),
        }
    }

    pub fn open_backups_in(&mut self, dir: PathBuf) {
        let entries = list_backups(&dir);
        self.mode = Mode::Backups {
            dir,
            entries,
            cursor: 0,
        };
        self.status_kind = StatusKind::Hint;
    }

    /// Writes a new backup into the dialog's directory and refreshes it.
    fn create_backup(&mut self) {
        let Mode::Backups { dir, .. } = &self.mode else {
            return;
        };
        let dir = dir.clone();
        let path = dir.join(crate::backup::file_name(
            "backup",
            crate::config::now_unix_secs(),
        ));
        match crate::backup::write(&self.config, &path) {
            Ok(()) => {
                self.open_backups_in(dir);
                self.set_status(
                    StatusKind::Success,
                    format!("Backed up to {}", path.display()),
                );
            }
            Err(err) => self.set_status(StatusKind::Warn, format!("Backup failed: {err:#}")),
        }
    }

    /// Restores the highlighted backup — merging new names, or replacing
    /// the profile — after writing a safety backup of the current one.
    fn restore_backup_with(
        &mut self,
        replace: bool,
        save: impl FnOnce(&Config) -> Result<()>,
    ) -> Result<()> {
        let Mode::Backups {
            dir,
            entries,
            cursor,
        } = &self.mode
        else {
            return Ok(());
        };
        let Some(entry) = cursor.checked_sub(1).and_then(|i| entries.get(i)).cloned() else {
            return Ok(());
        };
        let dir = dir.clone();
        let incoming = crate::backup::load(&entry.path)?;
        if !self.config.servers.is_empty() {
            let safety = dir.join(crate::backup::file_name(
                "before-restore",
                crate::config::now_unix_secs(),
            ));
            crate::backup::write(&self.config, &safety)?;
        }
        // Forwards belong to servers that may vanish in a replace.
        if replace {
            self.forwards.stop_all();
        }
        let mut next = self.config.clone();
        let report = crate::backup::restore(&mut next, incoming, replace);
        save(&next)?;
        self.config = next;
        self.config.sort_by_recency();
        self.selected = 0;
        self.ensure_selection_visible();
        self.mode = Mode::Normal;
        self.refresh_reachability();
        let message = if replace {
            format!(
                "Replaced profile with {} server(s) from {}",
                report.added, entry.file_name
            )
        } else if report.skipped.is_empty() {
            format!(
                "Restored {} server(s) from {}",
                report.added, entry.file_name
            )
        } else {
            format!(
                "Restored {} server(s); kept {} existing with the same name",
                report.added,
                report.skipped.len()
            )
        };
        self.set_status(StatusKind::Success, message);
        Ok(())
    }

    /// Opens the import dialog from `~/.ssh/config`.
    pub fn open_import(&mut self) {
        let Some(path) = crate::sshconfig::default_path() else {
            self.set_status(StatusKind::Warn, "Could not find a home directory");
            return;
        };
        if !path.exists() {
            self.set_status(
                StatusKind::Warn,
                format!("No ssh config at {}", path.display()),
            );
            return;
        }
        match crate::sshconfig::load(&path) {
            Ok(servers) => self.open_import_with(servers),
            Err(err) => self.set_status(StatusKind::Warn, err.to_string()),
        }
    }

    pub fn open_import_with(&mut self, servers: Vec<Server>) {
        let mut seen: Vec<String> = Vec::new();
        let candidates: Vec<ImportCandidate> = servers
            .into_iter()
            .filter(|server| {
                let key = server.name.to_lowercase();
                let fresh = !seen.contains(&key);
                seen.push(key);
                fresh
            })
            .map(|server| {
                let exists = self.config.index_of_name(&server.name).is_some();
                ImportCandidate {
                    server,
                    exists,
                    chosen: !exists,
                }
            })
            .collect();
        if candidates.is_empty() {
            self.set_status(StatusKind::Info, "No concrete Host entries in ssh config");
            return;
        }
        self.mode = Mode::Import {
            candidates,
            cursor: 0,
        };
        self.status_kind = StatusKind::Hint;
    }

    /// Imports the chosen candidates and saves.
    fn commit_import_with(&mut self, save: impl FnOnce(&Config) -> Result<()>) -> Result<()> {
        let Mode::Import { candidates, .. } = std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            return Ok(());
        };
        let chosen: Vec<Server> = candidates
            .into_iter()
            .filter(|c| c.chosen && !c.exists)
            .map(|c| c.server)
            .collect();
        let added = self.config.import(chosen);
        if added == 0 {
            self.set_status(StatusKind::Info, "Nothing imported");
            return Ok(());
        }
        save(&self.config)?;
        self.ensure_selection_visible();
        self.refresh_reachability();
        let s = if added == 1 { "" } else { "s" };
        self.set_status(StatusKind::Success, format!("Imported {added} server{s}"));
        Ok(())
    }

    pub fn open_bootstrap(&mut self) {
        self.mode = Mode::Form {
            draft: Box::default(),
            field: Field::Name,
            purpose: FormPurpose::Bootstrap,
        };
        self.status_kind = StatusKind::Hint;
    }

    /// Opens the settings dialog with the saved preference highlighted.
    pub fn open_settings(&mut self) {
        self.mode = Mode::Settings {
            launcher: self.config.launcher,
        };
        self.status_kind = StatusKind::Hint;
    }

    /// Stores `launcher` as the preference and closes the dialog, reporting
    /// whether the saved value actually changed. Persistence is the caller's
    /// job so the state change stays independent of touching disk.
    fn set_launcher(&mut self, launcher: SshLauncher) -> bool {
        let changed = self.config.launcher != launcher;
        self.config.launcher = launcher;
        self.mode = Mode::Normal;
        self.set_status(
            StatusKind::Success,
            format!("Launcher set to {}", launcher.label()),
        );
        changed
    }

    /// Confirms the settings dialog: keeps `launcher` and writes it out. An
    /// unchanged choice closes the dialog without rewriting the config.
    pub fn commit_settings(&mut self, launcher: SshLauncher) -> Result<()> {
        self.commit_settings_with(launcher, Config::save)
    }

    /// Persists a launcher candidate before making it the active preference.
    /// Keeping the write behind a closure makes save failures deterministic in
    /// tests without mutating process-wide config-directory environment.
    fn commit_settings_with(
        &mut self,
        launcher: SshLauncher,
        save: impl FnOnce(&Config) -> Result<()>,
    ) -> Result<()> {
        if self.config.launcher == launcher {
            self.set_launcher(launcher);
            return Ok(());
        }

        // Preserve the selected draft if persistence fails so the user can
        // retry or cancel without losing either choice.
        self.mode = Mode::Settings { launcher };

        let mut candidate = self.config.clone();
        candidate.launcher = launcher;
        save(&candidate)?;

        self.config = candidate;
        self.set_launcher(launcher);
        Ok(())
    }

    pub fn request_delete(&mut self) {
        if self.selected_server().is_none() {
            self.set_status(StatusKind::Warn, "No servers to delete");
            return;
        }
        self.mode = Mode::ConfirmDelete;
        self.status_kind = StatusKind::Hint;
    }

    pub fn delete_selected(&mut self) -> Result<()> {
        self.delete_selected_with(Config::save)
    }

    /// Deletes a visible selection with injectable persistence for tests.
    fn delete_selected_with(&mut self, save: impl FnOnce(&Config) -> Result<()>) -> Result<()> {
        if self.selected_server().is_none() {
            return Ok(());
        }
        let removed = self.config.remove(self.selected);
        if let Some(server) = &removed {
            self.forwards.stop_for(&server.name);
        }

        if self.selected >= self.config.servers.len() {
            self.selected = self.config.servers.len().saturating_sub(1);
        }

        self.ensure_selection_visible();
        save(&self.config)?;

        if let Some(server) = removed {
            self.set_status(StatusKind::Success, format!("Deleted {}", server.name));
        }
        Ok(())
    }

    /// Submits the form dialog. Add/edit saves to the config and stays in the
    /// TUI; bootstrap hands the validated draft back so the caller can leave
    /// the TUI and let `ssh` prompt for the remote password itself.
    pub fn submit_form(&mut self) -> Result<Option<AppExit>> {
        let Mode::Form { draft, purpose, .. } = &self.mode else {
            return Ok(None);
        };

        if *purpose == FormPurpose::Bootstrap {
            return match draft.to_bootstrap_server() {
                Ok(server) => {
                    self.mode = Mode::Normal;
                    Ok(Some(AppExit::Bootstrap(Box::new(server))))
                }
                // Keep the dialog open so the input can be corrected.
                Err(reason) => {
                    self.set_status(StatusKind::Warn, reason);
                    Ok(None)
                }
            };
        }

        self.save_draft()?;
        Ok(None)
    }

    pub fn save_draft(&mut self) -> Result<()> {
        self.save_draft_with(Config::save)
    }

    /// Saves the form and rechecks visibility with injectable persistence.
    fn save_draft_with(&mut self, save: impl FnOnce(&Config) -> Result<()>) -> Result<()> {
        let Mode::Form { draft, purpose, .. } = &self.mode else {
            return Ok(());
        };

        match draft.to_server() {
            Ok(server) => {
                let purpose = *purpose;
                let name = server.name.clone();
                self.mode = Mode::Normal;
                match purpose {
                    FormPurpose::Edit(index) if index < self.config.servers.len() => {
                        self.config.update_preserving_recency(index, server);
                        self.selected = index;
                        self.set_status(StatusKind::Success, format!("Updated {name}"));
                    }
                    _ => {
                        self.config.add(server);
                        self.selected = self.config.servers.len().saturating_sub(1);
                        self.set_status(StatusKind::Success, format!("Saved {name}"));
                    }
                }
                self.ensure_selection_visible();
                save(&self.config)?;
                self.refresh_reachability();
            }
            // Keep the dialog open so the input can be corrected.
            Err(reason) => self.set_status(StatusKind::Warn, reason),
        }

        Ok(())
    }
}

/// Every whitespace-separated term must match, ignoring case. `#term`
/// matches the start of a tag; a bare `#` matches tagged servers; any other
/// term is a substring of the name, host, description, or a tag.
pub fn matches_filter(server: &Server, query: &str) -> bool {
    query.split_whitespace().all(|term| {
        if let Some(tag) = term.strip_prefix('#') {
            return if tag.is_empty() {
                !server.tags.is_empty()
            } else {
                server.has_tag_prefix(tag)
            };
        }
        let term = term.to_lowercase();
        [&server.name, &server.host, &server.description]
            .into_iter()
            .chain(server.tags.iter())
            .any(|value| value.to_lowercase().contains(&term))
    })
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<Option<AppExit>> {
    handle_key_with_settings_commit(app, key, App::commit_settings)
}

fn handle_key_with_settings_commit(
    app: &mut App,
    key: KeyEvent,
    commit_settings: impl FnOnce(&mut App, SshLauncher) -> Result<()>,
) -> Result<Option<AppExit>> {
    if key.kind == KeyEventKind::Release {
        return Ok(None);
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Ok(Some(AppExit::Quit));
    }

    let forward_count = app.selected_server().map_or(0, |s| s.forwards.len());

    match &mut app.mode {
        Mode::Normal => match key.code {
            KeyCode::Char('/') => {
                app.mode = Mode::Search;
                app.status_kind = StatusKind::Hint;
                Ok(None)
            }
            KeyCode::Esc if !app.filter.is_empty() => {
                app.filter.clear();
                app.ensure_selection_visible();
                app.status_kind = StatusKind::Hint;
                Ok(None)
            }
            KeyCode::Char('q' | 'Q') | KeyCode::Esc => Ok(Some(AppExit::Quit)),
            KeyCode::Char('?') => {
                app.open_help();
                Ok(None)
            }
            KeyCode::Char('D') => {
                app.open_duplicate();
                Ok(None)
            }
            KeyCode::Char('y' | 'Y') => {
                app.copy_command();
                Ok(None)
            }
            KeyCode::Char('p' | 'P') => {
                if let Err(err) = app.toggle_pin() {
                    app.set_status(StatusKind::Warn, format!("Failed to save: {err}"));
                }
                Ok(None)
            }
            KeyCode::Char('i' | 'I') => {
                app.open_import();
                Ok(None)
            }
            KeyCode::Char('o' | 'O') => {
                app.open_launch();
                Ok(None)
            }
            KeyCode::Char('f' | 'F') => {
                app.open_forwards();
                Ok(None)
            }
            KeyCode::Char('x' | 'X') => {
                app.open_backups();
                Ok(None)
            }
            KeyCode::Char('t' | 'T') => {
                app.open_terminfo();
                Ok(None)
            }
            KeyCode::Char('r' | 'R') => {
                app.reach.clear();
                app.refresh_reachability();
                app.set_status(StatusKind::Info, "Checking reachability…");
                Ok(None)
            }
            KeyCode::Char('j' | 'J') | KeyCode::Down => {
                app.select_next();
                Ok(None)
            }
            KeyCode::Char('k' | 'K') | KeyCode::Up => {
                app.select_prev();
                Ok(None)
            }
            KeyCode::Char('a' | 'A') => {
                app.open_add();
                Ok(None)
            }
            KeyCode::Char('e' | 'E') => {
                app.open_edit();
                Ok(None)
            }
            KeyCode::Char('d') => {
                app.request_delete();
                Ok(None)
            }
            KeyCode::Char('b' | 'B') => {
                app.open_bootstrap();
                Ok(None)
            }
            KeyCode::Char('s' | 'S') => {
                app.open_settings();
                Ok(None)
            }
            KeyCode::Enter => {
                if app.selected_server().is_some() {
                    Ok(Some(AppExit::Connect(LaunchMode::Ssh)))
                } else {
                    app.set_status(StatusKind::Warn, "No server selected");
                    Ok(None)
                }
            }
            _ => Ok(None),
        },
        Mode::Search => {
            match key.code {
                KeyCode::Enter => app.mode = Mode::Normal,
                KeyCode::Esc => {
                    app.filter.clear();
                    app.mode = Mode::Normal;
                }
                KeyCode::Backspace => {
                    app.filter.pop();
                }
                KeyCode::Char(c) => app.filter.push(c),
                _ => {}
            }
            app.ensure_selection_visible();
            app.status_kind = StatusKind::Hint;
            Ok(None)
        }
        Mode::Form { draft, field, .. } => match key.code {
            KeyCode::Esc => {
                app.mode = Mode::Normal;
                app.set_status(StatusKind::Info, "Cancelled");
                Ok(None)
            }
            KeyCode::Enter | KeyCode::Tab => {
                if field.is_last() {
                    app.submit_form()
                } else {
                    *field = field.next();
                    Ok(None)
                }
            }
            KeyCode::BackTab => {
                *field = field.prev();
                Ok(None)
            }
            KeyCode::Char('s' | 'S') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.submit_form()
            }
            KeyCode::Backspace => {
                draft.current_value_mut(*field).pop();
                Ok(None)
            }
            KeyCode::Char(c) => {
                draft.current_value_mut(*field).push(c);
                Ok(None)
            }
            _ => Ok(None),
        },
        Mode::Settings { launcher } => match key.code {
            KeyCode::Char('j' | 'J') | KeyCode::Down | KeyCode::Tab => {
                *launcher = launcher.next();
                Ok(None)
            }
            KeyCode::Char('k' | 'K') | KeyCode::Up | KeyCode::BackTab => {
                *launcher = launcher.prev();
                Ok(None)
            }
            KeyCode::Enter => {
                let chosen = *launcher;
                if let Err(err) = commit_settings(app, chosen) {
                    app.set_status(StatusKind::Warn, format!("Failed to save settings: {err}"));
                }
                Ok(None)
            }
            KeyCode::Esc => {
                app.mode = Mode::Normal;
                app.set_status(StatusKind::Info, "Cancelled");
                Ok(None)
            }
            _ => Ok(None),
        },
        Mode::Help => {
            app.mode = Mode::Normal;
            Ok(None)
        }
        Mode::Terminfo { scope } => match key.code {
            KeyCode::Char('j' | 'J' | 'k' | 'K') | KeyCode::Down | KeyCode::Up | KeyCode::Tab => {
                *scope = scope.toggle();
                Ok(None)
            }
            KeyCode::Enter => {
                let chosen = *scope;
                app.mode = Mode::Normal;
                Ok(Some(AppExit::Terminfo(chosen)))
            }
            KeyCode::Esc | KeyCode::Char('q' | 'Q') => {
                app.mode = Mode::Normal;
                Ok(None)
            }
            _ => Ok(None),
        },
        Mode::Backups {
            entries, cursor, ..
        } => {
            match key.code {
                KeyCode::Char('j' | 'J') | KeyCode::Down => {
                    *cursor = (*cursor + 1).min(entries.len());
                }
                KeyCode::Char('k' | 'K') | KeyCode::Up => {
                    *cursor = cursor.saturating_sub(1);
                }
                KeyCode::Enter if *cursor == 0 => app.create_backup(),
                KeyCode::Char('n' | 'N') => app.create_backup(),
                KeyCode::Enter | KeyCode::Char('m' | 'M') => {
                    if let Err(err) = app.restore_backup_with(false, Config::save) {
                        app.set_status(StatusKind::Warn, format!("Restore failed: {err:#}"));
                    }
                }
                KeyCode::Char('R') => {
                    if let Err(err) = app.restore_backup_with(true, Config::save) {
                        app.set_status(StatusKind::Warn, format!("Restore failed: {err:#}"));
                    }
                }
                KeyCode::Esc | KeyCode::Char('q' | 'Q') => app.mode = Mode::Normal,
                _ => {}
            }
            Ok(None)
        }
        Mode::Launch { mode } => match key.code {
            KeyCode::Char('j' | 'J') | KeyCode::Down | KeyCode::Tab => {
                *mode = mode.next();
                Ok(None)
            }
            KeyCode::Char('k' | 'K') | KeyCode::Up | KeyCode::BackTab => {
                *mode = mode.prev();
                Ok(None)
            }
            KeyCode::Enter => {
                let chosen = *mode;
                app.mode = Mode::Normal;
                Ok(Some(AppExit::Connect(chosen)))
            }
            KeyCode::Esc | KeyCode::Char('q' | 'Q') => {
                app.mode = Mode::Normal;
                Ok(None)
            }
            _ => Ok(None),
        },
        Mode::Import { candidates, cursor } => match key.code {
            KeyCode::Char('j' | 'J') | KeyCode::Down => {
                *cursor = (*cursor + 1).min(candidates.len().saturating_sub(1));
                Ok(None)
            }
            KeyCode::Char('k' | 'K') | KeyCode::Up => {
                *cursor = cursor.saturating_sub(1);
                Ok(None)
            }
            KeyCode::Char(' ') => {
                if let Some(c) = candidates.get_mut(*cursor).filter(|c| !c.exists) {
                    c.chosen = !c.chosen;
                }
                Ok(None)
            }
            KeyCode::Char('a' | 'A') => {
                let all = candidates.iter().filter(|c| !c.exists).all(|c| c.chosen);
                for c in candidates.iter_mut().filter(|c| !c.exists) {
                    c.chosen = !all;
                }
                Ok(None)
            }
            KeyCode::Enter => {
                if let Err(err) = app.commit_import_with(Config::save) {
                    app.set_status(StatusKind::Warn, format!("Failed to save: {err}"));
                }
                Ok(None)
            }
            KeyCode::Esc | KeyCode::Char('q' | 'Q') => {
                app.mode = Mode::Normal;
                app.set_status(StatusKind::Info, "Import cancelled");
                Ok(None)
            }
            _ => Ok(None),
        },
        Mode::Forwards { cursor } => {
            match key.code {
                KeyCode::Char('j' | 'J') | KeyCode::Down => {
                    *cursor = (*cursor + 1).min(forward_count.saturating_sub(1));
                }
                KeyCode::Char('k' | 'K') | KeyCode::Up => {
                    *cursor = cursor.saturating_sub(1);
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    let index = *cursor;
                    app.toggle_forward(index);
                }
                KeyCode::Esc | KeyCode::Char('q' | 'Q' | 'f' | 'F') => {
                    app.mode = Mode::Normal;
                }
                _ => {}
            }
            Ok(None)
        }
        Mode::ConfirmDelete => match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                app.mode = Mode::Normal;
                app.delete_selected()?;
                Ok(None)
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                app.mode = Mode::Normal;
                app.set_status(StatusKind::Info, "Delete cancelled");
                Ok(None)
            }
            _ => Ok(None),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_server(name: &str) -> Server {
        Server {
            name: name.to_string(),
            description: String::new(),
            host: "example.com".to_string(),
            port: None,
            username: None,
            identity_file: None,
            extra_args: None,
            ..Default::default()
        }
    }

    #[test]
    fn filter_matches_all_searchable_fields() {
        let mut server = sample_server("Prod API");
        server.description = "European database".into();
        for query in ["prod", "API", "EXAMPLE", "database", "EUROPEAN", "", "   "] {
            assert!(matches_filter(&server, query), "{query}");
        }
        assert!(!matches_filter(&server, "missing"));
    }

    /// Builds alternating matches to exercise full-config selection indices.
    fn filtered_app() -> App {
        let mut app = App::new(Config::default());
        for name in ["dev", "prod-a", "stage", "prod-b"] {
            app.config.add(sample_server(name));
        }
        app.filter = "prod".into();
        app.ensure_selection_visible();
        app
    }

    #[test]
    fn filtered_navigation_and_hidden_actions() {
        let mut app = filtered_app();
        assert_eq!(app.visible_indices(), vec![1, 3]);
        assert_eq!(app.selected, 1);
        app.select_next();
        assert_eq!(app.selected, 3);
        app.select_next();
        assert_eq!(app.selected, 3);
        app.select_prev();
        assert_eq!(app.selected, 1);
        app.select_prev();
        assert_eq!(app.selected, 1);
        app.filter = "missing".into();
        assert!(app.selected_server().is_none());
        assert_eq!(handle_key(&mut app, KeyCode::Enter.into()).unwrap(), None);
        assert_eq!(app.status, "No server selected");
        app.open_edit();
        app.request_delete();
        assert!(matches!(app.mode, Mode::Normal));
        app.delete_selected_with(|_| panic!("hidden selection must not save"))
            .unwrap();
        app.ensure_selection_visible();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn search_key_lifecycle() {
        let mut app = filtered_app();
        app.filter.clear();
        handle_key(&mut app, KeyCode::Char('/').into()).unwrap();
        assert!(matches!(app.mode, Mode::Search));
        for ch in "prodX".chars() {
            handle_key(&mut app, KeyCode::Char(ch).into()).unwrap();
        }
        assert!(app.visible_indices().is_empty());
        handle_key(&mut app, KeyCode::Backspace.into()).unwrap();
        assert_eq!(app.visible_indices(), vec![1, 3]);
        assert_eq!(app.selected, 1);
        handle_key(&mut app, KeyCode::Enter.into()).unwrap();
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.filter, "prod");
        assert_eq!(handle_key(&mut app, KeyCode::Esc.into()).unwrap(), None);
        assert!(app.filter.is_empty());
        handle_key(&mut app, KeyCode::Char('/').into()).unwrap();
        handle_key(&mut app, KeyCode::Char('p').into()).unwrap();
        assert_eq!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
            )
            .unwrap(),
            Some(AppExit::Quit)
        );
        handle_key(&mut app, KeyCode::Esc.into()).unwrap();
        assert!(app.filter.is_empty());
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(
            handle_key(&mut app, KeyCode::Esc.into()).unwrap(),
            Some(AppExit::Quit)
        );
        app.filter = "prod".into();
        assert_eq!(
            handle_key(&mut app, KeyCode::Char('q').into()).unwrap(),
            Some(AppExit::Quit)
        );
    }

    #[test]
    fn mutations_reclamp_filtered_selection() {
        let mut app = filtered_app();
        app.open_edit();
        if let Mode::Form { draft, .. } = &mut app.mode {
            draft.name = "other".into();
        }
        app.save_draft_with(|_| Ok(())).unwrap();
        assert_eq!(app.selected, 3);
        app.delete_selected_with(|_| Ok(())).unwrap();
        assert_eq!(app.selected, 0);
        assert!(app.selected_server().is_none());
        app.open_add();
        if let Mode::Form { draft, .. } = &mut app.mode {
            draft.name = "prod-new".into();
            draft.host = "example.com".into();
        }
        app.save_draft_with(|_| Ok(())).unwrap();
        assert_eq!(app.selected, 3);
        app.open_add();
        if let Mode::Form { draft, .. } = &mut app.mode {
            draft.name = "hidden".into();
            draft.host = "example.com".into();
        }
        app.save_draft_with(|_| Ok(())).unwrap();
        assert_eq!(app.selected, 3);
    }

    fn key(app: &mut App, code: KeyCode) -> Option<AppExit> {
        handle_key(app, KeyEvent::from(code)).unwrap()
    }

    fn valid_draft() -> DraftServer {
        DraftServer {
            name: "box".into(),
            host: "example.com".into(),
            ..DraftServer::default()
        }
    }

    #[test]
    fn draft_parses_tags_jump_and_forwards() {
        let draft = DraftServer {
            tags: "#prod, eu  PROD homelab".into(),
            jump_host: " bastion ".into(),
            forwards: "L8080:localhost:80, -D 1080,L8080:localhost:80".into(),
            ..valid_draft()
        };
        let server = draft.to_server().unwrap();
        assert_eq!(server.tags, ["prod", "eu", "homelab"]);
        assert_eq!(server.jump_host.as_deref(), Some("bastion"));
        assert_eq!(server.forwards, ["L8080:localhost:80", "D1080"]);

        // And back: the draft shows what will be saved.
        let again = DraftServer::from_server(&server);
        assert_eq!(again.tags, "prod eu homelab");
        assert_eq!(again.forwards, "L8080:localhost:80, D1080");
        assert_eq!(again.to_server().unwrap(), server);
    }

    #[test]
    fn draft_rejects_unsafe_jump_hosts_and_bad_forwards() {
        for (jump, forwards) in [
            ("-oProxyCommand=x", ""),
            ("a b", ""),
            ("BOX", ""),
            ("", "nope"),
            ("", "L8080:localhost:80, X1"),
        ] {
            let draft = DraftServer {
                jump_host: jump.into(),
                forwards: forwards.into(),
                ..valid_draft()
            };
            assert!(draft.to_server().is_err(), "{jump:?} {forwards:?}");
        }
        let dash_user = DraftServer {
            username: "-x".into(),
            ..valid_draft()
        };
        assert!(dash_user.to_server().is_err());
    }

    #[test]
    fn filter_supports_tags_and_multiple_terms() {
        let mut server = sample_server("api");
        server.tags = vec!["prod".into(), "eu-west".into()];
        assert!(matches_filter(&server, "#prod"));
        assert!(matches_filter(&server, "#EU"));
        assert!(matches_filter(&server, "#"));
        assert!(matches_filter(&server, "api #prod"));
        assert!(
            matches_filter(&server, "west"),
            "plain terms search tags too"
        );
        assert!(!matches_filter(&server, "#staging"));
        assert!(!matches_filter(&server, "api #staging"));
        assert!(!matches_filter(&sample_server("bare"), "#"));
    }

    #[test]
    fn duplicate_opens_an_add_form_with_a_free_name() {
        let mut app = App::new(Config::default());
        let mut original = sample_server("node");
        original.tags = vec!["lab".into()];
        original.last_connected_at = Some(5);
        app.config.add(original);
        app.config.add(sample_server("node-copy"));
        app.selected = 0;

        key(&mut app, KeyCode::Char('D'));
        let Mode::Form { draft, purpose, .. } = &app.mode else {
            panic!("expected form, got {:?}", app.mode);
        };
        assert_eq!(*purpose, FormPurpose::Add);
        assert_eq!(draft.name, "node-copy-2");
        assert_eq!(draft.tags, "lab");
        app.save_draft_with(|_| Ok(())).unwrap();
        let copy = &app.config.servers[2];
        assert_eq!(copy.last_connected_at, None, "copies start fresh");
        assert_eq!(app.config.servers.len(), 3);
    }

    #[test]
    fn lowercase_d_still_deletes() {
        let mut app = App::new(Config::default());
        app.config.add(sample_server("a"));
        key(&mut app, KeyCode::Char('d'));
        assert!(matches!(app.mode, Mode::ConfirmDelete));
    }

    #[test]
    fn pin_moves_the_server_up_and_keeps_it_selected() {
        let mut app = App::new(Config::default());
        for name in ["a", "b", "c"] {
            app.config.add(sample_server(name));
        }
        app.selected = 2;
        let mut saved = None;
        app.toggle_pin_with(|c| {
            saved = Some(c.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(app.config.servers[0].name, "c");
        assert_eq!(app.selected, 0);
        assert!(saved.unwrap().servers[0].pinned);
        assert_eq!(app.status, "Pinned c");

        app.toggle_pin_with(|_| Ok(())).unwrap();
        assert!(!app.config.servers[app.selected].pinned);
        assert_eq!(app.config.servers[app.selected].name, "c");
    }

    #[test]
    fn copy_command_reports_the_exact_line() {
        let mut app = App::new(Config::default());
        let mut bastion = sample_server("bastion");
        bastion.host = "b.example".into();
        let mut server = sample_server("db");
        server.username = Some("sam".into());
        server.port = Some(2222);
        server.jump_host = Some("bastion".into());
        app.config.add(server);
        app.config.add(bastion);
        app.selected = 0;

        let mut copied = String::new();
        app.copy_command_with(|line| {
            copied = line.to_string();
            Ok("clipboard")
        });
        assert_eq!(copied, "ssh -p 2222 -J b.example sam@example.com");
        assert_eq!(app.status_kind, StatusKind::Success);

        app.copy_command_with(|_| anyhow::bail!("no display"));
        assert_eq!(app.status_kind, StatusKind::Warn);
        assert!(app.status.contains("no display"));
    }

    #[test]
    fn import_dialog_preselects_new_hosts_and_skips_existing() {
        let mut app = App::new(Config::default());
        app.config.add(sample_server("node-2"));
        app.open_import_with(vec![
            sample_server("node-2"),
            sample_server("gitea"),
            sample_server("GITEA"),
            sample_server("nas"),
        ]);
        let Mode::Import { candidates, .. } = &app.mode else {
            panic!("expected import mode");
        };
        let shape: Vec<_> = candidates
            .iter()
            .map(|c| (c.server.name.as_str(), c.exists, c.chosen))
            .collect();
        assert_eq!(
            shape,
            [
                ("node-2", true, false),
                ("gitea", false, true),
                ("nas", false, true)
            ]
        );

        // Space on an existing row does nothing; untick `nas`.
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Char(' '));
        let mut saves = 0;
        app.commit_import_with(|_| {
            saves += 1;
            Ok(())
        })
        .unwrap();
        let names: Vec<_> = app.config.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["node-2", "gitea"]);
        assert_eq!(saves, 1);
        assert_eq!(app.status, "Imported 1 server");
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn import_select_all_toggles_and_empty_input_is_reported() {
        let mut app = App::new(Config::default());
        app.open_import_with(vec![]);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.status_kind, StatusKind::Info);

        app.open_import_with(vec![sample_server("a"), sample_server("b")]);
        key(&mut app, KeyCode::Char('a'));
        let Mode::Import { candidates, .. } = &app.mode else {
            panic!()
        };
        assert!(candidates.iter().all(|c| !c.chosen));
        key(&mut app, KeyCode::Esc);
        assert!(app.config.servers.is_empty());
    }

    #[test]
    fn launch_dialog_returns_the_chosen_mode() {
        let mut app = App::new(Config::default());
        app.config.add(sample_server("a"));
        assert_eq!(
            key(&mut app, KeyCode::Enter),
            Some(AppExit::Connect(LaunchMode::Ssh))
        );
        key(&mut app, KeyCode::Char('o'));
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(
            key(&mut app, KeyCode::Enter),
            Some(AppExit::Connect(LaunchMode::Sftp))
        );
        key(&mut app, KeyCode::Char('o'));
        assert_eq!(key(&mut app, KeyCode::Esc), None);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn help_opens_with_question_mark_and_any_key_closes() {
        let mut app = App::new(Config::default());
        key(&mut app, KeyCode::Char('?'));
        assert!(matches!(app.mode, Mode::Help));
        // `q` closes the overlay instead of quitting.
        assert_eq!(key(&mut app, KeyCode::Char('q')), None);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn forwards_dialog_needs_saved_forwards() {
        let mut app = App::new(Config::default());
        app.config.add(sample_server("a"));
        key(&mut app, KeyCode::Char('f'));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.status_kind, StatusKind::Info);

        app.config.servers[0].forwards = vec!["D1080".into(), "L1:h:2".into()];
        key(&mut app, KeyCode::Char('f'));
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        assert!(matches!(app.mode, Mode::Forwards { cursor: 1 }));
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn backups_dialog_creates_lists_and_merges() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(Config::default());
        let mut a = sample_server("a");
        a.tags = vec!["lab".into()];
        app.config.add(a);
        app.open_backups_in(dir.path().to_path_buf());
        // Enter on row 0 creates a backup.
        key(&mut app, KeyCode::Enter);
        let Mode::Backups { entries, .. } = &app.mode else {
            panic!("{:?}", app.mode);
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].servers, Some(1));
        assert_eq!(app.status_kind, StatusKind::Success);

        // Change the live profile, then merge the backup back in.
        app.config.servers[0].name = "renamed".into();
        app.config.add(sample_server("b"));
        if let Mode::Backups { cursor, .. } = &mut app.mode {
            *cursor = 1;
        }
        let mut saved = None;
        app.restore_backup_with(false, |c| {
            saved = Some(c.clone());
            Ok(())
        })
        .unwrap();
        let mut names: Vec<_> = app.config.servers.iter().map(|s| s.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["a", "b", "renamed"]);
        assert_eq!(saved.unwrap().servers.len(), 3);
        assert!(matches!(app.mode, Mode::Normal));
        // The safety backup of the pre-restore profile was written.
        let listed = list_backups(dir.path());
        assert_eq!(listed.len(), 2);
        assert!(listed
            .iter()
            .any(|e| e.file_name.contains("before-restore")));
    }

    #[test]
    fn backups_replace_swaps_the_whole_profile() {
        let dir = tempfile::tempdir().unwrap();
        let mut backed_up = Config::default();
        backed_up.add(sample_server("from-backup"));
        backed_up.launcher = SshLauncher::Kitty;
        crate::backup::write(
            &backed_up,
            &dir.path().join(crate::backup::file_name("backup", 1_000)),
        )
        .unwrap();

        let mut app = App::new(Config::default());
        app.config.add(sample_server("current"));
        app.open_backups_in(dir.path().to_path_buf());
        key(&mut app, KeyCode::Down);
        app.restore_backup_with(true, |_| Ok(())).unwrap();
        let names: Vec<_> = app.config.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["from-backup"]);
        assert_eq!(app.config.launcher, SshLauncher::Kitty);
        assert!(app.status.starts_with("Replaced profile"));
    }

    #[test]
    fn failed_restore_save_leaves_the_profile_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let mut backed_up = Config::default();
        backed_up.add(sample_server("x"));
        crate::backup::write(
            &backed_up,
            &dir.path().join("lazyssh-backup-20260101-000000.json"),
        )
        .unwrap();
        let mut app = App::new(Config::default());
        app.config.add(sample_server("keep"));
        app.open_backups_in(dir.path().to_path_buf());
        key(&mut app, KeyCode::Down);
        assert!(app
            .restore_backup_with(true, |_| anyhow::bail!("disk full"))
            .is_err());
        assert_eq!(app.config.servers[0].name, "keep");
        assert!(matches!(app.mode, Mode::Backups { .. }));
    }

    #[test]
    fn backup_list_sorts_by_time_not_label() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "lazyssh-backup-20260101-000000.json",
            "lazyssh-before-restore-20260301-000000.json",
            "lazyssh-backup-20260201-000000.json",
            "unrelated.json",
            "lazyssh-broken-20260401-000000.json",
        ] {
            let body = if name.contains("broken") || name == "unrelated.json" {
                "nope".to_string()
            } else {
                crate::backup::to_json(&Config::default(), 0).unwrap()
            };
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        let listed = list_backups(dir.path());
        let names: Vec<_> = listed.iter().map(|e| e.file_name.as_str()).collect();
        assert_eq!(
            names,
            [
                "lazyssh-broken-20260401-000000.json",
                "lazyssh-before-restore-20260301-000000.json",
                "lazyssh-backup-20260201-000000.json",
                "lazyssh-backup-20260101-000000.json",
            ]
        );
        assert_eq!(listed[0].servers, None);
        assert!(list_backups(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn terminfo_dialog_defaults_to_system_and_exits_with_the_choice() {
        let mut app = App::new(Config::default());
        key(&mut app, KeyCode::Char('t'));
        assert!(matches!(app.mode, Mode::Normal), "needs a server");
        app.config.add(sample_server("prod"));
        key(&mut app, KeyCode::Char('t'));
        assert!(matches!(
            app.mode,
            Mode::Terminfo {
                scope: TerminfoScope::System
            }
        ));
        assert_eq!(
            key(&mut app, KeyCode::Enter),
            Some(AppExit::Terminfo(TerminfoScope::System))
        );
        key(&mut app, KeyCode::Char('t'));
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(
            key(&mut app, KeyCode::Enter),
            Some(AppExit::Terminfo(TerminfoScope::User))
        );
    }

    #[test]
    fn draft_requires_name_and_host() {
        let draft = DraftServer {
            name: "prod".to_string(),
            description: "Production".to_string(),
            ..DraftServer::default()
        };

        assert!(draft.to_server().is_err());
    }

    #[test]
    fn draft_trims_optional_values() {
        let draft = DraftServer {
            name: " prod ".to_string(),
            description: " Production ".to_string(),
            host: " 10.0.0.5 ".to_string(),
            port: " 2222 ".to_string(),
            username: " sam ".to_string(),
            identity_file: " ~/.ssh/id_ed25519 ".to_string(),
            extra_args: " -o ServerAliveInterval=30 ".to_string(),
            ..DraftServer::default()
        };

        let server = draft.to_server().unwrap();
        assert_eq!(server.name, "prod");
        assert_eq!(server.description, "Production");
        assert_eq!(server.host, "10.0.0.5");
        assert_eq!(server.port, Some(2222));
        assert_eq!(server.username.as_deref(), Some("sam"));
        assert_eq!(server.identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
        assert_eq!(
            server.extra_args.as_deref(),
            Some("-o ServerAliveInterval=30")
        );
    }

    #[test]
    fn draft_rejects_invalid_ports() {
        for bad in ["abc", "-1", "0", "70000"] {
            let draft = DraftServer {
                name: "prod".to_string(),
                host: "example.com".to_string(),
                port: bad.to_string(),
                ..DraftServer::default()
            };
            assert!(draft.to_server().is_err(), "port {bad:?} should be invalid");
        }
    }

    #[test]
    fn draft_round_trips_a_server() {
        let server = Server {
            name: "prod".to_string(),
            description: "Production".to_string(),
            host: "example.com".to_string(),
            port: Some(2200),
            username: Some("deploy".to_string()),
            identity_file: None,
            extra_args: Some("-A".to_string()),
            ..Default::default()
        };

        let rebuilt = DraftServer::from_server(&server).to_server().unwrap();
        assert_eq!(rebuilt, server);
    }

    #[test]
    fn drafts_build_servers_that_start_never_connected() {
        let draft = DraftServer {
            name: "prod".to_string(),
            host: "example.com".to_string(),
            ..DraftServer::default()
        };
        assert_eq!(draft.to_server().unwrap().last_connected_at, None);
    }

    #[test]
    fn new_app_orders_servers_by_recency() {
        let mut config = Config::default();
        config.add(sample_server("never"));
        config.add(sample_server("older"));
        config.add(sample_server("newest"));
        config.mark_connected(1, 100);
        config.mark_connected(2, 200);

        let app = App::new(config);
        let names: Vec<_> = app.config.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["newest", "older", "never"]);
        // Selection starts on the most recently used server.
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn field_traversal_clamps_at_both_ends() {
        assert_eq!(Field::Name.prev(), Field::Name);
        assert_eq!(Field::Name.next(), Field::Description);
        assert_eq!(Field::Forwards.next(), Field::Forwards);
        assert!(Field::Forwards.is_last());
        assert!(!Field::ExtraArgs.is_last());
        assert!(!Field::Name.is_last());
    }

    #[test]
    fn selection_stays_in_bounds() {
        let mut app = App::new(Config::default());
        app.select_next();
        assert_eq!(app.selected, 0);
        app.select_prev();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn edit_prefills_draft_from_selected_server() {
        let mut config = Config::default();
        config.add(sample_server("prod"));
        let mut app = App::new(config);

        app.open_edit();
        let Mode::Form { draft, purpose, .. } = &app.mode else {
            panic!("expected form mode");
        };
        assert_eq!(draft.name, "prod");
        assert_eq!(*purpose, FormPurpose::Edit(0));
    }

    #[test]
    fn bootstrap_draft_requires_username_and_key() {
        let mut draft = DraftServer {
            name: "prod".to_string(),
            host: "example.com".to_string(),
            ..DraftServer::default()
        };
        // Valid as a plain add/edit draft, but not for bootstrap.
        assert!(draft.to_server().is_ok());
        assert!(draft.to_bootstrap_server().is_err());

        draft.username = "deploy".to_string();
        assert!(draft.to_bootstrap_server().is_err());

        draft.identity_file = "~/.ssh/id_ed25519".to_string();
        let server = draft.to_bootstrap_server().unwrap();
        assert_eq!(server.username.as_deref(), Some("deploy"));
        assert_eq!(server.identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
    }

    #[test]
    fn bootstrap_draft_rejects_option_like_host_and_user() {
        for (host, user) in [("-oProxyCommand=x", "deploy"), ("example.com", "-fake")] {
            let draft = DraftServer {
                name: "prod".to_string(),
                host: host.to_string(),
                username: user.to_string(),
                identity_file: "~/.ssh/id_ed25519".to_string(),
                ..DraftServer::default()
            };
            assert!(
                draft.to_bootstrap_server().is_err(),
                "host {host:?} user {user:?} should be rejected"
            );
        }
    }

    #[test]
    fn b_key_opens_bootstrap_form() {
        let mut app = App::new(Config::default());
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('b'))).unwrap();
        let Mode::Form { purpose, .. } = &app.mode else {
            panic!("expected form mode");
        };
        assert_eq!(*purpose, FormPurpose::Bootstrap);
    }

    #[test]
    fn key_release_events_are_ignored() {
        let mut app = App::new(Config::default());
        app.open_add();

        handle_key(
            &mut app,
            KeyEvent::new_with_kind(KeyCode::Char('p'), KeyModifiers::NONE, KeyEventKind::Press),
        )
        .unwrap();
        handle_key(
            &mut app,
            KeyEvent::new_with_kind(
                KeyCode::Char('p'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            ),
        )
        .unwrap();

        let Mode::Form { draft, .. } = &app.mode else {
            panic!("expected form mode");
        };
        assert_eq!(draft.name, "p");
    }

    #[test]
    fn key_repeat_events_still_apply() {
        let mut app = App::new(Config::default());
        app.open_add();

        handle_key(
            &mut app,
            KeyEvent::new_with_kind(KeyCode::Char('p'), KeyModifiers::NONE, KeyEventKind::Press),
        )
        .unwrap();
        handle_key(
            &mut app,
            KeyEvent::new_with_kind(KeyCode::Char('p'), KeyModifiers::NONE, KeyEventKind::Repeat),
        )
        .unwrap();

        let Mode::Form { draft, .. } = &app.mode else {
            panic!("expected form mode");
        };
        assert_eq!(draft.name, "pp");
    }

    #[test]
    fn submitting_bootstrap_form_exits_without_saving() {
        let mut app = App::new(Config::default());
        app.open_bootstrap();
        if let Mode::Form { draft, .. } = &mut app.mode {
            draft.name = "new-box".to_string();
            draft.host = "10.0.0.9".to_string();
            draft.username = "deploy".to_string();
            draft.identity_file = "~/.ssh/id_ed25519".to_string();
        }

        let exit = app.submit_form().unwrap();
        let Some(AppExit::Bootstrap(server)) = exit else {
            panic!("expected bootstrap exit, got {exit:?}");
        };
        assert_eq!(server.name, "new-box");
        assert_eq!(server.username.as_deref(), Some("deploy"));
        // The entry is only saved after the user confirms outside the TUI.
        assert!(app.config.servers.is_empty());
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn invalid_bootstrap_submit_keeps_the_form_open() {
        let mut app = App::new(Config::default());
        app.open_bootstrap();
        if let Mode::Form { draft, .. } = &mut app.mode {
            draft.name = "new-box".to_string();
            draft.host = "10.0.0.9".to_string();
        }

        assert_eq!(app.submit_form().unwrap(), None);
        assert!(matches!(
            app.mode,
            Mode::Form {
                purpose: FormPurpose::Bootstrap,
                ..
            }
        ));
        assert_eq!(app.status_kind, StatusKind::Warn);
    }

    #[test]
    fn bootstrap_labels_mark_user_and_key_required() {
        assert_eq!(Field::Username.bootstrap_label(), "Username (required)");
        assert_eq!(
            Field::IdentityFile.bootstrap_label(),
            "SSH key path (required)"
        );
        assert_eq!(Field::Name.bootstrap_label(), Field::Name.label());
    }

    #[test]
    fn settings_open_with_saved_launcher_and_cancel_without_mutating_config() {
        let config = Config {
            launcher: SshLauncher::OpenSsh,
            ..Config::default()
        };
        let mut app = App::new(config);

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('s'))).unwrap();
        assert!(matches!(
            app.mode,
            Mode::Settings {
                launcher: SshLauncher::OpenSsh
            }
        ));

        handle_key(&mut app, KeyEvent::from(KeyCode::Down)).unwrap();
        assert!(matches!(
            app.mode,
            Mode::Settings {
                launcher: SshLauncher::Kitty
            }
        ));
        handle_key(&mut app, KeyEvent::from(KeyCode::Esc)).unwrap();

        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.config.launcher, SshLauncher::OpenSsh);
    }

    #[test]
    fn settings_navigation_clamps_at_both_ends() {
        let mut app = App::new(Config::default());
        app.open_settings();

        handle_key(&mut app, KeyEvent::from(KeyCode::Up)).unwrap();
        assert!(matches!(
            app.mode,
            Mode::Settings {
                launcher: SshLauncher::Auto
            }
        ));

        for _ in 0..3 {
            handle_key(&mut app, KeyEvent::from(KeyCode::Char('j'))).unwrap();
        }
        assert!(matches!(
            app.mode,
            Mode::Settings {
                launcher: SshLauncher::Kitty
            }
        ));
    }

    #[test]
    fn confirming_settings_updates_in_memory_preference_and_closes() {
        let mut app = App::new(Config::default());
        app.open_settings();

        assert!(app.set_launcher(SshLauncher::Kitty));
        assert_eq!(app.config.launcher, SshLauncher::Kitty);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.status_kind, StatusKind::Success);
    }

    #[test]
    fn failed_settings_save_keeps_dialog_open_and_persisted_launcher() {
        let mut app = App::new(Config::default());
        app.open_settings();

        let result =
            app.commit_settings_with(SshLauncher::Kitty, |_| Err(anyhow::anyhow!("disk full")));

        assert!(result.is_err(), "save failure must propagate");
        // The persisted preference is untouched and the draft survives so
        // the user can retry or cancel.
        assert_eq!(app.config.launcher, SshLauncher::Auto);
        assert!(matches!(
            app.mode,
            Mode::Settings {
                launcher: SshLauncher::Kitty
            }
        ));
        assert_ne!(app.status_kind, StatusKind::Success);
    }

    #[test]
    fn settings_enter_absorbs_save_error_and_shows_warning() {
        let mut app = App::new(Config::default());
        app.mode = Mode::Settings {
            launcher: SshLauncher::Kitty,
        };

        let result = handle_key_with_settings_commit(
            &mut app,
            KeyEvent::from(KeyCode::Enter),
            |app, launcher| {
                app.commit_settings_with(launcher, |_| Err(anyhow::anyhow!("disk full")))
            },
        );

        assert_eq!(result.unwrap(), None);
        assert_eq!(app.config.launcher, SshLauncher::Auto);
        assert!(matches!(
            app.mode,
            Mode::Settings {
                launcher: SshLauncher::Kitty
            }
        ));
        assert_eq!(app.status_kind, StatusKind::Warn);
        assert!(app.status.contains("disk full"));
    }

    #[test]
    fn successful_settings_save_writes_choice_then_closes() {
        let mut app = App::new(Config::default());
        app.open_settings();

        let mut saved = None;
        app.commit_settings_with(SshLauncher::Kitty, |config| {
            saved = Some(config.launcher);
            Ok(())
        })
        .unwrap();

        assert_eq!(saved, Some(SshLauncher::Kitty));
        assert_eq!(app.config.launcher, SshLauncher::Kitty);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.status_kind, StatusKind::Success);
    }

    #[test]
    fn enter_confirms_unchanged_settings_without_touching_disk() {
        let mut app = App::new(Config::default());
        app.open_settings();

        handle_key(&mut app, KeyEvent::from(KeyCode::Enter)).unwrap();

        assert_eq!(app.config.launcher, SshLauncher::Auto);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn delete_requires_confirmation_dialog() {
        let mut config = Config::default();
        config.add(sample_server("prod"));
        let mut app = App::new(config);

        app.request_delete();
        assert!(matches!(app.mode, Mode::ConfirmDelete));
        // Cancelling keeps the server.
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('n'))).unwrap();
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.config.servers.len(), 1);
    }
}
