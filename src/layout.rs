//! Message layout: word wrapping with unicode width and indented continuations.
//!
//! Each chat message becomes one or more [`LayoutLine`]s:
//! - the first line carries the nick and the first chunk of the body;
//! - continuation lines are indented so the body starts in the same column,
//!   never underneath the nick;
//! - between two messages there is exactly one blank separator line, and the
//!   whole list is framed by one more separator row above the first and below
//!   the last message (these replace a title row and a composer spacer — they
//!   scroll with the content and render as half-blocks when the adjacent
//!   message is selected). An empty message list lays out to no rows.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::message::ChatMessage;

/// One rendered row of the message list.
///
/// A separator line is `indent: 0, nick: None, body: ""`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutLine {
    /// Number of leading spaces before the body on continuation lines.
    pub indent: u16,
    /// `Some(nick)` only on the first line of a message; `None` on
    /// continuation lines and on nick-less messages (status lines), which
    /// have no nick column at all.
    pub nick: Option<String>,
    pub body: String,
}

/// Display columns taken by the nick column: nick width plus ": ".
pub fn nick_column_width(nick: &str) -> u16 {
    u16::try_from(nick.width())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
}

/// Wrap `text` into lines of at most `width` display columns.
///
/// Words are packed greedily and split on whitespace; a word wider than
/// `width` is hard-split character by character, where a two-column character
/// that no longer fits moves to the next line (leaving at most a one-column
/// gap). An empty string yields a single empty line.
pub fn wrap_body(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;

    for word in text.split_whitespace() {
        let word_w = word.width();
        if word_w <= width {
            // The word fits on a line by itself: pack greedily.
            if current.is_empty() {
                current_w = word_w;
                current.push_str(word);
            } else if current_w + 1 + word_w <= width {
                current_w += 1 + word_w;
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(std::mem::take(&mut current));
                current_w = word_w;
                current.push_str(word);
            }
        } else {
            // The word is wider than the whole line: hard-split per character.
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_w = 0;
            }
            for c in word.chars() {
                let c_w = UnicodeWidthChar::width(c).unwrap_or(0);
                if current_w + c_w > width && !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                    current_w = 0;
                }
                current_w += c_w;
                current.push(c);
            }
        }
    }

    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// Row span `(start, height)` of each message in the same coordinate system as
/// `layout_messages` (i.e. including the framing separator row above the first
/// message and the separator rows between messages). The total row count is
/// the last span end plus one (the trailing framing row), matching
/// `layout_messages(...).len()` — an empty list yields no spans.
pub fn message_spans(messages: &[ChatMessage], width: u16) -> Vec<(u16, u16)> {
    let mut spans = Vec::with_capacity(messages.len());
    let mut row = 1u16; // the framing separator row above the first message
    for (i, message) in messages.iter().enumerate() {
        if i > 0 {
            row += 1; // blank separator row before this message
        }
        let indent = if message.nick.is_empty() {
            0
        } else {
            nick_column_width(&message.nick).min(width.saturating_sub(1))
        };
        let body_width = width.saturating_sub(indent).max(1);
        let height = wrap_body(&message.text, body_width).len() as u16;
        spans.push((row, height));
        row += height;
    }
    spans
}

/// Lay out all messages for a viewport `width` columns wide.
///
/// The list is framed by one blank separator row above the first and below the
/// last message; an empty message list lays out to no rows at all.
pub fn layout_messages(messages: &[ChatMessage], width: u16) -> Vec<LayoutLine> {
    let mut lines = Vec::new();
    if messages.is_empty() {
        return lines;
    }
    let separator = || LayoutLine {
        indent: 0,
        nick: None,
        body: String::new(),
    };
    lines.push(separator()); // framing row above the first message
    for (i, message) in messages.iter().enumerate() {
        if i > 0 {
            lines.push(separator());
        }
        let indent = if message.nick.is_empty() {
            0
        } else {
            nick_column_width(&message.nick).min(width.saturating_sub(1))
        };
        let body_width = width.saturating_sub(indent).max(1);
        for (j, chunk) in wrap_body(&message.text, body_width).into_iter().enumerate() {
            if j == 0 {
                lines.push(LayoutLine {
                    indent: 0,
                    nick: (!message.nick.is_empty()).then(|| message.nick.clone()),
                    body: chunk,
                });
            } else {
                lines.push(LayoutLine {
                    indent,
                    nick: None,
                    body: chunk,
                });
            }
        }
    }
    lines.push(separator()); // framing row below the last message
    lines
}

/// Number of display lines the composer input occupies when hard-wrapped to
/// `width` columns (at least one line), including a trailing line when the
/// cursor sits at the end of a full line and needs room to sit on.
pub fn input_line_count(text: &str, cursor: usize, width: u16) -> usize {
    let width = usize::from(width.max(1));
    let total = text.chars().count();
    if total == 0 {
        return 1;
    }
    let mut lines = total.div_ceil(width);
    if cursor >= total && total.is_multiple_of(width) {
        lines += 1;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(nick: &str, text: &str) -> ChatMessage {
        ChatMessage {
            server: "srv".to_string(),
            channel: "#c".to_string(),
            nick: nick.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn short_message_produces_single_line_with_nick_and_zero_indent() {
        // Arrange
        let messages = vec![msg("alice", "hi")];

        // Act
        let lines = layout_messages(&messages, 40);

        // Assert: the message row is framed by a separator above and below.
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[1],
            LayoutLine {
                indent: 0,
                nick: Some("alice".to_string()),
                body: "hi".to_string(),
            }
        );
        for framing in [lines.first().unwrap(), lines.last().unwrap()] {
            assert_eq!(
                framing,
                &LayoutLine {
                    indent: 0,
                    nick: None,
                    body: String::new(),
                }
            );
        }
    }
    #[test]
    fn empty_nick_renders_without_nick_column() {
        // Arrange: a status-style line (empty nick) whose 19-column body
        // fits one full-width row but would wrap under a 2-column indent.
        let messages = vec![msg("", &"a".repeat(19))];

        // Act
        let lines = layout_messages(&messages, 20);
        let spans = message_spans(&messages, 20);

        // Assert: one body row with no nick column and no indent, using the
        // full width (a 2-column nick indent would have hard-split it).
        assert_eq!(lines.len(), 3); // framing blank + body + framing blank
        assert_eq!(
            lines[1],
            LayoutLine {
                indent: 0,
                nick: None,
                body: "a".repeat(19),
            }
        );
        assert_eq!(spans, vec![(1, 1)]);
    }

    #[test]
    fn empty_message_list_lays_out_to_no_rows() {
        // Arrange & Act
        let lines = layout_messages(&[], 40);
        let spans = message_spans(&[], 40);

        // Assert: no framing rows without messages.
        assert!(lines.is_empty());
        assert!(spans.is_empty());
    }

    #[test]
    fn wrapped_continuation_starts_at_body_column() {
        // Arrange: nick "alice" -> indent 7; body width is 20 - 7 = 13.
        let messages = vec![msg("alice", "one two three four five")];

        // Act
        let lines = layout_messages(&messages, 20);

        // Assert: framing blank, then the two message rows.
        assert_eq!(lines.len(), 4);
        assert!(lines[0].body.is_empty() && lines[0].nick.is_none());
        assert_eq!(lines[1].nick, Some("alice".to_string()));
        assert_eq!(lines[1].indent, 0);
        assert_eq!(lines[1].body, "one two three");
        assert_eq!(lines[2].nick, None);
        assert_eq!(lines[2].indent, 7);
        assert_eq!(lines[2].body, "four five");
        assert!(7 + UnicodeWidthStr::width(lines[2].body.as_str()) <= 20);
        assert!(lines[3].body.is_empty() && lines[3].nick.is_none());
    }

    #[test]
    fn separators_frame_the_list_and_separate_messages() {
        // Arrange
        let messages = vec![msg("a", "one"), msg("b", "two"), msg("c", "three")];

        // Act
        let lines = layout_messages(&messages, 40);

        // Assert: two separators between the three messages plus one framing
        // row at each end; first and last rows are the framing blanks.
        let blanks = lines
            .iter()
            .filter(|l| l.nick.is_none() && l.body.is_empty())
            .count();
        assert_eq!(blanks, 4);
        assert!(lines.first().unwrap().body.is_empty());
        assert!(lines.last().unwrap().body.is_empty());
        assert_eq!(lines[1].nick, Some("a".to_string()));
        assert_eq!(lines[3].nick, Some("b".to_string()));
        assert_eq!(lines[5].nick, Some("c".to_string()));
    }

    #[test]
    fn total_height_equals_message_lines_plus_separators() {
        // Arrange: nick "a" -> indent 3, body width 9 at window width 12.
        // Message 1 wraps to 3 lines ("one two" / "three" / "four five"),
        // message 2 wraps to 2 lines ("six seven" / "eight").
        let messages = vec![
            msg("a", "one two three four five"),
            msg("b", "six seven eight"),
        ];

        // Act
        let lines = layout_messages(&messages, 12);

        // Assert: 3 + 2 lines + 1 separator + 2 framing rows = 8.
        assert_eq!(lines.len(), 8);
    }

    #[test]
    fn cjk_fullwidth_chars_count_as_two_columns() {
        // Arrange & Act & Assert: five CJK chars are exactly 10 columns.
        assert_eq!(wrap_body("一二三四五", 10), vec!["一二三四五"]);
        // Six CJK chars (12 cols) wrap after the fifth.
        assert_eq!(wrap_body("一二三四五六", 10), vec!["一二三四五", "六"]);
    }

    #[test]
    fn two_col_char_that_does_not_fit_moves_to_next_line() {
        // Arrange: width 4; after "abc" (3 cols) only 1 column remains,
        // too little for a 2-column char — it must move down, not overflow.

        // Act
        let lines = wrap_body("abc一二", 4);

        // Assert
        assert_eq!(lines, vec!["abc".to_string(), "一二".to_string()]);
    }

    #[test]
    fn long_unbroken_word_is_hard_split() {
        // Act & Assert
        assert_eq!(wrap_body("abcdefgh", 4), vec!["abcd", "efgh"]);
    }

    #[test]
    fn width_too_small_falls_back_to_one_column_body() {
        // Arrange: very long nick, tiny window -> body width clamps to 1.
        let messages = vec![msg("verylongnick", "ab cd")];

        // Act
        let lines = layout_messages(&messages, 2);

        // Assert: every body is at most 1 column and nothing panicked.
        assert!(!lines.is_empty());
        for line in &lines {
            assert!(UnicodeWidthStr::width(line.body.as_str()) <= 1);
        }
    }

    #[test]
    fn empty_text_produces_one_empty_body_line() {
        // Act & Assert
        assert_eq!(wrap_body("", 10), vec!["".to_string()]);
    }

    #[test]
    fn nick_indent_uses_unicode_width() {
        // "中文" is 4 columns, plus ": " makes 6.
        assert_eq!(nick_column_width("中文"), 6);
        assert_eq!(nick_column_width("alice"), 7);
    }

    #[test]
    fn fullwidth_char_at_width_one_produces_no_phantom_blank_line() {
        // Arrange: a 2-column char can never fit in a 1-column line; it must
        // overflow in place rather than emitting an empty row first.

        // Act & Assert
        assert_eq!(wrap_body("一", 1), vec!["一".to_string()]);
        assert_eq!(
            wrap_body("中文", 1),
            vec!["中".to_string(), "文".to_string()]
        );
    }

    #[test]
    fn input_line_count_empty_is_one_line() {
        assert_eq!(input_line_count("", 0, 10), 1);
    }

    #[test]
    fn input_line_count_short_is_one_line() {
        assert_eq!(input_line_count("hi", 2, 10), 1);
    }

    #[test]
    fn input_line_count_wraps_to_multiple_lines() {
        // 5 chars at width 2 -> ceil(5/2) = 3 lines (cursor not at a full-line end).
        assert_eq!(input_line_count("abcde", 5, 2), 3);
        // 4 chars at width 2 -> 2 lines when the cursor is not at the end.
        assert_eq!(input_line_count("abcd", 2, 2), 2);
    }

    #[test]
    fn input_line_count_cursor_at_end_of_full_line_adds_a_line() {
        // 4 chars fill exactly 2 lines (width 2); the cursor at the end (index 4)
        // needs its own line -> 3 lines.
        assert_eq!(input_line_count("abcd", 4, 2), 3);
        // Cursor before the end does not add a line.
        assert_eq!(input_line_count("abcd", 3, 2), 2);
    }

    #[test]
    fn input_line_count_zero_width_falls_back_to_one_column() {
        // Width 0 clamps to 1 column per line (cursor not at end, so no extra line).
        assert_eq!(input_line_count("abc", 0, 0), 3);
    }

    #[test]
    fn message_spans_account_for_separator_rows() {
        // Arrange: three one-line messages -> 3 rows + 2 separators + 2 framing
        // rows = 7 rows.
        let messages = vec![msg("a", "one"), msg("b", "two"), msg("c", "three")];

        // Act
        let spans = message_spans(&messages, 40);

        // Assert: each message is one row; the leading framing row shifts every
        // span down by one.
        assert_eq!(spans, vec![(1, 1), (3, 1), (5, 1)]);
        assert_eq!(layout_messages(&messages, 40).len(), 7);
    }

    #[test]
    fn message_spans_height_matches_wrapped_lines() {
        // Arrange: nick "alice" -> indent 7, body width 11; message wraps to 2 rows.
        let messages = vec![msg("alice", "one two three four five")];

        // Act
        let spans = message_spans(&messages, 20);

        // Assert: one span with height 2 below the framing row; the trailing
        // framing row makes the layout 4 rows tall.
        assert_eq!(spans, vec![(1, 2)]);
        assert_eq!(layout_messages(&messages, 20).len(), 4);
    }

    #[test]
    fn message_spans_total_matches_layout_messages_len() {
        // Arrange
        let messages = vec![
            msg("alice", "one two three four five"),
            msg("bob", "short"),
            msg("carol", "another longer message over here"),
        ];

        // Act
        let spans = message_spans(&messages, 24);
        let total: u16 = spans.last().map(|&(s, h)| s + h).unwrap_or(0);

        // Assert: the trailing framing row adds one row past the last span end.
        assert_eq!(total as usize + 1, layout_messages(&messages, 24).len());
    }
}
