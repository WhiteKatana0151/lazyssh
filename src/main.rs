mod config;
mod ssh;

use std::io::{self, Stdout};

use anyhow::Result;
use config::{Config, Server};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Description,
    Host,
    Username,
    IdentityFile,
}

impl Field {
    const ALL: [Field; 5] = [
        Field::Name,
        Field::Description,
        Field::Host,
        Field::Username,
        Field::IdentityFile,
    ];

    fn label(self) -> &'static str {
        match self {
            Field::Name => "Name",
            Field::Description => "Description",
            Field::Host => "Host / IP",
            Field::Username => "Username (optional)",
            Field::IdentityFile => "SSH key path (optional)",
        }
    }

    fn next(self) -> Self {
        let index = Self::ALL.iter().position(|field| *field == self).unwrap();
        Self::ALL[(index + 1).min(Self::ALL.len() - 1)]
    }

    fn prev(self) -> Self {
        let index = Self::ALL.iter().position(|field| *field == self).unwrap();
        Self::ALL[index.saturating_sub(1)]
    }
}

#[derive(Debug, Default)]
struct DraftServer {
    name: String,
    description: String,
    host: String,
    username: String,
    identity_file: String,
}

impl DraftServer {
    fn current_value_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::Name => &mut self.name,
            Field::Description => &mut self.description,
            Field::Host => &mut self.host,
            Field::Username => &mut self.username,
            Field::IdentityFile => &mut self.identity_file,
        }
    }

    fn current_value(&self, field: Field) -> &str {
        match field {
            Field::Name => &self.name,
            Field::Description => &self.description,
            Field::Host => &self.host,
            Field::Username => &self.username,
            Field::IdentityFile => &self.identity_file,
        }
    }

    fn into_server(self) -> Option<Server> {
        let name = self.name.trim();
        let host = self.host.trim();

        if name.is_empty() || host.is_empty() {
            return None;
        }

        Some(Server {
            name: name.to_string(),
            description: self.description.trim().to_string(),
            host: host.to_string(),
            username: optional_trimmed(self.username),
            identity_file: optional_trimmed(self.identity_file),
        })
    }
}

fn optional_trimmed(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[derive(Debug)]
enum Mode {
    Normal,
    Adding { draft: DraftServer, field: Field },
}

#[derive(Debug)]
struct App {
    config: Config,
    selected: usize,
    mode: Mode,
    status: String,
}

impl App {
    fn new(config: Config) -> Self {
        Self {
            config,
            selected: 0,
            mode: Mode::Normal,
            status: "a add  enter connect  d delete  q quit".to_string(),
        }
    }

    fn selected_server(&self) -> Option<&Server> {
        self.config.servers.get(self.selected)
    }

    fn select_next(&mut self) {
        if self.config.servers.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected + 1).min(self.config.servers.len() - 1);
    }

    fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn delete_selected(&mut self) -> Result<()> {
        if self.config.servers.is_empty() {
            self.status = "No servers to delete".to_string();
            return Ok(());
        }

        let removed = self.config.remove(self.selected);
        self.config.save()?;

        if self.selected >= self.config.servers.len() {
            self.selected = self.config.servers.len().saturating_sub(1);
        }

        if let Some(server) = removed {
            self.status = format!("Deleted {}", server.name);
        }
        Ok(())
    }

    fn save_draft(&mut self) -> Result<()> {
        let Mode::Adding { draft, .. } = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return Ok(());
        };

        match draft.into_server() {
            Some(server) => {
                let name = server.name.clone();
                self.config.add(server);
                self.config.save()?;
                self.selected = self.config.servers.len().saturating_sub(1);
                self.status = format!("Saved {name}");
            }
            None => {
                self.status = "Name and host are required; add cancelled".to_string();
            }
        }

        Ok(())
    }
}

fn main() -> Result<()> {
    let config = Config::load()?;
    let mut app = App::new(config);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Some(server) = app
        .selected_server()
        .filter(|_| matches!(result, Ok(AppExit::Connect)))
    {
        if let Err(err) = ssh::connect(server) {
            eprintln!("failed to run ssh: {err}");
        }
    }

    match result {
        Ok(_) => Ok(()),
        Err(err) => Err(err),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppExit {
    Quit,
    Connect,
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn run_app(terminal: &mut Tui, app: &mut App) -> Result<AppExit> {
    loop {
        terminal.draw(|frame| render(frame, app))?;

        if let Event::Key(key) = event::read()? {
            if let Some(exit) = handle_key(app, key)? {
                return Ok(exit);
            }
        }
    }
}

fn handle_key(app: &mut App, key: KeyEvent) -> Result<Option<AppExit>> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Ok(Some(AppExit::Quit));
    }

    match &mut app.mode {
        Mode::Normal => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Ok(Some(AppExit::Quit)),
            KeyCode::Char('j') | KeyCode::Down => {
                app.select_next();
                Ok(None)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                app.select_prev();
                Ok(None)
            }
            KeyCode::Char('a') => {
                app.mode = Mode::Adding {
                    draft: DraftServer::default(),
                    field: Field::Name,
                };
                app.status = "Add server: enter values, Enter advances, Ctrl+s saves, Esc cancels"
                    .to_string();
                Ok(None)
            }
            KeyCode::Char('d') => {
                app.delete_selected()?;
                Ok(None)
            }
            KeyCode::Enter => {
                if app.selected_server().is_some() {
                    Ok(Some(AppExit::Connect))
                } else {
                    app.status = "No server selected".to_string();
                    Ok(None)
                }
            }
            _ => Ok(None),
        },
        Mode::Adding { draft, field } => match key.code {
            KeyCode::Esc => {
                app.mode = Mode::Normal;
                app.status = "Add cancelled".to_string();
                Ok(None)
            }
            KeyCode::Enter | KeyCode::Tab => {
                if *field == Field::IdentityFile {
                    app.save_draft()?;
                } else {
                    *field = field.next();
                }
                Ok(None)
            }
            KeyCode::BackTab => {
                *field = field.prev();
                Ok(None)
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.save_draft()?;
                Ok(None)
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
    }
}

fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let title = Paragraph::new("lazyssh")
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, chunks[0]);

    render_server_list(frame, app, chunks[1]);

    let status = Paragraph::new(app.status.as_str())
        .style(Style::default().fg(Color::Gray))
        .block(Block::default().borders(Borders::ALL).title("keys"));
    frame.render_widget(status, chunks[2]);

    if let Mode::Adding { draft, field } = &app.mode {
        render_add_popup(frame, draft, *field);
    }
}

fn render_server_list(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = if app.config.servers.is_empty() {
        vec![ListItem::new(Line::from(vec![Span::styled(
            "No servers yet. Press a to add one.",
            Style::default().fg(Color::DarkGray),
        )]))]
    } else {
        app.config
            .servers
            .iter()
            .map(|server| {
                let target = match &server.username {
                    Some(user) if !user.is_empty() => format!("{}@{}", user, server.host),
                    _ => server.host.clone(),
                };
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(&server.name, Style::default().fg(Color::Cyan).bold()),
                        Span::raw("  "),
                        Span::styled(target, Style::default().fg(Color::DarkGray)),
                    ]),
                    Line::from(vec![Span::styled(
                        if server.description.is_empty() {
                            "no description"
                        } else {
                            &server.description
                        },
                        Style::default().fg(Color::Gray),
                    )]),
                ])
            })
            .collect()
    };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("servers"))
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("❯ ");

    let mut state = ListState::default();
    if !app.config.servers.is_empty() {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_add_popup(frame: &mut Frame, draft: &DraftServer, active: Field) {
    let area = centered_rect(70, 55, frame.area());
    frame.render_widget(Clear, area);

    let mut lines = vec![
        Line::from("Add SSH server".magenta().bold()),
        Line::from("Enter/Tab next, Shift+Tab previous, Ctrl+s save, Esc cancel"),
        Line::from(""),
    ];

    for field in Field::ALL {
        let marker = if field == active { "❯" } else { " " };
        let style = if field == active {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} {}: ", field.label()), style),
            Span::raw(draft.current_value(field).to_string()),
        ]));
    }

    let popup = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("new server"));
    frame.render_widget(popup, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_requires_name_and_host() {
        let draft = DraftServer {
            name: "prod".to_string(),
            description: "Production".to_string(),
            host: "".to_string(),
            username: String::new(),
            identity_file: String::new(),
        };

        assert!(draft.into_server().is_none());
    }

    #[test]
    fn draft_trims_optional_values() {
        let draft = DraftServer {
            name: " prod ".to_string(),
            description: " Production ".to_string(),
            host: " 10.0.0.5 ".to_string(),
            username: " sam ".to_string(),
            identity_file: " ~/.ssh/id_ed25519 ".to_string(),
        };

        let server = draft.into_server().unwrap();
        assert_eq!(server.name, "prod");
        assert_eq!(server.description, "Production");
        assert_eq!(server.host, "10.0.0.5");
        assert_eq!(server.username.as_deref(), Some("sam"));
        assert_eq!(server.identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
    }
}
