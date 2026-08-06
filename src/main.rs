mod app;
mod config;
mod ssh;
mod theme;
mod ui;

use std::io::{self, BufRead, Stdout, Write};
use std::time::{Duration, Instant};

use anyhow::Result;
use app::{handle_key, App, AppExit};
use config::{Config, Server};
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

    match &result {
        Ok(AppExit::Connect) if app.selected_server().is_some() => {
            // Record the connection before handing off: on Unix `connect`
            // execs and never returns. A failed save shouldn't block the
            // connection.
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
        Ok(AppExit::Bootstrap(server)) => {
            let server = server.clone();
            run_bootstrap(server, &mut app.config);
        }
        _ => {}
    }

    match result {
        Ok(_) => Ok(()),
        Err(err) => Err(err),
    }
}

/// Runs the bootstrap in the normal terminal, after raw mode is torn down,
/// so any password prompt comes from `ssh` itself — LazySSH never touches
/// the password. On success, offers to save the entry to the config.
fn run_bootstrap(server: Server, config: &mut Config) {
    let destination = match &server.username {
        Some(user) => format!("{}@{}", user, server.host),
        None => server.host.clone(),
    };
    println!("Installing your public key on {destination} ...");
    println!("(ssh may ask for the remote password directly)");

    if let Err(err) = ssh::bootstrap(&server) {
        eprintln!("bootstrap failed: {err}");
        eprintln!("The server was not added to LazySSH.");
        return;
    }

    println!("Public key installed on {destination}.");
    print!("Add this server to LazySSH? [Y/n] ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    if io::stdin().lock().read_line(&mut answer).is_err() {
        answer.clear();
    }

    if wants_save(&answer) {
        let name = server.name.clone();
        config.add(server);
        if let Err(err) = config.save() {
            eprintln!("failed to save config: {err}");
        } else {
            println!("Saved {name}.");
        }
    } else {
        println!("Not saved.");
    }
}

/// Interprets the "Add this server?" answer; empty input means yes.
fn wants_save(answer: &str) -> bool {
    matches!(answer.trim(), "" | "y" | "Y" | "yes" | "Yes" | "YES")
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

#[cfg(test)]
mod tests {
    use super::wants_save;

    #[test]
    fn save_prompt_defaults_to_yes() {
        for yes in ["", "\n", "y", "Y", "yes\n", "Yes"] {
            assert!(wants_save(yes), "{yes:?} should save");
        }
        for no in ["n", "N", "no", "nope", "q"] {
            assert!(!wants_save(no), "{no:?} should not save");
        }
    }
}
