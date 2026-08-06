//! All rendering: layout math, the neon LAZYSSH wordmark, the server card,
//! the footer command bar, and the modal dialogs. State lives in
//! [`crate::app`]; every color comes from [`crate::theme`].

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph},
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

/// Most rows of wrapped description text shown in the inspector box.
const DESC_MAX_LINES: usize = 3;
/// Rows the inspector box adds around its text: top and bottom borders.
const DESC_CHROME_HEIGHT: u16 = 2;
/// Smallest useful inspector box: one text row plus the borders.
const DESC_MIN_HEIGHT: u16 = 3;

/// Width available for description text inside the inspector box: the box
/// width minus its borders and two cells of horizontal padding per side.
fn desc_text_width(box_width: u16) -> usize {
    (box_width as usize).saturating_sub(6).max(1)
}

/// Wrapped description rows for the inspector: at most `max_lines` rows of
/// `width` characters, ellipsizing the final row when text is cut off. An
/// empty or whitespace-only description yields no rows; the renderer shows a
/// placeholder instead.
fn description_lines(description: &str, width: usize, max_lines: usize) -> Vec<String> {
    let text = description.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let width = width.max(1);
    let mut lines = wrap_value(text, width);
    if lines.len() > max_lines.max(1) {
        lines.truncate(max_lines.max(1));
        if let Some(last) = lines.last_mut() {
            let mut cut: String = last.chars().take(width.saturating_sub(1)).collect();
            cut.push('…');
            *last = cut;
        }
    }
    lines
        .iter()
        .map(|line| line.trim_end().to_string())
        .collect()
}

/// Height of the inspector box holding `rows` of wrapped text. An empty
/// description still gets one row for the placeholder.
fn desc_box_height(rows: usize) -> u16 {
    rows.clamp(1, DESC_MAX_LINES) as u16 + DESC_CHROME_HEIGHT
}

/// Space plan below the server card: `(inspector height, hint shown)`. The
/// card wins, then the inspector — shrunk when the terminal is short — and
/// the connect hint only fits last. Each shown section needs a one-row gap
/// above it.
fn main_stack(area_height: u16, card_h: u16, desc_h: u16) -> (u16, bool) {
    let after_card = area_height.saturating_sub(card_h);
    let desc = if desc_h == 0 || after_card <= DESC_MIN_HEIGHT {
        0
    } else {
        desc_h.min(after_card - 1)
    };
    let used = card_h + if desc > 0 { desc + 1 } else { 0 };
    let hint = area_height > used.saturating_add(1);
    (desc, hint)
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

/// Places the centered server card in `area`, the description inspector for
/// the selected server beneath it, and the "press Enter to connect" hint
/// below that when there is room. The hint is the first thing dropped on
/// short terminals; the inspector shrinks before disappearing.
fn render_main(frame: &mut Frame, app: &App, area: Rect, t: &Theme) {
    let rows = app.config.servers.len();
    let empty = rows == 0;
    let width = card_width(area.width).min(area.width);
    let height = card_height(rows, empty).min(area.height);

    let full_desc_h = match app.selected_server() {
        Some(server) => {
            let lines =
                description_lines(&server.description, desc_text_width(width), DESC_MAX_LINES);
            desc_box_height(lines.len())
        }
        None => 0,
    };
    let (desc_h, show_hint) = main_stack(area.height, height, full_desc_h);

    let reserved = height + if desc_h > 0 { desc_h + 1 } else { 0 } + if show_hint { 2 } else { 0 };

    let x = area.x + center_offset(area.width, width);
    let y = area.y + center_offset(area.height, reserved);
    let card = Rect::new(x, y, width, height);

    render_server_card(frame, app, card, t);

    let mut next_y = card.y.saturating_add(card.height);
    if desc_h > 0 {
        let desc_y = next_y.saturating_add(1);
        let bottom = area.y.saturating_add(area.height);
        if desc_y < bottom {
            let h = desc_h.min(bottom - desc_y);
            if h >= DESC_MIN_HEIGHT {
                render_description_box(frame, app, Rect::new(x, desc_y, width, h), t);
                next_y = desc_y.saturating_add(h);
            }
        }
    }

    if show_hint {
        let hint_y = next_y.saturating_add(1);
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

/// The description inspector: a small rounded box under the server card,
/// titled with the selected server's name so it reads as an extension of the
/// highlighted row. Shows the wrapped description, or a dim placeholder when
/// none is saved.
fn render_description_box(frame: &mut Frame, app: &App, area: Rect, t: &Theme) {
    let Some(server) = app.selected_server() else {
        return;
    };

    let name = truncate_label(&server.name, (area.width as usize).saturating_sub(8));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border))
        .style(Style::default().bg(t.panel_bg))
        .padding(Padding::new(2, 2, 0, 0))
        .title(Line::from(vec![
            Span::styled(
                " ❯ ",
                Style::default().fg(t.green).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{name} "),
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    let lines = description_lines(
        &server.description,
        desc_text_width(area.width),
        (inner.height as usize).min(DESC_MAX_LINES),
    );
    let paragraph = if lines.is_empty() {
        Paragraph::new(Line::styled(
            "No description saved.",
            Style::default().fg(t.muted).add_modifier(Modifier::ITALIC),
        ))
    } else {
        Paragraph::new(
            lines
                .into_iter()
                .map(|line| Line::styled(line, Style::default().fg(t.muted_primary)))
                .collect::<Vec<_>>(),
        )
    };
    frame.render_widget(paragraph.style(Style::default().bg(t.panel_bg)), inner);
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

/// Column where form field values start: marker + space + padded label.
const FORM_LABEL_COL: usize = 28;
/// Wrap-width floor so values stay legible even on tiny terminals, at the
/// cost of truncated lines there.
const FORM_MIN_VALUE_WIDTH: usize = 8;
/// Rows the popup needs beyond its content lines: borders and padding.
const FORM_CHROME_HEIGHT: u16 = 4;
/// Content rows above the first field: the key-hint line and a spacer.
const FORM_PREAMBLE_LINES: usize = 2;

/// Width available for a field's value inside the form: the popup's inner
/// width minus the label column and one cell reserved for the cursor.
fn form_value_width(inner_width: u16) -> usize {
    (inner_width as usize)
        .saturating_sub(FORM_LABEL_COL + 1)
        .max(FORM_MIN_VALUE_WIDTH)
}

/// Greedy word wrap of `value` into lines of at most `width` characters,
/// breaking at the last space when possible and hard-breaking longer runs.
/// Every character is preserved (spaces stay at line ends, so the cursor
/// keeps its spot while typing) and even an empty value yields one line so
/// the field keeps a row.
fn wrap_value(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current: Vec<char> = Vec::new();
    for ch in value.chars() {
        current.push(ch);
        if current.len() > width {
            let break_at = current[..width]
                .iter()
                .rposition(|c| *c == ' ')
                .map(|i| i + 1)
                .unwrap_or(width);
            let rest = current.split_off(break_at);
            lines.push(current.into_iter().collect());
            current = rest;
        }
    }
    lines.push(current.into_iter().collect());
    lines
}

/// Each form field paired with its wrapped value lines at `value_width`.
fn form_field_lines(draft: &DraftServer, value_width: usize) -> Vec<(Field, Vec<String>)> {
    Field::ALL
        .iter()
        .map(|&field| (field, wrap_value(draft.current_value(field), value_width)))
        .collect()
}

/// First content-line index and line count of `active` within the form body.
fn form_active_span(fields: &[(Field, Vec<String>)], active: Field) -> (usize, usize) {
    let mut start = FORM_PREAMBLE_LINES;
    for (field, lines) in fields {
        if *field == active {
            return (start, lines.len());
        }
        start += lines.len();
    }
    (start, 1)
}

/// Scroll offset keeping the active field visible when the form's content
/// overflows `visible` rows. Scrolls just far enough to show the field's
/// last line, but never pushes its first line off the top.
fn form_scroll_offset(
    active_start: usize,
    active_len: usize,
    visible: usize,
    total: usize,
) -> usize {
    if visible == 0 || total <= visible {
        return 0;
    }
    let max_offset = total - visible;
    (active_start + active_len)
        .saturating_sub(visible)
        .min(active_start)
        .min(max_offset)
}

fn render_form_popup(
    frame: &mut Frame,
    app: &App,
    draft: &DraftServer,
    active: Field,
    editing: bool,
    t: &Theme,
) {
    let width = frame.area().width.saturating_sub(4).min(72);
    // Inner width after borders and horizontal padding.
    let inner_width = width.saturating_sub(6);
    let value_width = form_value_width(inner_width);

    let fields = form_field_lines(draft, value_width);
    let content_lines: usize =
        FORM_PREAMBLE_LINES + fields.iter().map(|(_, lines)| lines.len()).sum::<usize>();
    let height = (content_lines as u16).saturating_add(FORM_CHROME_HEIGHT);
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

    let full_hint = hint_line(FORM_HELP, t);
    let hint = if full_hint.width() > inner_width as usize {
        hint_line(FORM_HELP_SHORT, t)
    } else {
        full_hint
    };
    let mut lines = vec![hint, Line::raw("")];

    for (field, value_lines) in &fields {
        let is_active = *field == active;
        let label_style = if is_active {
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(t.muted)
        };
        let last = value_lines.len() - 1;
        for (i, chunk) in value_lines.iter().enumerate() {
            let mut spans = if i == 0 {
                let marker = if is_active { "❯" } else { " " };
                let marker_style = if is_active {
                    Style::default().fg(t.green).add_modifier(Modifier::BOLD)
                } else {
                    label_style
                };
                vec![
                    Span::styled(format!("{marker} "), marker_style),
                    Span::styled(format!("{:<26}", field.label()), label_style),
                ]
            } else {
                vec![Span::raw(" ".repeat(FORM_LABEL_COL))]
            };
            let mut used = FORM_LABEL_COL + chunk.chars().count();
            spans.push(Span::styled(chunk.clone(), Style::default().fg(t.text)));
            if is_active && i == last && cursor_visible(app.tick) {
                spans.push(Span::styled("▏", Style::default().fg(t.primary)));
                used += 1;
            }
            let line = if is_active {
                // Pad to the full inner width so the highlight tints the row.
                spans.push(Span::raw(
                    " ".repeat((inner_width as usize).saturating_sub(used)),
                ));
                Line::from(spans).style(Style::default().bg(t.selected_bg))
            } else {
                Line::from(spans)
            };
            lines.push(line);
        }
    }

    let (active_start, active_len) = form_active_span(&fields, active);
    let offset = form_scroll_offset(
        active_start,
        active_len,
        inner.height as usize,
        content_lines,
    );

    let popup = Paragraph::new(lines).scroll((offset as u16, 0));
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
    fn desc_text_width_reserves_borders_and_padding() {
        assert_eq!(desc_text_width(62), 56);
        // Tiny boxes floor at one column instead of collapsing to zero.
        assert_eq!(desc_text_width(6), 1);
        assert_eq!(desc_text_width(0), 1);
    }

    #[test]
    fn description_lines_wrap_and_clamp() {
        assert!(description_lines("", 20, 3).is_empty());
        assert!(description_lines("   ", 20, 3).is_empty());

        assert_eq!(description_lines("eu api box", 20, 3), vec!["eu api box"]);

        let long = "primary EU api box behind the office vpn; page the infra \
                    rotation before rebooting, nginx config is hand-rolled";
        let lines = description_lines(long, 20, 3);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.chars().count() <= 20));
        assert!(lines.last().unwrap().ends_with('…'));

        // Untruncated wraps keep every word and gain no ellipsis.
        let lines = description_lines("hello wide world", 8, 3);
        assert_eq!(lines, vec!["hello", "wide", "world"]);
    }

    #[test]
    fn desc_box_height_clamps_rows() {
        assert_eq!(desc_box_height(0), 3);
        assert_eq!(desc_box_height(1), 3);
        assert_eq!(desc_box_height(3), 5);
        assert_eq!(desc_box_height(10), DESC_MAX_LINES as u16 + 2);
    }

    #[test]
    fn main_stack_prioritizes_card_then_inspector_then_hint() {
        // Plenty of room: full inspector and the hint.
        assert_eq!(main_stack(20, 9, 3), (3, true));
        // Room for the inspector but not the hint: hint goes first.
        assert_eq!(main_stack(13, 9, 3), (3, false));
        // Inspector shrinks before disappearing.
        assert_eq!(main_stack(14, 9, 5), (4, false));
        // Too short for even a minimal box: inspector hidden.
        assert_eq!(main_stack(12, 9, 3), (0, true));
        assert_eq!(main_stack(9, 9, 3), (0, false));
        assert_eq!(main_stack(0, 9, 3), (0, false));
        // No selection (empty list) keeps the old card + hint behavior.
        assert_eq!(main_stack(13, 10, 0), (0, true));
        assert_eq!(main_stack(11, 10, 0), (0, false));
    }

    #[test]
    fn description_inspector_shows_selected_description() {
        let mut app = sample_app(3);
        app.config.servers[1].description = "primary EU api box behind the vpn".to_string();
        app.selected = 1;

        let backend = TestBackend::new(100, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("primary EU api box behind the vpn"));
        assert!(!text.contains("No description saved."));

        // Moving the selection to a server without a description swaps the
        // text for the placeholder.
        app.selected = 0;
        terminal.draw(|f| render(f, &app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("No description saved."));
        assert!(!text.contains("primary EU api box"));
    }

    #[test]
    fn description_inspector_wraps_long_descriptions() {
        let mut app = sample_app(2);
        app.config.servers[0].description =
            "primary EU api box behind the office vpn; page the infra rotation \
             before rebooting, nginx config is hand-rolled and fragile"
                .to_string();

        let backend = TestBackend::new(100, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let text = buffer_text(&terminal);
        // Wider than one inspector row, so the text must span multiple lines.
        assert!(text.contains("primary EU api box"));
        let hit_rows = text
            .lines()
            .filter(|line| line.contains("vpn") || line.contains("infra") || line.contains("nginx"))
            .count();
        assert!(hit_rows > 1, "description did not wrap:\n{text}");
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
    fn form_value_width_reserves_label_column_and_cursor() {
        assert_eq!(form_value_width(66), 66 - FORM_LABEL_COL - 1);
        // Tiny terminals floor at the minimum instead of collapsing to zero.
        assert_eq!(form_value_width(10), FORM_MIN_VALUE_WIDTH);
        assert_eq!(form_value_width(0), FORM_MIN_VALUE_WIDTH);
    }

    #[test]
    fn wrap_value_preserves_every_character() {
        for (value, width) in [
            ("", 10),
            ("short", 10),
            ("a value that wraps over several lines nicely", 12),
            ("trailing space ", 10),
            ("superlongunbrokenword", 5),
            ("x", 0),
        ] {
            let lines = wrap_value(value, width);
            assert!(!lines.is_empty());
            assert_eq!(lines.concat(), value, "value {value:?} width {width}");
            for line in &lines {
                assert!(
                    line.chars().count() <= width.max(1),
                    "line {line:?} exceeds width {width}"
                );
            }
        }
    }

    #[test]
    fn wrap_value_breaks_at_word_boundaries() {
        assert_eq!(wrap_value("", 8), vec![""]);
        assert_eq!(wrap_value("hello world", 8), vec!["hello ", "world"]);
        assert_eq!(wrap_value("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn form_field_lines_grow_with_long_description() {
        let mut draft = DraftServer::default();
        let short = form_field_lines(&draft, 20);
        assert_eq!(short.len(), Field::ALL.len());
        assert!(short.iter().all(|(_, lines)| lines.len() == 1));

        draft.description = "a much longer description that needs to wrap".to_string();
        let long = form_field_lines(&draft, 20);
        let (field, lines) = &long[1];
        assert_eq!(*field, Field::Description);
        assert!(lines.len() > 1);
    }

    #[test]
    fn form_active_span_skips_preamble_and_prior_fields() {
        let draft = DraftServer {
            description: "a much longer description that needs to wrap".to_string(),
            ..DraftServer::default()
        };
        let fields = form_field_lines(&draft, 20);
        let desc_lines = fields[1].1.len();

        assert_eq!(form_active_span(&fields, Field::Name), (2, 1));
        assert_eq!(
            form_active_span(&fields, Field::Description),
            (3, desc_lines)
        );
        assert_eq!(form_active_span(&fields, Field::Host), (3 + desc_lines, 1));
    }

    #[test]
    fn form_scroll_offset_keeps_active_field_visible() {
        // Everything fits: no scrolling.
        assert_eq!(form_scroll_offset(2, 1, 10, 9), 0);
        // Active field below the fold scrolls just enough to show its end.
        assert_eq!(form_scroll_offset(8, 1, 5, 12), 4);
        // Never scrolls past the end of the content.
        assert_eq!(form_scroll_offset(11, 1, 5, 12), 7);
        // A field taller than the viewport anchors to its first line.
        assert_eq!(form_scroll_offset(3, 9, 5, 15), 3);
        // Zero-height viewport is a no-op, not a panic.
        assert_eq!(form_scroll_offset(3, 1, 0, 12), 0);
    }

    #[test]
    fn form_popup_renders_long_description_at_any_size() {
        let mut app = sample_app(2);
        app.open_edit();
        if let crate::app::Mode::Form { draft, field, .. } = &mut app.mode {
            draft.description =
                "a very long description that goes on and on describing the box, its \
                 quirks, the vpn hop needed to reach it, and who to page when it dies"
                    .to_string();
            *field = Field::Description;
        }
        for (w, h) in [(100, 40), (80, 24), (60, 14), (40, 10), (20, 6), (5, 3)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| render(f, &app)).unwrap();
        }
    }

    #[test]
    fn render_does_not_panic_at_any_size() {
        let mut apps = [sample_app(0), sample_app(3), sample_app(30)];
        for app in apps.iter_mut().skip(1) {
            app.config.servers[0].description =
                "a very long description that keeps going to stress the inspector \
                 box wrapping and clamping at every terminal size we throw at it"
                    .to_string();
        }
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
        app.config.servers[0].description =
            "primary EU api box behind the office vpn; page the infra rotation before rebooting"
                .to_string();

        let backend = TestBackend::new(100, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        dump_buffer(&terminal);
    }

    #[test]
    fn manual_form_preview_render() {
        let mut app = sample_app(2);
        app.open_edit();
        if let Mode::Form { draft, field, .. } = &mut app.mode {
            draft.description = "primary EU api box behind the office vpn; page the infra \
                                 rotation before rebooting, nginx config is hand-rolled"
                .to_string();
            *field = Field::Description;
        }

        let backend = TestBackend::new(90, 26);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        dump_buffer(&terminal);
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn dump_buffer(terminal: &Terminal<TestBackend>) {
        eprint!("{}", buffer_text(terminal));
    }
}
