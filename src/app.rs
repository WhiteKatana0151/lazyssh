//! Application state and key handling. Rendering lives in [`crate::ui`];
//! persistence in [`crate::config`]; the ssh handoff in [`crate::ssh`].

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::config::{Config, Server};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Description,
    Host,
    Port,
    Username,
    IdentityFile,
    ExtraArgs,
}

impl Field {
    pub const ALL: [Field; 7] = [
        Field::Name,
        Field::Description,
        Field::Host,
        Field::Port,
        Field::Username,
        Field::IdentityFile,
        Field::ExtraArgs,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Field::Name => "Name",
            Field::Description => "Description",
            Field::Host => "Host / IP",
            Field::Port => "Port (optional)",
            Field::Username => "Username (optional)",
            Field::IdentityFile => "SSH key path (optional)",
            Field::ExtraArgs => "Extra ssh args (optional)",
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
    pub extra_args: String,
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
            extra_args: server.extra_args.clone().unwrap_or_default(),
        }
    }

    pub fn current_value_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::Name => &mut self.name,
            Field::Description => &mut self.description,
            Field::Host => &mut self.host,
            Field::Port => &mut self.port,
            Field::Username => &mut self.username,
            Field::IdentityFile => &mut self.identity_file,
            Field::ExtraArgs => &mut self.extra_args,
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
            Field::ExtraArgs => &self.extra_args,
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

        Ok(Server {
            name: name.to_string(),
            description: self.description.trim().to_string(),
            host: host.to_string(),
            port,
            username: optional_trimmed(&self.username),
            identity_file: optional_trimmed(&self.identity_file),
            extra_args: optional_trimmed(&self.extra_args),
            last_connected_at: None,
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
        // The host and user become the `user@host` ssh destination; a
        // leading '-' would let it be parsed as an ssh option instead.
        if server.host.starts_with('-') || server.username.as_deref().unwrap().starts_with('-') {
            return Err("Host and username must not start with '-'");
        }
        Ok(server)
    }
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
    /// The add/edit/bootstrap dialog.
    Form {
        draft: DraftServer,
        field: Field,
        purpose: FormPurpose,
    },
    /// The delete confirmation dialog for the selected server.
    ConfirmDelete,
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
    Connect,
    /// Leave the TUI and install this (not yet saved) server's public key on
    /// the remote host; the user is asked afterwards whether to save it.
    Bootstrap(Server),
}

#[derive(Debug)]
pub struct App {
    pub config: Config,
    pub selected: usize,
    pub mode: Mode,
    pub status: String,
    pub status_kind: StatusKind,
    pub tick: u64,
}

impl App {
    pub fn new(mut config: Config) -> Self {
        // Most recently connected first, so the last-used server is already
        // selected on launch.
        config.sort_by_recency();
        Self {
            config,
            selected: 0,
            mode: Mode::Normal,
            status: String::new(),
            status_kind: StatusKind::Hint,
            tick: 0,
        }
    }

    pub fn set_status(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.status_kind = kind;
        self.status = message.into();
    }

    pub fn selected_server(&self) -> Option<&Server> {
        self.config.servers.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if self.config.servers.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected + 1).min(self.config.servers.len() - 1);
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn open_add(&mut self) {
        self.mode = Mode::Form {
            draft: DraftServer::default(),
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
            draft,
            field: Field::Name,
            purpose: FormPurpose::Edit(self.selected),
        };
        self.status_kind = StatusKind::Hint;
    }

    pub fn open_bootstrap(&mut self) {
        self.mode = Mode::Form {
            draft: DraftServer::default(),
            field: Field::Name,
            purpose: FormPurpose::Bootstrap,
        };
        self.status_kind = StatusKind::Hint;
    }

    pub fn request_delete(&mut self) {
        if self.config.servers.is_empty() {
            self.set_status(StatusKind::Warn, "No servers to delete");
            return;
        }
        self.mode = Mode::ConfirmDelete;
        self.status_kind = StatusKind::Hint;
    }

    pub fn delete_selected(&mut self) -> Result<()> {
        let removed = self.config.remove(self.selected);
        self.config.save()?;

        if self.selected >= self.config.servers.len() {
            self.selected = self.config.servers.len().saturating_sub(1);
        }

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
                    Ok(Some(AppExit::Bootstrap(server)))
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
                self.config.save()?;
            }
            // Keep the dialog open so the input can be corrected.
            Err(reason) => self.set_status(StatusKind::Warn, reason),
        }

        Ok(())
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<Option<AppExit>> {
    if key.kind == KeyEventKind::Release {
        return Ok(None);
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Ok(Some(AppExit::Quit));
    }

    match &mut app.mode {
        Mode::Normal => match key.code {
            KeyCode::Char('q' | 'Q') | KeyCode::Esc => Ok(Some(AppExit::Quit)),
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
            KeyCode::Char('d' | 'D') => {
                app.request_delete();
                Ok(None)
            }
            KeyCode::Char('b' | 'B') => {
                app.open_bootstrap();
                Ok(None)
            }
            KeyCode::Enter => {
                if app.selected_server().is_some() {
                    Ok(Some(AppExit::Connect))
                } else {
                    app.set_status(StatusKind::Warn, "No server selected");
                    Ok(None)
                }
            }
            _ => Ok(None),
        },
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
            last_connected_at: None,
        }
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
            last_connected_at: None,
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
        assert_eq!(Field::ExtraArgs.next(), Field::ExtraArgs);
        assert!(Field::ExtraArgs.is_last());
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
