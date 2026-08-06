mod app;
mod config;
mod ssh;
mod theme;
mod ui;

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::Result;
use app::{handle_key, App, AppExit};
use config::Config;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

/// How often the UI advances its tick counter (cursor blink) while idle.
/// Kept coarse so redraws stay cheap and the app never busy-loops.
const TICK_RATE: Duration = Duration::from_millis(120);

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

    if matches!(result, Ok(AppExit::Connect)) && app.selected_server().is_some() {
        // Record the connection before handing off: on Unix `connect` execs
        // and never returns. A failed save shouldn't block the connection.
        app.config
            .mark_connected(app.selected, config::now_unix_secs());
        if let Err(err) = app.config.save() {
            eprintln!("warning: failed to save connection history: {err}");
        }
        if let Some(server) = app.selected_server() {
            if let Err(err) = ssh::connect(server) {
                eprintln!("failed to run ssh: {err}");
            }
        }
    }

    match result {
        Ok(_) => Ok(()),
        Err(err) => Err(err),
    }
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn run_app(terminal: &mut Tui, app: &mut App) -> Result<AppExit> {
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|frame| ui::render(frame, app))?;

        let timeout = TICK_RATE.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if let Some(exit) = handle_key(app, key)? {
                    return Ok(exit);
                }
            }
        }

        if last_tick.elapsed() >= TICK_RATE {
            app.tick = app.tick.wrapping_add(1);
            last_tick = Instant::now();
        }
    }
}
