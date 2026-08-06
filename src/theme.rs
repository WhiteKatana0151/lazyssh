//! Centralized color palette and terminal-capability detection.
//!
//! Every color the UI uses lives here so rendering code never hardcodes
//! styles. True color is used when the terminal advertises it; otherwise the
//! palette degrades to the nearest ANSI-16 colors.

use std::env;
use std::sync::OnceLock;

use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Full-screen background wash.
    pub bg: Color,
    /// Background of the server card, footer bar, and dialogs.
    pub panel_bg: Color,
    /// Default border color for panels and the footer bar.
    pub border: Color,
    /// Primary cyan accent: titles, active fields, dialog borders.
    pub primary: Color,
    /// Muted gray-blue cyan for secondary copy like the tagline and hints.
    pub muted_primary: Color,
    /// Bright green accent: selection chevron, status dots, Enter hints.
    pub green: Color,
    /// Dim green backdrop behind the ↵ icon in the connect hint.
    pub green_dim_bg: Color,
    /// Background bar of the selected server row.
    pub selected_bg: Color,
    /// Name color inside the selected row.
    pub selected_fg: Color,
    /// Primary body text.
    pub text: Color,
    /// De-emphasized text.
    pub muted: Color,
    /// Warning accent: delete confirmation, validation errors.
    pub warn: Color,
    /// Muted dark-blue shadow layer behind the wordmark.
    pub logo_shadow: Color,
    /// Fill behind footer key caps.
    pub keycap_bg: Color,
    /// Text color inside the filled Enter key cap.
    pub keycap_primary_fg: Color,
    truecolor: bool,
}

impl Theme {
    pub const TRUECOLOR: Theme = Theme {
        bg: Color::Rgb(0x08, 0x0d, 0x16),
        panel_bg: Color::Rgb(0x0b, 0x11, 0x1c),
        border: Color::Rgb(0x2a, 0x34, 0x42),
        primary: Color::Rgb(0x55, 0xdf, 0xf5),
        muted_primary: Color::Rgb(0x6a, 0xa7, 0xbd),
        green: Color::Rgb(0x8b, 0xea, 0x72),
        green_dim_bg: Color::Rgb(0x0c, 0x2d, 0x1a),
        selected_bg: Color::Rgb(0x12, 0x3b, 0x55),
        selected_fg: Color::Rgb(0x55, 0xdf, 0xf5),
        text: Color::Rgb(0xd8, 0xde, 0xe9),
        muted: Color::Rgb(0x7f, 0x8b, 0x9d),
        warn: Color::Rgb(0xd2, 0x8b, 0x63),
        logo_shadow: Color::Rgb(0x28, 0x39, 0x5c),
        keycap_bg: Color::Rgb(0x2a, 0x34, 0x42),
        keycap_primary_fg: Color::Rgb(0x08, 0x0d, 0x16),
        truecolor: true,
    };

    pub const ANSI: Theme = Theme {
        bg: Color::Reset,
        panel_bg: Color::Reset,
        border: Color::DarkGray,
        primary: Color::Cyan,
        muted_primary: Color::Cyan,
        green: Color::Green,
        green_dim_bg: Color::Reset,
        selected_bg: Color::Blue,
        selected_fg: Color::LightCyan,
        text: Color::Gray,
        muted: Color::DarkGray,
        warn: Color::Yellow,
        logo_shadow: Color::DarkGray,
        keycap_bg: Color::DarkGray,
        keycap_primary_fg: Color::Black,
        truecolor: false,
    };

    pub fn detect() -> Self {
        if supports_truecolor() {
            Self::TRUECOLOR
        } else {
            Self::ANSI
        }
    }

    /// Wordmark stroke color for glyph row `row` of `rows`: bright green at
    /// the top blending into cyan at the bottom. ANSI terminals get two flat
    /// color bands instead of a gradient.
    pub fn logo_color(&self, row: usize, rows: usize) -> Color {
        let rows = rows.max(2);
        let row = row.min(rows - 1);
        if !self.truecolor {
            return if row < rows / 2 {
                Color::LightGreen
            } else {
                Color::LightCyan
            };
        }
        let (Color::Rgb(r0, g0, b0), Color::Rgb(r1, g1, b1)) = (self.green, self.primary) else {
            return self.primary;
        };
        let t = row as f32 / (rows - 1) as f32;
        let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
        Color::Rgb(lerp(r0, r1), lerp(g0, g1), lerp(b0, b1))
    }
}

/// Whether the terminal advertises 24-bit color. `COLORTERM` is the de facto
/// standard; Windows Terminal supports true color but only sets `WT_SESSION`.
fn supports_truecolor() -> bool {
    let colorterm = env::var("COLORTERM").unwrap_or_default().to_lowercase();
    colorterm.contains("truecolor")
        || colorterm.contains("24bit")
        || env::var_os("WT_SESSION").is_some()
}

/// The process-wide theme, detected once on first use.
pub fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(Theme::detect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truecolor_gradient_runs_green_to_cyan() {
        let t = Theme::TRUECOLOR;
        assert_eq!(t.logo_color(0, 6), t.green);
        assert_eq!(t.logo_color(5, 6), t.primary);
        // Out-of-range rows clamp to the bottom color.
        assert_eq!(t.logo_color(20, 6), t.primary);
    }

    #[test]
    fn ansi_gradient_falls_back_to_two_bands() {
        let t = Theme::ANSI;
        assert_eq!(t.logo_color(0, 6), Color::LightGreen);
        assert_eq!(t.logo_color(2, 6), Color::LightGreen);
        assert_eq!(t.logo_color(3, 6), Color::LightCyan);
        assert_eq!(t.logo_color(5, 6), Color::LightCyan);
    }
}
