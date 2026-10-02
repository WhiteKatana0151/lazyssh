mod app;
mod cli;
mod clipboard;
mod config;
mod forwards;
mod probe;
mod ssh;
mod sshconfig;
mod theme;
mod ui;

use std::io::{self, BufRead, Stdout, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use app::{handle_key, App, AppExit};
use config::{Config, Server};
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use ssh::LaunchMode;

/// How often the UI advances its tick counter (cursor blink) while idle.
/// Kept coarse so redraws stay cheap and the app never busy-loops.
const TICK_RATE: Duration = Duration::from_millis(120);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse(&args) {
        Ok(command) => command,
        Err(err) => {
            eprintln!("lazyssh: {err}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    match run(command) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("lazyssh: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: cli::Command) -> Result<u8> {
    match command {
        cli::Command::Help => {
            print!("{}", cli::USAGE);
            Ok(0)
        }
        cli::Command::Version => {
            println!("lazyssh {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        cli::Command::List { tag } => {
            for row in cli::list_rows(&Config::load()?, tag.as_deref()) {
                println!("{row}");
            }
            Ok(0)
        }
        cli::Command::Print { name } => {
            let config = Config::load()?;
            let index = cli::find_server(&config, &name).map_err(anyhow::Error::msg)?;
            let resolved = config.resolved(&config.servers[index])?;
            println!("{}", ssh::command_line(&ssh::build_command(&resolved)));
            Ok(0)
        }
        cli::Command::Import { path } => {
            let path = match path {
                Some(path) => ssh::expand_tilde(&path),
                None => sshconfig::default_path().context("could not find a home directory")?,
            };
            let mut config = Config::load()?;
            let found = sshconfig::load(&path)?;
            let total = found.len();
            let added = config.import(found);
            if added > 0 {
                config.save()?;
            }
            println!(
                "Imported {added} new server(s) from {} ({} already saved).",
                path.display(),
                total - added
            );
            Ok(0)
        }
        cli::Command::Connect { name, mode } => {
            let mut config = Config::load()?;
            let index = cli::find_server(&config, &name).map_err(anyhow::Error::msg)?;
            launch(&mut config, index, mode, true)
        }
        cli::Command::Tui => run_tui(),
    }
}

/// Opens `config.servers[index]` in `mode`. With `replace`, the process is
/// replaced by the session on Unix; otherwise the session runs as a child
/// and its exit code is returned.
fn launch(config: &mut Config, index: usize, mode: LaunchMode, replace: bool) -> Result<u8> {
    let server = config.servers[index].clone();
    let resolved = config.resolved(&server)?;
    let cmd = match mode {
        LaunchMode::Ssh => {
            // Resolve before recording recency: a forced Kitty launcher that
            // is not installed is a configuration error, not a connection.
            let launcher = ssh::resolve_launcher_from_env(config.launcher)?;
            ssh::build_launch_command(&resolved, mode, launcher)
        }
        other => {
            if ssh::find_executable(other.program()).is_none() {
                bail!("`{}` is not installed or not on PATH", other.program());
            }
            ssh::build_launch_command(&resolved, other, ssh::Launcher::OpenSsh)
        }
    };
    // Record before handing off: on Unix with `replace` this never returns.
    // A failed save should not block the session.
    config.mark_connected(index, config::now_unix_secs());
    if let Err(err) = config.save() {
        eprintln!("warning: failed to save connection history: {err}");
    }
    let code = ssh::run_interactive(cmd, replace)
        .with_context(|| format!("failed to run {}", mode.program()))?;
    Ok(u8::try_from(code).unwrap_or(1))
}

fn run_tui() -> Result<u8> {
    let mut app = App::new(Config::load()?);
    app.refresh_reachability();

    loop {
        let exit = with_terminal(|terminal| run_app(terminal, &mut app))?;
        match exit {
            AppExit::Quit => {
                if !app.forwards.is_empty() {
                    println!("Stopped {} port forward(s).", app.forwards.len());
                }
                return Ok(0);
            }
            AppExit::Connect(mode) => {
                if app.selected_server().is_none() {
                    return Ok(0);
                }
                // With forwards running, LazySSH must outlive the session to
                // keep them up, so it waits and then reopens the TUI.
                let stay = !app.forwards.is_empty();
                let index = app.selected;
                let name = app.config.servers[index].name.clone();
                match launch(&mut app.config, index, mode, !stay) {
                    Ok(_) if stay => {
                        app.config.sort_by_recency();
                        app.selected = app.config.index_of_name(&name).unwrap_or(0);
                        app.set_status(
                            app::StatusKind::Info,
                            format!(
                                "Session ended; {} forward(s) still running",
                                app.forwards.len()
                            ),
                        );
                    }
                    Ok(code) => return Ok(code),
                    // Missing mosh/Kitty, a jump-host loop, or a failed
                    // exec: report it in the TUI instead of dropping out.
                    Err(err) => app.set_status(app::StatusKind::Warn, format!("{err:#}")),
                }
            }
            AppExit::Bootstrap(server) => {
                run_bootstrap(*server, &mut app.config);
                return Ok(0);
            }
        }
    }
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Runs `body` inside raw mode on the alternate screen, restoring the
/// terminal afterwards even when `body` fails.
fn with_terminal<T>(body: impl FnOnce(&mut Tui) -> Result<T>) -> Result<T> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = body(&mut terminal);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
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

    let resolved = match config.resolved(&server) {
        Ok(resolved) => resolved,
        Err(err) => {
            eprintln!("bootstrap failed: {err}");
            return;
        }
    };
    if let Err(err) = ssh::bootstrap(&resolved) {
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
            app.poll_background();
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
