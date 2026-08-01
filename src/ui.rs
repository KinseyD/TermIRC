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
/// Blank padding (each side) between a region's edge and its content.
pub const HORIZONTAL_PAD: u16 = 2;
/// Rows taken by the channel title at the top of the main column.
pub const TITLE_ROWS: u16 = 1;
/// Rows taken by the composer at the bottom of the main column.
pub const INPUT_ROWS: u16 = 4;

/// Accent color for the composer's decorative bar and the active channel.
const ACCENT: Color = Color::Magenta;
/// Background color distinguishing the composer region.
const INPUT_BG: Color = Color::DarkGray;
const ACCENT_STYLE: Style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
const DIM_STYLE: Style = Style::new().fg(Color::DarkGray);

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
    let columns = Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)])
        .split(frame.area());
    render_sidebar(frame, columns[0], chrome);

    let rows = Layout::vertical([
        Constraint::Length(TITLE_ROWS), // channel title
        Constraint::Min(0),             // messages
        Constraint::Length(INPUT_ROWS), // composer
    ])
    .split(columns[1]);

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
    render_input(frame, rows[2], chrome.status);
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
/// accent bar on the left (top blank / input / blank / tips).
fn render_input(frame: &mut Frame, area: Rect, status: &str) {
    let accent = Span::styled("┃", Style::new().fg(ACCENT));
    let tips = if status.is_empty() {
        "q quit · PgUp/PgDn scroll".to_string()
    } else {
        format!("{status}  ·  q quit · PgUp/PgDn scroll")
    };
    let lines = vec![
        Line::from(vec![accent.clone()]),
        Line::from(vec![accent.clone(), Span::raw(" ")]),
        Line::from(vec![accent.clone()]),
        Line::from(vec![accent, Span::raw(" "), Span::raw(tips)]),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::new().bg(INPUT_BG)),
        area,
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

    /// The first content column of the main area (after sidebar + 2-col pad).
    const MAIN_COL_X: u16 = SIDEBAR_WIDTH + HORIZONTAL_PAD;

    fn buffer_line(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).unwrap().symbol())
            .collect()
    }

    #[test]
    fn channel_name_shown_on_top_row_and_no_border() {
        // Arrange: 50x10 terminal -> sidebar 22, main 28; viewport 24x5.
        let mut app = App::new(24, 5);
        app.push_message(msg("alice", "hi"));
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

        // Assert: channel name on the top row at the main column's content start.
        assert_eq!(buffer.cell((MAIN_COL_X, 0)).unwrap().symbol(), "#");
        assert!(buffer_line(&buffer, 0).contains("#osu"));
        assert!(!buffer_line(&buffer, 0).contains("termirc"));

        // Assert: no light box-drawing characters anywhere (the heavy `┃`
        // accent is allowed - it is not in this set).
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let symbol = buffer.cell((x, y)).unwrap().symbol();
                assert!(
                    !symbol.chars().any(|c| "│─┌┐└┘├┤┬┴┼".contains(c)),
                    "border char {symbol:?} at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn render_shows_nick_then_body_on_first_message_row() {
        // Arrange
        let mut app = App::new(24, 5);
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
        // Arrange: terminal 44x8 -> main 22, viewport 18x3; nick "alice" takes
        // 7 columns, body width 11; "one two three four" -> "one two" / "three four".
        let mut app = App::new(18, 3);
        app.push_message(msg("alice", "one two three four"));
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 44, 8);

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
        let mut app = App::new(24, 5);
        app.push_message(msg("a", "first"));
        app.push_message(msg("b", "second"));
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

        // Assert: in the main column, the row between the messages is blank
        // (the sidebar occupies the left columns on the same row); the row
        // after it carries the second message.
        let main_slice: String = (SIDEBAR_WIDTH..buffer.area.width)
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
        // Arrange: 5 one-line messages -> 9 content rows; viewport shows 5.
        let mut app = App::new(24, 5);
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
        let app = App::new(24, 5);
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
        let app = App::new(24, 5);
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
        // Arrange: 50x10 -> input occupies the bottom 4 rows (6..9).
        let app = App::new(24, 5);
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "", 50, 10);

        // Assert: each input row has the `┃` accent at the main column's left
        // edge (col SIDEBAR_WIDTH) in the accent color, and the region's
        // background is INPUT_BG.
        for y in 6..=9 {
            let cell = buffer.cell((SIDEBAR_WIDTH, y)).unwrap();
            assert_eq!(cell.symbol(), "┃", "accent missing on row {y}");
            assert_eq!(cell.fg, ACCENT, "accent color wrong on row {y}");
        }
        assert_eq!(buffer.cell((SIDEBAR_WIDTH + 2, 7)).unwrap().bg, INPUT_BG);
    }

    #[test]
    fn input_area_shows_tips_on_last_row() {
        // Arrange: a wide terminal so the full status + tips line fits.
        let app = App::new(74, 5);
        let config = test_config();

        // Act
        let buffer = render_sized(&app, &config, "connected to irc.example.org", 100, 10);

        // Assert: the last row carries the status plus key hints.
        let tips = buffer_line(&buffer, 9);
        assert!(
            tips.contains("connected to irc.example.org"),
            "tips: {tips:?}"
        );
        assert!(tips.contains("q quit"), "tips: {tips:?}");
    }
}
