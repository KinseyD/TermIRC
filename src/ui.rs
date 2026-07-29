//! Rendering: turns app state into ratatui widgets.
//!
//! Lines are pre-wrapped by `layout`, so the `Paragraph` is rendered without
//! `Wrap`: continuation rows already carry their own indentation, keeping the
//! message body clear of the nick column.

use ratatui::{
    Frame,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph},
};

use crate::app::App;

const NICK_STYLE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);

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

/// Render the whole screen: a bordered chat pane scrolled to the app offset.
pub fn draw(frame: &mut Frame, app: &App, status: &str) {
    let paragraph = Paragraph::new(build_text(app))
        .block(Block::bordered().title("termirc").title_bottom(status))
        .scroll((app.scroll_offset(), 0));
    frame.render_widget(paragraph, frame.area());
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

    /// Render into a headless terminal; the app viewport is the area inside
    /// the 1-cell border, so the terminal is 2 wider and 2 taller.
    fn render_sized(app: &App, term_width: u16, term_height: u16) -> Buffer {
        let backend = TestBackend::new(term_width, term_height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app, "")).unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_line(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).unwrap().symbol())
            .collect()
    }

    #[test]
    fn render_shows_nick_then_body_on_first_row() {
        // Arrange: app viewport 28x6 inside a 30x8 terminal.
        let mut app = App::new(28, 6);
        app.push_message(msg("alice", "hello"));

        // Act
        let buffer = render_sized(&app, 30, 8);

        // Assert: first content row starts in column 1, right of the border.
        assert_eq!(buffer.cell((1, 1)).unwrap().symbol(), "a");
        assert!(buffer_line(&buffer, 1).contains("alice: hello"));
    }

    #[test]
    fn continuation_rows_are_blank_under_username() {
        // Arrange: width 18 -> nick "alice" takes 7 columns, body width 11;
        // "one two three four" wraps into "one two" / "three four".
        let mut app = App::new(18, 4);
        app.push_message(msg("alice", "one two three four"));

        // Act
        let buffer = render_sized(&app, 20, 6);

        // Assert: row 2 is blank under the nick, body resumes at column 8.
        for x in 1..=7 {
            assert_eq!(
                buffer.cell((x, 2)).unwrap().symbol(),
                " ",
                "column {x} not blank"
            );
        }
        assert_eq!(buffer.cell((8, 2)).unwrap().symbol(), "t");
        assert!(buffer_line(&buffer, 2).contains("three four"));
    }

    #[test]
    fn blank_separator_between_messages_but_not_after_last() {
        // Arrange
        let mut app = App::new(28, 6);
        app.push_message(msg("a", "first"));
        app.push_message(msg("b", "second"));

        // Act
        let buffer = render_sized(&app, 30, 8);

        // Assert: row between the messages is blank (borders aside); the row
        // after it (the last message's own row) carries its text.
        let expected_blank = format!("│{}│", " ".repeat(28));
        assert_eq!(buffer_line(&buffer, 2), expected_blank);
        assert!(buffer_line(&buffer, 3).contains("b: second"));
    }

    #[test]
    fn scroll_offset_shifts_visible_content() {
        // Arrange: 5 one-line messages -> 9 content rows; viewport shows 4.
        // Content rows: m0, sep, m1, sep, m2, sep, m3, sep, m4.
        let mut app = App::new(28, 4);
        for i in 0..5 {
            app.push_message(msg("u", &format!("m{i}")));
        }
        app.set_scroll_offset(2);

        // Act
        let buffer = render_sized(&app, 30, 6);

        // Assert: offset 2 puts content row 2 ("m1") on the first visible row.
        let row = buffer_line(&buffer, 1);
        assert!(row.contains("u: m1"), "row was: {row:?}");
    }
}
