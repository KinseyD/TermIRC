//! Rendering: turns app state into ratatui widgets.
//!
//! Screen layout (no box frames):
//! ```text
//!  sidebar (fixed)  │  main column
//!  osu_irc           │  (blank separator)          <- framing separator rows
//!    ▶ #osu         │  alice: hello ...            <- messages (scroll)
//!      #chinese     │
//!                    │  ┃                          <- input (4 rows, bg-shaded,
//!                    │  ┃   (empty input)               accent ┃ on the left)
//!                    │  ┃  connected · q quit …    <- tips row
//! ```
//! Before any channel is opened the main column shows only the centered logo -
//! no composer. Lines are pre-wrapped by `layout`, so the messages `Paragraph`
//! is rendered without `Wrap`: continuation rows already carry their own
//! indentation, keeping the message body clear of the nick column.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
};

use crate::app::{App, Focus};

const NICK_STYLE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);

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

/// Global background color for the whole screen.
const GLOBAL_BG: Color = Color::Rgb(0x0a, 0x0a, 0x0a);
/// Background color distinguishing the composer region.
const INPUT_BG: Color = Color::Rgb(0x1e, 0x1e, 0x1e);
/// Color of the thin vertical line separating the sidebar from the main column.
const SEPARATOR: Color = Color::Rgb(0x55, 0x55, 0x55);
/// Color of the composer's left accent `┃` (pale green), drawn on the global
/// background (no composer-panel background behind it).
const INPUT_LINE: Color = Color::Rgb(0x9a, 0xd9, 0x9a);
/// The composer accent when the composer does not have focus (dimmed).
const INPUT_LINE_DIM: Color = Color::Rgb(0x4a, 0x6a, 0x4a);
/// Background of the sidebar row under the cursor.
const SIDEBAR_CURSOR_BG: Color = Color::Rgb(0x20, 0x20, 0x20);
/// Background of the sidebar's active channel row.
const SIDEBAR_ACTIVE_BG: Color = Color::Rgb(0x2e, 0x2e, 0x2e);
/// Background highlight of the selected message in the message pane.
const MESSAGE_SELECT_BG: Color = Color::Rgb(0x24, 0x24, 0x24);
const DIM_STYLE: Style = Style::new().fg(Color::Rgb(0x70, 0x70, 0x70));

/// Render-only chrome state: everything the renderer needs beyond the App.
pub struct Chrome<'a> {
    pub status: &'a str,
}

/// Convert the app's laid-out rows into ratatui `Text`.
///
/// Borrows the row bodies instead of cloning them: the `Paragraph` only needs
/// the text for the duration of the render call, so per-frame allocations stay
/// flat no matter how much scrollback is held.
pub fn build_text(app: &App) -> Text<'_> {
    let lines: Vec<Line<'_>> = app
        .lines()
        .iter()
        .map(|row| match row.nick.as_ref() {
            Some(nick) => Line::from(vec![
                Span::styled(format!("{nick}: "), NICK_STYLE),
                Span::raw(row.body.as_str()),
            ]),
            None if row.body.is_empty() => Line::from(""),
            None => Line::from(vec![
                Span::raw(" ".repeat(usize::from(row.indent))),
                Span::raw(row.body.as_str()),
            ]),
        })
        .collect();
    Text::from(lines)
}

/// Render the whole screen: sidebar on the left, main column (messages /
/// composer) on the right. There is no title row - the message stream frames
/// itself with separator rows above the first and below the last message.
pub fn draw(frame: &mut Frame, app: &App, chrome: &Chrome<'_>) {
    // Fill the screen with the global background.
    frame.render_widget(
        Paragraph::new("").style(Style::new().bg(GLOBAL_BG)),
        frame.area(),
    );

    let columns = Layout::horizontal([
        Constraint::Length(SIDEBAR_WIDTH), // sidebar
        Constraint::Length(SEPARATOR_GAP), // │ separator + 1-col blank gap
        Constraint::Min(0),                // main column
    ])
    .split(frame.area());
    render_sidebar(frame, columns[0], app);
    render_separator(frame, columns[1]);

    // Welcome page (no channel opened yet): the logo centered in the whole
    // main column - no composer on this page.
    if app.active_channel().is_none() {
        render_welcome(frame, inset(columns[2], HORIZONTAL_PAD));
        return;
    }

    // The composer's wrapped input lines drive its height.
    let (_, input_text_width) = input_text_geometry(frame.area().width);
    let focused = app.focus() == Focus::Composer;
    let input_lines =
        build_wrapped_input_lines(app.input(), app.input_cursor(), input_text_width, focused);
    let input_height = composer_height(input_lines.len());

    let rows = Layout::vertical([
        Constraint::Min(0), // messages (framing separators live in the stream)
        Constraint::Length(input_height), // composer
        Constraint::Length(GAP_ROWS), // fade + blank below the composer
    ])
    .split(columns[2]);

    let message_rect = inset(rows[0], HORIZONTAL_PAD);
    frame.render_widget(
        Paragraph::new(build_text(app)).scroll((app.scroll_offset(), 0)),
        message_rect,
    );
    render_selection(frame, message_rect, app);
    render_composer(
        frame,
        rows[1],
        frame.area().width,
        input_lines,
        chrome.status,
        app,
    );
    render_gap(frame, rows[2], frame.area().width, app);
}

/// Render the thin gray vertical line that separates the sidebar from the main
/// column, spanning the full height (left edge of the separator-gap segment).
fn render_separator(frame: &mut Frame, area: Rect) {
    let col = Rect::new(area.x, area.y, 1, area.height);
    let line = Line::from(Span::styled("│", Style::new().fg(SEPARATOR)));
    let lines = vec![line; usize::from(area.height)];
    frame.render_widget(Paragraph::new(Text::from(lines)), col);
}

/// Render the server/channel sidebar from the app's row model: a fold marker
/// per server, its channels beneath, the active channel row with a brighter
/// background, and (while the sidebar has focus) a cursor row with a slightly
/// brighter background plus a block cursor at its start.
fn render_sidebar(frame: &mut Frame, area: Rect, app: &App) {
    let inner = inset(area, 1);
    let focused = app.focus() == Focus::Sidebar;
    let cursor = app.sidebar_cursor();
    let active = app.active_channel();
    let rows = app.sidebar_rows();

    for (i, row) in rows.iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let is_cursor = focused && cursor == Some(i);
        let is_active = active.is_some_and(|(srv, ch)| {
            row.server.eq_ignore_ascii_case(srv) && row.channel.as_deref() == Some(ch)
        });

        // Row background: the active channel row is brightest; a cursor row is
        // slightly brighter than the plain background.
        let bg = if is_active {
            SIDEBAR_ACTIVE_BG
        } else if is_cursor {
            SIDEBAR_CURSOR_BG
        } else {
            Color::Reset
        };
        let row_rect = Rect::new(inner.x, y, inner.width, 1);
        if bg != Color::Reset {
            frame.buffer_mut().set_style(row_rect, Style::new().bg(bg));
        }

        // Row text: a leading space reserves room for the block cursor.
        let line = match &row.channel {
            None => Line::from(vec![
                Span::raw(" "),
                Span::raw(if app.server_collapsed(&row.server) {
                    "▸ "
                } else {
                    "▾ "
                }),
                Span::styled(
                    row.server.as_str(),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
            ]),
            Some(channel) => Line::from(vec![
                Span::raw(" "),
                Span::raw("  "),
                Span::styled(channel.as_str(), DIM_STYLE),
            ]),
        };
        frame.render_widget(Paragraph::new(line), row_rect);

        // Block cursor at the start of the focused row.
        if is_cursor {
            frame.buffer_mut()[(inner.x, y)]
                .set_symbol(" ")
                .set_style(Style::new().fg(INPUT_BG).bg(Color::White));
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
    (text_left, text_right.saturating_sub(text_left))
}

/// Height of the composer region for a given number of wrapped input lines.
pub fn composer_height(input_lines: usize) -> u16 {
    input_lines as u16 + INPUT_FIXED_ROWS
}

/// Build the composer's input lines: hard-wrap `text` to `width` columns; when
/// `show_cursor` is set, render a reverse-video cursor at the `cursor` char
/// index (a solid block when the cursor sits at the end of the text).
fn build_wrapped_input_lines(
    text: &str,
    cursor: usize,
    width: u16,
    show_cursor: bool,
) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();
    let cursor = cursor.min(total);

    let mut segments: Vec<Vec<char>> = Vec::new();
    let mut i = 0;
    while i < total {
        let end = (i + width).min(total);
        segments.push(chars[i..end].to_vec());
        i = end;
    }
    if segments.is_empty() {
        segments.push(Vec::new());
    }
    // A cursor at the end of a full line needs its own line to sit on.
    if show_cursor && cursor == total && total > 0 && total.is_multiple_of(width) {
        segments.push(Vec::new());
    }

    let cursor_style = Style::new().fg(INPUT_BG).bg(Color::White);
    let mut lines = Vec::new();
    let mut base = 0usize;
    for (si, seg) in segments.iter().enumerate() {
        let is_last = si == segments.len() - 1;
        let mut before = String::new();
        let mut cursor_str: Option<String> = None;
        let mut after = String::new();
        if show_cursor {
            for (j, &c) in seg.iter().enumerate() {
                let gidx = base + j;
                if gidx == cursor && cursor_str.is_none() {
                    cursor_str = Some(c.to_string());
                } else if cursor_str.is_none() {
                    before.push(c);
                } else {
                    after.push(c);
                }
            }
            if cursor_str.is_none() && cursor == base + seg.len() && is_last {
                cursor_str = Some(" ".to_string());
            }
        } else {
            before = seg.iter().collect();
        }
        let mut spans: Vec<Span<'static>> = Vec::new();
        if !before.is_empty() {
            spans.push(Span::raw(before));
        }
        if let Some(cs) = cursor_str {
            spans.push(Span::styled(cs, cursor_style));
        }
        if !after.is_empty() {
            spans.push(Span::raw(after));
        }
        lines.push(Line::from(spans));
        base += seg.len();
    }
    lines
}

/// Render the composer: an INPUT_BG panel with a pale-green `┃` accent on its
/// left (standing on the global background), the wrapped input lines with a
/// reverse-video cursor, and a tips row. Rows: blank / text×N / blank / tips.
/// When the composer is not focused the accent dims and no cursor is shown.
fn render_composer(
    frame: &mut Frame,
    area: Rect,
    screen_w: u16,
    input_lines: Vec<Line<'static>>,
    status: &str,
    app: &App,
) {
    let panel_left = COMPOSER_ACCENT_X + 1;
    let panel_w = screen_w
        .saturating_sub(INPUT_RIGHT_GAP)
        .saturating_sub(panel_left);
    // Panel background (starts right of the ┃, so the ┃ sits on the global bg).
    frame.buffer_mut().set_style(
        Rect::new(panel_left, area.y, panel_w, area.height),
        Style::new().bg(INPUT_BG),
    );

    // ┃ accent at the composer's left edge, spanning all its rows. Dimmed
    // while another region holds the focus.
    let accent_color = if app.focus() == Focus::Composer {
        INPUT_LINE
    } else {
        INPUT_LINE_DIM
    };
    let accent = Span::styled("┃", Style::new().fg(accent_color));
    let accent_lines: Vec<Line<'static>> = (0..area.height)
        .map(|_| Line::from(accent.clone()))
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(accent_lines)),
        Rect::new(COMPOSER_ACCENT_X, area.y, 1, area.height),
    );

    // Wrapped input lines on rows 1..=n; tips row after the bottom blank.
    let (text_left, text_width) = input_text_geometry(screen_w);
    let n = input_lines.len() as u16;
    frame.render_widget(
        Paragraph::new(Text::from(input_lines)),
        Rect::new(text_left, area.y + 1, text_width, n),
    );
    let tips = if status.is_empty() {
        "Esc quit · PgUp/PgDn scroll".to_string()
    } else {
        format!("{status}  ·  Esc quit · PgUp/PgDn scroll")
    };
    frame.render_widget(
        Paragraph::new(tips),
        Rect::new(text_left, area.y + n + 2, text_width, 1),
    );
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
fn render_selection(frame: &mut Frame, msg_rect: Rect, app: &App) {
    if app.focus() != Focus::Messages {
        return;
    }
    let Some((start, height)) = app.selected_span() else {
        return;
    };
    let off = i64::from(app.scroll_offset());
    let first = i64::from(start) - off;
    let last = first + i64::from(height) - 1;
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
        Style::new().bg(MESSAGE_SELECT_BG),
    );

    // 2. Half-block separator rows (clipped to the pane): ▄ above, ▀ below
    //    (fg = highlight color, background untouched).
    let buf = frame.buffer_mut();
    if first > 0 {
        fill_half_block_row(
            buf,
            block_x,
            msg_rect.y + (first - 1) as u16,
            block_w,
            "▄",
            MESSAGE_SELECT_BG,
        );
    }
    if last + 1 < vh {
        fill_half_block_row(
            buf,
            block_x,
            msg_rect.y + (last + 1) as u16,
            block_w,
            "▀",
            MESSAGE_SELECT_BG,
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
            .set_style(Style::new().fg(INPUT_LINE));
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
fn render_welcome(frame: &mut Frame, area: Rect) {
    let logo_w = WELCOME_LOGO
        .iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let logo_h = WELCOME_LOGO.len() as u16;
    let top = area.y + area.height.saturating_sub(logo_h) / 2;
    let left = area.x + area.width.saturating_sub(logo_w) / 2;
    let style = Style::new().fg(INPUT_LINE).add_modifier(Modifier::BOLD);
    let art: Vec<Line<'_>> = WELCOME_LOGO
        .iter()
        .map(|row| Line::from(Span::styled(*row, style)))
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(art)),
        Rect::new(left, top, logo_w.min(area.width), logo_h.min(area.height)),
    );
}

/// Fill one row with a half-block character in the given color.
fn fill_half_block_row(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    width: u16,
    symbol: &str,
    color: Color,
) {
    for col in x..x + width {
        buf[(col, y)]
            .set_symbol(symbol)
            .set_style(Style::new().fg(color));
    }
}

/// Render the gap below the composer: the `┃` accent tapers into a `╹` and the
/// composer panel fades out via `▀` across its width.
fn render_gap(frame: &mut Frame, area: Rect, screen_w: u16, app: &App) {
    let panel_left = COMPOSER_ACCENT_X + 1;
    let panel_w = screen_w
        .saturating_sub(INPUT_RIGHT_GAP)
        .saturating_sub(panel_left);
    let taper_color = if app.focus() == Focus::Composer {
        INPUT_LINE
    } else {
        INPUT_LINE_DIM
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("╹", Style::new().fg(taper_color)))),
        Rect::new(COMPOSER_ACCENT_X, area.y, 1, 1),
    );
    let fade = Span::styled("▀".repeat(panel_w as usize), Style::new().fg(INPUT_BG));
    frame.render_widget(
        Paragraph::new(Line::from(fade)),
        Rect::new(panel_left, area.y, panel_w, 1),
    );
}

/// Shrink a region by `pad` columns on each side (vertical unchanged) so
/// content never touches the region's left/right edges.
fn inset(area: Rect, pad: u16) -> Rect {
    Rect::new(
        area.x + pad,
        area.y,
        area.width.saturating_sub(2 * pad),
        area.height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ChatMessage;
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    fn msg(nick: &str, text: &str) -> ChatMessage {
        ChatMessage {
            server: "osu_irc".to_string(),
            channel: "#osu".to_string(),
            nick: nick.to_string(),
            text: text.to_string(),
        }
    }

    /// An app with both channels of the test config registered, viewing #osu,
    /// with focus moved off the sidebar (as if the user opened a channel).
    fn test_app(width: u16, height: u16) -> App {
        let mut app = App::new(width, height);
        app.open_channel("osu_irc", "#osu");
        app.open_channel("osu_irc", "#chinese");
        app.select_channel(0);
        app.tab(); // Sidebar -> Messages
        app.tab(); // Messages -> Composer
        app
    }

    /// An app with #osu open and viewed (so the composer is rendered, unlike
    /// on the welcome page).
    fn viewing_app(width: u16, height: u16) -> App {
        let mut app = App::new(width, height);
        app.open_channel("osu_irc", "#osu");
        app.select_channel(0);
        app
    }

    /// Render into a headless terminal. The message viewport is sized to match
    /// what `main.rs` would compute: main column width minus 2*HORIZONTAL_PAD,
    /// height minus the fixed chrome rows.
    fn render_sized(app: &App, status: &str, w: u16, h: u16) -> Buffer {
        let chrome = Chrome { status };
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
        assert_eq!(app.active_channel(), None);

        // Act: a 100x30 terminal gives the whole main column to the welcome
        // page; the 6-row logo is vertically centered -> top at row 12.
        let buffer = render_sized(&app, "", 100, 30);

        // Assert: the logo art is rendered in the accent color...
        let top_row = buffer_line(&buffer, 12);
        assert!(top_row.contains("████████╗"), "row 12: {top_row:?}");
        let logo_cell = buffer
            .cell((top_row.find('█').unwrap() as u16, 12))
            .unwrap();
        assert_eq!(logo_cell.fg, INPUT_LINE);
        assert!(buffer_line(&buffer, 17).contains("╚═════╝"));
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
                    INPUT_BG,
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

        // Assert: server (with expand marker) and both channels in the sidebar.
        assert!(buffer_line(&buffer, 0).contains("osu_irc"));
        assert!(buffer_line(&buffer, 0).contains("▾"));
        assert!(buffer_line(&buffer, 1).contains("#osu"));
        assert!(buffer_line(&buffer, 2).contains("#chinese"));
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
        assert_eq!(active_bg, SIDEBAR_ACTIVE_BG);
        assert_ne!(inactive_bg, SIDEBAR_ACTIVE_BG);
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
        assert_eq!(cursor_cell.bg, Color::White);
        let inner_cell = buffer.cell((10, 1)).unwrap();
        assert_eq!(inner_cell.bg, SIDEBAR_ACTIVE_BG);
        // Row 0 (server row, not under the cursor) keeps the plain bg.
        assert_ne!(buffer.cell((10, 0)).unwrap().bg, SIDEBAR_ACTIVE_BG);
    }

    #[test]
    fn sidebar_collapsed_server_shows_fold_marker() {
        // Arrange: collapse the server, then check the marker and hidden rows.
        let mut app = test_app(22, 3);
        app.tab(); // Composer -> Sidebar (cursor onto #osu, row 1)
        app.sidebar_up(); // walk to the server header row
        app.sidebar_enter(); // collapse osu_irc

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: ▸ marker on the server row; channel rows hidden.
        assert!(buffer_line(&buffer, 0).contains("▸"));
        assert!(!buffer_line(&buffer, 1).contains("#osu"));
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
        assert_eq!(accent.fg, INPUT_LINE_DIM);
        assert_ne!(buffer.cell((MAIN_COL_X, 5)).unwrap().bg, Color::White);
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
        assert_eq!(buffer.cell((MAIN_COL_X, 2)).unwrap().bg, MESSAGE_SELECT_BG);
        // ...the separator above renders ▄ and the one below ▀ in that color...
        let above = buffer.cell((MAIN_COL_X, 1)).unwrap();
        assert_eq!(above.symbol(), "▄");
        assert_eq!(above.fg, MESSAGE_SELECT_BG);
        let below = buffer.cell((MAIN_COL_X, 3)).unwrap();
        assert_eq!(below.symbol(), "▀");
        assert_eq!(below.fg, MESSAGE_SELECT_BG);
        // ...and a pale-green accent runs down the front of the block,
        // tapering at both ends (╻ … ┃ … ╹ — details in the taper test).
        for y in 1..=3 {
            let accent = buffer.cell((INPUT_X, y)).unwrap();
            assert_eq!(accent.fg, INPUT_LINE, "accent missing on row {y}");
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
        assert_eq!(buffer.cell((INPUT_X + 1, 2)).unwrap().bg, MESSAGE_SELECT_BG);
        // ...and the half-block separators reach into it as well.
        assert_eq!(buffer.cell((INPUT_X + 1, 1)).unwrap().symbol(), "▄");
        assert_eq!(buffer.cell((INPUT_X + 1, 1)).unwrap().fg, MESSAGE_SELECT_BG);
        assert_eq!(buffer.cell((INPUT_X + 1, 3)).unwrap().symbol(), "▀");
        assert_eq!(buffer.cell((INPUT_X + 1, 3)).unwrap().fg, MESSAGE_SELECT_BG);
        // The accent itself keeps standing on the global background.
        assert_eq!(buffer.cell((INPUT_X, 2)).unwrap().bg, GLOBAL_BG);
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
        assert_eq!(top.fg, INPUT_LINE);
        let middle = buffer.cell((INPUT_X, 2)).unwrap();
        assert_eq!(middle.symbol(), "┃");
        assert_eq!(middle.fg, INPUT_LINE);
        let bottom = buffer.cell((INPUT_X, 3)).unwrap();
        assert_eq!(bottom.symbol(), "╹");
        assert_eq!(bottom.fg, INPUT_LINE);
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
        assert_eq!(buffer.cell((MAIN_COL_X, 1)).unwrap().bg, MESSAGE_SELECT_BG);
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
            assert_ne!(buffer.cell((MAIN_COL_X, y)).unwrap().bg, MESSAGE_SELECT_BG);
            assert!(!["┃", "╻", "╹"].contains(&buffer.cell((INPUT_X, y)).unwrap().symbol()));
        }
    }

    #[test]
    fn input_area_has_background_and_accent_column() {
        // Arrange: 50x10 -> input occupies rows 4..=7 (above the 2-row gap).
        let app = test_app(22, 3);

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: each input row has the `┃` accent at the composer's left edge
        // (col INPUT_X) in pale green, standing on the global bg (no panel bg
        // behind it); the panel background starts one column to the right.
        for y in 4..=7 {
            let cell = buffer.cell((INPUT_X, y)).unwrap();
            assert_eq!(cell.symbol(), "┃", "accent missing on row {y}");
            assert_eq!(cell.fg, INPUT_LINE, "accent color wrong on row {y}");
            assert_eq!(
                cell.bg, GLOBAL_BG,
                "accent should sit on global bg on row {y}"
            );
        }
        assert_eq!(buffer.cell((INPUT_X + 1, 5)).unwrap().bg, INPUT_BG); // panel starts here
    }

    #[test]
    fn input_area_shows_tips_on_last_row() {
        // Arrange: a wide terminal so the full status + tips line fits.
        let app = viewing_app(72, 3);

        // Act
        let buffer = render_sized(&app, "connected to irc.example.org", 100, 10);

        // Assert: the input's last row (row 7) carries status plus key hints.
        let tips = buffer_line(&buffer, 7);
        assert!(
            tips.contains("connected to irc.example.org"),
            "tips: {tips:?}"
        );
        assert!(tips.contains("Esc quit"), "tips: {tips:?}");
    }

    #[test]
    fn input_row_shows_typed_text_and_cursor() {
        // Arrange
        let mut app = test_app(22, 3);
        app.type_char('h');
        app.type_char('i');

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: the input line (row 5) shows the text and a reverse-video cursor.
        let row = buffer_line(&buffer, 5);
        assert!(row.contains("hi"), "row was: {row:?}");
        // The cursor sits just past "hi" (at end) - a White-bg block at col 28.
        assert_eq!(buffer.cell((MAIN_COL_X + 2, 5)).unwrap().bg, Color::White);
    }

    #[test]
    fn separator_uninterrupted_full_height() {
        // Arrange
        let app = viewing_app(22, 3);

        // Act
        let buffer = render_sized(&app, "", 50, 10);

        // Assert: the gray `│` runs the full height (every row) at col SIDEBAR_WIDTH,
        // no longer overridden by the composer (the `┃` now sits at col INPUT_X).
        for y in 0..buffer.area.height {
            assert_eq!(
                buffer.cell((SIDEBAR_WIDTH, y)).unwrap().symbol(),
                "│",
                "separator missing on row {y}"
            );
            assert_eq!(buffer.cell((SIDEBAR_WIDTH, y)).unwrap().fg, SEPARATOR);
        }
        // The `┃` lives only on the composer rows, at col INPUT_X.
        for y in 4..=7 {
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

        // Assert: on the fade row (row 8), the composer's `┃` tapers via `╹`
        // (pale green, on the global bg); the rest of the row is the `▀` fade.
        let taper = buffer.cell((INPUT_X, 8)).unwrap();
        assert_eq!(taper.symbol(), "╹");
        assert_eq!(taper.fg, INPUT_LINE);
        assert_eq!(taper.bg, GLOBAL_BG);
        let fade = buffer.cell((INPUT_X + 1, 8)).unwrap();
        assert_eq!(fade.symbol(), "▀");
        assert_eq!(fade.fg, INPUT_BG);
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
        assert_eq!(buffer.cell((SIDEBAR_WIDTH, 1)).unwrap().fg, SEPARATOR);
        assert_eq!(buffer.cell((SIDEBAR_WIDTH + 1, 1)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((SIDEBAR_WIDTH + 1, 1)).unwrap().bg, GLOBAL_BG);
        // The global background fills every non-composer region.
        assert_eq!(buffer.cell((0, 5)).unwrap().bg, GLOBAL_BG); // sidebar blank
        assert_eq!(buffer.cell((INPUT_X + 5, 9)).unwrap().bg, GLOBAL_BG); // gap blank
        assert_eq!(buffer.cell((INPUT_X + 4, 5)).unwrap().bg, INPUT_BG); // composer
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
        assert_eq!(buffer.cell((46, row)).unwrap().bg, INPUT_BG); // panel rightmost
        for x in 47..=49 {
            assert_eq!(buffer.cell((x, row)).unwrap().bg, GLOBAL_BG, "col {x}");
        }
        // Assert: no text reaches the last 2 panel columns (the text margin).
        assert_eq!(buffer.cell((45, row)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((46, row)).unwrap().symbol(), " ");
    }

    #[test]
    fn message_viewport_extends_down_to_the_composer() {
        // Arrange: H=11 with empty input -> the message viewport owns rows
        // 0..=4 (the old static spacer row is now the trailing separator in
        // the stream), composer rows 5..=8. An empty channel lays out to no
        // rows, so the pane is blank.
        let mut app = App::new(22, 3);
        app.open_channel("osu_irc", "#osu");
        app.select_channel(0);

        // Act
        let buffer = render_sized(&app, "", 50, 11);

        // Assert: the pane's bottom row is blank with the global background
        // across the main column, and the composer ┃ starts on the row below.
        for x in INPUT_X..50 {
            assert_eq!(buffer.cell((x, 4)).unwrap().symbol(), " ", "col {x}");
            assert_eq!(buffer.cell((x, 4)).unwrap().bg, GLOBAL_BG, "col {x}");
        }
        assert_eq!(buffer.cell((INPUT_X, 5)).unwrap().symbol(), "┃");
    }

    #[test]
    fn multiline_input_grows_the_composer() {
        // Arrange: 25 chars at text width 19 wrap into 2 lines, so the composer
        // is 5 rows (blank + 2 text + blank + tips). For H=11 that puts the
        // composer on rows 4..=8.
        let mut app = viewing_app(22, 2);
        for _ in 0..25 {
            app.type_char('x');
        }

        // Act
        let buffer = render_sized(&app, "", 50, 11);

        // Assert: the ┃ spans all 5 composer rows, both wrapped text lines are
        // shown, and the tips row moved down to row 8.
        for y in 4..=8 {
            assert_eq!(buffer.cell((INPUT_X, y)).unwrap().symbol(), "┃", "row {y}");
        }
        assert!(buffer_line(&buffer, 5).contains('x'));
        assert!(buffer_line(&buffer, 6).contains('x'));
        assert!(buffer_line(&buffer, 8).contains("Esc quit"));
    }

    #[test]
    fn multiline_input_squeezes_the_message_pane() {
        // Arrange / Act: empty input -> composer 4 rows (┃ top at row 5 for H=11);
        // 2-line input -> composer 5 rows (┃ top moves up to row 4).

        let app_empty = viewing_app(22, 3);
        let buf_empty = render_sized(&app_empty, "", 50, 11);
        assert_eq!(buf_empty.cell((INPUT_X, 5)).unwrap().symbol(), "┃");
        assert_eq!(buf_empty.cell((INPUT_X, 4)).unwrap().symbol(), " ");

        let mut app_full = viewing_app(22, 2);
        for _ in 0..25 {
            app_full.type_char('x');
        }
        let buf_full = render_sized(&app_full, "", 50, 11);

        // Assert: the composer's top edge moved up one row.
        assert_eq!(buf_full.cell((INPUT_X, 4)).unwrap().symbol(), "┃");
    }
}
