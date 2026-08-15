//! Application state: message list, viewport, and scroll behavior.
//!
//! Auto-scroll rule: a newly arriving message only moves the viewport when the
//! newest message's last line is currently visible — i.e. the content bottom
//! is in view. Once the user has scrolled up so that line leaves the window,
//! the viewport stays put until they scroll back to the bottom.

use crate::layout::{LayoutLine, layout_messages};
use crate::message::ChatMessage;

/// Maximum number of messages kept in memory; the oldest are dropped first.
pub const MAX_MESSAGES: usize = 5000;

/// Maximum number of laid-out rows kept in memory; the oldest messages are
/// dropped when the layout grows past this. Must stay comfortably below
/// `u16::MAX`: ratatui's scroll offset is a `u16`, and the scroll model
/// (offsets, `is_at_bottom`) only stays honest while the row count fits.
/// IRC lines are at most ~512 bytes, so even pathological single messages add
/// only a few hundred rows on top of this cap.
pub const MAX_LINES: usize = 50_000;

/// Maximum input buffer length, in chars.
pub const MAX_INPUT: usize = 512;

pub struct App {
    messages: Vec<ChatMessage>,
    message_cap: usize,
    line_cap: usize,
    lines: Vec<LayoutLine>,
    width: u16,
    viewport_height: u16,
    /// First visible content row (0 = top).
    scroll_offset: u16,
    running: bool,
    /// Text the user is typing in the composer (not sent anywhere).
    input: String,
    /// Cursor position as a char index into `input`.
    input_cursor: usize,
}

impl App {
    pub fn new(width: u16, viewport_height: u16) -> App {
        Self::with_caps(width, viewport_height, MAX_MESSAGES, MAX_LINES)
    }

    fn with_caps(width: u16, viewport_height: u16, message_cap: usize, line_cap: usize) -> App {
        App {
            messages: Vec::new(),
            message_cap,
            line_cap,
            lines: Vec::new(),
            width,
            viewport_height,
            scroll_offset: 0,
            running: true,
            input: String::new(),
            input_cursor: 0,
        }
    }

    /// Append a message and re-layout, following the auto-scroll rule.
    pub fn push_message(&mut self, message: ChatMessage) {
        let was_at_bottom = self.is_at_bottom();
        self.messages.push(message);
        if self.messages.len() > self.message_cap {
            let excess = self.messages.len() - self.message_cap;
            self.messages.drain(..excess);
        }
        self.relayout(was_at_bottom);
    }

    /// Scroll up by one third of the viewport height (at least one line).
    pub fn scroll_page_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(self.page_step());
    }

    /// Scroll down by one third of the viewport height (at least one line).
    pub fn scroll_page_down(&mut self) {
        self.scroll_offset = self
            .scroll_offset
            .saturating_add(self.page_step())
            .min(self.max_offset());
    }

    /// Set the scroll offset, clamped to the valid range.
    pub fn set_scroll_offset(&mut self, offset: u16) {
        self.scroll_offset = offset.min(self.max_offset());
    }

    /// Update the viewport size and re-layout, following the auto-scroll rule.
    pub fn resize(&mut self, width: u16, viewport_height: u16) {
        let was_at_bottom = self.is_at_bottom();
        self.width = width;
        self.viewport_height = viewport_height;
        self.relayout(was_at_bottom);
    }

    pub fn quit(&mut self) {
        self.running = false;
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// First visible content row.
    pub fn scroll_offset(&self) -> u16 {
        self.scroll_offset
    }

    /// Total height of the laid-out content, in rows.
    pub fn total_height(&self) -> u16 {
        u16::try_from(self.lines.len()).unwrap_or(u16::MAX)
    }

    /// Largest valid scroll offset for the current content and viewport.
    pub fn max_offset(&self) -> u16 {
        self.total_height().saturating_sub(self.viewport_height)
    }

    /// Whether the newest message's last line is currently visible.
    pub fn is_at_bottom(&self) -> bool {
        self.scroll_offset >= self.max_offset()
    }

    /// The laid-out rows to render.
    pub fn lines(&self) -> &[LayoutLine] {
        &self.lines
    }

    /// The stored messages, oldest first.
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// The current message viewport size `(width, height)`.
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.viewport_height)
    }

    // ----- composer input (receive-only: no sending) -----

    /// The text currently typed in the composer.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Cursor position as a char index.
    pub fn input_cursor(&self) -> usize {
        self.input_cursor
    }

    /// Insert a character at the cursor (subject to MAX_INPUT).
    pub fn type_char(&mut self, c: char) {
        if self.input.chars().count() >= MAX_INPUT {
            return;
        }
        match self.input.char_indices().nth(self.input_cursor) {
            Some((byte, _)) => self.input.insert(byte, c),
            None => self.input.push(c),
        }
        self.input_cursor += 1;
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let idx = self.input_cursor - 1;
        if let Some((byte, _)) = self.input.char_indices().nth(idx) {
            self.input.remove(byte);
            self.input_cursor = idx;
        }
    }

    /// Delete the character at the cursor.
    pub fn delete(&mut self) {
        if let Some((byte, _)) = self.input.char_indices().nth(self.input_cursor) {
            self.input.remove(byte);
        }
    }

    pub fn cursor_left(&mut self) {
        self.input_cursor = self.input_cursor.saturating_sub(1);
    }

    pub fn cursor_right(&mut self) {
        if self.input_cursor < self.input.chars().count() {
            self.input_cursor += 1;
        }
    }

    pub fn cursor_home(&mut self) {
        self.input_cursor = 0;
    }

    pub fn cursor_end(&mut self) {
        self.input_cursor = self.input.chars().count();
    }

    fn page_step(&self) -> u16 {
        (self.viewport_height / 3).max(1)
    }

    fn relayout(&mut self, was_at_bottom: bool) {
        self.lines = layout_messages(&self.messages, self.width);
        // Bound the row count so the u16 scroll model stays valid: drop the
        // oldest messages until the layout fits the line cap.
        while self.lines.len() > self.line_cap && self.messages.len() > 1 {
            self.messages.remove(0);
            self.lines = layout_messages(&self.messages, self.width);
        }
        self.scroll_offset = if was_at_bottom {
            self.max_offset()
        } else {
            self.scroll_offset.min(self.max_offset())
        };
    }
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
    fn new_app_is_at_bottom_with_zero_offset() {
        let app = App::new(40, 10);

        assert_eq!(app.scroll_offset(), 0);
        assert!(app.is_at_bottom());
        assert!(app.is_running());
        assert_eq!(app.total_height(), 0);
    }

    #[test]
    fn push_message_while_at_bottom_follows_to_new_max_offset() {
        // Arrange: viewport of 3 rows; each short message is 1 content row.
        let mut app = App::new(40, 3);

        // Act & Assert: after every push the viewport stays glued to the bottom.
        for i in 0..6 {
            app.push_message(msg("u", &format!("m{i}")));
            assert!(app.is_at_bottom(), "not at bottom after push {i}");
            assert_eq!(app.scroll_offset(), app.max_offset());
        }
        // 6 messages = 6 rows + 5 separators = 11 rows; max offset = 11 - 3.
        assert_eq!(app.total_height(), 11);
        assert_eq!(app.scroll_offset(), 8);
    }

    #[test]
    fn push_message_while_scrolled_up_keeps_offset_unchanged() {
        // Arrange: overflowing content, viewport scrolled to the top.
        let mut app = App::new(40, 3);
        for i in 0..5 {
            app.push_message(msg("u", &format!("m{i}")));
        }
        assert_eq!(app.max_offset(), 6);
        app.set_scroll_offset(0);
        assert!(!app.is_at_bottom());

        // Act
        app.push_message(msg("u", "new"));

        // Assert: newest message's last line was not visible -> view unmoved.
        assert_eq!(app.scroll_offset(), 0);
    }

    #[test]
    fn push_message_with_last_line_exactly_visible_counts_as_at_bottom() {
        // Arrange: offset exactly at max — the last line is the bottom row.
        let mut app = App::new(40, 3);
        for i in 0..5 {
            app.push_message(msg("u", &format!("m{i}")));
        }
        app.set_scroll_offset(app.max_offset());
        assert_eq!(app.scroll_offset(), 6);

        // Act
        app.push_message(msg("u", "new"));

        // Assert: boundary counts as "at bottom" -> follows to the new max.
        assert_eq!(app.max_offset(), 8);
        assert_eq!(app.scroll_offset(), 8);
    }

    #[test]
    fn content_smaller_than_viewport_always_at_bottom() {
        let mut app = App::new(40, 10);

        app.push_message(msg("u", "hi"));

        assert_eq!(app.max_offset(), 0);
        assert!(app.is_at_bottom());
        assert_eq!(app.scroll_offset(), 0);
    }

    #[test]
    fn page_up_decrements_by_one_third_viewport() {
        // Arrange: viewport height 9 -> step 3; 8 messages -> max offset 6.
        let mut app = App::new(40, 9);
        for i in 0..8 {
            app.push_message(msg("u", &format!("m{i}")));
        }
        assert_eq!(app.max_offset(), 6);

        // Act
        app.scroll_page_up();

        // Assert
        assert_eq!(app.scroll_offset(), 3);
    }

    #[test]
    fn page_up_clamps_at_zero() {
        // Arrange
        let mut app = App::new(40, 9);
        for i in 0..8 {
            app.push_message(msg("u", &format!("m{i}")));
        }
        app.set_scroll_offset(1);

        // Act: step is 3, which would go below zero.
        app.scroll_page_up();

        // Assert
        assert_eq!(app.scroll_offset(), 0);
    }

    #[test]
    fn page_down_clamps_at_max_offset() {
        // Arrange
        let mut app = App::new(40, 9);
        for i in 0..8 {
            app.push_message(msg("u", &format!("m{i}")));
        }
        app.set_scroll_offset(5);

        // Act: step 3 would exceed max offset 6.
        app.scroll_page_down();

        // Assert
        assert_eq!(app.scroll_offset(), 6);
    }

    #[test]
    fn page_step_is_at_least_one_for_tiny_viewport() {
        // Arrange: height 2 -> 2/3 = 0, floored to a step of 1.
        let mut app = App::new(40, 2);
        for i in 0..5 {
            app.push_message(msg("u", &format!("m{i}")));
        }
        let start = app.max_offset();
        app.set_scroll_offset(start);

        // Act
        app.scroll_page_up();

        // Assert
        assert_eq!(app.scroll_offset(), start - 1);
    }

    #[test]
    fn resize_while_at_bottom_stays_at_bottom() {
        // Arrange: 2 messages in a tall viewport, at the bottom.
        let mut app = App::new(40, 10);
        app.push_message(msg("alice", "hi"));
        app.push_message(msg("bob", "hiya"));
        assert!(app.is_at_bottom());

        // Act: narrow the window so both bodies wrap to 2 rows
        // (alice: indent 7 -> body width 1; bob: indent 5 -> body width 3, "hiya" splits).
        app.resize(8, 2);

        // Assert: still glued to the bottom, at the recomputed max.
        assert!(app.is_at_bottom());
        assert_eq!(app.scroll_offset(), app.max_offset());
        assert_eq!(app.total_height(), 5); // 2+2 rows + 1 separator
    }

    #[test]
    fn resize_while_scrolled_up_preserves_offset_clamped() {
        // Arrange: 8 short messages -> height 15; with h=10 max offset is 5.
        let mut app = App::new(30, 10);
        for _ in 0..8 {
            app.push_message(msg("u", "x"));
        }
        app.set_scroll_offset(2);
        assert!(!app.is_at_bottom());

        // Act & Assert: growing the viewport shrinks max to 3, still >= 2.
        app.resize(30, 12);
        assert_eq!(app.max_offset(), 3);
        assert_eq!(app.scroll_offset(), 2);

        // Act & Assert: growing further shrinks max to 1 -> offset clamps down.
        app.resize(30, 14);
        assert_eq!(app.max_offset(), 1);
        assert_eq!(app.scroll_offset(), 1);
    }

    #[test]
    fn quit_sets_running_false() {
        let mut app = App::new(40, 10);

        app.quit();

        assert!(!app.is_running());
    }

    #[test]
    fn message_cap_drops_oldest_beyond_limit() {
        // Arrange: small cap so the test stays fast.
        let mut app = App::with_caps(40, 10, 3, MAX_LINES);

        // Act
        for i in 0..4 {
            app.push_message(msg("u", &format!("m{i}")));
        }

        // Assert: oldest ("m0") is gone, three remain.
        assert_eq!(app.messages().len(), 3);
        assert_eq!(app.messages().first().unwrap().text, "m1");
    }

    #[test]
    fn line_cap_evicts_oldest_to_keep_height_bounded() {
        // Arrange: width 10 -> nick "u" takes 3 columns, body width 7;
        // "aa bb cc N" wraps into 2 rows, so n messages occupy 3n-1 rows.
        // With a line cap of 8, at most 3 messages fit (3*3-1 = 8).
        let mut app = App::with_caps(10, 5, 1000, 8);

        // Act
        for i in 0..10 {
            app.push_message(msg("u", &format!("aa bb cc {i}")));
        }

        // Assert: height stays at the cap and the oldest messages were dropped.
        assert_eq!(app.total_height(), 8);
        assert_eq!(app.messages().len(), 3);
        assert_eq!(app.messages().first().unwrap().text, "aa bb cc 7");
    }

    #[test]
    fn eviction_while_scrolled_up_keeps_offset_clamped() {
        // Arrange: exactly at the line cap, then scrolled to the top.
        let mut app = App::with_caps(10, 3, 1000, 8);
        for i in 0..3 {
            app.push_message(msg("u", &format!("aa bb cc {i}")));
        }
        assert_eq!(app.total_height(), 8);
        app.set_scroll_offset(0);
        assert!(!app.is_at_bottom());

        // Act: a fourth message forces eviction back down to the cap.
        app.push_message(msg("u", "aa bb cc 3"));

        // Assert: m0 evicted, height bounded, offset still valid and unmoved.
        assert_eq!(app.messages().first().unwrap().text, "aa bb cc 1");
        assert_eq!(app.total_height(), 8);
        assert!(app.scroll_offset() <= app.max_offset());
        assert_eq!(app.scroll_offset(), 0);
    }

    #[test]
    fn type_char_inserts_at_cursor_and_advances() {
        // Arrange
        let mut app = App::new(40, 10);

        // Act
        app.type_char('h');
        app.type_char('i');

        // Assert
        assert_eq!(app.input(), "hi");
        assert_eq!(app.input_cursor(), 2);
    }

    #[test]
    fn type_char_inserts_in_the_middle_at_cursor() {
        // Arrange
        let mut app = App::new(40, 10);
        app.type_char('a');
        app.type_char('c');
        app.cursor_left(); // cursor between a and c

        // Act
        app.type_char('b');

        // Assert
        assert_eq!(app.input(), "abc");
        assert_eq!(app.input_cursor(), 2);
    }

    #[test]
    fn backspace_deletes_behind_cursor() {
        // Arrange
        let mut app = App::new(40, 10);
        app.type_char('h');
        app.type_char('i');

        // Act
        app.backspace();

        // Assert
        assert_eq!(app.input(), "h");
        assert_eq!(app.input_cursor(), 1);
    }

    #[test]
    fn backspace_at_start_is_a_noop() {
        let mut app = App::new(40, 10);
        app.backspace();
        assert_eq!(app.input(), "");
        assert_eq!(app.input_cursor(), 0);
    }

    #[test]
    fn delete_removes_char_at_cursor() {
        // Arrange
        let mut app = App::new(40, 10);
        app.type_char('a');
        app.type_char('b');
        app.type_char('c');
        app.cursor_home(); // cursor before 'a'

        // Act
        app.delete();

        // Assert
        assert_eq!(app.input(), "bc");
        assert_eq!(app.input_cursor(), 0);
    }

    #[test]
    fn cursor_left_right_clamp_at_bounds() {
        let mut app = App::new(40, 10);
        app.type_char('a');
        app.type_char('b');
        // at end (2): left -> 1, left -> 0, left -> 0 (clamp)
        app.cursor_left();
        assert_eq!(app.input_cursor(), 1);
        app.cursor_left();
        assert_eq!(app.input_cursor(), 0);
        app.cursor_left();
        assert_eq!(app.input_cursor(), 0);
        // right -> 1, right -> 2, right -> 2 (clamp)
        app.cursor_right();
        assert_eq!(app.input_cursor(), 1);
        app.cursor_right();
        assert_eq!(app.input_cursor(), 2);
        app.cursor_right();
        assert_eq!(app.input_cursor(), 2);
    }

    #[test]
    fn cursor_home_and_end() {
        let mut app = App::new(40, 10);
        app.type_char('a');
        app.type_char('b');
        app.type_char('c');
        app.cursor_home();
        assert_eq!(app.input_cursor(), 0);
        app.cursor_end();
        assert_eq!(app.input_cursor(), 3);
    }

    #[test]
    fn type_beyond_max_input_is_capped() {
        // Arrange
        let mut app = App::new(40, 10);
        for _ in 0..(MAX_INPUT + 10) {
            app.type_char('x');
        }

        // Assert: never exceeds the cap; cursor sits at the end.
        assert_eq!(app.input().chars().count(), MAX_INPUT);
        assert_eq!(app.input_cursor(), MAX_INPUT);
    }
}
