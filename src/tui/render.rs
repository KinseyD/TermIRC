//! Rendering: turns app state into ratatui widgets.
//!
//! Screen layout (no box frames):
//! ```text
//!  sidebar (fixed)  │  main column
//!  osu_irc           │  (blank separator)          <- framing separator rows
//!    ▶ #osu         │  alice: hello ...            <- messages (scroll)
//!      #chinese     │  ────────────────            <- hint bar (only while a
//!                    │  /join — Join or focus ...      `/` command is drafted;
//!                    │  ┃                             highlighted row reversed)
//!                    │  ┃   /join #osu             <- input (4 rows, bg-shaded,
//!                    │  ┃  connected · q quit …    <- tips row  accent ┃ left)
//!  server osu_irc ▶ channel #osu ▶ type in    <- status bar (bottom row;
//!                                                   the focused zone is
//!                                                   highlighted, and clicking
//!                                                   a segment focuses it)
//! ```
//! Before any channel is opened the main column shows only the centered logo -
//! no composer. Lines are pre-wrapped by `layout`, so the messages `Paragraph`
//! is rendered without `Wrap`: continuation rows already carry their own
//! indentation, keeping the message body clear of the nick column. The hint
//! bar's rows are reserved by `geometry` out of the message pane, so showing
//! it compresses the messages upward instead of overlaying them.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
};

use crate::i18n;
use crate::theme::Theme;
use crate::tui::{App, Focus, StatusZone};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Fixed width of the left server/channel sidebar, in columns.
pub const SIDEBAR_WIDTH: u16 = 22;
/// Columns between the sidebar and the main content: the `│` separator plus a
/// 1-column blank gap.
pub const SEPARATOR_GAP: u16 = 2;
/// Blank padding (each side) between a region's edge and its content.
pub const HORIZONTAL_PAD: u16 = 2;
/// Fixed rows of the composer besides the typed text: a blank row above, a
/// blank row below, and the tips row.
pub const INPUT_FIXED_ROWS: u16 = 3;
/// Rows below the composer: a half-block "fade" of its background on the row
/// immediately under it, then a blank row before the window bottom.
pub const GAP_ROWS: u16 = 2;
/// Blank padding between the composer panel's left edge and its text.
pub const INPUT_LEFT_PAD: u16 = 1;
/// Blank padding between the composer text and the panel's right edge.
pub const INPUT_RIGHT_PAD: u16 = 2;
/// Blank columns (global background) between the composer panel's right edge
/// and the screen's right edge.
pub const INPUT_RIGHT_GAP: u16 = 3;
/// The composer text is this many columns narrower than the message viewport
/// (INPUT_LEFT_PAD + INPUT_RIGHT_PAD, given how the panel is positioned).
pub const INPUT_TEXT_INSET: u16 = INPUT_LEFT_PAD + INPUT_RIGHT_PAD;

/// Render-only chrome state: everything the renderer needs beyond the App.
pub struct Chrome<'a> {
    pub status: &'a str,
    /// The color palette for this render pass.
    pub theme: &'a crate::theme::Theme,
}

/// Convert the app's laid-out rows into ratatui `Text`.
///
/// Owns only the visible rows, so per-frame text allocations depend on the
/// viewport size rather than the amount of scrollback.
pub fn build_text(app: &App, theme: &Theme) -> Text<'static> {
    Text::from(
        app.visible_lines()
            .into_iter()
            .map(|row| match row.nick {
                Some(nick) => Line::from(vec![
                    Span::styled(
                        format!("{nick}: "),
                        Style::new().fg(theme.nick).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(row.body),
                ]),
                None if row.body.is_empty() => Line::from(""),
                None => Line::from(vec![
                    Span::raw(" ".repeat(usize::from(row.indent))),
                    Span::raw(row.body),
                ]),
            })
            .collect::<Vec<_>>(),
    )
}

/// The same terminal rectangles are used by drawing, viewport sizing, and mouse input.
pub struct ScreenGeometry {
    pub sidebar: Rect,
    pub separator: Rect,
    pub messages: Rect,
    /// The inline slash-command hint bar: directly above the composer, its
    /// rows taken from the message pane (height 0 while hidden).
    pub hints: Rect,
    pub composer: Rect,
    pub gap: Rect,
    /// The one-row status bar across the very bottom of the screen:
    /// ` server … ▶ channel … ▶ type in `, the focused zone highlighted.
    /// Height 0 on a terminal too short to keep a content row above it.
    pub status_bar: Rect,
    pub input: Rect,
    pub first_input_line: usize,
    pub input_layout: super::input::InputLayout,
}
pub fn geometry(
    width: u16,
    height: u16,
    input: &str,
    cursor: usize,
    active: bool,
    hint_rows: u16,
) -> ScreenGeometry {
    // The status bar owns the bottom row whenever the terminal is tall
    // enough to keep one content row above it; a 1-row terminal keeps the
    // composer instead (the shadowed `height` below excludes the bar).
    let bar_height = u16::from(height >= 2);
    let status_bar = Rect::new(0, height - bar_height, width, bar_height);
    let height = height - bar_height;
    let sidebar = Rect::new(0, 0, width.min(SIDEBAR_WIDTH), height);
    let separator = Rect::new(
        sidebar.width,
        0,
        width.saturating_sub(sidebar.width).min(SEPARATOR_GAP),
        height,
    );
    let main = Rect::new(
        separator.right(),
        0,
        width.saturating_sub(separator.right()),
        height,
    );
    let (text_x, text_width) = input_text_geometry(width);
    let input_layout = super::input::InputLayout::new(input, cursor, text_width);
    let gap_height = if active {
        GAP_ROWS.min(height.saturating_sub(1))
    } else {
        0
    };
    let composer_height = if active {
        composer_height(input_layout.lines.len()).min(height.saturating_sub(gap_height))
    } else {
        0
    };
    let chrome_height = composer_height + gap_height;
    // The hint bar eats into the message pane, never into the composer, and
    // keeps at least one message row when the terminal can afford both; a
    // terminal too short for that hides the bar rather than the messages.
    let hint_height = hint_rows.min(available_rows(height, chrome_height));
    let messages = inset(
        Rect::new(
            main.x,
            0,
            main.width,
            height
                .saturating_sub(chrome_height)
                .saturating_sub(hint_height),
        ),
        HORIZONTAL_PAD,
    );
    let hints = Rect::new(main.x, messages.bottom(), main.width, hint_height);
    let composer = Rect::new(main.x, hints.bottom(), main.width, composer_height);
    let gap = Rect::new(main.x, composer.bottom(), main.width, gap_height);
    // Remove decorative rows before sacrificing the cursor's input row.
    let input_padding = u16::from(composer_height > INPUT_FIXED_ROWS);
    let rows = if input_padding > 0 {
        composer_height - INPUT_FIXED_ROWS
    } else {
        composer_height
    }
    .min(u16::try_from(input_layout.lines.len()).unwrap_or(u16::MAX));
    let first_input_line = input_layout
        .cursor_row
        .saturating_sub(usize::from(rows.saturating_sub(1)));
    let input = Rect::new(
        text_x,
        composer
            .y
            .saturating_add(input_padding)
            .min(composer.bottom()),
        text_width,
        rows,
    );
    ScreenGeometry {
        sidebar,
        separator,
        messages,
        hints,
        composer,
        gap,
        status_bar,
        input,
        first_input_line,
        input_layout,
    }
}

/// Rows the hint bar may take from a message pane that shares `height` with
/// `chrome_height` rows of composer and gap: all of them minus one kept for
/// the messages, or none when no message row would survive.
fn available_rows(height: u16, chrome_height: u16) -> u16 {
    height.saturating_sub(chrome_height).saturating_sub(1)
}

/// Render the whole screen: sidebar on the left, main column (messages /
/// composer) on the right. There is no title row - the message stream frames
/// itself with separator rows above the first and below the last message.
pub fn draw(frame: &mut Frame, app: &App, chrome: &Chrome<'_>) {
    let theme = chrome.theme;
    // Fill the screen with the global background.
    frame.render_widget(
        Paragraph::new("").style(Style::new().bg(theme.global_bg)),
        frame.area(),
    );

    let g = geometry(
        frame.area().width,
        frame.area().height,
        app.input(),
        app.input_cursor(),
        app.active_buffer().is_some(),
        app.hint_rows(),
    );
    render_sidebar(frame, g.sidebar, app, theme);
    render_separator(frame, g.separator, theme);
    if app.active_buffer().is_none() {
        render_welcome(frame, g.messages, theme);
        render_status_bar(frame, g.status_bar, app, theme);
        return;
    }
    frame.render_widget(Paragraph::new(build_text(app, theme)), g.messages);
    render_pre_selection(frame, g.messages, app, theme);
    render_selection(frame, g.messages, app, theme);
    render_hints(frame, g.hints, app, theme);
    let input_lines = build_wrapped_input_lines(
        &g.input_layout,
        app.input_cursor(),
        app.focus() == Focus::Composer,
        theme,
    );
    render_composer(
        frame,
        &g,
        frame.area().width,
        input_lines,
        chrome.status,
        app,
        theme,
    );
    render_gap(frame, g.gap, frame.area().width, app, theme);
    render_status_bar(frame, g.status_bar, app, theme);
}

/// Render the inline slash-command hint bar between the message pane and the
/// composer: a thin rule in the sidebar divider color, then one row per
/// matching command (`/name — description`) with the highlighted row in bold
/// reverse video and the others dimming their descriptions. The rows come
/// from `geometry`, which reserves them out of the message pane.
fn render_hints(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let rule: String = "─".repeat(usize::from(area.width));
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            rule,
            Style::new().fg(theme.separator),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let selected = app.hint_selected();
    let rows: Vec<Line<'static>> = app
        .command_hints()
        .into_iter()
        .enumerate()
        .map(|(index, (name, key))| {
            // The table's second column is a catalog key; resolve it here so
            // the view state stays raw identifiers and only this presentation
            // layer localizes.
            let description = i18n::t(key);
            if index == selected {
                Line::from(Span::styled(
                    format!("/{name} — {description}"),
                    Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED),
                ))
            } else {
                Line::from(vec![
                    Span::raw(format!("/{name}")),
                    Span::styled(format!(" — {description}"), Style::new().fg(theme.dim)),
                ])
            }
        })
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(rows)),
        inset(
            Rect::new(area.x, area.y + 1, area.width, area.height - 1),
            HORIZONTAL_PAD,
        ),
    );
}
/// Render the thin gray vertical line that separates the sidebar from the main
/// column, spanning the full height (left edge of the separator-gap segment).
fn render_separator(frame: &mut Frame, area: Rect, theme: &Theme) {
    let col = Rect::new(area.x, area.y, area.width.min(1), area.height);
    let line = Line::from(Span::styled("│", Style::new().fg(theme.separator)));
    let lines = vec![line; usize::from(area.height)];
    frame.render_widget(Paragraph::new(Text::from(lines)), col);
}

/// Render the server/channel sidebar from the app's row model: a bold
/// server header per server with its channels beneath, the active row (the
/// viewed channel, or the server header while its console is viewed) with
/// a brighter background, and (while the sidebar has focus) a cursor row
/// with a slightly brighter background plus a block cursor at its start.
fn render_sidebar(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let inner = inset(area, 1);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let focused = app.focus() == Focus::Sidebar;
    let cursor = app.sidebar_cursor();
    let active = app.active_buffer();
    let rows = app.sidebar_rows();

    for (visible, (index, row)) in rows
        .iter()
        .enumerate()
        .skip(app.sidebar_scroll_offset())
        .take(usize::from(inner.height))
        .enumerate()
    {
        let y = inner.y + visible as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let is_cursor = focused && cursor == Some(index);
        let is_active = active.is_some_and(|buffer| row.id == Some(buffer.id));

        let is_hovered = !is_active && !is_cursor && app.sidebar_hovered() == Some(index);

        // Row background: the active channel row is brightest; a cursor row
        // is slightly brighter than the plain background; a hovered row is
        // dimmer than both.
        let bg = if is_active {
            theme.sidebar_active_bg
        } else if is_cursor {
            theme.sidebar_cursor_bg
        } else if is_hovered {
            theme.hover_bg
        } else {
            Color::Reset
        };
        let row_rect = Rect::new(inner.x, y, inner.width, 1);
        if bg != Color::Reset {
            frame.buffer_mut().set_style(row_rect, Style::new().bg(bg));
        }

        // Row text: a leading space reserves room for the block cursor.
        let state = app.connection_state(&row.server, row.kind.channel());
        let (symbol, color) = match state {
            crate::connection::ConnectionState::Connected => ("● ", theme.status_ok),
            crate::connection::ConnectionState::Stopped => ("● ", theme.status_err),
            crate::connection::ConnectionState::Connecting => (
                if app.connecting_dot_visible() {
                    "● "
                } else {
                    "  "
                },
                theme.status_pending,
            ),
        };
        let dot = Span::styled(symbol, Style::new().fg(color));
        let line = match &row.kind {
            crate::core::BufferKind::Server => Line::from(vec![
                Span::raw(" "),
                dot,
                Span::styled(
                    row.server.as_str(),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
            ]),
            crate::core::BufferKind::Channel(channel) => Line::from(vec![
                Span::raw(" "),
                Span::raw("  "),
                dot,
                Span::styled(channel.as_str(), Style::new().fg(theme.dim)),
            ]),
            crate::core::BufferKind::Query(nickname) => {
                let unread = row
                    .id
                    .and_then(|id| app.buffer(id))
                    .is_some_and(|buffer| buffer.unread);
                Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("@{nickname}{}", if unread { "*" } else { "" }),
                        Style::new().fg(theme.dim),
                    ),
                ])
            }
        };
        frame.render_widget(Paragraph::new(line), row_rect);

        // Block cursor at the start of the focused row.
        if is_cursor {
            frame.buffer_mut()[(inner.x, y)].set_symbol(" ").set_style(
                Style::new()
                    .fg(theme.input_cursor_fg)
                    .bg(theme.input_cursor_bg),
            );
        }
    }
}

/// The composer accent column: right after the sidebar separator gap.
const COMPOSER_ACCENT_X: u16 = SIDEBAR_WIDTH + SEPARATOR_GAP;

/// Composer text geometry for a given screen width: `(text_left_col, text_width)`.
pub fn input_text_geometry(screen_w: u16) -> (u16, u16) {
    let panel_left = COMPOSER_ACCENT_X + 1; // panel starts right of the ┃
    let panel_right = screen_w.saturating_sub(INPUT_RIGHT_GAP); // exclusive
    let text_left = panel_left + INPUT_LEFT_PAD;
    let text_right = panel_right.saturating_sub(INPUT_RIGHT_PAD); // exclusive
    (
        text_left.min(screen_w),
        text_right.saturating_sub(text_left),
    )
}

/// Height of the composer region for a given number of wrapped input lines.
pub fn composer_height(input_lines: usize) -> u16 {
    u16::try_from(input_lines)
        .unwrap_or(u16::MAX)
        .saturating_add(INPUT_FIXED_ROWS)
}

/// Build the composer's input lines: hard-wrap `text` to `width` columns; when
/// `show_cursor` is set, render a reverse-video cursor at the `cursor` char
/// index (a solid block when the cursor sits at the end of the text).
fn build_wrapped_input_lines(
    layout: &super::input::InputLayout,
    cursor: usize,
    show_cursor: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    layout
        .lines
        .iter()
        .enumerate()
        .map(|(row, line)| {
            if !show_cursor || row != layout.cursor_row {
                return Line::from(line.text.clone());
            }
            let local = cursor.saturating_sub(line.start);
            let chars: Vec<char> = line.text.chars().collect();
            let before: String = chars.iter().take(local).collect();
            let at = chars
                .get(local)
                .map(char::to_string)
                .unwrap_or_else(|| " ".into());
            let after: String = chars.iter().skip(local + 1).collect();
            Line::from(vec![
                Span::raw(before),
                Span::styled(
                    at,
                    Style::new()
                        .fg(theme.input_cursor_fg)
                        .bg(theme.input_cursor_bg),
                ),
                Span::raw(after),
            ])
        })
        .collect()
}

/// Render the composer: an `input_bg` panel with a pale-green `┃` accent on its
/// left (standing on the global background), the wrapped input lines with a
/// reverse-video cursor, and a tips row. Rows: blank / text×N / blank / tips.
/// When the composer is not focused the accent dims and no cursor is shown.
fn render_composer(
    frame: &mut Frame,
    g: &ScreenGeometry,
    screen_w: u16,
    input_lines: Vec<Line<'static>>,
    status: &str,
    app: &App,
    theme: &Theme,
) {
    let area = g.composer;
    if area.is_empty() {
        return;
    }
    let panel_left = (COMPOSER_ACCENT_X + 1).min(screen_w);
    let panel_w = screen_w
        .saturating_sub(INPUT_RIGHT_GAP)
        .saturating_sub(panel_left);
    // Panel background (starts right of the ┃, so the ┃ sits on the global bg).
    frame.buffer_mut().set_style(
        Rect::new(panel_left, area.y, panel_w, area.height),
        Style::new().bg(theme.input_bg),
    );

    // ┃ accent at the composer's left edge, spanning all its rows. Dimmed
    // while another region holds the focus.
    let accent_color = if app.focus() == Focus::Composer {
        theme.accent
    } else {
        theme.accent_dim
    };
    let accent = Span::styled("┃", Style::new().fg(accent_color));
    let accent_lines: Vec<Line<'static>> = (0..area.height)
        .map(|_| Line::from(accent.clone()))
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(accent_lines)),
        Rect::new(COMPOSER_ACCENT_X, area.y, 1, area.height),
    );

    let n = g.input.height;
    let visible: Vec<_> = input_lines
        .into_iter()
        .skip(g.first_input_line)
        .take(usize::from(n))
        .collect();
    frame.render_widget(Paragraph::new(Text::from(visible)), g.input);
    let tips = if status.is_empty() {
        i18n::t("hint.tips").to_string()
    } else {
        format!("{}  ·  {}", status, i18n::t("hint.tips"))
    };
    frame.render_widget(
        Paragraph::new(tips),
        Rect::new(
            g.input.x,
            (area.y + n + 2).min(area.bottom()),
            g.input.width,
            u16::from(area.height > n + 2),
        ),
    );
}

/// The block color one message contributes to its side of an adjacent
/// separator row: the formal selection's color, the hover's, or the
/// global background. Order-independent: derived from app state, not
/// from which renderer painted last.
fn message_block_color(app: &App, idx: usize, theme: &Theme) -> Color {
    if app.focus() == Focus::Messages && app.selected() == Some(idx) {
        theme.message_select_bg
    } else if app.hovered() == Some(idx) {
        theme.hover_bg
    } else {
        theme.global_bg
    }
}

/// Overlay the hovered message's pre-selection highlight: exactly the
/// formal selection's block, minus its accent column — background on the
/// message's rows, a lower-half cap (`▄`) on the separator row above and
/// an upper-half cap (`▀`) on the one below, all in the dimmer hover
/// color. Rendered before `render_selection`, so the formal styling wins
/// wherever both point at the same message.
fn render_pre_selection(frame: &mut Frame, msg_rect: Rect, app: &App, theme: &Theme) {
    if msg_rect.is_empty() {
        return;
    }
    let Some((start, height)) = app.hovered_span() else {
        return;
    };
    let Some(idx) = app.hovered() else {
        return;
    };
    if app.focus() == Focus::Messages && app.hovered() == app.selected() {
        return; // same message: the formal selection styling takes over
    }
    let off = app.scroll_offset() as i64;
    let first = start as i64 - off;
    let last = first + height as i64 - 1;
    let vh = i64::from(msg_rect.height);
    if last < 0 || first >= vh {
        return; // hovered message entirely off-screen
    }

    let block_x = (COMPOSER_ACCENT_X + 1).min(msg_rect.x);
    let block_w = msg_rect.x + msg_rect.width - block_x;

    // 1. Highlight background on the message's own rows (clipped to the
    //    pane), exactly like the formal selection.
    let top = first.max(0);
    let bottom = last.min(vh - 1);
    frame.buffer_mut().set_style(
        Rect::new(
            block_x,
            msg_rect.y + top as u16,
            block_w,
            (bottom - top + 1) as u16,
        ),
        Style::new().bg(theme.hover_bg),
    );

    // 2. Half-block caps on the separator rows (▄ above, ▀ below): the
    //    half touching the hovered message shows the hover color, the
    //    other half blends toward the adjacent message's block color.
    let buf = frame.buffer_mut();
    if first > 0 {
        let above = idx
            .checked_sub(1)
            .map_or(theme.global_bg, |a| message_block_color(app, a, theme));
        fill_half_block_row(
            buf,
            block_x,
            msg_rect.y + (first - 1) as u16,
            block_w,
            "▄",
            theme.hover_bg,
            above,
        );
    }
    if last + 1 < vh {
        let below = message_block_color(app, idx + 1, theme);
        fill_half_block_row(
            buf,
            block_x,
            msg_rect.y + (last + 1) as u16,
            block_w,
            "▀",
            theme.hover_bg,
            below,
        );
    }
    // Deliberately nothing else: the accent column is the one decoration
    // the pre-selection omits.
}

/// Overlay the selected-message highlight while the message pane has focus.
/// The layout frames the message list with separator rows (above the first and
/// below the last message), so the selection block is always bracketed by
/// them: the selected message's rows get the highlight background, the
/// separator row above becomes a lower-half block (`▄`) and the one below an
/// upper-half block (`▀`) in the highlight color, and a pale-green `┃` accent
/// runs down the front of the whole block (like the composer's) — starting
/// with a lower-half stroke (`╻`) and ending with an upper-half stroke (`╹`).
/// The highlight starts flush against the accent so no global-bg gap shows;
/// rows scrolled out of the pane are simply clipped.
fn render_selection(frame: &mut Frame, msg_rect: Rect, app: &App, theme: &Theme) {
    if msg_rect.is_empty() {
        return;
    }
    if app.focus() != Focus::Messages {
        return;
    }
    let Some((start, height)) = app.selected_span() else {
        return;
    };
    let Some(idx) = app.selected() else {
        return;
    };
    let off = app.scroll_offset() as i64;
    let first = start as i64 - off;
    let last = first + height as i64 - 1;
    let vh = i64::from(msg_rect.height);
    if last < 0 || first >= vh {
        return; // selection entirely off-screen
    }

    // The selection block (highlight rows + half-block separators) starts
    // flush against the ┃ accent — no global-bg gap cell in between.
    let block_x = (COMPOSER_ACCENT_X + 1).min(msg_rect.x);
    let block_w = msg_rect.x + msg_rect.width - block_x;

    // 1. Highlight background on the message's own rows (clipped to the pane).
    let top = first.max(0);
    let bottom = last.min(vh - 1);
    frame.buffer_mut().set_style(
        Rect::new(
            block_x,
            msg_rect.y + top as u16,
            block_w,
            (bottom - top + 1) as u16,
        ),
        Style::new().bg(theme.message_select_bg),
    );

    // 2. Half-block separator rows (clipped to the pane): ▄ above, ▀
    //    below. The half touching the selected message shows the
    //    highlight color; the other half blends toward the adjacent
    //    message's block color (hover or global bg).
    let buf = frame.buffer_mut();
    if first > 0 {
        let above = idx
            .checked_sub(1)
            .map_or(theme.global_bg, |a| message_block_color(app, a, theme));
        fill_half_block_row(
            buf,
            block_x,
            msg_rect.y + (first - 1) as u16,
            block_w,
            "▄",
            theme.message_select_bg,
            above,
        );
    }
    if last + 1 < vh {
        let below = message_block_color(app, idx + 1, theme);
        fill_half_block_row(
            buf,
            block_x,
            msg_rect.y + (last + 1) as u16,
            block_w,
            "▀",
            theme.message_select_bg,
            below,
        );
    }

    // 3. ┃ accent down the front of the selection block, at the same column
    //    as the composer's. The top cell keeps only the lower half of the
    //    stroke (╻) and the bottom cell only the upper half (╹), so the line
    //    tapers at both ends.
    let accent_top = first - 1; // the ▄ separator row above
    let accent_bottom = last + 1; // the ▀ separator row below
    for row in accent_top.max(0)..=accent_bottom.min(vh - 1) {
        let symbol = if row == accent_top && row == accent_bottom {
            "┃"
        } else if row == accent_top {
            "╻"
        } else if row == accent_bottom {
            "╹"
        } else {
            "┃"
        };
        buf[(COMPOSER_ACCENT_X, msg_rect.y + row as u16)]
            .set_symbol(symbol)
            .set_style(Style::new().fg(theme.accent));
    }
}

/// The welcome-page logo: "termirc" in box-drawing block letters. Every row is
/// the same width, so the block centers as a unit.
const WELCOME_LOGO: [&str; 6] = [
    "████████╗███████╗██████╗ ███╗   ███╗██╗██████╗  ██████╗",
    "╚══██╔══╝██╔════╝██╔══██╗████╗ ████║██║██╔══██╗██╔════╝",
    "   ██║   █████╗  ██████╔╝██╔████╔██║██║██████╔╝██║     ",
    "   ██║   ██╔══╝  ██╔══██╗██║╚██╔╝██║██║██╔══██╗██║     ",
    "   ██║   ███████╗██║  ██║██║ ╚═╝ ██║██║██║  ██║╚██████╗",
    "   ╚═╝   ╚══════╝╚═╝  ╚═╝╚═╝     ╚═╝╚═╝╚═╝  ╚═╝ ╚═════╝",
];

/// Render the welcome page shown before any channel is opened: the logo
/// centered in the pane, in the composer-accent color. The composer is not
/// shown on this page - the logo gets the whole main column.
fn render_welcome(frame: &mut Frame, area: Rect, theme: &Theme) {
    let logo_w = WELCOME_LOGO
        .iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let logo_h = WELCOME_LOGO.len() as u16;
    let top = area.y + area.height.saturating_sub(logo_h) / 2;
    let left = area.x + area.width.saturating_sub(logo_w) / 2;
    let style = Style::new().fg(theme.accent).add_modifier(Modifier::BOLD);
    let art: Vec<Line<'_>> = WELCOME_LOGO
        .iter()
        .map(|row| Line::from(Span::styled(*row, style)))
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(art)),
        Rect::new(left, top, logo_w.min(area.width), logo_h.min(area.height)),
    );
}

/// Fill one row with a half-block character: the filled half in `fg`,
/// the blank half showing `bg`.
fn fill_half_block_row(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    width: u16,
    symbol: &str,
    fg: Color,
    bg: Color,
) {
    for col in x..x + width {
        buf[(col, y)]
            .set_symbol(symbol)
            .set_style(Style::new().fg(fg).bg(bg));
    }
}

/// Render the gap below the composer: the `┃` accent tapers into a `╹` and the
/// composer panel fades out via `▀` across its width.
fn render_gap(frame: &mut Frame, area: Rect, screen_w: u16, app: &App, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let panel_left = (COMPOSER_ACCENT_X + 1).min(screen_w);
    let panel_w = screen_w
        .saturating_sub(INPUT_RIGHT_GAP)
        .saturating_sub(panel_left);
    let taper_color = if app.focus() == Focus::Composer {
        theme.accent
    } else {
        theme.accent_dim
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("╹", Style::new().fg(taper_color)))),
        Rect::new(COMPOSER_ACCENT_X, area.y, 1, 1),
    );
    let fade = Span::styled(
        "▀".repeat(panel_w as usize),
        Style::new().fg(theme.input_bg),
    );
    frame.render_widget(
        Paragraph::new(Line::from(fade)),
        Rect::new(panel_left, area.y, panel_w, 1),
    );
}

/// The status bar's visible blocks: `label value` for the server and
/// channel zones, and the input zone's standing label. A block with no
/// applicable value hides entirely — the server/channel blocks need an
/// active buffer, and the channel block also hides while a server console
/// is viewed (the server block already names it); the input block always
/// shows, so the bar never renders empty. Names are data, labels are
/// localized.
fn status_bar_texts(app: &App) -> Vec<(StatusZone, String)> {
    let mut segments = Vec::with_capacity(3);
    if let Some(buffer) = app.active_buffer() {
        segments.push((
            StatusZone::Server,
            i18n::format(
                i18n::t("statusbar.server"),
                &[("name", buffer.server_label.as_str())],
            ),
        ));
        let channel_name = match &buffer.kind {
            crate::core::BufferKind::Channel(name) => Some(name.clone()),
            crate::core::BufferKind::Query(nick) => Some(format!("@{nick}")),
            crate::core::BufferKind::Server => None,
        };
        if let Some(name) = channel_name {
            segments.push((
                StatusZone::Channel,
                i18n::format(i18n::t("statusbar.channel"), &[("name", name.as_str())]),
            ));
        }
    }
    segments.push((StatusZone::Input, i18n::t("statusbar.input").to_string()));
    segments
}

/// `text` clipped to at most `width` display columns, ending in `…` when
/// any part was cut. Unicode-width aware, so CJK labels halve correctly.
fn clip_columns(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        // Reserve one column for the ellipsis whenever anything is cut.
        if used + w > width - 1 {
            out.push('…');
            return out;
        }
        out.push(ch);
        used += w;
    }
    out
}

/// Fit the status bar's visible blocks into `width` columns. Each block
/// renders as ` text ` plus the `▶` arrow on its right edge (the arrow is
/// part of the block, hit range included), so consecutive blocks nest
/// arrow-to-tail powerline-style. When the texts overflow, the value
/// blocks shrink — the wider one yielding columns to the narrower first —
/// and are then ellipsized; the fixed input label keeps its width and the
/// terminal clips the line as a last resort.
fn fit_status_bar(app: &App, width: u16) -> Vec<(StatusZone, u16, u16, String)> {
    let texts = status_bar_texts(app);
    let n = texts.len();
    // Chrome: one space of padding on each side of every block plus its
    // right-edge arrow.
    let budget = usize::from(width).saturating_sub(3 * n);
    let mut widths: Vec<usize> = texts.iter().map(|(_, text)| text.width()).collect();
    if widths.iter().sum::<usize>() > budget {
        // The trailing input label is fixed; the value blocks share what
        // is left, the wider one yielding columns to the narrower first.
        let target = budget.saturating_sub(widths[n - 1]);
        match n - 1 {
            0 => {}
            1 => widths[0] = widths[0].min(target),
            _ => {
                let (wide, narrow) = if widths[0] >= widths[1] {
                    (0, 1)
                } else {
                    (1, 0)
                };
                widths[narrow] = widths[narrow].min(target);
                widths[wide] = target - widths[narrow];
            }
        }
    }
    let mut segments = Vec::with_capacity(n);
    let mut x = 0u16;
    for (i, (zone, text)) in texts.iter().enumerate() {
        let block = format!(" {} ", clip_columns(text, widths[i]));
        let body = u16::try_from(block.width()).unwrap_or(u16::MAX);
        // The range includes the block's trailing `▶` column.
        segments.push((*zone, x, x.saturating_add(body + 1), block));
        x = x.saturating_add(body + 1);
    }
    segments
}

/// The status-bar zone containing screen column `x`: shared by drawing
/// and mouse routing, so clicks land on exactly the block that was
/// painted (its trailing arrow included). Hidden blocks have no range;
/// columns past the last block map to none.
pub fn status_bar_zone_at(app: &App, width: u16, x: u16) -> Option<StatusZone> {
    fit_status_bar(app, width)
        .iter()
        .find(|&&(_, start, end, _)| x >= start && x < end)
        .map(|&(zone, _, _, _)| zone)
}

/// Render the bottom status bar: one block per zone with a value, each
/// ending in a `▶` whose color is the block's own fill on the background
/// of whatever follows — so consecutive blocks interlock arrow-to-tail
/// (powerline style) and a highlighted block's arrow reads as part of it.
/// All colors come from existing roles: the arrow needs no separate one.
/// Rendered on every page, including the welcome logo.
fn render_status_bar(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let highlighted = app.status_zone();
    let segments = fit_status_bar(app, area.width);
    let mut spans = Vec::with_capacity(2 * segments.len());
    for (i, &(zone, _, _, ref body)) in segments.iter().enumerate() {
        let highlighted_here = zone == highlighted;
        let style = if highlighted_here {
            Style::new()
                .fg(theme.global_bg)
                .bg(theme.status_bar_highlight)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.dim).bg(theme.global_bg)
        };
        spans.push(Span::styled(body.clone(), style));
        // The right-edge arrow: the block's fill color in the foreground,
        // the following block's background (or the screen's) behind it.
        let arrow_bg = segments
            .get(i + 1)
            .map_or(theme.global_bg, |&(next, _, _, _)| {
                if next == highlighted {
                    theme.status_bar_highlight
                } else {
                    theme.global_bg
                }
            });
        spans.push(Span::styled(
            "▶",
            Style::new()
                .fg(if highlighted_here {
                    theme.status_bar_highlight
                } else {
                    theme.dim
                })
                .bg(arrow_bg),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Shrink a region by `pad` columns on each side (vertical unchanged) so
/// content never touches the region's left/right edges.
fn inset(area: Rect, pad: u16) -> Rect {
    Rect::new(
        area.x + pad.min(area.width),
        area.y,
        area.width.saturating_sub(2 * pad),
        area.height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn theme() -> Theme {
        Theme::default()
    }

    #[test]
    fn queries_render_nick_without_connection_dot_or_header_highlight() {
        let mut app = App::new(40, 10);
        app.open_server("srv");
        app.open_buffer("srv", crate::core::BufferKind::Query("alice".into()));
        app.select_buffer(1);
        app.focus_composer();
        let buffer = render_sized(&app, "", 70, 16);
        assert!(buffer_line(&buffer, 1).contains("@alice"));
        assert!(!buffer_line(&buffer, 1).contains('●'));
        assert_eq!(buffer.cell((10, 1)).unwrap().bg, theme().sidebar_active_bg);
        assert_ne!(buffer.cell((10, 0)).unwrap().bg, theme().sidebar_active_bg);
    }

    #[test]
    fn unread_query_label_and_scrolled_sidebar_render_visible_slice() {
        let mut app = App::new(40, 10);
        app.open_server("srv");
        for index in 0..8 {
            app.open_query("srv", &format!("nick{index}"));
        }
        app.session.push_message(RoutedMessage {
            server: "srv".into(),
            target: BufferKind::Query("nick7".into()),
            content: MessageContent::chat("nick7", "hello"),
        });
        app.set_sidebar_height(3);
        for _ in 0..8 {
            app.sidebar_down();
        }
        let buffer = render_sized(&app, "", 70, 4);
        assert!(buffer_line(&buffer, 0).contains("@nick5"));
        assert!(buffer_line(&buffer, 2).contains("@nick7*"));
        assert!(!buffer_line(&buffer, 2).contains('●'));
    }
    fn active_target(app: &App) -> Option<(&str, &str)> {
        app.active_buffer()
            .map(|b| (b.server_label.as_str(), b.kind.channel().unwrap_or("")))
    }

    fn msg_to(server: &str, channel: &str, nick: &str, text: &str) -> RoutedMessage {
        RoutedMessage {
            server: server.into(),
            target: if channel.is_empty() {
                BufferKind::Server
            } else {
                BufferKind::Channel(channel.into())
            },
            content: MessageContent::chat(nick, text),
        }
    }

    use crate::core::{BufferKind, MessageContent, RoutedMessage};
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    fn msg(nick: &str, text: &str) -> RoutedMessage {
        msg_to("osu_irc", "#osu", nick, text)
    }

    /// An app with both channels of the test config registered, viewing #osu,
    /// with focus moved off the sidebar (as if the user opened a channel).
    fn test_app(width: u16, height: u16) -> App {
        let mut app = App::new(width, height);
        app.open_channel("osu_irc", "#osu");
        app.open_channel("osu_irc", "#chinese");
        app.select_buffer(0);
        app.tab(); // Sidebar -> Messages
        app.tab(); // Messages -> Composer
        app
    }

    /// An app with #osu open and viewed (so the composer is rendered, unlike
    /// on the welcome page).
    fn viewing_app(width: u16, height: u16) -> App {
        let mut app = App::new(width, height);
        app.open_channel("osu_irc", "#osu");
        app.select_buffer(0);
        app
    }

    /// Render into a headless terminal. The message viewport is sized to match
    /// what `main.rs` would compute: main column width minus 2*HORIZONTAL_PAD,
    /// height minus the fixed chrome rows.
    fn render_sized(app: &App, status: &str, w: u16, h: u16) -> Buffer {
        render_with_theme(app, status, w, h, &theme())
    }

    /// The same, with an explicit theme: for tests that override roles.
    fn render_with_theme(app: &App, status: &str, w: u16, h: u16, theme: &Theme) -> Buffer {
        let chrome = Chrome { status, theme };
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app, &chrome)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// The composer's left accent (`┃`) column: after the sidebar + separator gap.
    const INPUT_X: u16 = SIDEBAR_WIDTH + SEPARATOR_GAP;
    /// The first content column of the main area (after sidebar + gap + 2-col pad).
    const MAIN_COL_X: u16 = SIDEBAR_WIDTH + SEPARATOR_GAP + HORIZONTAL_PAD;

    fn buffer_line(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).unwrap().symbol())
            .collect()
    }

    #[test]
    fn no_title_row_leading_separator_occupies_the_top() {
        // Arrange: 50x10 terminal; one message lays out to 3 stream rows
        // (framing blank, message, framing blank) with the view at the bottom.
        let mut app = test_app(22, 3);
        app.push_message(msg("alice", "hi"));

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: no channel-name title anywhere in the main column — the top
        // row is the leading separator (blank) and the message follows at the
        // next row.
        let main_row: String = (SIDEBAR_WIDTH + SEPARATOR_GAP..buffer.area.width)
            .map(|x| buffer.cell((x, 1)).unwrap().symbol())
            .collect();
        assert!(
            !main_row.contains("#osu"),
            "channel name leaked into the message area"
        );
        let main_slice: String = (SIDEBAR_WIDTH + HORIZONTAL_PAD..buffer.area.width)
            .map(|x| buffer.cell((x, 0)).unwrap().symbol())
            .collect();
        assert!(
            main_slice.chars().all(|c| c == ' '),
            "top row should be the leading separator, got: {main_slice:?}"
        );
        assert!(buffer_line(&buffer, 1).contains("alice: hi"));

        // Assert: no box-drawing characters anywhere except the thin gray `│`
        // separator (and the heavy `┃` accent) - no corners/tees/horizontals.
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let symbol = buffer.cell((x, y)).unwrap().symbol();
                assert!(
                    !symbol.chars().any(|c| "─┌┐└┘├┤┬┴┼".contains(c)),
                    "border char {symbol:?} at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn render_shows_nick_then_body_on_first_message_row() {
        // Arrange
        let mut app = App::new(22, 3);
        app.push_message(msg("alice", "hello"));

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: first message row (row 1) starts at the main content column.
        assert_eq!(buffer.cell((MAIN_COL_X, 1)).unwrap().symbol(), "a");
        assert!(buffer_line(&buffer, 1).contains("alice: hello"));
    }

    #[test]
    fn continuation_rows_are_blank_under_username() {
        // Arrange: terminal 46x10 -> main 22, viewport 18x3; nick "alice" takes
        // 7 columns, body width 11; "one two three four" -> "one two" / "three four".
        let mut app = App::new(18, 3);
        app.push_message(msg("alice", "one two three four"));

        // Act
        let buffer = render_sized(&app, "", 46, 10);

        // Assert: the continuation row (screen row 1) is blank under the nick,
        // body resumes at the next column.
        let nick_end = MAIN_COL_X + 7; // "alice: " is 7 columns
        for x in MAIN_COL_X..nick_end {
            assert_eq!(
                buffer.cell((x, 1)).unwrap().symbol(),
                " ",
                "column {x} not blank"
            );
        }
        assert_eq!(buffer.cell((nick_end, 1)).unwrap().symbol(), "t");
        assert!(buffer_line(&buffer, 1).contains("three four"));
    }

    #[test]
    fn blank_separators_frame_the_list_and_separate_messages() {
        // Arrange: terminal 50x13 -> message viewport 7 rows; two messages lay
        // out to 5 stream rows, so the whole list fits on one screen.
        let mut app = App::new(22, 5);
        app.push_message(msg("a", "first"));
        app.push_message(msg("b", "second"));

        // Act
        let buffer = render_sized(&app, "", 50, 13);

        // Assert: leading framing blank, first message, between-separator
        // blank, second message, trailing framing blank.
        assert!(buffer_line(&buffer, 1).contains("a: first"));
        assert!(buffer_line(&buffer, 3).contains("b: second"));
        for y in [0, 2, 4] {
            let slice: String = (SIDEBAR_WIDTH + HORIZONTAL_PAD..buffer.area.width)
                .map(|x| buffer.cell((x, y)).unwrap().symbol())
                .collect();
            assert!(
                slice.chars().all(|c| c == ' '),
                "expected blank separator row at {y}, got: {slice:?}"
            );
        }
    }

    #[test]
    fn scroll_offset_shifts_visible_content() {
        // Arrange: 5 one-line messages -> 9 content rows; viewport shows 3.
        let mut app = App::new(22, 3);
        for i in 0..5 {
            app.push_message(msg("u", &format!("m{i}")));
        }
        app.set_scroll_offset(2);

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: offset 2 puts content row 2 ("m1") on the first visible row.
        let row = buffer_line(&buffer, 1);
        assert!(row.contains("u: m1"), "row was: {row:?}");
    }

    #[test]
    fn welcome_page_shows_centered_logo_and_no_composer() {
        // Arrange: channels registered but none viewed; focus starts on the
        // sidebar (fresh-start state).
        let mut app = App::new(22, 6);
        app.open_channel("osu_irc", "#osu");
        app.open_channel("osu_irc", "#chinese");
        assert_eq!(active_target(&app), None);

        // Act: a 100x30 terminal gives the whole main column to the welcome
        // page; the 6-row logo is vertically centered in the 29 rows above
        // the status bar -> top at row 11.
        let buffer = render_sized(&app, "", 100, 30);

        // Assert: the logo art is rendered in the accent color...
        let top_row = buffer_line(&buffer, 11);
        assert!(top_row.contains("████████╗"), "row 11: {top_row:?}");
        let logo_cell = buffer
            .cell((top_row.find('█').unwrap() as u16, 11))
            .unwrap();
        assert_eq!(logo_cell.fg, theme().accent);
        assert!(buffer_line(&buffer, 16).contains("╚═════╝"));

        // ...the old welcome text and key hints are gone...
        for y in 0..30 {
            assert!(
                !buffer_line(&buffer, y).contains("Welcome"),
                "old heading at row {y}"
            );
            assert!(!buffer_line(&buffer, y).contains("Enter"));
        }
        // ...and the composer is not shown at all: no input panel background
        // in the main column and no ┃ accent or ╹ taper anywhere.
        for y in 0..30 {
            for x in INPUT_X..100 {
                assert_ne!(
                    buffer.cell((x, y)).unwrap().bg,
                    theme().input_bg,
                    "panel bg at ({x},{y})"
                );
            }
            assert!(
                !["┃", "╹"].contains(&buffer.cell((INPUT_X, y)).unwrap().symbol()),
                "accent at row {y}"
            );
        }
    }

    #[test]
    fn sidebar_lists_configured_servers_and_channels() {
        // Arrange
        let app = test_app(22, 3);

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: server row (no fold marker) and both channels in the
        // sidebar.
        assert!(buffer_line(&buffer, 0).contains("osu_irc"));
        assert!(!buffer_line(&buffer, 0).contains("▸"));
        assert!(!buffer_line(&buffer, 0).contains("▾"));
        assert!(buffer_line(&buffer, 1).contains("#osu"));
        assert!(buffer_line(&buffer, 2).contains("#chinese"));
    }

    #[test]
    fn connection_dots_follow_confirmed_server_and_channel_states() {
        use crate::connection::{ConnectionState as S, IrcEvent as E};
        use crate::core::{ChannelState, ChannelStatus};
        let mut app = test_app(22, 3);
        for channel in ["#osu", "#chinese"] {
            app.apply_connection_event(&E::Channel(
                "osu_irc".into(),
                channel.into(),
                ChannelStatus {
                    state: ChannelState::NotJoined,
                    desired: true,
                },
            ));
        }
        app.apply_connection_event(&E::Connection("osu_irc".into(), S::Connecting));
        let pending = render_sized(&app, "", 50, 10);
        assert_eq!(pending[(2, 0)].symbol(), "●");
        assert_eq!(pending[(2, 0)].fg, theme().status_pending);
        assert_eq!(pending[(4, 1)].fg, theme().status_pending);
        app.tick(std::time::Duration::from_millis(500));
        let off = render_sized(&app, "", 50, 10);
        assert_eq!(off[(2, 0)].symbol(), " ");
        assert_eq!(off[(4, 1)].symbol(), " ");
        for x in 4..11 {
            assert_eq!(pending[(x, 0)].symbol(), off[(x, 0)].symbol());
        }
        app.apply_connection_event(&E::Connection("OSU_IRC".into(), S::Connected));
        app.apply_connection_event(&E::Channel(
            "osu_irc".into(),
            "#osu".into(),
            ChannelStatus {
                state: ChannelState::Joined,
                desired: true,
            },
        ));
        app.apply_connection_event(&E::Channel(
            "osu_irc".into(),
            "#chinese".into(),
            ChannelStatus {
                state: ChannelState::NotJoined,
                desired: true,
            },
        ));
        let connected = render_sized(&app, "", 50, 10);
        assert_eq!(connected[(2, 0)].symbol(), "●");
        assert_eq!(connected[(2, 0)].fg, theme().status_ok);
        assert_eq!(connected[(4, 1)].fg, theme().status_ok);
        assert_eq!(connected[(4, 2)].fg, theme().status_err);
        app.apply_connection_event(&E::Connection("osu_irc".into(), S::Stopped));
        let stopped = render_sized(&app, "", 50, 10);
        for pos in [(2, 0), (4, 1), (4, 2)] {
            assert_eq!(stopped[pos].symbol(), "●");
            assert_eq!(stopped[pos].fg, theme().status_err);
        }
    }

    #[test]
    fn channel_dots_blink_only_for_pending_membership_and_require_connected_server_for_green() {
        use crate::connection::{ConnectionState, IrcEvent};
        use crate::core::{ChannelState, ChannelStatus};
        for (state, color, blinking) in [
            (ChannelState::Joined, theme().status_ok, false),
            (ChannelState::Joining, theme().status_pending, true),
            (ChannelState::Parting, theme().status_pending, true),
            (ChannelState::NotJoined, theme().status_err, false),
            (ChannelState::Uncertain, theme().status_err, false),
        ] {
            let mut app = App::new(22, 3);
            app.open_server("srv");
            app.open_channel("srv", "#room");
            app.apply_connection_event(&IrcEvent::Connection(
                "srv".into(),
                ConnectionState::Connected,
            ));
            app.apply_connection_event(&IrcEvent::Channel(
                "srv".into(),
                "#room".into(),
                ChannelStatus {
                    state,
                    desired: true,
                },
            ));
            let on = render_sized(&app, "", 50, 10);
            assert_eq!(on[(4, 1)].fg, color, "{state:?}");
            assert_eq!(on[(4, 1)].symbol(), "●");
            app.tick(std::time::Duration::from_millis(500));
            let off = render_sized(&app, "", 50, 10);
            assert_eq!(
                off[(4, 1)].symbol(),
                if blinking { " " } else { "●" },
                "{state:?}"
            );
        }
        let mut app = App::new(22, 3);
        app.open_server("srv");
        app.apply_connection_event(&IrcEvent::Channel(
            "srv".into(),
            "#room".into(),
            ChannelStatus {
                state: ChannelState::Joined,
                desired: true,
            },
        ));
        let offline = render_sized(&app, "", 50, 10);
        assert_eq!(offline[(4, 1)].fg, theme().status_err);
    }

    #[test]
    fn intentionally_left_channel_stays_red_while_server_reconnects() {
        use crate::connection::{ConnectionState, IrcEvent};
        use crate::core::ChannelStatus;
        let mut app = App::new(22, 3);
        app.open_server("srv");
        app.open_channel("srv", "#left");
        app.apply_connection_event(&IrcEvent::Channel(
            "srv".into(),
            "#left".into(),
            ChannelStatus::default(),
        ));
        app.begin_connection_change("srv", ConnectionState::Connecting);
        for elapsed in [0, 500] {
            app.tick(std::time::Duration::from_millis(elapsed));
            let rendered = render_sized(&app, "", 50, 10);
            assert_eq!(rendered[(4, 1)].symbol(), "●");
            assert_eq!(rendered[(4, 1)].fg, theme().status_err);
        }
    }

    #[test]
    fn sidebar_highlights_active_channel_row() {
        // Arrange
        let app = test_app(22, 3);

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: the active "#osu" row (row 1) has the brighter background,
        // the inactive "#chinese" row (row 2) does not. Column 10 is inside
        // the sidebar's inner area.
        let active_bg = buffer.cell((10, 1)).unwrap().bg;
        let inactive_bg = buffer.cell((10, 2)).unwrap().bg;
        assert_eq!(active_bg, theme().sidebar_active_bg);
        assert_ne!(inactive_bg, theme().sidebar_active_bg);
    }

    #[test]
    fn sidebar_cursor_row_gets_cursor_bg_and_block_cursor() {
        // Arrange: focus the sidebar; the cursor starts on row 0 (the server).
        // Arrange: focus the sidebar; the cursor snaps onto the viewed
        // channel's row (#osu, row 1).
        let mut app = test_app(22, 3);
        app.tab(); // Composer -> Sidebar
        assert_eq!(app.focus(), Focus::Sidebar);
        assert_eq!(app.sidebar_cursor(), Some(1));

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: row 1 shows the reverse-video block cursor; its background
        // is the (brighter) active-channel one, which takes precedence.
        let cursor_cell = buffer.cell((1, 1)).unwrap();
        assert_eq!(cursor_cell.bg, theme().input_cursor_bg);
        let inner_cell = buffer.cell((10, 1)).unwrap();
        assert_eq!(inner_cell.bg, theme().sidebar_active_bg);
        // Row 0 (server row, not under the cursor) keeps the plain bg.
        assert_ne!(buffer.cell((10, 0)).unwrap().bg, theme().sidebar_active_bg);
    }

    #[test]
    fn server_console_view_renders_echo_and_status_with_composer() {
        // Arrange: the osu_irc console viewed, holding one echoed raw
        // command (with nick) and one status line (empty nick).
        let mut app = App::new(22, 6);
        app.open_server("osu_irc");
        app.open_channel("osu_irc", "#osu");
        app.select_buffer(0); // the console
        app.push_message(msg_to("osu_irc", "", "test", "WHOIS nick"));
        app.push_message(msg_to("osu_irc", "", "", "connected"));

        // Act: 50x20 terminal -> composer rows 15..=17 (accent at row 15).
        let buffer = render_sized(&app, "", 50, 20);

        // Assert: the echo line carries the nick column; the status line
        // starts at the main column with no nick column.
        assert!(buffer_line(&buffer, 1).contains("test: WHOIS nick"));
        assert_eq!(buffer.cell((MAIN_COL_X, 3)).unwrap().symbol(), "c");
        assert!(buffer_line(&buffer, 3).contains("connected"));
        // The composer is rendered in the console view, like in a channel.
        assert_eq!(buffer.cell((INPUT_X, 15)).unwrap().symbol(), "┃");
    }

    #[test]
    fn active_server_row_is_highlighted_when_console_viewed() {
        // Arrange: the osu_irc console viewed; focus off the sidebar.
        let mut app = App::new(22, 3);
        app.open_server("osu_irc");
        app.open_channel("osu_irc", "#osu");
        app.select_buffer(0); // the console
        app.focus_composer();

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: the server row (row 0) gets the active background, the
        // channel row (row 1) does not. Column 10 is inside the sidebar's
        // inner area.
        assert_eq!(buffer.cell((10, 0)).unwrap().bg, theme().sidebar_active_bg);
        assert_ne!(buffer.cell((10, 1)).unwrap().bg, theme().sidebar_active_bg);
    }

    #[test]
    fn composer_dims_and_hides_cursor_when_not_focused() {
        // Arrange: type text, then move focus to the messages pane.
        let mut app = test_app(22, 3);
        app.type_char('h');
        app.tab(); // sidebar
        app.tab(); // messages -> composer unfocused

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: the ┃ accent is dimmed and the input row has no cursor block.
        let accent = buffer.cell((INPUT_X, 5)).unwrap();
        assert_eq!(accent.fg, theme().accent_dim);
        assert_ne!(
            buffer.cell((MAIN_COL_X, 5)).unwrap().bg,
            theme().input_cursor_bg
        );
    }

    #[test]
    fn selected_message_gets_highlight_half_blocks_and_accent() {
        // Arrange: 3 one-line messages in a 4-row viewport (terminal 50x12).
        // Spans (framing row shifts all down one): m0=(1) sep m1=(3) sep
        // m2=(5) + trailing frame = 7 rows; auto-follow scrolls to offset 3,
        // so the default selection is m2, which lands on screen row 2 with its
        // separator rows at screen rows 1 and 3.
        let mut app = test_app(22, 4);
        app.push_message(msg("a", "one"));
        app.push_message(msg("b", "two"));
        app.push_message(msg("c", "three"));
        app.tab();
        app.tab(); // messages focused -> selects m2

        // Act
        let buffer = render_sized(&app, "", 50, 12);

        // Assert: the selected message row is highlighted...
        assert_eq!(
            buffer.cell((MAIN_COL_X, 2)).unwrap().bg,
            theme().message_select_bg
        );
        // ...the separator above renders ▄ and the one below ▀ in that color...
        let above = buffer.cell((MAIN_COL_X, 1)).unwrap();
        assert_eq!(above.symbol(), "▄");
        assert_eq!(above.fg, theme().message_select_bg);
        let below = buffer.cell((MAIN_COL_X, 3)).unwrap();
        assert_eq!(below.symbol(), "▀");
        assert_eq!(below.fg, theme().message_select_bg);
        // ...and a pale-green accent runs down the front of the block,
        // tapering at both ends (╻ … ┃ … ╹ — details in the taper test).
        for y in 1..=3 {
            let accent = buffer.cell((INPUT_X, y)).unwrap();
            assert_eq!(accent.fg, theme().accent, "accent missing on row {y}");
        }
    }

    #[test]
    fn selection_highlight_starts_flush_against_the_accent() {
        // Arrange: as in selected_message_gets_highlight_half_blocks_and_accent,
        // the selected message lands on screen row 2 with separators at 1 and 3.
        let mut app = test_app(22, 4);
        app.push_message(msg("a", "one"));
        app.push_message(msg("b", "two"));
        app.push_message(msg("c", "three"));
        app.tab();
        app.tab(); // messages focused -> selects m2

        // Act
        let buffer = render_sized(&app, "", 50, 12);

        // Assert: the column right after the ┃ accent is part of the highlight
        // (no global-bg gap between accent and highlight)...
        assert_eq!(
            buffer.cell((INPUT_X + 1, 2)).unwrap().bg,
            theme().message_select_bg
        );
        // ...and the half-block separators reach into it as well.
        assert_eq!(buffer.cell((INPUT_X + 1, 1)).unwrap().symbol(), "▄");
        assert_eq!(
            buffer.cell((INPUT_X + 1, 1)).unwrap().fg,
            theme().message_select_bg
        );
        assert_eq!(buffer.cell((INPUT_X + 1, 3)).unwrap().symbol(), "▀");
        assert_eq!(
            buffer.cell((INPUT_X + 1, 3)).unwrap().fg,
            theme().message_select_bg
        );
        // The accent itself keeps standing on the global background.
        assert_eq!(buffer.cell((INPUT_X, 2)).unwrap().bg, theme().global_bg);
    }

    #[test]
    fn selection_accent_tapers_at_both_ends() {
        // Arrange: selection block spans screen rows 1..=3 (▄ / message / ▀).
        let mut app = test_app(22, 4);
        app.push_message(msg("a", "one"));
        app.push_message(msg("b", "two"));
        app.push_message(msg("c", "three"));
        app.tab();
        app.tab(); // messages focused -> selects m2

        // Act
        let buffer = render_sized(&app, "", 50, 12);

        // Assert: the accent starts with a lower-half stroke, runs full ┃
        // through the middle, and ends with an upper-half stroke.
        let top = buffer.cell((INPUT_X, 1)).unwrap();
        assert_eq!(top.symbol(), "╻");
        assert_eq!(top.fg, theme().accent);
        let middle = buffer.cell((INPUT_X, 2)).unwrap();
        assert_eq!(middle.symbol(), "┃");
        assert_eq!(middle.fg, theme().accent);
        let bottom = buffer.cell((INPUT_X, 3)).unwrap();
        assert_eq!(bottom.symbol(), "╹");
        assert_eq!(bottom.fg, theme().accent);
    }

    #[test]
    fn first_and_last_message_blocks_are_framed_by_separators() {
        // Arrange: 3 one-line messages, message pane focused. The layout frames
        // the list with separator rows above the first and below the last
        // message, so selecting those must put the half-block (and the taper)
        // on the framing row — never on the message row itself.
        let mut app = test_app(22, 4);
        app.push_message(msg("a", "one"));
        app.push_message(msg("b", "two"));
        app.push_message(msg("c", "three"));
        app.tab();
        app.tab(); // messages focused -> selects m2 (span 5)

        // Act / Assert (last message): already at the bottom after the
        // default selection, so the trailing framing row is visible below it.
        let buffer = render_sized(&app, "", 50, 12);
        assert_eq!(buffer.cell((INPUT_X, 2)).unwrap().symbol(), "┃");
        assert_eq!(buffer.cell((INPUT_X, 3)).unwrap().symbol(), "╹");
        assert_eq!(buffer.cell((MAIN_COL_X, 3)).unwrap().symbol(), "▀");

        // Act / Assert (first message): selecting it scrolls up until the
        // leading framing row is visible above it.
        app.select_prev(); // -> m1
        app.select_prev(); // -> m0 (span 1): the reveal reaches the block top
        let buffer = render_sized(&app, "", 50, 12);
        assert_eq!(buffer.cell((MAIN_COL_X, 0)).unwrap().symbol(), "▄");
        assert_eq!(buffer.cell((INPUT_X, 0)).unwrap().symbol(), "╻");
        assert_eq!(buffer.cell((INPUT_X, 1)).unwrap().symbol(), "┃");
        assert_eq!(
            buffer.cell((MAIN_COL_X, 1)).unwrap().bg,
            theme().message_select_bg
        );
    }

    #[test]
    fn selection_not_rendered_when_messages_unfocused() {
        // Arrange: same setup but WITHOUT focusing the message pane.
        let mut app = test_app(22, 4);
        app.push_message(msg("a", "one"));
        app.push_message(msg("b", "two"));
        app.push_message(msg("c", "three"));

        // Act
        let buffer = render_sized(&app, "", 50, 12);

        // Assert: no highlight, half-blocks, or accent anywhere in the pane.
        for y in 0..=4 {
            assert_ne!(
                buffer.cell((MAIN_COL_X, y)).unwrap().bg,
                theme().message_select_bg
            );
            assert!(!["┃", "╻", "╹"].contains(&buffer.cell((INPUT_X, y)).unwrap().symbol()));
        }
    }

    #[test]
    fn input_area_has_background_and_accent_column() {
        // Arrange: 50x10 -> input occupies rows 3..=6 (above the 2-row gap
        // and the status bar on row 9).
        let app = test_app(22, 3);

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: each input row has the `┃` accent at the composer's left edge
        // (col INPUT_X) in pale green, standing on the global bg (no panel bg
        // behind it); the panel background starts one column to the right.
        for y in 3..=6 {
            let cell = buffer.cell((INPUT_X, y)).unwrap();
            assert_eq!(cell.symbol(), "┃", "accent missing on row {y}");
            assert_eq!(cell.fg, theme().accent, "accent color wrong on row {y}");
            assert_eq!(
                cell.bg,
                theme().global_bg,
                "accent should sit on global bg on row {y}"
            );
        }
        assert_eq!(buffer.cell((INPUT_X + 1, 4)).unwrap().bg, theme().input_bg); // panel starts here
    }

    #[test]
    fn input_area_shows_tips_on_last_row() {
        // Arrange: a wide terminal so the full status + tips line fits.
        let app = viewing_app(72, 3);

        // Act
        let buffer = render_sized(&app, "connected to irc.example.org", 100, 10);

        // Assert: the input's last row (row 6) carries status plus key hints.
        let tips = buffer_line(&buffer, 6);
        assert!(
            tips.contains("connected to irc.example.org"),
            "tips: {tips:?}"
        );
        assert!(tips.contains("Esc quit"), "tips: {tips:?}");
        assert!(tips.contains("Enter send"), "tips: {tips:?}");
    }

    #[test]
    fn input_row_shows_typed_text_and_cursor() {
        // Arrange
        let mut app = test_app(22, 3);
        app.type_char('h');
        app.type_char('i');

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: the input line (row 4) shows the text and a reverse-video cursor.
        let row = buffer_line(&buffer, 4);
        assert!(row.contains("hi"), "row was: {row:?}");
        // The cursor sits just past "hi" (at end) - a block cursor at col 28.
        assert_eq!(
            buffer.cell((MAIN_COL_X + 2, 4)).unwrap().bg,
            theme().input_cursor_bg
        );
    }

    #[test]
    fn separator_uninterrupted_full_height() {
        // Arrange
        let app = viewing_app(22, 3);

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: the gray `│` runs the full content height (every row above
        // the status bar) at col SIDEBAR_WIDTH, no longer overridden by the
        // composer (the `┃` now sits at col INPUT_X).
        for y in 0..buffer.area.height - 1 {
            assert_eq!(
                buffer.cell((SIDEBAR_WIDTH, y)).unwrap().symbol(),
                "│",
                "separator missing on row {y}"
            );
            assert_eq!(
                buffer.cell((SIDEBAR_WIDTH, y)).unwrap().fg,
                theme().separator
            );
        }
        // The `┃` lives only on the composer rows, at col INPUT_X.
        for y in 3..=6 {
            assert_eq!(buffer.cell((INPUT_X, y)).unwrap().symbol(), "┃");
        }
        assert_eq!(buffer.cell((INPUT_X, 1)).unwrap().symbol(), " ");
    }

    #[test]
    fn fade_row_uses_heavy_up_taper() {
        // Arrange
        let app = test_app(22, 3);

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: on the fade row (row 7), the composer's `┃` tapers via `╹`
        // (pale green, on the global bg); the rest of the row is the `▀` fade.
        let taper = buffer.cell((INPUT_X, 7)).unwrap();
        assert_eq!(taper.symbol(), "╹");
        assert_eq!(taper.fg, theme().accent);
        assert_eq!(taper.bg, theme().global_bg);
        let fade = buffer.cell((INPUT_X + 1, 7)).unwrap();
        assert_eq!(fade.symbol(), "▀");
        assert_eq!(fade.fg, theme().input_bg);
    }

    #[test]
    fn separator_and_background_colors() {
        // Arrange
        let app = viewing_app(22, 3);

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: a thin gray `│` separates sidebar and main on the message rows;
        // col SIDEBAR_WIDTH+1 is the 1-char blank gap (global bg).
        assert_eq!(buffer.cell((SIDEBAR_WIDTH, 1)).unwrap().symbol(), "│");
        assert_eq!(
            buffer.cell((SIDEBAR_WIDTH, 1)).unwrap().fg,
            theme().separator
        );
        assert_eq!(buffer.cell((SIDEBAR_WIDTH + 1, 1)).unwrap().symbol(), " ");
        assert_eq!(
            buffer.cell((SIDEBAR_WIDTH + 1, 1)).unwrap().bg,
            theme().global_bg
        );
        // The global background fills every non-composer region.
        assert_eq!(buffer.cell((0, 5)).unwrap().bg, theme().global_bg); // sidebar blank
        assert_eq!(buffer.cell((INPUT_X + 5, 9)).unwrap().bg, theme().global_bg); // gap blank
        assert_eq!(buffer.cell((INPUT_X + 4, 5)).unwrap().bg, theme().input_bg); // composer
    }

    /// Golden: with the default theme the render uses exactly the palette
    /// that was hardcoded before theming existed, byte for byte.
    #[test]
    fn default_theme_matches_the_pre_change_golden_palette() {
        // Arrange
        let mut app = viewing_app(22, 3);
        app.push_message(msg("alice", "hi"));

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: one pinned cell per pre-change hardcoded constant.
        assert_eq!(
            buffer.cell((0, 5)).unwrap().bg,
            Color::Rgb(0x0a, 0x0a, 0x0a) // GLOBAL_BG
        );
        assert_eq!(
            buffer.cell((SIDEBAR_WIDTH, 1)).unwrap().fg,
            Color::Rgb(0x55, 0x55, 0x55) // SEPARATOR
        );
        assert_eq!(
            buffer.cell((INPUT_X + 1, 5)).unwrap().bg,
            Color::Rgb(0x1e, 0x1e, 0x1e) // INPUT_BG panel
        );
        assert_eq!(
            buffer.cell((INPUT_X, 5)).unwrap().fg,
            Color::Rgb(0x4a, 0x6a, 0x4a) // INPUT_LINE_DIM (unfocused accent)
        );
        assert_eq!(
            buffer.cell((10, 1)).unwrap().bg,
            Color::Rgb(0x2e, 0x2e, 0x2e) // SIDEBAR_ACTIVE_BG
        );
        assert_eq!(buffer.cell((4, 1)).unwrap().fg, Color::Red); // status dot (not joined)
    }

    #[test]
    fn overriding_a_theme_role_changes_only_that_roles_pixels() {
        // Arrange
        let mut app = viewing_app(22, 3);
        app.push_message(msg("alice", "hi"));
        let theme = Theme {
            separator: Color::Rgb(0x33, 0x66, 0xff),
            ..Theme::default()
        };

        // Act: the same scene under the default and the overridden theme.
        let base = render_sized(&app, "", 50, 10);
        let themed = render_with_theme(&app, "", 50, 10, &theme);

        // Assert: the separator column carries the override...
        assert_eq!(base[(SIDEBAR_WIDTH, 1)].fg, Theme::default().separator);
        assert_eq!(themed[(SIDEBAR_WIDTH, 1)].fg, theme.separator);
        // ...and every other cell is untouched (the status bar's segment
        // separators also use the role, so its row is skipped with the
        // divider column).
        for y in 0..10 {
            for x in 0..50 {
                if x == SIDEBAR_WIDTH || y == 9 {
                    continue;
                }
                assert_eq!(
                    base[(x, y)],
                    themed[(x, y)],
                    "cell ({x},{y}) changed by the separator override"
                );
            }
        }
    }

    #[test]
    fn composer_right_margins() {
        // Arrange: for W=50 the panel spans cols 25..=46 (right gap 47..49 is
        // global bg), and the text keeps a 2-col margin inside the panel.
        let mut app = viewing_app(22, 3);
        app.type_char('h');
        app.type_char('i');

        // Act
        let buffer = render_sized(&app, "", 50, 10);
        let row = 5; // the input text row

        // Assert: panel right edge is 3 columns from the screen edge.
        assert_eq!(buffer.cell((46, row)).unwrap().bg, theme().input_bg); // panel rightmost
        for x in 47..=49 {
            assert_eq!(
                buffer.cell((x, row)).unwrap().bg,
                theme().global_bg,
                "col {x}"
            );
        }
        // Assert: no text reaches the last 2 panel columns (the text margin).
        assert_eq!(buffer.cell((45, row)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((46, row)).unwrap().symbol(), " ");
    }

    #[test]
    fn message_viewport_extends_down_to_the_composer() {
        // Arrange: H=11 with empty input -> the message viewport owns rows
        // 0..=3 (the old static spacer row is now the trailing separator in
        // the stream), composer rows 4..=7. An empty channel lays out to no
        // rows, so the pane is blank.
        let mut app = App::new(22, 3);
        app.open_channel("osu_irc", "#osu");
        app.select_buffer(0);

        // Act
        let buffer = render_sized(&app, "", 50, 11);

        // Assert: the pane's bottom row is blank with the global background
        // across the main column, and the composer ┃ starts on the row below.
        for x in INPUT_X..50 {
            assert_eq!(buffer.cell((x, 3)).unwrap().symbol(), " ", "col {x}");
            assert_eq!(
                buffer.cell((x, 3)).unwrap().bg,
                theme().global_bg,
                "col {x}"
            );
        }
        assert_eq!(buffer.cell((INPUT_X, 4)).unwrap().symbol(), "┃");
    }

    #[test]
    fn multiline_input_grows_the_composer() {
        // Arrange: 25 chars at text width 19 wrap into 2 lines, so the composer
        // is 5 rows (blank + 2 text + blank + tips). For H=11 that puts the
        // composer on rows 3..=7.
        let mut app = viewing_app(22, 2);
        for _ in 0..25 {
            app.type_char('x');
        }

        // Act
        let buffer = render_sized(&app, "", 50, 11);

        // Assert: the ┃ spans all 5 composer rows, both wrapped text lines are
        // shown, and the tips row moved down to row 7.
        for y in 3..=7 {
            assert_eq!(buffer.cell((INPUT_X, y)).unwrap().symbol(), "┃", "row {y}");
        }
        assert!(buffer_line(&buffer, 4).contains('x'));
        assert!(buffer_line(&buffer, 5).contains('x'));
        assert!(buffer_line(&buffer, 7).contains("Esc quit"));
    }

    #[test]
    fn multiline_input_squeezes_the_message_pane() {
        // Arrange / Act: empty input -> composer 4 rows (┃ top at row 4 for H=11);
        // 2-line input -> composer 5 rows (┃ top moves up to row 3).

        let app_empty = viewing_app(22, 3);
        let buf_empty = render_sized(&app_empty, "", 50, 11);
        assert_eq!(buf_empty.cell((INPUT_X, 4)).unwrap().symbol(), "┃");
        assert_eq!(buf_empty.cell((INPUT_X, 3)).unwrap().symbol(), " ");

        let mut app_full = viewing_app(22, 2);
        for _ in 0..25 {
            app_full.type_char('x');
        }
        let buf_full = render_sized(&app_full, "", 50, 11);

        // Assert: the composer's top edge moved up one row.
        assert_eq!(buf_full.cell((INPUT_X, 3)).unwrap().symbol(), "┃");
    }

    #[test]
    fn hovered_message_gets_dim_block_with_half_block_caps_sans_accent() {
        // Arrange: 3 one-line messages, viewport 4 (offset 3 at the
        // bottom): m1 sits on screen row 0, m2 on row 2. Focus stays on
        // the composer so no formal selection renders at all.
        let mut app = test_app(22, 4);
        app.push_message(msg("a", "one"));
        app.push_message(msg("b", "two"));
        app.push_message(msg("c", "three"));
        app.set_hover_message(Some(1));

        // Act
        let buffer = render_sized(&app, "", 50, 12);

        // Assert: m1's row carries the hover background, flush against the
        // accent column (same x-range as the formal selection block)...
        assert_eq!(buffer.cell((MAIN_COL_X, 0)).unwrap().bg, theme().hover_bg);
        assert_eq!(buffer.cell((INPUT_X + 1, 0)).unwrap().bg, theme().hover_bg);
        // ...the separator row below m1 carries the block's bottom cap, an
        // upper-half block in the hover color (fg only, exactly like the
        // formal selection's caps); m1's above-separator is scrolled
        // off-screen, so no top cap is visible.
        let cap = buffer.cell((MAIN_COL_X, 1)).unwrap();
        assert_eq!(cap.symbol(), "▀");
        assert_eq!(cap.fg, theme().hover_bg);
        assert_eq!(cap.bg, theme().global_bg);
        // ...no accent column chars anywhere...
        for y in 0..=4 {
            assert!(
                !["┃", "╻", "╹"].contains(&buffer.cell((INPUT_X, y)).unwrap().symbol()),
                "accent at row {y}"
            );
        }
        // ...and no half-block symbols on any other row (the cap is row 1).
        for y in [0, 2, 3, 4] {
            let symbol = buffer.cell((MAIN_COL_X, y)).unwrap().symbol();
            assert!(symbol != "▄" && symbol != "▀", "half block at row {y}");
        }
        // The un-hovered m2 row carries no highlight.
        assert_ne!(buffer.cell((MAIN_COL_X, 2)).unwrap().bg, theme().hover_bg);
    }

    #[test]
    fn formal_selection_wins_over_hover_on_the_same_message() {
        // Arrange: messages focused (m2 selected), pointer hovering the
        // same m2.
        let mut app = test_app(22, 4);
        app.push_message(msg("a", "one"));
        app.push_message(msg("b", "two"));
        app.push_message(msg("c", "three"));
        app.tab();
        app.tab(); // messages focused -> selects m2
        app.set_hover_message(app.selected());

        // Act
        let buffer = render_sized(&app, "", 50, 12);

        // Assert: exactly the formal selection background on m2's row -
        // no dim layer underneath can be distinguished.
        assert_eq!(
            buffer.cell((MAIN_COL_X, 2)).unwrap().bg,
            theme().message_select_bg
        );
        assert_ne!(buffer.cell((MAIN_COL_X, 2)).unwrap().bg, theme().hover_bg);
    }

    #[test]
    fn hover_and_formal_selection_coexist_on_different_messages() {
        // Arrange: m2 formally selected (pane focused), m1 hovered.
        let mut app = test_app(22, 4);
        app.push_message(msg("a", "one"));
        app.push_message(msg("b", "two"));
        app.push_message(msg("c", "three"));
        app.tab();
        app.tab();
        app.set_hover_message(Some(1));

        // Act
        let buffer = render_sized(&app, "", 50, 12);

        // Assert: row 0 (m1) hover, row 2 (m2) formal selection.
        assert_eq!(buffer.cell((MAIN_COL_X, 0)).unwrap().bg, theme().hover_bg);
        assert_eq!(
            buffer.cell((MAIN_COL_X, 2)).unwrap().bg,
            theme().message_select_bg
        );
        // The shared separator row (screen row 1) blends the two adjacent
        // blocks: the lower half follows the message below (selected m2 ->
        // "▄" with fg = select color) and the upper half follows the message
        // above (hovered m1 -> bg = hover color).
        assert_eq!(buffer.cell((MAIN_COL_X, 1)).unwrap().symbol(), "▄");
        assert_eq!(
            buffer.cell((MAIN_COL_X, 1)).unwrap().fg,
            theme().message_select_bg
        );
        assert_eq!(buffer.cell((MAIN_COL_X, 1)).unwrap().bg, theme().hover_bg);
    }

    #[test]
    fn adjacent_blocks_blend_on_the_shared_separator() {
        // Arrange: 3 one-line messages, viewport 4. Focusing the messages
        // selects m2 and auto-follow keeps offset 3; select_prev to m1
        // reveals m1's block top, scrolling to offset 2 - m1's top cap on
        // screen row 0, m1 on row 1, the shared separator row 2, m2 on
        // row 3. Formally select m1 (the message ABOVE), hover m2 (BELOW).
        let mut app = test_app(22, 4);
        app.push_message(msg("a", "one"));
        app.push_message(msg("b", "two"));
        app.push_message(msg("c", "three"));
        app.tab();
        app.tab(); // messages focused -> selects m2 (newest visible)
        app.select_prev(); // -> m1 (reveal scrolls offset 3 -> 2)
        assert_eq!(app.selected(), Some(1));
        app.set_hover_message(Some(2));

        // Act
        let buffer = render_sized(&app, "", 50, 12);

        // Assert: upper half of the shared separator follows the message
        // above (selected m1 -> ▀ with fg = select color), lower half
        // follows the message below (hovered m2 -> bg = hover color).
        assert_eq!(
            buffer.cell((MAIN_COL_X, 1)).unwrap().bg,
            theme().message_select_bg
        );
        assert_eq!(buffer.cell((MAIN_COL_X, 3)).unwrap().bg, theme().hover_bg);
        let shared = buffer.cell((MAIN_COL_X, 2)).unwrap();
        assert_eq!(shared.symbol(), "▀");
        assert_eq!(shared.fg, theme().message_select_bg);
        assert_eq!(shared.bg, theme().hover_bg);
    }

    #[test]
    fn sidebar_hover_highlights_plain_rows_only() {
        // Arrange: hover the plain #chinese row (row 2).
        let mut app = test_app(22, 3);
        app.set_hover_sidebar(Some(2));

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: plain hovered row gets the hover background; hovering
        // the active channel row keeps its brighter active background.
        assert_eq!(buffer.cell((10, 2)).unwrap().bg, theme().hover_bg);
        app.set_hover_sidebar(Some(1));
        let buffer = render_sized(&app, "", 50, 10);
        assert_eq!(buffer.cell((10, 1)).unwrap().bg, theme().sidebar_active_bg);
    }

    /// Screen row of the first line containing `needle`, if any.
    fn row_containing(buffer: &Buffer, needle: &str) -> Option<u16> {
        (0..buffer.area.height).find(|&y| buffer_line(buffer, y).contains(needle))
    }

    /// Fit the app like the event loop does (viewport sized to the current
    /// geometry, including hint rows), then render.
    fn render_fitted(app: &mut App, w: u16, h: u16) -> Buffer {
        let g = geometry(
            w,
            h,
            app.input(),
            app.input_cursor(),
            app.active_buffer().is_some(),
            app.hint_rows(),
        );
        app.set_sidebar_height(g.sidebar.height);
        if app.size() != (g.messages.width, g.messages.height) {
            app.resize(g.messages.width, g.messages.height);
        }
        render_sized(app, "", w, h)
    }

    /// A viewed #osu channel with the composer focused, sitting at the
    /// bottom of a 12-message history.
    fn hint_scroll_app() -> App {
        let mut app = App::new(40, 8);
        app.open_channel("osu_irc", "#osu");
        app.select_buffer(0);
        app.focus_composer();
        for index in 0..12 {
            app.push_message(msg("alice", &format!("line{index}")));
        }
        app
    }

    #[test]
    fn hint_rows_come_out_of_the_message_pane_and_shift_content_up() {
        // Arrange: a long, bottom-anchored history with no draft.
        let mut app = hint_scroll_app();
        app.restore_input(String::new());
        let plain = render_fitted(&mut app, 60, 14);
        let plain_row = row_containing(&plain, "line10").unwrap();

        // Act: drafting `/j` shows one hint row under its separator rule.
        app.restore_input("/j".into());
        let hinted = render_fitted(&mut app, 60, 14);

        // Assert: the pane shrinks by exactly the hint block's two rows,
        // the block sits directly above the composer, and the bottom-
        // anchored content moves up by the same two rows.
        let plain_g = geometry(60, 14, "", 0, true, 0);
        let hinted_g = geometry(60, 14, "/j", 2, true, 2);
        assert_eq!(hinted_g.hints.height, 2);
        assert_eq!(
            plain_g.messages.height - hinted_g.messages.height,
            hinted_g.hints.height
        );
        assert_eq!(hinted_g.hints.bottom(), hinted_g.composer.y);
        assert_eq!(
            row_containing(&hinted, "line10").unwrap(),
            plain_row - hinted_g.hints.height
        );

        // The rule and the described command render inside the block.
        let rule_row = hinted_g.hints.y;
        assert!(buffer_line(&hinted, rule_row).contains('─'));
        let hint_row = row_containing(&hinted, "/join").unwrap();
        assert_eq!(hint_row, rule_row + 1);
        assert!(buffer_line(&hinted, hint_row).contains("Join or focus"));
    }

    #[test]
    fn hiding_the_hint_bar_restores_the_message_pane() {
        // Arrange: the hint bar is visible and the pane is compressed.
        let mut app = hint_scroll_app();
        app.restore_input("/j".into());
        let hinted = render_fitted(&mut app, 60, 14);
        assert!(row_containing(&hinted, "/join").is_some());

        // Act: clearing the draft hides the bar.
        app.restore_input(String::new());
        let plain = render_fitted(&mut app, 60, 14);

        // Assert: no hint text remains and the pane grew back.
        assert!(row_containing(&plain, "/join").is_none());
        let plain_g = geometry(60, 14, "", 0, true, 0);
        assert_eq!(plain_g.messages.height, 7);
        assert_eq!(plain_g.hints.height, 0);
        assert!(app.is_at_bottom());
    }

    #[test]
    fn highlighted_hint_row_renders_bold_reversed_with_descriptions() {
        // Arrange: two hints with the second row highlighted.
        let mut app = hint_scroll_app();
        app.restore_input("/c".into());
        app.hint_down();
        let buffer = render_fitted(&mut app, 60, 14);
        let close_row = row_containing(&buffer, "/close").unwrap();
        let connect_row = row_containing(&buffer, "/connect").unwrap();

        // Assert: both rows show their descriptions; only the highlighted
        // row carries the bold reverse-video highlight.
        assert!(buffer_line(&buffer, close_row).contains("Hide the current"));
        assert!(
            buffer_line(&buffer, connect_row).contains("Connect a stopped"),
            "connect description missing"
        );
        let close_cell = buffer.cell((MAIN_COL_X, close_row)).unwrap();
        assert!(
            !close_cell
                .modifier
                .contains(ratatui::style::Modifier::REVERSED),
            "unhighlighted row must stay plain"
        );
        let connect_cell = buffer.cell((MAIN_COL_X, connect_row)).unwrap();
        assert!(
            connect_cell
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        assert!(
            connect_cell
                .modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn tiny_terminals_clamp_the_hint_bar_without_panicking() {
        // Arrange: a draft whose hint block wants three rows.
        let mut app = hint_scroll_app();
        app.restore_input("/c".into());
        for h in 0..12u16 {
            for w in [24u16, 50] {
                // Act: fit and draw (panics surface here).
                let g = geometry(w, h, app.input(), app.input_cursor(), true, app.hint_rows());
                app.set_sidebar_height(g.sidebar.height);
                app.resize(g.messages.width, g.messages.height);
                render_sized(&app, "", w, h);

                // Assert: composer plus gap plus hints plus messages never
                // exceed the terminal, and one message row survives while
                // any hint row does.
                assert!(g.messages.bottom() <= h);
                if g.hints.height > 0 {
                    assert!(g.messages.height >= 1, "h={h} w={w}");
                }
            }
        }
    }

    /// Column ranges (arrow column included) of the bar's three blocks at
    /// width 50 with the osu_irc/#osu buffer:
    /// ` server osu_irc ▶ channel #osu ▶ type in ▶`.
    fn bar_segments() -> [(StatusZone, u16, u16); 3] {
        [
            (StatusZone::Server, 0, 17),
            (StatusZone::Channel, 17, 32),
            (StatusZone::Input, 32, 42),
        ]
    }

    #[test]
    fn status_bar_renders_three_segments_on_the_bottom_row() {
        // Arrange: viewing #osu on osu_irc with the composer focused.
        let app = test_app(22, 3);

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: the bottom row carries all three zone labels and their
        // values, and nothing above the bar mentions them.
        let bottom = buffer_line(&buffer, 9);
        assert!(bottom.contains("server osu_irc"), "bottom: {bottom:?}");
        assert!(bottom.contains("channel #osu"), "bottom: {bottom:?}");
        assert!(bottom.contains("type in"), "bottom: {bottom:?}");
        for y in 0..9 {
            assert!(
                !buffer_line(&buffer, y).contains("type in"),
                "bar text leaked to row {y}"
            );
        }
    }

    #[test]
    fn focused_composer_highlights_the_status_bar_input_segment() {
        // Arrange: test_app leaves the composer focused.
        let app = test_app(22, 3);

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: every body cell of the input block (padding included) is
        // bold on the highlight background; its trailing arrow is the
        // highlight color pointing onto the global background, reading as
        // part of the block; the other blocks stay dim throughout.
        for &(zone, start, end) in bar_segments().iter() {
            for x in start..end {
                let cell = buffer.cell((x, 9)).unwrap();
                let arrow = x + 1 == end; // the block's trailing `▶`
                match (zone, arrow) {
                    (StatusZone::Input, false) => {
                        assert_eq!(cell.bg, theme().status_bar_highlight, "col {x}");
                        assert_eq!(cell.fg, theme().global_bg, "col {x}");
                        assert!(
                            cell.modifier.contains(ratatui::style::Modifier::BOLD),
                            "col {x}"
                        );
                    }
                    (StatusZone::Input, true) => {
                        assert_eq!(cell.symbol(), "▶", "col {x}");
                        assert_eq!(cell.fg, theme().status_bar_highlight, "col {x}");
                        assert_eq!(cell.bg, theme().global_bg, "col {x}");
                    }
                    _ => {
                        assert_eq!(cell.fg, theme().dim, "col {x}");
                        if arrow {
                            // The arrow points onto the following block's
                            // background: the channel block's arrow sits on
                            // the highlighted input block, the server
                            // block's on the plain channel block.
                            assert_eq!(cell.symbol(), "▶", "col {x}");
                            let next_highlighted = zone == StatusZone::Channel;
                            assert_eq!(
                                cell.bg,
                                if next_highlighted {
                                    theme().status_bar_highlight
                                } else {
                                    theme().global_bg
                                },
                                "col {x}"
                            );
                        } else {
                            assert_eq!(cell.bg, theme().global_bg, "col {x}");
                        }
                    }
                }
            }
        }
        // Nothing is painted past the last block's arrow.
        assert_eq!(buffer.cell((42, 9)).unwrap().symbol(), " ");
    }

    #[test]
    fn sidebar_navigation_highlights_the_selected_zone_segment() {
        // Arrange: viewing #osu, then focus the sidebar with its cursor on
        // the server header, then move down onto the channel row.
        let mut app = viewing_app(22, 3);
        app.focus_sidebar_server();
        let server = render_sized(&app, "", 50, 10);
        assert_eq!(app.sidebar_cursor(), Some(0)); // the osu_irc header
        app.sidebar_down();
        let channel = render_sized(&app, "", 50, 10);
        assert_eq!(app.sidebar_cursor(), Some(1)); // the #osu row

        // Assert: the highlighted segment follows the cursor's row kind —
        // the server zone while on the header, the channel zone on #osu.
        let [(_, ss, _), (_, cs, _), _] = bar_segments();
        assert_eq!(
            server.cell((ss + 1, 9)).unwrap().bg,
            theme().status_bar_highlight
        );
        assert_eq!(
            channel.cell((ss + 1, 9)).unwrap().bg,
            theme().global_bg,
            "server segment must dim once the cursor leaves the header"
        );
        assert_eq!(
            channel.cell((cs + 1, 9)).unwrap().bg,
            theme().status_bar_highlight
        );
    }

    #[test]
    fn status_bar_zone_hit_testing_matches_the_painted_blocks() {
        // Arrange: viewing #osu on osu_irc at width 50.
        let app = viewing_app(22, 3);

        // Act / Assert: each painted block maps to its zone — body and
        // trailing arrow alike — and columns past the last block map to
        // nothing.
        for &(zone, start, end) in bar_segments().iter() {
            assert_eq!(status_bar_zone_at(&app, 50, start + 1), Some(zone));
            assert_eq!(status_bar_zone_at(&app, 50, end - 1), Some(zone)); // the `▶`
        }
        assert_eq!(status_bar_zone_at(&app, 50, 42), None); // past the bar
        assert_eq!(status_bar_zone_at(&app, 50, 49), None);
    }

    #[test]
    fn status_bar_hides_blocks_without_a_value_instead_of_placeholders() {
        // Arrange: a fresh app with no buffers (the welcome page).
        let app = App::new(50, 10);

        // Act
        let buffer = render_sized(&app, "", 80, 24);

        // Assert: only the standing input block remains — no server or
        // channel label, no placeholder, one trailing arrow.
        let bottom = buffer_line(&buffer, 23);
        assert!(bottom.contains("type in"), "bottom: {bottom:?}");
        assert!(bottom.contains('▶'), "bottom: {bottom:?}");
        assert!(!bottom.contains("server"), "bottom: {bottom:?}");
        assert!(!bottom.contains("channel"), "bottom: {bottom:?}");
        assert!(!bottom.contains("not connected"), "bottom: {bottom:?}");
        // Only the input block's columns are clickable; a server-column
        // click (where the block would start) is dead space.
        assert_eq!(status_bar_zone_at(&app, 80, 1), Some(StatusZone::Input));
        assert_eq!(status_bar_zone_at(&app, 80, 40), None);
    }

    #[test]
    fn server_console_view_hides_the_channel_block() {
        // Arrange: a server console buffer viewed with the composer focused.
        let mut app = App::new(50, 10);
        let console = app.open_server("osu_irc");
        app.activate_buffer(console);
        app.focus_composer();

        // Act
        let buffer = render_sized(&app, "", 80, 24);

        // Assert: the server block names the console and the channel block
        // hides (the server block already identifies it).
        let bottom = buffer_line(&buffer, 23);
        assert!(bottom.contains("server osu_irc"), "bottom: {bottom:?}");
        assert!(bottom.contains("type in"), "bottom: {bottom:?}");
        assert!(!bottom.contains("channel"), "bottom: {bottom:?}");
    }

    #[test]
    fn status_bar_survives_tiny_terminals_without_panicking() {
        // Arrange: a viewed channel with long-ish names so the bar must
        // shrink and clip its segments.
        let mut app = App::new(20, 3);
        app.open_channel("a_very_long_server_name", "#a-long-channel-name");
        app.select_buffer(0);
        app.focus_composer();

        // Act: fit and draw across a grid of tiny sizes (panics surface
        // here); at h=1 the bar yields its row to the composer.
        for w in 0..=12u16 {
            for h in 0..=4u16 {
                let g = geometry(w, h, app.input(), app.input_cursor(), true, 0);
                app.set_sidebar_height(g.sidebar.height);
                app.resize(g.messages.width, g.messages.height);
                render_sized(&app, "", w, h);
                if h >= 2 {
                    assert_eq!(g.status_bar.height, 1, "h={h} w={w}");
                    assert_eq!(g.status_bar.y, h - 1, "h={h} w={w}");
                    assert!(g.status_bar.bottom() <= h);
                } else {
                    assert_eq!(g.status_bar.height, 0, "h={h} w={w}");
                }
            }
        }
    }
}
