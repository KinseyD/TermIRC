//! Rendering: turns app state into ratatui widgets.
//!
//! Screen layout (no box frames):
//! ```text
//!  sidebar (fixed)  │  main column
//!  osu_irc           │  #osu                       <- title row
//!    ▶ #osu         │  alice: hello ...            <- messages (scroll)
//!      #chinese     │
//!                    │  ┃                          <- input (4 rows, bg-shaded,
//!                    │  ┃   (empty input)               accent ┃ on the left)
//!                    │  ┃  connected · q quit …    <- tips row
//! ```
//! Lines are pre-wrapped by `layout`, so the messages `Paragraph` is rendered
//! without `Wrap`: continuation rows already carry their own indentation,
//! keeping the message body clear of the nick column.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
};

use crate::app::App;
use crate::config::Config;

const NICK_STYLE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);

/// Fixed width of the left server/channel sidebar, in columns.
pub const SIDEBAR_WIDTH: u16 = 22;
/// Columns between the sidebar and the main content: the `│` separator plus a
/// 1-column blank gap.
pub const SEPARATOR_GAP: u16 = 2;
/// Blank padding (each side) between a region's edge and its content.
pub const HORIZONTAL_PAD: u16 = 2;
/// Rows taken by the channel title at the top of the main column.
pub const TITLE_ROWS: u16 = 1;
/// Rows taken by the composer at the bottom of the main column.
pub const INPUT_ROWS: u16 = 4;
/// Rows below the composer: a half-block "fade" of its background on the row
/// immediately under it, then a blank row before the window bottom.
pub const GAP_ROWS: u16 = 2;

/// Global background color for the whole screen.
const GLOBAL_BG: Color = Color::Rgb(0x0a, 0x0a, 0x0a);
/// Background color distinguishing the composer region.
const INPUT_BG: Color = Color::Rgb(0x1e, 0x1e, 0x1e);
/// Color of the thin vertical line separating the sidebar from the main column.
const SEPARATOR: Color = Color::Rgb(0x55, 0x55, 0x55);
/// Accent color for the active channel in the sidebar.
const ACCENT: Color = Color::Magenta;
const ACCENT_STYLE: Style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
const DIM_STYLE: Style = Style::new().fg(Color::Rgb(0x70, 0x70, 0x70));

/// Render-only chrome state: what to show outside the message scrollback.
pub struct Chrome<'a> {
    pub config: &'a Config,
    pub active_server: &'a str,
    pub active_channel: &'a str,
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

/// Render the whole screen: sidebar on the left, main column (channel title /
/// messages / composer) on the right.
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
    render_sidebar(frame, columns[0], chrome);
    render_separator(frame, columns[1]);

    let rows = Layout::vertical([
        Constraint::Length(TITLE_ROWS), // channel title
        Constraint::Min(0),             // messages
        Constraint::Length(INPUT_ROWS), // composer
        Constraint::Length(GAP_ROWS),   // fade + blank below the composer
    ])
    .split(columns[2]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            chrome.active_channel,
            Style::new().add_modifier(Modifier::BOLD),
        ))),
        inset(rows[0], HORIZONTAL_PAD),
    );
    frame.render_widget(
        Paragraph::new(build_text(app)).scroll((app.scroll_offset(), 0)),
        inset(rows[1], HORIZONTAL_PAD),
    );
    render_input(frame, rows[2], app, chrome.status);
    render_gap(frame, rows[3]);
}

/// Render the thin gray vertical line that separates the sidebar from the main
/// column, spanning the full height (left edge of the separator-gap segment).
fn render_separator(frame: &mut Frame, area: Rect) {
    let col = Rect::new(area.x, area.y, 1, area.height);
    let line = Line::from(Span::styled("│", Style::new().fg(SEPARATOR)));
    let lines = vec![line; usize::from(area.height)];
    frame.render_widget(Paragraph::new(Text::from(lines)), col);
}

/// Render the server/channel sidebar from the config, highlighting the active
/// channel.
fn render_sidebar(frame: &mut Frame, area: Rect, chrome: &Chrome<'_>) {
    let mut lines: Vec<Line<'_>> = Vec::new();
    for (name, server) in chrome.config.servers.iter() {
        lines.push(Line::from(Span::styled(
            name.as_str(),
            Style::new().add_modifier(Modifier::BOLD),
        )));
        for channel in &server.channels {
            let active =
                name.as_str() == chrome.active_server && channel.as_str() == chrome.active_channel;
            let (marker, style) = if active {
                ("▶ ", ACCENT_STYLE)
            } else {
                ("  ", DIM_STYLE)
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::raw(marker),
                Span::styled(channel.as_str(), style),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), inset(area, 1));
}

/// Render the composer: a background-shaded 4-row region with a decorative
/// accent bar on the left (top blank / input line / blank / tips). The input
/// line shows the user's text with a reverse-video cursor.
fn render_input(frame: &mut Frame, area: Rect, app: &App, status: &str) {
    // The left accent bar uses the global background color (a dark seam on
    // the lighter composer background).
    let accent = Span::styled("┃", Style::new().fg(GLOBAL_BG));
    let tips = if status.is_empty() {
        "Esc quit · PgUp/PgDn scroll".to_string()
    } else {
        format!("{status}  ·  Esc quit · PgUp/PgDn scroll")
    };
    let mut input_spans: Vec<Span<'static>> = vec![accent.clone(), Span::raw(" ")];
    input_spans.extend(build_input_spans(app.input(), app.input_cursor()));
    let lines = vec![
        Line::from(vec![accent.clone()]),
        Line::from(input_spans),
        Line::from(vec![accent.clone()]),
        Line::from(vec![accent, Span::raw(" "), Span::raw(tips)]),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::new().bg(INPUT_BG)),
        area,
    );
}

/// Build the composer's input line spans: the typed text with a reverse-video
/// cursor at the cursor position (a solid block when the cursor is past the end).
fn build_input_spans(text: &str, cursor: usize) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let cur = cursor.min(chars.len());
    let before: String = chars[..cur].iter().collect();
    let (cursor_ch, after): (String, String) = if cur < chars.len() {
        (chars[cur].to_string(), chars[cur + 1..].iter().collect())
    } else {
        (" ".to_string(), String::new())
    };
    let cursor_style = Style::new().fg(INPUT_BG).bg(Color::White);
    let mut spans: Vec<Span<'static>> = Vec::new();
    if !before.is_empty() {
        spans.push(Span::raw(before));
    }
    spans.push(Span::styled(cursor_ch, cursor_style));
    if !after.is_empty() {
        spans.push(Span::raw(after));
    }
    spans
}

/// Render the gap below the composer: the left accent tapers into an upper-half
/// heavy vertical (`╹`) on the row immediately below it, the rest of that row
/// is a half-block fade of the composer background, then a blank row.
fn render_gap(frame: &mut Frame, area: Rect) {
    let top = Rect::new(area.x, area.y, area.width, 1);
    let taper = Span::styled("╹", Style::new().fg(GLOBAL_BG).bg(INPUT_BG));
    let fade = Span::styled(
        "▀".repeat(usize::from(top.width.saturating_sub(1))),
        Style::new().fg(INPUT_BG),
    );
    frame.render_widget(Paragraph::new(Line::from(vec![taper, fade])), top);
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
            nick: nick.to_string(),
            text: text.to_string(),
        }
    }

    /// A config with one server and two channels; "#osu" is the active channel.
    fn test_config() -> Config {
        Config::parse(
            r##"
[servers.osu_irc]
username = "alice"
nickname = "alice"
password = "secret"
server = "irc.example.org"
port = 6667
channels = ["#osu", "#chinese"]
"##,
        )
        .unwrap()
    }

    /// Render into a headless terminal. The message viewport is sized to match
    /// what `main.rs` would compute: main column width minus 2*HORIZONTAL_PAD,
    /// height minus TITLE_ROWS and INPUT_ROWS.
    fn render_sized(app: &App, config: &Config, status: &str, w: u16, h: u16) -> Buffer {
        let chrome = Chrome {
            config,
            active_server: "osu_irc",
            active_channel: "#osu",
            status,
        };
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
    fn channel_name_shown_on_top_row_and_no_border() {
        // Arrange: 50x10 terminal -> sidebar 22, main 28; viewport 24x5.
        let mut app = App::new(22, 3);
        app.push_message(msg("alice", "hi"));
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

        // Assert: channel name on the top row at the main column's content start.
        assert_eq!(buffer.cell((MAIN_COL_X, 0)).unwrap().symbol(), "#");
        assert!(buffer_line(&buffer, 0).contains("#osu"));
        assert!(!buffer_line(&buffer, 0).contains("termirc"));

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
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

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
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 46, 10);

        // Assert: row 2 is blank under the nick, body resumes at the next column.
        let nick_end = MAIN_COL_X + 7; // "alice: " is 7 columns
        for x in MAIN_COL_X..nick_end {
            assert_eq!(
                buffer.cell((x, 2)).unwrap().symbol(),
                " ",
                "column {x} not blank"
            );
        }
        assert_eq!(buffer.cell((nick_end, 2)).unwrap().symbol(), "t");
        assert!(buffer_line(&buffer, 2).contains("three four"));
    }

    #[test]
    fn blank_separator_between_messages_but_not_after_last() {
        // Arrange
        let mut app = App::new(22, 3);
        app.push_message(msg("a", "first"));
        app.push_message(msg("b", "second"));
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

        // Assert: in the main content area, the row between the messages is
        // blank (the sidebar separator `│` sits in the pad column to its left);
        // the row after it carries the second message.
        let main_slice: String = (SIDEBAR_WIDTH + HORIZONTAL_PAD..buffer.area.width)
            .map(|x| buffer.cell((x, 2)).unwrap().symbol())
            .collect();
        assert!(
            main_slice.chars().all(|c| c == ' '),
            "expected blank main-column row, got: {main_slice:?}"
        );
        assert!(buffer_line(&buffer, 3).contains("b: second"));
    }

    #[test]
    fn scroll_offset_shifts_visible_content() {
        // Arrange: 5 one-line messages -> 9 content rows; viewport shows 3.
        let mut app = App::new(22, 3);
        for i in 0..5 {
            app.push_message(msg("u", &format!("m{i}")));
        }
        app.set_scroll_offset(2);
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

        // Assert: offset 2 puts content row 2 ("m1") on the first visible row.
        let row = buffer_line(&buffer, 1);
        assert!(row.contains("u: m1"), "row was: {row:?}");
    }

    #[test]
    fn sidebar_lists_configured_servers_and_channels() {
        // Arrange
        let app = App::new(22, 3);
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

        // Assert: server and both channels appear in the sidebar.
        assert!(buffer_line(&buffer, 0).contains("osu_irc"));
        assert!(buffer_line(&buffer, 1).contains("#osu"));
        assert!(buffer_line(&buffer, 2).contains("#chinese"));
    }

    #[test]
    fn sidebar_highlights_active_channel() {
        // Arrange
        let app = App::new(22, 3);
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

        // Assert: the active "#osu" row has the ▶ marker and accent color; the
        // inactive "#chinese" row has neither.
        assert!(buffer_line(&buffer, 1).contains("▶"));
        assert_eq!(buffer.cell((5, 1)).unwrap().fg, ACCENT); // '#' of "#osu"
        assert!(!buffer_line(&buffer, 2).contains("▶"));
        assert_ne!(buffer.cell((5, 2)).unwrap().fg, ACCENT); // '#' of "#chinese"
    }

    #[test]
    fn input_area_has_background_and_accent_column() {
        // Arrange: 50x10 -> input occupies rows 4..=7 (above the 2-row gap).
        let app = App::new(22, 3);
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

        // Assert: each input row has the `┃` accent at the composer's left edge
        // (col INPUT_X) in the global bg color, and the region's background is INPUT_BG.
        for y in 4..=7 {
            let cell = buffer.cell((INPUT_X, y)).unwrap();
            assert_eq!(cell.symbol(), "┃", "accent missing on row {y}");
            assert_eq!(cell.fg, GLOBAL_BG, "accent color wrong on row {y}");
        }
        assert_eq!(buffer.cell((INPUT_X + 4, 5)).unwrap().bg, INPUT_BG);
    }

    #[test]
    fn input_area_shows_tips_on_last_row() {
        // Arrange: a wide terminal so the full status + tips line fits.
        let app = App::new(72, 3);
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "connected to irc.example.org", 100, 10);

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
        let mut app = App::new(22, 3);
        app.type_char('h');
        app.type_char('i');
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

        // Assert: the input line (row 5) shows the text and a reverse-video cursor.
        let row = buffer_line(&buffer, 5);
        assert!(row.contains("hi"), "row was: {row:?}");
        // The cursor sits just past "hi" (at end) - a White-bg block at col 28.
        assert_eq!(buffer.cell((MAIN_COL_X + 2, 5)).unwrap().bg, Color::White);
    }

    #[test]
    fn separator_uninterrupted_full_height() {
        // Arrange
        let app = App::new(22, 3);
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

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
        let app = App::new(22, 3);
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

        // Assert: on the fade row (row 8), the composer's `┃` tapers via `╹`
        // (fg GLOBAL_BG on INPUT_BG); the rest of the row is the `▀` fade.
        let taper = buffer.cell((INPUT_X, 8)).unwrap();
        assert_eq!(taper.symbol(), "╹");
        assert_eq!(taper.fg, GLOBAL_BG);
        assert_eq!(taper.bg, INPUT_BG);
        let fade = buffer.cell((INPUT_X + 1, 8)).unwrap();
        assert_eq!(fade.symbol(), "▀");
        assert_eq!(fade.fg, INPUT_BG);
    }

    #[test]
    fn separator_and_background_colors() {
        // Arrange
        let app = App::new(22, 3);
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

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
}
