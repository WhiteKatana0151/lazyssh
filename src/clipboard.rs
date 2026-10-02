//! Copies text to the system clipboard without extra crates.
//!
//! Prefers a native helper (`wl-copy` on Wayland, `xclip`/`xsel` on X11)
//! and falls back to the OSC 52 terminal escape, which most modern
//! terminals — Kitty, WezTerm, foot, Windows Terminal, tmux with
//! `set-clipboard on` — honour even over SSH.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

/// How the text will reach the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// A helper program reading the text on stdin, with its fixed args.
    Program(PathBuf, &'static [&'static str]),
    /// The OSC 52 escape written to the terminal.
    Osc52,
}

/// Picks a route from display-server hints and installed helpers. Pure so
/// it can be tested without touching the environment.
pub fn choose_route(
    windows: bool,
    wayland: bool,
    x11: bool,
    find: impl Fn(&str) -> Option<PathBuf>,
) -> Route {
    if windows {
        if let Some(path) = find("clip") {
            return Route::Program(path, &[]);
        }
    }
    if wayland {
        if let Some(path) = find("wl-copy") {
            return Route::Program(path, &[]);
        }
    }
    if x11 {
        if let Some(path) = find("xclip") {
            return Route::Program(path, &["-selection", "clipboard"]);
        }
        if let Some(path) = find("xsel") {
            return Route::Program(path, &["--clipboard", "--input"]);
        }
    }
    Route::Osc52
}

/// Standard base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The OSC 52 "set clipboard" escape for `text`.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// Copies `text`, returning a short label of the method used.
pub fn copy(text: &str) -> Result<&'static str> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some();
    match choose_route(cfg!(windows), wayland, x11, crate::ssh::find_executable) {
        Route::Program(path, args) => {
            // Helper output would scribble over the TUI, so silence it.
            let mut child = Command::new(&path)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .with_context(|| format!("failed to start {}", path.display()))?;
            child
                .stdin
                .take()
                .context("clipboard helper has no stdin")?
                .write_all(text.as_bytes())?;
            // wl-copy/xclip fork a server and return, so this is quick.
            let status = child.wait()?;
            if !status.success() {
                bail!("{} exited with {status}", path.display());
            }
            Ok("clipboard")
        }
        Route::Osc52 => {
            let mut stdout = std::io::stdout();
            stdout.write_all(osc52(text).as_bytes())?;
            stdout.flush()?;
            Ok("terminal clipboard (OSC 52)")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(names: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        move |name| {
            names
                .contains(&name)
                .then(|| PathBuf::from(format!("/bin/{name}")))
        }
    }

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want, "{input}");
        }
    }

    #[test]
    fn osc52_wraps_base64_payload() {
        assert_eq!(osc52("ssh a"), "\x1b]52;c;c3NoIGE=\x07");
    }

    #[test]
    fn route_prefers_native_helpers_for_the_running_display() {
        assert_eq!(
            choose_route(false, true, true, only(&["wl-copy", "xclip"])),
            Route::Program("/bin/wl-copy".into(), &[])
        );
        assert!(matches!(
            choose_route(false, false, true, only(&["wl-copy", "xsel"])),
            Route::Program(p, _) if p.ends_with("xsel")
        ));
        assert!(matches!(
            choose_route(true, false, false, only(&["clip"])),
            Route::Program(p, _) if p.ends_with("clip")
        ));
        // No display, or no helper: the terminal does it.
        assert_eq!(
            choose_route(false, false, false, only(&["wl-copy"])),
            Route::Osc52
        );
        assert_eq!(choose_route(false, true, false, only(&[])), Route::Osc52);
    }
}
