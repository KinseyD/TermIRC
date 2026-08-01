//! Rendering: turns app state into ratatui widgets.
//!
//! Lines are pre-wrapped by `layout`, so the `Paragraph` is rendered without
//! `Wrap`: continuation rows already carry their own indentation, keeping the
//! message body clear of the nick column.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
};

use crate::app::App;

const NICK_STYLE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);

/// Blank padding (each side) between the screen edge and the content.
const HORIZONTAL_PAD: u16 = 2;

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

/// Render the whole screen: a channel title on top, the scrollable chat in the
/// middle, and a status line at the bottom — no box border, with 2 columns of
/// blank padding on each side.
pub fn draw(frame: &mut Frame, app: &App, channel: &str, status: &str) {
    let chunks = Layout::vertical([
        Constraint::Length(1), // channel title
        Constraint::Min(0),    // messages
        Constraint::Length(1), // status
    ])
    .split(frame.area());

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            channel,
            Style::new().add_modifier(Modifier::BOLD),
        ))),
        inset(chunks[0]),
    );
    frame.render_widget(
        Paragraph::new(build_text(app)).scroll((app.scroll_offset(), 0)),
        inset(chunks[1]),
    );
    frame.render_widget(Paragraph::new(status), inset(chunks[2]));
}

/// Shrink a region by `HORIZONTAL_PAD` columns on each side (vertical unchanged)
/// so content never touches the left/right screen edges.
fn inset(area: Rect) -> Rect {
    Rect::new(
        area.x + HORIZONTAL_PAD,
        area.y,
        area.width.saturating_sub(2 * HORIZONTAL_PAD),
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

    /// Render into a headless terminal. The layout reserves 1 row at the top
    /// (channel title), 1 row at the bottom (status), and insets 2 columns on
    /// each side — so the message viewport is (term_width-4, term_height-2).
    fn render_sized(app: &App, channel: &str, term_width: u16, term_height: u16) -> Buffer {
        let backend = TestBackend::new(term_width, term_height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw(frame, app, channel, ""))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_line(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).unwrap().symbol())
            .collect()
    }

    #[test]
    fn channel_name_shown_on_top_row_and_no_border() {
        // Arrange: app viewport 26x6 inside a 30x8 terminal.
        let mut app = App::new(26, 6);
        app.push_message(msg("alice", "hi"));

        // Act
        let buffer = render_sized(&app, "#osu", 30, 8);

        // Assert: channel name on the top row, starting at column 2; no "termirc".
        assert_eq!(buffer.cell((2, 0)).unwrap().symbol(), "#");
        assert!(buffer_line(&buffer, 0).contains("#osu"));
        assert!(!buffer_line(&buffer, 0).contains("termirc"));

        // Assert: no box-drawing characters anywhere on the screen.
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
    fn render_shows_nick_then_body_on_first_row() {
        // Arrange: app viewport 26x6 inside a 30x8 terminal.
        let mut app = App::new(26, 6);
        app.push_message(msg("alice", "hello"));

        // Act
        let buffer = render_sized(&app, "#osu", 30, 8);

        // Assert: first message row (row 1) starts at column 2 (after the pad).
        assert_eq!(buffer.cell((2, 1)).unwrap().symbol(), "a");
        assert!(buffer_line(&buffer, 1).contains("alice: hello"));
    }

    #[test]
    fn continuation_rows_are_blank_under_username() {
        // Arrange: terminal 20x6 -> message viewport 16x4; nick "alice" takes
        // 7 columns (terminal cols 2..8), body width is 16-7 = 9;
        // "one two three four" wraps into "one two" / "three" / "four".
        let mut app = App::new(16, 4);
        app.push_message(msg("alice", "one two three four"));

        // Act
        let buffer = render_sized(&app, "#osu", 20, 6);

        // Assert: row 2 is blank under the nick, body resumes at column 9.
        for x in 2..=8 {
            assert_eq!(
                buffer.cell((x, 2)).unwrap().symbol(),
                " ",
                "column {x} not blank"
            );
        }
        assert_eq!(buffer.cell((9, 2)).unwrap().symbol(), "t");
        assert!(buffer_line(&buffer, 2).contains("three"));
    }

    #[test]
    fn blank_separator_between_messages_but_not_after_last() {
        // Arrange
        let mut app = App::new(26, 6);
        app.push_message(msg("a", "first"));
        app.push_message(msg("b", "second"));

        // Act
        let buffer = render_sized(&app, "#osu", 30, 8);

        // Assert: row between the messages is entirely blank (no borders now);
        // the row after it (the last message's own row) carries its text.
        assert!(
            buffer_line(&buffer, 2).chars().all(|c| c == ' '),
            "expected blank row, got: {:?}",
            buffer_line(&buffer, 2)
        );
        assert!(buffer_line(&buffer, 3).contains("b: second"));
    }

    #[test]
    fn scroll_offset_shifts_visible_content() {
        // Arrange: 5 one-line messages -> 9 content rows; viewport shows 4.
        // Content rows: m0, sep, m1, sep, m2, sep, m3, sep, m4.
        let mut app = App::new(26, 4);
        for i in 0..5 {
            app.push_message(msg("u", &format!("m{i}")));
        }
        app.set_scroll_offset(2);

        // Act
        let buffer = render_sized(&app, "#osu", 30, 6);

        // Assert: offset 2 puts content row 2 ("m1") on the first visible row.
        let row = buffer_line(&buffer, 1);
        assert!(row.contains("u: m1"), "row was: {row:?}");
    }
}
