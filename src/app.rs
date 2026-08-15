//! Application state: per-channel message histories with independent scroll,
//! plus the composer input buffer.
//!
//! Auto-scroll rule (per channel): a newly arriving message only moves the
//! viewport when the newest message's last line is currently visible. Once the
//! user has scrolled up so that line leaves the window, the viewport stays put
//! until they scroll back to the bottom.

use crate::layout::{LayoutLine, layout_messages};
use crate::message::ChatMessage;

/// Maximum number of messages kept per channel; the oldest are dropped first.
pub const MAX_MESSAGES: usize = 5000;

/// Maximum number of laid-out rows kept per channel; the oldest messages are
/// dropped when the layout grows past this. Must stay comfortably below
/// `u16::MAX`: ratatui's scroll offset is a `u16`, and the scroll model
/// (offsets, `is_at_bottom`) only stays honest while the row count fits.
/// IRC lines are at most ~512 bytes, so even pathological single messages add
/// only a few hundred rows on top of this cap.
pub const MAX_LINES: usize = 50_000;

/// Maximum input buffer length, in chars.
pub const MAX_INPUT: usize = 512;

/// One channel's history plus its scroll state.
struct ChannelState {
    server: String,
    channel: String,
    messages: Vec<ChatMessage>,
    lines: Vec<LayoutLine>,
    /// First visible content row (0 = top).
    scroll_offset: u16,
}

impl ChannelState {
    fn new(server: String, channel: String) -> ChannelState {
        ChannelState {
            server,
            channel,
            messages: Vec::new(),
            lines: Vec::new(),
            scroll_offset: 0,
        }
    }

    fn total_height(&self) -> u16 {
        u16::try_from(self.lines.len()).unwrap_or(u16::MAX)
    }
}

/// Which region currently holds the keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Messages,
    Composer,
}

/// One visible row of the server/channel sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRow {
    pub server: String,
    /// `None` for a server header row, `Some(channel)` for a channel row.
    pub channel: Option<String>,
}

pub struct App {
    /// Registered channels in sidebar order; `active` indexes the viewed one.
    channels: Vec<ChannelState>,
    active: usize,
    focus: Focus,
    /// Sidebar cursor (visible-row index), present only while the sidebar is focused.
    sidebar_cursor: Option<usize>,
    /// Lower-cased names of servers whose channel lists are collapsed.
    collapsed: std::collections::BTreeSet<String>,
    width: u16,
    viewport_height: u16,
    running: bool,
    /// Text the user is typing in the composer (not sent anywhere).
    input: String,
    /// Cursor position as a char index into `input`.
    input_cursor: usize,
    message_cap: usize,
    line_cap: usize,
}

impl App {
    pub fn new(width: u16, viewport_height: u16) -> App {
        Self::with_caps(width, viewport_height, MAX_MESSAGES, MAX_LINES)
    }

    fn with_caps(width: u16, viewport_height: u16, message_cap: usize, line_cap: usize) -> App {
        App {
            channels: Vec::new(),
            active: 0,
            focus: Focus::Composer,
            sidebar_cursor: None,
            collapsed: std::collections::BTreeSet::new(),
            width,
            viewport_height,
            running: true,
            input: String::new(),
            input_cursor: 0,
            message_cap,
            line_cap,
        }
    }

    // ----- channels -----

    /// Register a channel (idempotent, matched case-insensitively). The first
    /// registered channel is the initially viewed one.
    pub fn open_channel(&mut self, server: &str, channel: &str) {
        if self.find_channel(server, channel).is_none() {
            self.channels
                .push(ChannelState::new(server.to_string(), channel.to_string()));
        }
    }

    /// Number of registered channels.
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// The `(server, channel)` currently being viewed.
    pub fn active_channel(&self) -> (&str, &str) {
        match self.channels.get(self.active) {
            Some(state) => (&state.server, &state.channel),
            None => ("", ""),
        }
    }

    /// Switch the message pane to a registered channel, keeping that channel's
    /// scroll position (non-following).
    pub fn select_channel(&mut self, index: usize) {
        if index < self.channels.len() {
            self.active = index;
            self.relayout_active(false);
        }
    }

    fn find_channel(&self, server: &str, channel: &str) -> Option<usize> {
        self.channels.iter().position(|state| {
            state.server.eq_ignore_ascii_case(server) && state.channel.eq_ignore_ascii_case(channel)
        })
    }

    // ----- messages -----

    /// Route a message to its channel. The active channel re-lays-out and
    /// follows the auto-scroll rule; background channels only accumulate
    /// history. Messages for unregistered channels are dropped (when no
    /// channel is registered at all, the first message opens one - this keeps
    /// the App usable standalone, e.g. in tests).
    pub fn push_message(&mut self, message: ChatMessage) {
        let idx = match self.find_channel(&message.server, &message.channel) {
            Some(idx) => idx,
            None if self.channels.is_empty() => {
                self.channels.push(ChannelState::new(
                    message.server.clone(),
                    message.channel.clone(),
                ));
                self.active = 0;
                0
            }
            None => return,
        };
        if idx == self.active {
            let was_at_bottom = self.is_at_bottom();
            self.push_into(idx, message);
            self.relayout_active(was_at_bottom);
        } else {
            self.push_into(idx, message);
        }
    }

    fn push_into(&mut self, idx: usize, message: ChatMessage) {
        let cap = self.message_cap;
        let state = &mut self.channels[idx];
        state.messages.push(message);
        if state.messages.len() > cap {
            let excess = state.messages.len() - cap;
            state.messages.drain(..excess);
        }
    }

    // ----- scrolling (active channel) -----

    /// Scroll up by one third of the viewport height (at least one line).
    pub fn scroll_page_up(&mut self) {
        let step = self.page_step();
        if let Some(state) = self.channels.get_mut(self.active) {
            state.scroll_offset = state.scroll_offset.saturating_sub(step);
        }
    }

    /// Scroll down by one third of the viewport height (at least one line).
    pub fn scroll_page_down(&mut self) {
        let step = self.page_step();
        let viewport_height = self.viewport_height;
        if let Some(state) = self.channels.get_mut(self.active) {
            let max = state.total_height().saturating_sub(viewport_height);
            state.scroll_offset = state.scroll_offset.saturating_add(step).min(max);
        }
    }

    /// Set the scroll offset, clamped to the valid range.
    pub fn set_scroll_offset(&mut self, offset: u16) {
        let viewport_height = self.viewport_height;
        if let Some(state) = self.channels.get_mut(self.active) {
            let max = state.total_height().saturating_sub(viewport_height);
            state.scroll_offset = offset.min(max);
        }
    }

    /// First visible content row.
    pub fn scroll_offset(&self) -> u16 {
        self.channels
            .get(self.active)
            .map(|s| s.scroll_offset)
            .unwrap_or(0)
    }

    /// Total height of the laid-out content, in rows.
    pub fn total_height(&self) -> u16 {
        self.channels
            .get(self.active)
            .map_or(0, |s| s.total_height())
    }

    /// Largest valid scroll offset for the current content and viewport.
    pub fn max_offset(&self) -> u16 {
        self.total_height().saturating_sub(self.viewport_height)
    }

    /// Whether the newest message's last line is currently visible.
    pub fn is_at_bottom(&self) -> bool {
        self.scroll_offset() >= self.max_offset()
    }

    /// The laid-out rows to render.
    pub fn lines(&self) -> &[LayoutLine] {
        self.channels.get(self.active).map_or(&[], |s| &s.lines)
    }

    /// The stored messages of the viewed channel, oldest first.
    pub fn messages(&self) -> &[ChatMessage] {
        self.channels.get(self.active).map_or(&[], |s| &s.messages)
    }

    /// The current message viewport size `(width, height)`.
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.viewport_height)
    }

    // ----- lifecycle -----

    pub fn quit(&mut self) {
        self.running = false;
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Update the viewport size and re-layout, following the auto-scroll rule.
    pub fn resize(&mut self, width: u16, viewport_height: u16) {
        let was_at_bottom = self.is_at_bottom();
        self.width = width;
        self.viewport_height = viewport_height;
        self.relayout_active(was_at_bottom);
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

    /// Re-wrap the active channel at the current width, enforce the line cap,
    /// and settle the scroll offset (follow the bottom or keep it clamped).
    fn relayout_active(&mut self, was_at_bottom: bool) {
        let width = self.width;
        let line_cap = self.line_cap;
        let viewport_height = self.viewport_height;
        let state = &mut self.channels[self.active];
        state.lines = layout_messages(&state.messages, width);
        // Bound the row count so the u16 scroll model stays valid: drop the
        // oldest messages until the layout fits the line cap.
        while state.lines.len() > line_cap && state.messages.len() > 1 {
            state.messages.remove(0);
            state.lines = layout_messages(&state.messages, width);
        }
        let max = state.total_height().saturating_sub(viewport_height);
        state.scroll_offset = if was_at_bottom {
            max
        } else {
            state.scroll_offset.min(max)
        };
    }

    // ----- focus & sidebar -----

    /// The region that currently holds keyboard focus.
    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// Move focus: Composer -> Sidebar -> Messages -> Composer. Entering the
    /// sidebar places the cursor on its first row.
    pub fn tab(&mut self) {
        self.focus = match self.focus {
            Focus::Composer => {
                if self.channels.is_empty() {
                    Focus::Composer // nothing to navigate without channels
                } else {
                    if self.sidebar_cursor.is_none() {
                        self.sidebar_cursor = Some(0);
                    }
                    Focus::Sidebar
                }
            }
            Focus::Sidebar => Focus::Messages,
            Focus::Messages => Focus::Composer,
        };
    }

    /// One visible row of the sidebar (a server header, or one of its channels).
    pub fn sidebar_rows(&self) -> Vec<SidebarRow> {
        // Servers in first-appearance order, each followed by its channels
        // unless collapsed.
        let mut servers: Vec<&str> = Vec::new();
        for state in &self.channels {
            if !servers
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&state.server))
            {
                servers.push(&state.server);
            }
        }
        let mut rows = Vec::new();
        for server in servers {
            rows.push(SidebarRow {
                server: server.to_string(),
                channel: None,
            });
            if self.server_collapsed(server) {
                continue;
            }
            for state in &self.channels {
                if state.server.eq_ignore_ascii_case(server) {
                    rows.push(SidebarRow {
                        server: server.to_string(),
                        channel: Some(state.channel.clone()),
                    });
                }
            }
        }
        rows
    }

    /// The sidebar cursor row, present only while the sidebar has focus.
    pub fn sidebar_cursor(&self) -> Option<usize> {
        self.sidebar_cursor
    }

    /// Move the sidebar cursor down one visible row (no-op unless focused).
    pub fn sidebar_down(&mut self) {
        self.move_sidebar_cursor(|c, len| (c + 1).min(len - 1));
    }

    /// Move the sidebar cursor up one visible row (no-op unless focused).
    pub fn sidebar_up(&mut self) {
        self.move_sidebar_cursor(|c, _| c.saturating_sub(1));
    }

    /// Whether a server's channel list is collapsed.
    pub fn server_collapsed(&self, server: &str) -> bool {
        self.collapsed.contains(&server.to_lowercase())
    }

    /// Interact with the row under the sidebar cursor: on a server row,
    /// collapse/expand its channel list; on a channel row, switch the message
    /// pane to it and return focus to the composer.
    pub fn sidebar_enter(&mut self) {
        if self.focus != Focus::Sidebar {
            return;
        }
        let Some(cursor) = self.sidebar_cursor else {
            return;
        };
        let rows = self.sidebar_rows();
        let Some(row) = rows.get(cursor) else {
            return;
        };
        match &row.channel {
            None => self.toggle_server_collapse(&row.server),
            Some(channel) => {
                if let Some(idx) = self.find_channel(&row.server, channel) {
                    self.select_channel(idx);
                    self.focus = Focus::Composer;
                    self.sidebar_cursor = None;
                }
            }
        }
    }

    fn toggle_server_collapse(&mut self, server: &str) {
        let key = server.to_lowercase();
        if self.collapsed.remove(&key) {
            return;
        }
        self.collapsed.insert(key);
        // Collapse shortens the row list; keep the cursor within bounds.
        let len = self.sidebar_rows().len().max(1);
        if let Some(cursor) = self.sidebar_cursor {
            self.sidebar_cursor = Some(cursor.min(len - 1));
        }
    }

    fn move_sidebar_cursor(&mut self, step: impl Fn(usize, usize) -> usize) {
        if self.focus != Focus::Sidebar {
            return;
        }
        let len = self.sidebar_rows().len();
        if len == 0 {
            return;
        }
        let cursor = self.sidebar_cursor.unwrap_or(0);
        self.sidebar_cursor = Some(step(cursor, len).min(len - 1));
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

    // ----- multi-channel routing -----

    fn chan_msg(server: &str, channel: &str, text: &str) -> ChatMessage {
        ChatMessage {
            server: server.to_string(),
            channel: channel.to_string(),
            nick: "u".to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn open_channel_registers_and_routes() {
        // Arrange
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#osu");

        // Act
        app.push_message(chan_msg("srv", "#osu", "hi"));

        // Assert
        assert_eq!(app.channel_count(), 1);
        assert_eq!(app.messages().len(), 1);
        assert_eq!(app.active_channel(), ("srv", "#osu"));
    }

    #[test]
    fn open_channel_is_idempotent_ignoring_case() {
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#osu");
        app.open_channel("srv", "#OSU");
        assert_eq!(app.channel_count(), 1);
    }

    #[test]
    fn push_routes_to_channel_ignoring_case() {
        // Arrange
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#osu");
        app.open_channel("srv", "#chinese");

        // Act / Assert: "#OSU" from "SRV" hits the active #osu channel.
        app.push_message(chan_msg("SRV", "#OSU", "a"));
        assert_eq!(app.messages().len(), 1);
        // #chinese is background: its history grows, the view stays #osu.
        app.push_message(chan_msg("srv", "#chinese", "b"));
        assert_eq!(app.messages().len(), 1);
        app.select_channel(1);
        assert_eq!(app.messages().len(), 1);
    }

    #[test]
    fn push_to_unopened_channel_is_dropped() {
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#osu");
        app.push_message(chan_msg("srv", "#other", "x"));
        assert_eq!(app.messages().len(), 0);
    }

    #[test]
    fn switching_channel_switches_history_and_back_preserves_scroll() {
        // Arrange
        let mut app = App::new(40, 3);
        app.open_channel("srv", "#a");
        app.open_channel("srv", "#b");
        for i in 0..6 {
            app.push_message(chan_msg("srv", "#a", &format!("m{i}")));
        }
        app.scroll_page_up();
        let offset = app.scroll_offset();
        assert!(offset < app.max_offset());

        // Act
        app.select_channel(1);
        assert_eq!(app.active_channel().1, "#b");
        assert_eq!(app.messages().len(), 0);
        app.push_message(chan_msg("srv", "#b", "bee"));
        app.select_channel(0);

        // Assert: #a's history and scroll position are intact.
        assert_eq!(app.active_channel().1, "#a");
        assert_eq!(app.messages().len(), 6);
        assert_eq!(app.scroll_offset(), offset);
    }

    #[test]
    fn background_push_leaves_active_layout_stable() {
        // Arrange
        let mut app = App::new(40, 3);
        app.open_channel("srv", "#a");
        app.open_channel("srv", "#b");
        app.push_message(chan_msg("srv", "#a", "m"));
        let height = app.total_height();

        // Act: pushes to the background channel.
        for i in 0..5 {
            app.push_message(chan_msg("srv", "#b", &format!("b{i}")));
        }

        // Assert
        assert_eq!(app.total_height(), height);
    }

    // ----- focus & sidebar -----

    #[test]
    fn tab_cycles_composer_sidebar_messages() {
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");

        assert_eq!(app.focus(), Focus::Composer);
        app.tab();
        assert_eq!(app.focus(), Focus::Sidebar);
        app.tab();
        assert_eq!(app.focus(), Focus::Messages);
        app.tab();
        assert_eq!(app.focus(), Focus::Composer);
    }

    #[test]
    fn tab_into_sidebar_places_cursor_on_first_row() {
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.tab();
        assert_eq!(app.sidebar_cursor(), Some(0));
        // Leaving the sidebar keeps the cursor position for the next visit.
        app.tab();
        assert_eq!(app.focus(), Focus::Messages);
        assert_eq!(app.sidebar_cursor(), Some(0));
    }

    #[test]
    fn sidebar_jk_moves_cursor_and_clamps() {
        // Arrange: one server with two channels -> 3 visible rows.
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.open_channel("srv", "#b");
        app.tab();

        // Act / Assert
        app.sidebar_down();
        assert_eq!(app.sidebar_cursor(), Some(1));
        app.sidebar_down();
        assert_eq!(app.sidebar_cursor(), Some(2));
        app.sidebar_down();
        assert_eq!(app.sidebar_cursor(), Some(2)); // clamped
        app.sidebar_up();
        app.sidebar_up();
        app.sidebar_up();
        assert_eq!(app.sidebar_cursor(), Some(0)); // clamped
    }

    #[test]
    fn sidebar_jk_ignored_when_sidebar_not_focused() {
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.sidebar_down(); // focus is Composer - no-op
        assert_eq!(app.sidebar_cursor(), None);
    }

    #[test]
    fn sidebar_enter_on_server_row_toggles_collapse() {
        // Arrange: rows = [srv, #a, #b]
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.open_channel("srv", "#b");
        app.tab();

        // Act: collapse, then expand.
        app.sidebar_enter();
        assert!(app.server_collapsed("srv"));
        assert_eq!(app.sidebar_rows().len(), 1); // only the server row remains
        app.sidebar_enter();
        assert!(!app.server_collapsed("srv"));
        assert_eq!(app.sidebar_rows().len(), 3);
    }

    #[test]
    fn collapsing_keeps_cursor_on_the_server_row() {
        // Arrange: cursor on the server header row (row 0).
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.open_channel("srv", "#b");
        app.tab();
        assert_eq!(app.sidebar_cursor(), Some(0));

        // Act: collapse via Enter on the server row.
        app.sidebar_enter();

        // Assert: the cursor stays on the (now only) server row and is valid.
        assert_eq!(app.sidebar_rows().len(), 1);
        assert_eq!(app.sidebar_cursor(), Some(0));
    }

    #[test]
    fn collapse_moves_a_cursor_stranded_past_the_end() {
        // Arrange: two servers [s1, #a, s2, #b]; cursor on #b (row 3), then
        // walk it back to s1's header (row 0) before collapsing s1.
        let mut app = App::new(40, 10);
        app.open_channel("s1", "#a");
        app.open_channel("s2", "#b");
        app.tab();
        app.sidebar_down();
        app.sidebar_down();
        app.sidebar_down();
        assert_eq!(app.sidebar_cursor(), Some(3)); // on #b

        // Act: collapse s1 (cursor walked to its header at row 0).
        app.sidebar_up();
        app.sidebar_up();
        app.sidebar_up();
        app.sidebar_enter();

        // Assert: rows shrink to [s1, s2, #b]; the cursor is valid and clamped.
        assert_eq!(app.sidebar_rows().len(), 3);
        assert_eq!(app.sidebar_cursor(), Some(0));
    }

    #[test]
    fn sidebar_enter_on_channel_switches_view_and_focuses_composer() {
        // Arrange: rows = [srv, #a, #b]; cursor on #b.
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.open_channel("srv", "#b");
        app.push_message(chan_msg("srv", "#a", "hello a"));
        app.tab();
        app.sidebar_down();
        app.sidebar_down();

        // Act
        app.sidebar_enter();

        // Assert: view switched to #b, focus returned to the composer.
        assert_eq!(app.active_channel(), ("srv", "#b"));
        assert_eq!(app.messages().len(), 0);
        assert_eq!(app.focus(), Focus::Composer);
        assert_eq!(app.sidebar_cursor(), None);
    }
}
