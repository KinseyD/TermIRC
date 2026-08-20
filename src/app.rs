//! Application state: per-channel message histories with independent scroll,
//! plus the composer input buffer.
//!
//! Auto-scroll rule (per channel): a newly arriving message only moves the
//! viewport when the newest message's last line is currently visible. Once the
//! user has scrolled up so that line leaves the window, the viewport stays put
//! until they scroll back to the bottom.

use crate::layout::{LayoutLine, layout_messages, message_spans};
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
    /// Selected message index (used while the message pane has focus).
    selected: Option<usize>,
}

impl ChannelState {
    fn new(server: String, channel: String) -> ChannelState {
        ChannelState {
            server,
            channel,
            messages: Vec::new(),
            lines: Vec::new(),
            scroll_offset: 0,
            selected: None,
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
    /// Registered channels in sidebar order; `active` indexes the viewed one
    /// (`None` = no channel open yet: the message pane shows a welcome page).
    channels: Vec<ChannelState>,
    active: Option<usize>,
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
            active: None,
            focus: Focus::Sidebar,
            sidebar_cursor: Some(0),
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

    /// Register a channel (idempotent, matched case-insensitively).
    /// Registering does not view the channel — that happens through
    /// `select_channel` (the sidebar's Enter).
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

    /// The `(server, channel)` currently being viewed, or `None` while the
    /// welcome page is shown (no channel opened yet).
    pub fn active_channel(&self) -> Option<(&str, &str)> {
        self.active
            .and_then(|i| self.channels.get(i))
            .map(|s| (s.server.as_str(), s.channel.as_str()))
    }

    /// Switch the message pane to a registered channel, keeping that channel's
    /// scroll position (non-following).
    pub fn select_channel(&mut self, index: usize) {
        if index < self.channels.len() {
            self.active = Some(index);
            self.relayout_active(false);
        }
    }

    fn find_channel(&self, server: &str, channel: &str) -> Option<usize> {
        self.channels.iter().position(|state| {
            state.server.eq_ignore_ascii_case(server) && state.channel.eq_ignore_ascii_case(channel)
        })
    }

    fn active_state(&self) -> Option<&ChannelState> {
        self.active.and_then(|i| self.channels.get(i))
    }

    fn active_state_mut(&mut self) -> Option<&mut ChannelState> {
        self.active.and_then(|i| self.channels.get_mut(i))
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
                self.active = Some(0);
                0
            }
            None => return,
        };
        if Some(idx) == self.active {
            // Auto-follow is paused while the message pane has a selection, so
            // the selected message stays put instead of being nudged by a new one.
            let was_at_bottom = self.is_at_bottom()
                && !(self.focus == Focus::Messages && self.selected().is_some());
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
            if let Some(sel) = state.selected {
                state.selected = Some(sel.saturating_sub(excess));
            }
        }
    }

    // ----- scrolling (active channel) -----

    /// Scroll up by one third of the viewport height (at least one line).
    pub fn scroll_page_up(&mut self) {
        let step = self.page_step();
        if let Some(state) = self.active_state_mut() {
            state.scroll_offset = state.scroll_offset.saturating_sub(step);
        }
    }

    /// Scroll down by one third of the viewport height (at least one line).
    pub fn scroll_page_down(&mut self) {
        let step = self.page_step();
        let viewport_height = self.viewport_height;
        if let Some(state) = self.active_state_mut() {
            let max = state.total_height().saturating_sub(viewport_height);
            state.scroll_offset = state.scroll_offset.saturating_add(step).min(max);
        }
    }

    /// Set the scroll offset, clamped to the valid range.
    pub fn set_scroll_offset(&mut self, offset: u16) {
        let viewport_height = self.viewport_height;
        if let Some(state) = self.active_state_mut() {
            let max = state.total_height().saturating_sub(viewport_height);
            state.scroll_offset = offset.min(max);
        }
    }

    /// First visible content row.
    pub fn scroll_offset(&self) -> u16 {
        self.active_state().map_or(0, |s| s.scroll_offset)
    }

    /// Total height of the laid-out content, in rows.
    pub fn total_height(&self) -> u16 {
        self.active_state().map_or(0, |s| s.total_height())
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
        self.active_state().map_or(&[], |s| &s.lines)
    }

    /// The stored messages of the viewed channel, oldest first.
    pub fn messages(&self) -> &[ChatMessage] {
        self.active_state().map_or(&[], |s| &s.messages)
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
        let Some(active) = self.active else {
            return;
        };
        let state = &mut self.channels[active];
        state.lines = layout_messages(&state.messages, width);
        // Bound the row count so the u16 scroll model stays valid: drop the
        // oldest messages until the layout fits the line cap.
        while state.lines.len() > line_cap && state.messages.len() > 1 {
            state.messages.remove(0);
            if let Some(sel) = state.selected {
                state.selected = Some(sel.saturating_sub(1));
            }
            state.lines = layout_messages(&state.messages, width);
        }
        let max = state.total_height().saturating_sub(viewport_height);
        state.scroll_offset = if was_at_bottom {
            max
        } else {
            state.scroll_offset.min(max)
        };
    }

    // ----- message selection (message pane) -----

    /// The selected message index, when one is selected.
    pub fn selected(&self) -> Option<usize> {
        self.active_state().and_then(|s| s.selected)
    }

    /// The selected message's row span `(start, height)` in the laid-out
    /// coordinate system, or `None` when nothing is selected.
    pub fn selected_span(&self) -> Option<(u16, u16)> {
        let state = self.active_state()?;
        let idx = state.selected?;
        message_spans(&state.messages, self.width).get(idx).copied()
    }

    /// Move the selection to the next (newer) message, revealing it with the
    /// minimal scroll. No-op unless the message pane has focus.
    pub fn select_next(&mut self) {
        if self.focus != Focus::Messages {
            return;
        }
        self.ensure_selection();
        let Some(len) = self.active_state().map(|s| s.messages.len()) else {
            return;
        };
        if len == 0 {
            return;
        }
        let next = self
            .active_state_mut()
            .unwrap()
            .selected
            .unwrap_or(0)
            .min(len - 1);
        if let Some(state) = self.active_state_mut() {
            state.selected = Some((next + 1).min(len - 1));
        }
        self.reveal_selection();
    }

    /// Move the selection to the previous (older) message, revealing it with
    /// the minimal scroll. No-op unless the message pane has focus.
    pub fn select_prev(&mut self) {
        if self.focus != Focus::Messages {
            return;
        }
        self.ensure_selection();
        let cur = self.active_state().map_or(0, |s| s.selected.unwrap_or(0));
        if let Some(state) = self.active_state_mut() {
            state.selected = Some(cur.saturating_sub(1));
        }
        self.reveal_selection();
    }

    /// Ensure a message is selected (when the pane has focus): the newest one
    /// fully visible, or the newest message otherwise.
    fn ensure_selection(&mut self) {
        let width = self.width;
        let viewport_height = self.viewport_height;
        let Some(active) = self.active else {
            return;
        };
        let state = &self.channels[active];
        if state.selected.is_some_and(|i| i < state.messages.len()) {
            return;
        }
        let spans = message_spans(&state.messages, width);
        if spans.is_empty() {
            return;
        }
        let off = state.scroll_offset;
        let chosen = (0..spans.len())
            .rev()
            .find(|&i| {
                let (start, h) = spans[i];
                start >= off && start + h <= off + viewport_height
            })
            .unwrap_or(spans.len() - 1);
        self.channels[active].selected = Some(chosen);
    }

    /// Scroll the minimum amount so the selected message is fully visible, and
    /// clamp the offset once the layout is stable.
    fn reveal_selection(&mut self) {
        let Some(active) = self.active else {
            return;
        };
        let idx = self.channels[active].selected;
        let Some(idx) = idx else {
            return;
        };
        let width = self.width;
        let viewport_height = self.viewport_height;
        let spans = message_spans(&self.channels[active].messages, width);
        let Some(&(start, height)) = spans.get(idx) else {
            return;
        };
        let end = start + height;
        let state = &mut self.channels[active];
        if start < state.scroll_offset {
            state.scroll_offset = start;
        } else if end > state.scroll_offset + viewport_height {
            state.scroll_offset = end.saturating_sub(viewport_height);
        }
        let max = state.total_height().saturating_sub(viewport_height);
        state.scroll_offset = state.scroll_offset.min(max);
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
                    // Entering the sidebar snaps the cursor onto the viewed
                    // channel's row (or the first row when none is open).
                    self.sidebar_cursor = Some(self.active_sidebar_row());
                    Focus::Sidebar
                }
            }
            Focus::Sidebar => {
                self.ensure_selection();
                Focus::Messages
            }
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

    /// The sidebar row index of the viewed channel (0 when none is open).
    fn active_sidebar_row(&self) -> usize {
        let Some(active) = self.active else {
            return 0;
        };
        let target = &self.channels[active];
        self.sidebar_rows()
            .iter()
            .position(|row| {
                row.server.eq_ignore_ascii_case(&target.server)
                    && row.channel.as_deref() == Some(target.channel.as_str())
            })
            .unwrap_or(0)
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
        // 6 messages = 6 rows + 5 separators + 2 framing rows = 13 rows;
        // max offset = 13 - 3.
        assert_eq!(app.total_height(), 13);
        assert_eq!(app.scroll_offset(), 10);
    }

    #[test]
    fn push_message_while_scrolled_up_keeps_offset_unchanged() {
        // Arrange: overflowing content, viewport scrolled to the top.
        let mut app = App::new(40, 3);
        for i in 0..5 {
            app.push_message(msg("u", &format!("m{i}")));
        }
        assert_eq!(app.max_offset(), 8);
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
        assert_eq!(app.scroll_offset(), 8);

        // Act
        app.push_message(msg("u", "new"));

        // Assert: boundary counts as "at bottom" -> follows to the new max.
        assert_eq!(app.max_offset(), 10);
        assert_eq!(app.scroll_offset(), 10);
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
        // Arrange: viewport height 9 -> step 3; 8 messages -> 8 rows + 7
        // separators + 2 framing rows = 17 -> max offset 8.
        let mut app = App::new(40, 9);
        for i in 0..8 {
            app.push_message(msg("u", &format!("m{i}")));
        }
        assert_eq!(app.max_offset(), 8);

        // Act
        app.scroll_page_up();

        // Assert
        assert_eq!(app.scroll_offset(), 5);
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
        app.set_scroll_offset(7);

        // Act: step 3 would exceed max offset 8.
        app.scroll_page_down();

        // Assert
        assert_eq!(app.scroll_offset(), 8);
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
        assert_eq!(app.total_height(), 7); // 2+2 rows + 1 separator + 2 framing
    }

    #[test]
    fn resize_while_scrolled_up_preserves_offset_clamped() {
        // Arrange: 8 short messages -> height 17; with h=10 max offset is 7.
        let mut app = App::new(30, 10);
        for _ in 0..8 {
            app.push_message(msg("u", "x"));
        }
        app.set_scroll_offset(2);
        assert!(!app.is_at_bottom());

        // Act & Assert: growing the viewport shrinks max to 5, still >= 2.
        app.resize(30, 12);
        assert_eq!(app.max_offset(), 5);
        assert_eq!(app.scroll_offset(), 2);

        // Act & Assert: growing further shrinks max to 1 -> offset clamps down.
        app.resize(30, 16);
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
        // "aa bb cc N" wraps into 2 rows, so n messages occupy 3n+1 rows
        // (with the framing separators). With a line cap of 8, at most 2
        // messages fit (3*2+1 = 7).
        let mut app = App::with_caps(10, 5, 1000, 8);

        // Act
        for i in 0..10 {
            app.push_message(msg("u", &format!("aa bb cc {i}")));
        }

        // Assert: height stays under the cap and the oldest were dropped.
        assert_eq!(app.total_height(), 7);
        assert_eq!(app.messages().len(), 2);
        assert_eq!(app.messages().first().unwrap().text, "aa bb cc 8");
    }

    #[test]
    fn eviction_while_scrolled_up_keeps_offset_clamped() {
        // Arrange: exactly under the line cap, then scrolled to the top.
        let mut app = App::with_caps(10, 3, 1000, 11);
        for i in 0..3 {
            app.push_message(msg("u", &format!("aa bb cc {i}")));
        }
        assert_eq!(app.total_height(), 10);
        app.set_scroll_offset(0);
        assert!(!app.is_at_bottom());

        // Act: a fourth message forces eviction back down under the cap.
        app.push_message(msg("u", "aa bb cc 3"));

        // Assert: m0 evicted, height bounded, offset still valid and unmoved.
        assert_eq!(app.messages().first().unwrap().text, "aa bb cc 1");
        assert_eq!(app.total_height(), 10);
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
        app.select_channel(0);

        // Act
        app.push_message(chan_msg("srv", "#osu", "hi"));

        // Assert
        assert_eq!(app.channel_count(), 1);
        assert_eq!(app.messages().len(), 1);
        assert_eq!(app.active_channel(), Some(("srv", "#osu")));
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
        app.select_channel(0);

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
        app.select_channel(0);
        for i in 0..6 {
            app.push_message(chan_msg("srv", "#a", &format!("m{i}")));
        }
        app.scroll_page_up();
        let offset = app.scroll_offset();
        assert!(offset < app.max_offset());

        // Act
        app.select_channel(1);
        assert_eq!(app.active_channel().map(|(_, c)| c), Some("#b"));
        assert_eq!(app.messages().len(), 0);
        app.push_message(chan_msg("srv", "#b", "bee"));
        app.select_channel(0);

        // Assert: #a's history and scroll position are intact.
        assert_eq!(app.active_channel().map(|(_, c)| c), Some("#a"));
        assert_eq!(app.messages().len(), 6);
        assert_eq!(app.scroll_offset(), offset);
    }

    #[test]
    fn background_push_leaves_active_layout_stable() {
        // Arrange
        let mut app = App::new(40, 3);
        app.open_channel("srv", "#a");
        app.open_channel("srv", "#b");
        app.select_channel(0);
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
    fn tab_cycles_sidebar_messages_composer() {
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");

        // Fresh start: sidebar focused, no channel viewed.
        assert_eq!(app.focus(), Focus::Sidebar);
        assert_eq!(app.active_channel(), None);
        app.tab();
        assert_eq!(app.focus(), Focus::Messages);
        app.tab();
        assert_eq!(app.focus(), Focus::Composer);
        app.tab();
        assert_eq!(app.focus(), Focus::Sidebar);
    }

    #[test]
    fn startup_focuses_sidebar_with_cursor_on_first_row() {
        // Fresh start: no channel open -> cursor on the first row.
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        assert_eq!(app.focus(), Focus::Sidebar);
        assert_eq!(app.sidebar_cursor(), Some(0));
    }

    #[test]
    fn tab_into_sidebar_snaps_cursor_to_active_channel_row() {
        // Arrange: open both channels, view #b (row 2), focus ends on Composer.
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.open_channel("srv", "#b");
        app.select_channel(1); // view #b
        app.tab(); // Sidebar -> Messages
        app.tab(); // Messages -> Composer
        assert_eq!(app.focus(), Focus::Composer);

        // Act: tab back into the sidebar.
        app.tab();

        // Assert: the cursor sits on #b's row.
        assert_eq!(app.focus(), Focus::Sidebar);
        assert_eq!(app.sidebar_cursor(), Some(2));
    }

    #[test]
    fn sidebar_jk_moves_cursor_and_clamps() {
        // Arrange: one server with two channels -> 3 visible rows.
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.open_channel("srv", "#b");

        // Act / Assert (focus starts on the sidebar)
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
        app.tab(); // Sidebar -> Messages: the sidebar is no longer focused
        let before = app.sidebar_cursor();
        app.sidebar_down();
        assert_eq!(app.sidebar_cursor(), before);
    }

    #[test]
    fn sidebar_enter_on_server_row_toggles_collapse() {
        // Arrange: rows = [srv, #a, #b]
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.open_channel("srv", "#b");

        // Act: collapse, then expand (focus starts on the sidebar).
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
        app.sidebar_down();
        app.sidebar_down();

        // Act
        app.sidebar_enter();

        // Assert: view switched to #b, focus returned to the composer.
        assert_eq!(app.active_channel(), Some(("srv", "#b")));
        assert_eq!(app.messages().len(), 0);
        assert_eq!(app.focus(), Focus::Composer);
        assert_eq!(app.sidebar_cursor(), None);
    }

    // ----- message selection -----

    #[test]
    fn no_selection_when_channel_is_empty() {
        let mut app = App::new(40, 10);
        app.open_channel("srv", "#a");
        app.tab();
        assert_eq!(app.focus(), Focus::Messages);
        assert_eq!(app.selected(), None);
    }

    #[test]
    fn focusing_messages_selects_lowest_fully_visible() {
        // Arrange: 5 one-line messages in a 3-row viewport (total 11 rows,
        // max 8). Spans (framing row shifts everything down by one):
        // m0=(1) m1=(3) m2=(5) m3=(7) m4=(9); at the bottom (off=8) the only
        // fully-visible message is m4, so it is selected.
        let mut app = App::new(40, 3);
        app.open_channel("srv", "#a");
        app.select_channel(0);
        for i in 0..5 {
            app.push_message(chan_msg("srv", "#a", &format!("m{i}")));
        }
        assert_eq!(app.scroll_offset(), app.max_offset());

        // Act
        app.tab();

        // Assert
        assert_eq!(app.focus(), Focus::Messages);
        assert_eq!(app.selected(), Some(4));
        assert_eq!(app.selected_span(), Some((9, 1)));
    }

    #[test]
    fn select_prev_moves_up_and_reveals_with_minimal_scroll() {
        // Arrange: as above, selected m4 (span (9,1)), offset 8.
        let mut app = App::new(40, 3);
        app.open_channel("srv", "#a");
        app.select_channel(0);
        for i in 0..5 {
            app.push_message(chan_msg("srv", "#a", &format!("m{i}")));
        }
        app.tab();
        assert_eq!(app.selected(), Some(4));

        // Act / Assert: each step scrolls up just enough to reveal the top of
        // the newly selected message (offset lands on its span start).
        app.select_prev(); // -> m3 (span 7)
        assert_eq!(app.selected(), Some(3));
        assert_eq!(app.scroll_offset(), 7);
        app.select_prev(); // -> m2 (span 5)
        assert_eq!(app.selected(), Some(2));
        assert_eq!(app.scroll_offset(), 5);
        app.select_prev(); // -> m1
        assert_eq!(app.selected(), Some(1));
        assert_eq!(app.scroll_offset(), 3);
        app.select_prev(); // -> m0
        assert_eq!(app.selected(), Some(0));
        assert_eq!(app.scroll_offset(), 1);
        app.select_prev(); // clamped at oldest
        assert_eq!(app.selected(), Some(0));
    }

    #[test]
    fn select_next_moves_down_and_clamps_at_newest() {
        let mut app = App::new(40, 3);
        app.open_channel("srv", "#a");
        app.select_channel(0);
        for i in 0..5 {
            app.push_message(chan_msg("srv", "#a", &format!("m{i}")));
        }
        app.tab();
        for _ in 0..4 {
            app.select_prev();
        }
        assert_eq!(app.selected(), Some(0));

        app.select_next();
        assert_eq!(app.selected(), Some(1));
        app.select_next();
        assert_eq!(app.selected(), Some(2));
        // jump repeatedly past the end clamps at the newest, fully visible
        for _ in 0..10 {
            app.select_next();
        }
        assert_eq!(app.selected(), Some(4));
        let (start, height) = app.selected_span().unwrap();
        assert!(start >= app.scroll_offset());
        assert!(start + height <= app.scroll_offset() + 3);
    }

    #[test]
    fn follow_is_paused_while_a_message_is_selected() {
        // Arrange
        let mut app = App::new(40, 3);
        app.open_channel("srv", "#a");
        app.select_channel(0);
        for i in 0..4 {
            app.push_message(chan_msg("srv", "#a", &format!("m{i}")));
        }
        app.tab();
        let selected = app.selected().unwrap();
        let offset = app.scroll_offset();

        // Act: a new message arrives while a message is selected.
        app.push_message(chan_msg("srv", "#a", "m4"));

        // Assert: neither the viewport nor the selection was disturbed.
        assert_eq!(app.scroll_offset(), offset);
        assert_eq!(app.selected(), Some(selected));
    }

    #[test]
    fn selection_ignored_when_messages_not_focused() {
        // Arrange: selection changes are no-ops unless the message pane is focused.
        let mut app = App::new(40, 3);
        app.open_channel("srv", "#a");
        app.push_message(chan_msg("srv", "#a", "m0"));
        assert_eq!(app.focus(), Focus::Sidebar);

        // Act
        app.select_prev();
        app.select_next();

        // Assert
        assert_eq!(app.selected(), None);
    }
}
