//! All rendering: layout math, the neon LAZYSSH wordmark, the server card,
//! the footer command bar, and the modal dialogs. State lives in
//! [`crate::app`]; every color comes from [`crate::theme`].

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, DraftServer, Field, Mode, StatusKind};
use crate::config::Server;
use crate::theme::{theme, Theme};

/// Terminal width at which the dashboard switches from a compact single-line
/// brand to the full neon wordmark + tagline treatment.
const WIDE_LAYOUT_MIN_WIDTH: u16 = 84;

/// Terminal height, on top of `WIDE_LAYOUT_MIN_WIDTH`, required before the
/// tall multi-row logo header is shown instead of the compact one.
const HEADER_TALL_MIN_HEIGHT: u16 = 24;
const HEADER_HEIGHT_TALL: u16 = 11;
const HEADER_HEIGHT_COMPACT: u16 = 2;
const FOOTER_HEIGHT: u16 = 3;

/// The LAZYSSH wordmark. Every line must have the same visible width; the
/// same text is drawn twice — once as a shadow, once as the main layer — to
/// get the LazyVim-style depth effect.
const LOGO_LINES: [&str; 6] = [
    "██╗      █████╗ ███████╗██╗   ██╗███████╗███████╗██╗  ██╗",
    "██║     ██╔══██╗╚══███╔╝╚██╗ ██╔╝██╔════╝██╔════╝██║  ██║",
    "██║     ███████║  ███╔╝  ╚████╔╝ ███████╗███████╗███████║",
    "██║     ██╔══██║ ███╔╝    ╚██╔╝  ╚════██║╚════██║██╔══██║",
    "███████╗██║  ██║███████╗   ██║   ███████║███████║██║  ██║",
    "╚══════╝╚═╝  ╚═╝╚══════╝   ╚═╝   ╚══════╝╚══════╝╚═╝  ╚═╝",
];

/// Offset of the shadow layer relative to the main logo layer.
const LOGO_SHADOW_DX: usize = 2;
const LOGO_SHADOW_DY: usize = 1;
/// Left margin on the logo canvas, leaving room for decorative marks.
const LOGO_MARGIN: usize = 4;
/// Extra canvas columns right of the logo for the z trail and shadow overhang.
const LOGO_RIGHT_PAD: usize = 10;

/// Decorative tone for a sparkle mark, resolved against the active theme.
#[derive(Debug, Clone, Copy)]
enum MarkTone {
    Green,
    Cyan,
}

const SUBTITLE: &str = "SSH made simple. Connect. Work. Done.";
const SUBTITLE_DIVIDER_WIDTH: usize = 12;

const NORMAL_HELP: &[(&str, &str)] = &[
    ("A", "Add"),
    ("E", "Edit"),
    ("D", "Delete"),
    ("Enter", "Connect"),
    ("Q", "Quit"),
];
const NORMAL_HELP_SHORT: &[(&str, &str)] = &[("A", ""), ("E", ""), ("D", ""), ("↵", ""), ("Q", "")];

const FORM_HELP: &[(&str, &str)] = &[
    ("Enter/Tab", "Next"),
    ("Shift+Tab", "Prev"),
    ("Ctrl+S", "Save"),
    ("Esc", "Cancel"),
];
const FORM_HELP_SHORT: &[(&str, &str)] = &[("Ctrl+S", "Save"), ("Esc", "Cancel")];

const CONFIRM_HELP: &[(&str, &str)] = &[("Y", "Delete"), ("N", "Cancel")];

pub fn render(frame: &mut Frame, app: &App) {
    let t = theme();
    let area = frame.area();

    frame.render_widget(Block::default().style(Style::default().bg(t.bg)), area);

    let wide = use_wide_layout(area.width);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height(wide, area.height)),
            Constraint::Min(5),
            Constraint::Length(FOOTER_HEIGHT),
        ])
        .split(area);

    render_header(frame, sections[0], wide, t);
    render_main(frame, app, sections[1], t);
    render_footer(frame, app, sections[2], t);

    match &app.mode {
        Mode::Form {
            draft,
            field,
            editing,
        } => render_form_popup(frame, app, draft, *field, editing.is_some(), t),
        Mode::ConfirmDelete => render_confirm_popup(frame, app, t),
        Mode::Normal => {}
    }
}

/// Whether the terminal is wide enough for the full neon wordmark treatment.
fn use_wide_layout(width: u16) -> bool {
    width >= WIDE_LAYOUT_MIN_WIDTH
}

/// Height reserved for the header: the tall multi-row logo when the terminal
/// is both wide and tall enough, otherwise a compact two-line brand strip.
fn header_height(wide: bool, terminal_height: u16) -> u16 {
    if wide && terminal_height >= HEADER_TALL_MIN_HEIGHT {
        HEADER_HEIGHT_TALL
    } else {
        HEADER_HEIGHT_COMPACT
    }
}

/// Blink state for the fake text-input cursor in the form popup.
fn cursor_visible(tick: u64) -> bool {
    (tick / 4).is_multiple_of(2)
}

/// Renders `pairs` as `[ key ] label │ [ key ] label ...` on one line, used
/// for the footer command bar and the popup headers. The Enter/↵ key cap is
/// filled with the green accent since connecting is the primary action.
fn hint_line(pairs: &[(&str, &str)], t: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, desc)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  │  ", Style::default().fg(t.border)));
        }
        let key_style = if *key == "Enter" || *key == "↵" {
            Style::default()
                .bg(t.green)
                .fg(t.keycap_primary_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .bg(t.keycap_bg)
                .fg(t.primary)
                .add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled(format!(" {key} "), key_style));
        if !desc.is_empty() {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                (*desc).to_string(),
                Style::default().fg(t.muted),
            ));
        }
    }
    Line::from(spans)
}

/// Width of the centered server card, in cells. Pure so the clamping logic
/// can be tested without constructing a `Rect`.
fn card_width(area_width: u16) -> u16 {
    area_width.saturating_sub(6).clamp(20, 62)
}

/// Card height: one line per server plus the SERVERS header, its divider,
/// one row of vertical padding on each side, and the borders.
fn card_height(rows: usize, empty: bool) -> u16 {
    if empty {
        10
    } else {
        rows as u16 + 6
    }
}

/// How many server rows fit below the header + divider in `inner_height`
/// cells.
fn visible_rows(inner_height: u16) -> usize {
    inner_height.saturating_sub(2) as usize
}

/// First visible row index so `selected` always stays on screen.
fn scroll_offset(selected: usize, total: usize, visible: usize) -> usize {
    if visible == 0 || total <= visible {
        return 0;
    }
    selected
        .saturating_sub(visible.saturating_sub(1))
        .min(total - visible)
}

/// Truncates `label` to at most `max` characters, ellipsizing when needed.
fn truncate_label(label: &str, max: usize) -> String {
    if label.chars().count() <= max {
        label.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let mut shortened: String = label.chars().take(max - 1).collect();
        shortened.push('…');
        shortened
    }
}

/// How far to offset `content` inside `container` to center it along one axis.
fn center_offset(container: u16, content: u16) -> u16 {
    container.saturating_sub(content) / 2
}

/// Width of the bordered footer command bar for a given content width.
fn footer_bar_width(content_width: u16, area_width: u16) -> u16 {
    content_width.saturating_add(6).min(area_width)
}

/// A `width` x `height` rect centered inside `area`, clamped to fit.
fn centered_fixed(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(
        area.x + center_offset(area.width, w),
        area.y + center_offset(area.height, h),
        w,
        h,
    )
}

fn logo_width() -> usize {
    LOGO_LINES[0].chars().count()
}

/// Canvas size: the logo sits at (1, `LOGO_MARGIN`), its shadow one row down
/// and two columns right, with the z trail floating off the top-right edge.
fn logo_canvas_size() -> (usize, usize) {
    (
        LOGO_MARGIN + logo_width() + LOGO_SHADOW_DX + LOGO_RIGHT_PAD,
        LOGO_LINES.len() + LOGO_SHADOW_DY + 1,
    )
}

fn logo_grid() -> Vec<Vec<char>> {
    LOGO_LINES
        .iter()
        .map(|line| line.chars().collect())
        .collect()
}

/// Character of one logo layer at canvas cell (`row`, `col`): the layer's
/// origin is (1 + `dy`, `LOGO_MARGIN` + `dx`). Spaces count as transparent.
fn layer_char(grid: &[Vec<char>], row: usize, col: usize, dy: usize, dx: usize) -> Option<char> {
    let r = row.checked_sub(1 + dy)?;
    let c = col.checked_sub(LOGO_MARGIN + dx)?;
    grid.get(r)?.get(c).copied().filter(|ch| *ch != ' ')
}

/// Decorative sparkle marks around the wordmark: green z's drifting up and to
/// the right of the logo, plus a few cyan/green plus signs on the sides.
/// Positions are (row, col) on the logo canvas and never overlap the logo.
fn logo_marks() -> Vec<(usize, usize, char, MarkTone)> {
    let edge = LOGO_MARGIN + logo_width();
    vec![
        (1, 1, '+', MarkTone::Cyan),
        (4, 1, '+', MarkTone::Green),
        (0, edge + 8, 'z', MarkTone::Green),
        (1, edge + 6, 'z', MarkTone::Green),
        (2, edge + 4, 'z', MarkTone::Green),
        (6, edge + 6, '+', MarkTone::Cyan),
    ]
}

fn mark_at(
    marks: &[(usize, usize, char, MarkTone)],
    row: usize,
    col: usize,
) -> Option<(char, MarkTone)> {
    marks
        .iter()
        .find(|&&(r, c, ..)| r == row && c == col)
        .map(|&(.., ch, tone)| (ch, tone))
}

/// Whether canvas cell (`row`, `col`) lies inside the main logo's bounding
/// box. The main layer is printed as whole lines, so its spaces are opaque
/// there and cover the shadow underneath.
fn in_main_logo_bounds(row: usize, col: usize) -> bool {
    (1..=LOGO_LINES.len()).contains(&row)
        && (LOGO_MARGIN..LOGO_MARGIN + logo_width()).contains(&col)
}

/// Composites the two logo layers and the decorative marks: the shadow is the
/// same text offset by (+2, +1) in a muted dark blue, and the main layer is
/// printed over it with the green-to-cyan gradient, so the shadow only peeks
/// out along the bottom-right silhouette.
fn logo_lines(t: &Theme) -> Vec<Line<'static>> {
    let grid = logo_grid();
    let marks = logo_marks();
    let (width, height) = logo_canvas_size();

    (0..height)
        .map(|row| {
            let spans = (0..width)
                .map(|col| {
                    if let Some(ch) = layer_char(&grid, row, col, 0, 0) {
                        Span::styled(
                            ch.to_string(),
                            Style::default()
                                .fg(t.logo_color(row.saturating_sub(1), LOGO_LINES.len()))
                                .add_modifier(Modifier::BOLD),
                        )
                    } else if in_main_logo_bounds(row, col) {
                        Span::raw(" ")
                    } else if let Some(ch) =
                        layer_char(&grid, row, col, LOGO_SHADOW_DY, LOGO_SHADOW_DX)
                    {
                        Span::styled(ch.to_string(), Style::default().fg(t.logo_shadow))
                    } else if let Some((mark, tone)) = mark_at(&marks, row, col) {
                        let color = match tone {
                            MarkTone::Green => t.green,
                            MarkTone::Cyan => t.primary,
                        };
                        Span::styled(mark.to_string(), Style::default().fg(color))
                    } else {
                        Span::raw(" ")
                    }
                })
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect()
}

/// `────────  •  tagline  •  ────────`, all on one centered line.
fn subtitle_line(t: &Theme) -> Line<'static> {
    let divider = "─".repeat(SUBTITLE_DIVIDER_WIDTH);
    Line::from(vec![
        Span::styled(divider.clone(), Style::default().fg(t.border)),
        Span::styled("  •  ", Style::default().fg(t.green)),
        Span::styled(SUBTITLE, Style::default().fg(t.muted_primary)),
        Span::styled("  •  ", Style::default().fg(t.green)),
        Span::styled(divider, Style::default().fg(t.border)),
    ])
}

fn render_header(frame: &mut Frame, area: Rect, wide: bool, t: &Theme) {
    let lines = if wide && area.height >= HEADER_HEIGHT_TALL {
        let mut lines = vec![Line::raw("")];
        lines.extend(logo_lines(t));
        lines.push(Line::raw(""));
        lines.push(subtitle_line(t));
        lines
    } else {
        vec![
            Line::styled(
                "LazySSH",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ),
            Line::styled(SUBTITLE, Style::default().fg(t.muted_primary)),
        ]
    };

    let paragraph = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(paragraph, area);
}

/// Places the centered server card in `area`, plus the "press Enter to
/// connect" hint beneath it when there is room.
fn render_main(frame: &mut Frame, app: &App, area: Rect, t: &Theme) {
    let rows = app.config.servers.len();
    let empty = rows == 0;
    let width = card_width(area.width).min(area.width);
    let height = card_height(rows, empty).min(area.height);
    let show_hint = area.height > height.saturating_add(1);
    let reserved = if show_hint {
        height.saturating_add(2)
    } else {
        height
    };

    let x = area.x + center_offset(area.width, width);
    let y = area.y + center_offset(area.height, reserved);
    let card = Rect::new(x, y, width, height);

    render_server_card(frame, app, card, t);

    if show_hint {
        let hint_y = card.y.saturating_add(card.height).saturating_add(1);
        if hint_y < area.y.saturating_add(area.height) {
            render_connect_hint(frame, Rect::new(area.x, hint_y, area.width, 1), t);
        }
    }
}

/// Recency treatment for a server row's trailing status dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowRecency {
    /// The most recently connected server; gets a dim `recent` tag.
    Recent,
    /// Connected before, but not most recently.
    Connected,
    /// Never connected; the status dot renders dimmed.
    Never,
}

/// Index of the most recently connected server, if any has been connected.
fn most_recent_index(servers: &[Server]) -> Option<usize> {
    servers
        .iter()
        .enumerate()
        .filter_map(|(i, server)| server.last_connected_at.map(|at| (at, i)))
        .max_by_key(|&(at, _)| at)
        .map(|(_, i)| i)
}

/// One server row: `  ❯ ▣ name ······ ●  `, padded to exactly `width`
/// characters so the selection bar can tint the full card width. The most
/// recently used server carries a dim `recent` tag before its dot.
fn server_row_line(
    name: &str,
    selected: bool,
    width: usize,
    recency: RowRecency,
    t: &Theme,
) -> Line<'static> {
    let tag = if recency == RowRecency::Recent {
        "recent "
    } else {
        ""
    };
    // Chrome around the name: 2 lead + 2 chevron + 2 icon + 1 dot + 2 trail.
    let chrome = 9 + tag.chars().count();
    let label = truncate_label(name, width.saturating_sub(chrome + 1));
    let pad = width.saturating_sub(chrome + label.chars().count()).max(1);

    let (chevron, icon_style, name_style) = if selected {
        (
            "❯ ",
            Style::default().fg(t.primary),
            Style::default()
                .fg(t.selected_fg)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            "  ",
            Style::default().fg(t.muted),
            Style::default().fg(t.text),
        )
    };

    let dot_color = if recency == RowRecency::Never {
        t.muted
    } else {
        t.green
    };

    let line = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            chevron,
            Style::default().fg(t.green).add_modifier(Modifier::BOLD),
        ),
        Span::styled("▣ ", icon_style),
        Span::styled(label, name_style),
        Span::raw(" ".repeat(pad)),
        Span::styled(tag.to_string(), Style::default().fg(t.muted)),
        Span::styled("●", Style::default().fg(dot_color)),
        Span::raw("  "),
    ]);

    if selected {
        line.style(Style::default().bg(t.selected_bg))
    } else {
        line
    }
}

/// The SERVERS header row inside the card, with its icon aligned to the row
/// icons below it, followed by a divider line.
fn card_header_lines(inner_width: u16, t: &Theme) -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            Span::raw("    "),
            Span::styled("▣ ", Style::default().fg(t.primary)),
            Span::styled(
                "SERVERS",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::styled(
            "─".repeat(inner_width as usize),
            Style::default().fg(t.border),
        ),
    ]
}

fn render_server_card(frame: &mut Frame, app: &App, area: Rect, t: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border))
        .style(Style::default().bg(t.panel_bg))
        .padding(Padding::new(0, 0, 1, 1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.config.servers.is_empty() {
        render_empty_state(frame, inner, t);
        return;
    }

    let total = app.config.servers.len();
    let visible = visible_rows(inner.height);
    let offset = scroll_offset(app.selected, total, visible);

    let most_recent = most_recent_index(&app.config.servers);
    let mut lines = card_header_lines(inner.width, t);
    for (i, server) in app
        .config
        .servers
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible)
    {
        let recency = if most_recent == Some(i) {
            RowRecency::Recent
        } else if server.last_connected_at.is_some() {
            RowRecency::Connected
        } else {
            RowRecency::Never
        };
        lines.push(server_row_line(
            &server.name,
            i == app.selected,
            inner.width as usize,
            recency,
            t,
        ));
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(t.panel_bg)),
        inner,
    );
}

fn render_empty_state(frame: &mut Frame, area: Rect, t: &Theme) {
    let mut lines = card_header_lines(area.width, t);
    lines.extend([
        Line::raw(""),
        Line::styled(
            "no servers yet",
            Style::default().fg(t.muted).add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center),
        Line::raw(""),
        Line::from(vec![
            Span::styled("press ", Style::default().fg(t.muted)),
            Span::styled(
                " A ",
                Style::default()
                    .bg(t.keycap_bg)
                    .fg(t.primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" to add your first server", Style::default().fg(t.muted)),
        ])
        .alignment(Alignment::Center),
    ]);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(t.panel_bg)),
        area,
    );
}

fn render_connect_hint(frame: &mut Frame, area: Rect, t: &Theme) {
    let line = Line::from(vec![
        Span::styled(
            " ↵ ",
            Style::default()
                .fg(t.green)
                .bg(t.green_dim_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  Press ", Style::default().fg(t.muted_primary)),
        Span::styled(
            "Enter",
            Style::default().fg(t.green).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" to connect", Style::default().fg(t.muted_primary)),
    ])
    .alignment(Alignment::Center);
    frame.render_widget(Paragraph::new(line), area);
}

fn help_for(mode: &Mode) -> &'static [(&'static str, &'static str)] {
    match mode {
        Mode::Normal => NORMAL_HELP,
        Mode::Form { .. } => FORM_HELP,
        Mode::ConfirmDelete => CONFIRM_HELP,
    }
}

fn short_help_for(mode: &Mode) -> &'static [(&'static str, &'static str)] {
    match mode {
        Mode::Normal => NORMAL_HELP_SHORT,
        Mode::Form { .. } => FORM_HELP_SHORT,
        Mode::ConfirmDelete => CONFIRM_HELP,
    }
}

/// The bottom command bar: a centered, bordered box holding either the
/// contextual key badges or the latest status message. Falls back to a
/// shortened badge set when the terminal is too narrow for the full one.
fn render_footer(frame: &mut Frame, app: &App, area: Rect, t: &Theme) {
    let line = match app.status_kind {
        StatusKind::Hint => {
            let full = hint_line(help_for(&app.mode), t);
            if (full.width() as u16).saturating_add(4) > area.width {
                hint_line(short_help_for(&app.mode), t)
            } else {
                full
            }
        }
        StatusKind::Success => Line::from(vec![
            Span::styled("✓ ", Style::default().fg(t.green)),
            Span::styled(app.status.clone(), Style::default().fg(t.green)),
        ]),
        StatusKind::Warn => Line::from(vec![
            Span::styled("! ", Style::default().fg(t.warn)),
            Span::styled(app.status.clone(), Style::default().fg(t.warn)),
        ]),
        StatusKind::Info => Line::from(vec![
            Span::styled("i ", Style::default().fg(t.primary)),
            Span::styled(app.status.clone(), Style::default().fg(t.primary)),
        ]),
    };

    let bar_width = footer_bar_width(line.width() as u16, area.width);
    let bar = Rect::new(
        area.x + center_offset(area.width, bar_width),
        area.y,
        bar_width,
        area.height.min(FOOTER_HEIGHT),
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border))
        .style(Style::default().bg(t.panel_bg));
    let inner = block.inner(bar);
    frame.render_widget(block, bar);
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Center), inner);
}

fn render_form_popup(
    frame: &mut Frame,
    app: &App,
    draft: &DraftServer,
    active: Field,
    editing: bool,
    t: &Theme,
) {
    // Fields + hint line + spacer, plus padding and borders.
    let height = Field::ALL.len() as u16 + 6;
    let width = frame.area().width.saturating_sub(4).min(72);
    let area = centered_fixed(width, height, frame.area());
    frame.render_widget(Clear, area);

    let (icon, title) = if editing {
        (" ✎ ", "EDIT SERVER ")
    } else {
        (" ✚ ", "ADD SERVER ")
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.primary))
        .style(Style::default().bg(t.panel_bg))
        .padding(Padding::new(2, 2, 1, 1))
        .title(Line::from(vec![
            Span::styled(icon, Style::default().fg(t.green)),
            Span::styled(
                title,
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![hint_line(FORM_HELP, t), Line::raw("")];

    for field in Field::ALL {
        let is_active = field == active;
        let marker = if is_active { "❯" } else { " " };
        let label_style = if is_active {
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(t.muted)
        };
        let mut spans = vec![
            Span::styled(format!("{marker} {:<26}", field.label()), label_style),
            Span::styled(
                draft.current_value(field).to_string(),
                Style::default().fg(t.text),
            ),
        ];
        if is_active && cursor_visible(app.tick) {
            spans.push(Span::styled("▏", Style::default().fg(t.primary)));
        }
        lines.push(Line::from(spans));
    }

    let popup = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(popup, inner);
}

fn render_confirm_popup(frame: &mut Frame, app: &App, t: &Theme) {
    let name = app
        .selected_server()
        .map(|server| server.name.clone())
        .unwrap_or_default();

    let width = frame.area().width.saturating_sub(4).min(50);
    let area = centered_fixed(width, 9, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.warn))
        .style(Style::default().bg(t.panel_bg))
        .padding(Padding::new(2, 2, 1, 1))
        .title(Line::from(vec![
            Span::styled(" ⚠ ", Style::default().fg(t.warn)),
            Span::styled(
                "DELETE SERVER ",
                Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
            ),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = vec![
        Line::from(vec![
            Span::styled("Delete ", Style::default().fg(t.text)),
            Span::styled(
                name,
                Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
            ),
            Span::styled("?", Style::default().fg(t.text)),
        ]),
        Line::raw(""),
        Line::styled("This cannot be undone.", Style::default().fg(t.muted)),
        Line::raw(""),
        hint_line(CONFIRM_HELP, t),
    ];

    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::config::{Config, Server};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn sample_server(name: &str) -> Server {
        Server {
            name: name.to_string(),
            description: String::new(),
            host: format!("{name}.example.com"),
            port: None,
            username: None,
            identity_file: None,
            extra_args: None,
            last_connected_at: None,
        }
    }

    fn sample_app(count: usize) -> App {
        let mut config = Config::default();
        for i in 0..count {
            config.add(sample_server(&format!("server-{i}")));
        }
        App::new(config)
    }

    #[test]
    fn wide_layout_kicks_in_at_threshold() {
        assert!(!use_wide_layout(WIDE_LAYOUT_MIN_WIDTH - 1));
        assert!(use_wide_layout(WIDE_LAYOUT_MIN_WIDTH));
    }

    #[test]
    fn header_height_expands_when_wide_and_tall() {
        assert_eq!(
            header_height(true, HEADER_TALL_MIN_HEIGHT - 1),
            HEADER_HEIGHT_COMPACT
        );
        assert_eq!(
            header_height(true, HEADER_TALL_MIN_HEIGHT),
            HEADER_HEIGHT_TALL
        );
        assert_eq!(header_height(false, 100), HEADER_HEIGHT_COMPACT);
    }

    #[test]
    fn cursor_blinks_on_a_fixed_period() {
        assert!(cursor_visible(0));
        assert!(cursor_visible(3));
        assert!(!cursor_visible(4));
        assert!(!cursor_visible(7));
        assert!(cursor_visible(8));
    }

    #[test]
    fn card_width_clamps_between_bounds() {
        assert_eq!(card_width(200), 62);
        assert_eq!(card_width(10), 20);
    }

    #[test]
    fn card_height_accounts_for_header_and_chrome() {
        assert_eq!(card_height(0, true), 10);
        assert_eq!(card_height(1, false), 7);
        assert_eq!(card_height(3, false), 9);
        assert_eq!(card_height(10, false), 16);
    }

    #[test]
    fn visible_rows_reserves_header_and_divider() {
        assert_eq!(visible_rows(0), 0);
        assert_eq!(visible_rows(2), 0);
        assert_eq!(visible_rows(3), 1);
        assert_eq!(visible_rows(5), 3);
    }

    #[test]
    fn scroll_offset_keeps_selection_visible() {
        assert_eq!(scroll_offset(0, 10, 3), 0);
        assert_eq!(scroll_offset(2, 10, 3), 0);
        assert_eq!(scroll_offset(5, 10, 3), 3);
        assert_eq!(scroll_offset(9, 10, 3), 7);
        assert_eq!(scroll_offset(1, 2, 5), 0);
        assert_eq!(scroll_offset(1, 2, 0), 0);
    }

    #[test]
    fn truncate_label_ellipsizes_long_names() {
        assert_eq!(truncate_label("web", 10), "web");
        assert_eq!(truncate_label("abcdef", 4), "abc…");
        assert_eq!(truncate_label("abcdef", 0), "");
    }

    #[test]
    fn server_row_line_fills_exact_width() {
        for width in [20usize, 40, 60] {
            for selected in [false, true] {
                for recency in [RowRecency::Recent, RowRecency::Connected, RowRecency::Never] {
                    let line = server_row_line(
                        "staging-eu-west",
                        selected,
                        width,
                        recency,
                        &Theme::TRUECOLOR,
                    );
                    assert_eq!(line.width(), width, "width {width}, recency {recency:?}");
                }
            }
        }
    }

    #[test]
    fn most_recent_index_finds_latest_connection() {
        let mut servers = vec![sample_server("a"), sample_server("b"), sample_server("c")];
        assert_eq!(most_recent_index(&servers), None);

        servers[0].last_connected_at = Some(100);
        servers[2].last_connected_at = Some(300);
        assert_eq!(most_recent_index(&servers), Some(2));
    }

    #[test]
    fn center_offset_splits_remaining_space() {
        assert_eq!(center_offset(10, 4), 3);
        assert_eq!(center_offset(4, 10), 0);
    }

    #[test]
    fn footer_bar_width_clamps_to_area() {
        assert_eq!(footer_bar_width(40, 120), 46);
        assert_eq!(footer_bar_width(40, 30), 30);
    }

    #[test]
    fn centered_fixed_clamps_to_area() {
        let area = Rect::new(0, 0, 20, 10);
        let rect = centered_fixed(50, 50, area);
        assert_eq!(rect, area);
        let rect = centered_fixed(10, 4, area);
        assert_eq!(rect, Rect::new(5, 3, 10, 4));
    }

    #[test]
    fn logo_lines_have_uniform_width() {
        let width = logo_width();
        assert!(width > 0);
        for line in LOGO_LINES {
            assert_eq!(line.chars().count(), width, "ragged logo line {line:?}");
        }
    }

    #[test]
    fn logo_canvas_has_expected_dimensions() {
        let lines = logo_lines(&Theme::TRUECOLOR);
        let (width, height) = logo_canvas_size();
        assert_eq!(lines.len(), height);
        assert!(lines.iter().all(|line| line.width() == width));
    }

    #[test]
    fn shadow_layer_is_offset_by_two_and_one() {
        let grid = logo_grid();
        // Top-left logo stroke: main layer at (1, LOGO_MARGIN), its shadow
        // copy exactly (+1, +2) away.
        assert!(layer_char(&grid, 1, LOGO_MARGIN, 0, 0).is_some());
        assert!(layer_char(
            &grid,
            1 + LOGO_SHADOW_DY,
            LOGO_MARGIN + LOGO_SHADOW_DX,
            LOGO_SHADOW_DY,
            LOGO_SHADOW_DX
        )
        .is_some());
        // The bottom overhang row holds only shadow, never main strokes.
        let bottom = LOGO_LINES.len() + LOGO_SHADOW_DY;
        assert!(layer_char(&grid, bottom, LOGO_MARGIN + LOGO_SHADOW_DX, 0, 0).is_none());
        assert!(layer_char(
            &grid,
            bottom,
            LOGO_MARGIN + LOGO_SHADOW_DX,
            LOGO_SHADOW_DY,
            LOGO_SHADOW_DX
        )
        .is_some());
    }

    #[test]
    fn logo_marks_land_outside_both_logo_layers() {
        let grid = logo_grid();
        let (width, height) = logo_canvas_size();
        for (r, c, ..) in logo_marks() {
            assert!(r < height && c < width, "mark ({r},{c}) out of bounds");
            assert!(
                layer_char(&grid, r, c, 0, 0).is_none(),
                "mark ({r},{c}) collides with the logo"
            );
            assert!(
                layer_char(&grid, r, c, LOGO_SHADOW_DY, LOGO_SHADOW_DX).is_none(),
                "mark ({r},{c}) collides with the shadow"
            );
        }
    }

    #[test]
    fn render_does_not_panic_at_any_size() {
        let apps = [sample_app(0), sample_app(3), sample_app(30)];
        for app in &apps {
            for (w, h) in [(120, 40), (84, 24), (60, 18), (40, 12), (20, 8), (5, 3)] {
                let backend = TestBackend::new(w, h);
                let mut terminal = Terminal::new(backend).unwrap();
                terminal.draw(|f| render(f, app)).unwrap();
            }
        }
    }

    #[test]
    fn dialogs_render_without_panicking() {
        let mut app = sample_app(2);
        app.open_edit();
        for (w, h) in [(100, 30), (40, 10), (10, 5)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| render(f, &app)).unwrap();
        }

        app.request_delete();
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
    }

    #[test]
    fn manual_preview_render() {
        let mut app = sample_app(0);
        for name in [
            "prod-api-01",
            "staging-web",
            "homelab-main",
            "backup-node",
            "db-primary",
            "coolify-vm",
        ] {
            app.config.add(sample_server(name));
        }
        app.config.mark_connected(0, 200);
        app.config.mark_connected(3, 100);
        app.selected = 0;

        let backend = TestBackend::new(100, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            eprintln!("{line}");
        }
    }
}
