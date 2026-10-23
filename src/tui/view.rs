//! Terminal-only view state. Session data and histories live in application/history.
use crate::application::{Session, SubmissionEffect};
use crate::core::{BufferId, BufferKind, MessageId, RoutedMessage, ServerId};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
const MAX_MESSAGES: usize = 5000;
use super::layout::{LayoutCache, LayoutLine};

/// At most this many matching commands fill the inline hint bar.
const MAX_HINT_ROWS: usize = 5;

/// The draft's leading command word without its `/`, ASCII-lowercased.
/// `None` when the draft (after trimming) does not start a slash command.
fn hint_word(input: &str) -> Option<String> {
    let rest = input.trim().strip_prefix('/')?;
    let word = rest.split_whitespace().next()?;
    Some(word.to_ascii_lowercase())
}

/// Table-order command suggestions whose names strictly extend `prefix`,
/// capped at `cap`. An exact match yields nothing: the hint bar only
/// suggests completions still left to type.
fn prefix_matches(
    table: &[(&'static str, &'static str)],
    prefix: &str,
    cap: usize,
) -> Vec<(&'static str, &'static str)> {
    table
        .iter()
        .filter(|(name, _)| name.starts_with(prefix) && name.len() > prefix.len())
        .take(cap)
        .copied()
        .collect()
}

/// The hint bar's items for `input`: [`prefix_matches`] over the command
/// table, but only while the composer is focused (the bar lives on the
/// composer, so it hides whenever the composer does).
fn hint_matches(input: &str, composer_focused: bool) -> Vec<(&'static str, &'static str)> {
    if !composer_focused {
        return Vec::new();
    }
    let Some(prefix) = hint_word(input) else {
        return Vec::new();
    };
    prefix_matches(&crate::command::COMMANDS, &prefix, MAX_HINT_ROWS)
}

pub const MAX_CACHED_LINES: usize = 50_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Messages,
    Composer,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRow {
    pub server: String,
    pub id: Option<BufferId>,
    pub kind: BufferKind,
}
impl SidebarRow {
    fn identity(&self) -> (ServerId, Option<BufferId>) {
        (
            ServerId::new(&self.server),
            if self.kind == BufferKind::Server {
                None
            } else {
                self.id
            },
        )
    }
}
#[derive(Default)]
struct ViewState {
    layout: LayoutCache,
    scroll_offset: usize,
    selected: Option<MessageId>,
    hovered: Option<MessageId>,
    viewed: bool,
}

pub struct App {
    pub session: Session,
    views: HashMap<BufferId, ViewState>,
    focus: Focus,
    sidebar_cursor: Option<usize>,
    sidebar_hovered: Option<usize>,
    sidebar_cursor_id: Option<(ServerId, Option<BufferId>)>,
    sidebar_hovered_id: Option<(ServerId, Option<BufferId>)>,
    sidebar_offset: usize,
    sidebar_height: usize,
    width: u16,
    viewport_height: u16,
    running: bool,
    connecting_dot_visible: bool,
    cache_budget: usize,
    /// The hint bar's highlight: the leading word it was last moved on
    /// (`join`, no slash) and the row index chosen then. A different word
    /// means a different item list, so the highlight reads back as row 0.
    hint_selection_word: String,
    hint_selected: usize,
}
impl Deref for App {
    type Target = Session;
    fn deref(&self) -> &Session {
        &self.session
    }
}
impl DerefMut for App {
    fn deref_mut(&mut self) -> &mut Session {
        &mut self.session
    }
}
impl App {
    pub fn new(width: u16, viewport_height: u16) -> Self {
        Self::with_caps(width, viewport_height, MAX_MESSAGES, MAX_CACHED_LINES)
    }
    fn with_caps(
        width: u16,
        viewport_height: u16,
        message_cap: usize,
        cache_budget: usize,
    ) -> Self {
        Self {
            session: Session::new(message_cap),
            views: HashMap::new(),
            focus: Focus::Sidebar,
            sidebar_cursor: Some(0),
            sidebar_hovered: None,
            sidebar_cursor_id: None,
            sidebar_hovered_id: None,
            sidebar_offset: 0,
            sidebar_height: usize::from(viewport_height),
            width,
            viewport_height,
            running: true,
            connecting_dot_visible: true,
            hint_selection_word: String::new(),
            hint_selected: 0,
            cache_budget,
        }
    }
    fn view(&self) -> Option<&ViewState> {
        self.session.active.and_then(|id| self.views.get(&id))
    }
    fn view_mut(&mut self) -> Option<&mut ViewState> {
        self.session.active.and_then(|id| self.views.get_mut(&id))
    }
    pub fn select_buffer(&mut self, index: usize) {
        if let Some(buffer) = self.session.buffers.get(index) {
            self.activate_buffer(buffer.id);
        }
    }
    pub fn activate_buffer(&mut self, id: BufferId) {
        if !self.session.buffer(id).is_some_and(|buffer| !buffer.hidden) {
            return;
        }
        self.session.select_buffer_id(id);
        let state = self.views.entry(id).or_default();
        let first = !state.viewed;
        state.viewed = true;
        state.hovered = None;
        self.sync_layout(first);
        self.reveal_sidebar_row(self.active_sidebar_row());
    }
    pub fn apply_submission_effect(&mut self, effect: SubmissionEffect) {
        if let SubmissionEffect::Activate(id) = effect {
            self.activate_buffer(id);
            self.focus = Focus::Composer;
            self.sidebar_cursor = None;
            self.sidebar_cursor_id = None;
        }
    }
    pub fn push_message(&mut self, message: RoutedMessage) {
        let was_empty = self.session.buffers.is_empty();
        let follow =
            self.is_at_bottom() && !(self.focus == Focus::Messages && self.selected().is_some());
        self.session.push_message(message);
        self.sync_layout(was_empty || follow);
    }
    /// Reconcile histories after application/network events without recomputing unchanged messages.
    pub fn sync_view(&mut self) {
        self.observe_hint_word();
        self.reconcile_sidebar();
        let follow =
            self.is_at_bottom() && !(self.focus == Focus::Messages && self.selected().is_some());
        self.sync_layout(follow);
    }
    fn sync_layout(&mut self, follow: bool) {
        let Some(id) = self.session.active else {
            return;
        };
        let messages = self.session.history.messages(id);
        let view = self.views.entry(id).or_default();
        let follow = follow || !view.viewed;
        view.viewed = true;
        let anchor = view.layout.anchor(view.scroll_offset);
        view.layout.sync(messages, self.width);
        if view
            .selected
            .is_some_and(|id| !messages.iter().any(|m| m.id == id))
        {
            view.selected = messages.iter().find(|m| m.selectable()).map(|m| m.id);
        }
        if view
            .hovered
            .is_some_and(|id| !messages.iter().any(|m| m.id == id))
        {
            view.hovered = None;
        }
        let max = view
            .layout
            .total_height()
            .saturating_sub(usize::from(self.viewport_height));
        view.scroll_offset = if follow {
            max
        } else {
            view.layout.resolve_anchor(anchor).unwrap_or(0).min(max)
        };
        view.layout.prune(
            view.scroll_offset,
            usize::from(self.viewport_height),
            self.cache_budget,
        );
        if self.width > 0
            && self.viewport_height > 0
            && view.scroll_offset == max
            && self
                .session
                .buffer(id)
                .is_some_and(|buffer| !buffer.hidden && matches!(buffer.kind, BufferKind::Query(_)))
        {
            self.session.mark_read(id);
        }
    }
    pub fn tick(&mut self, elapsed: std::time::Duration) {
        self.connecting_dot_visible = elapsed.as_millis() / 500 % 2 == 0;
        self.sync_view();
    }
    pub fn connecting_dot_visible(&self) -> bool {
        self.connecting_dot_visible
    }
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.viewport_height)
    }
    pub fn resize(&mut self, width: u16, height: u16) {
        let follow = self.is_at_bottom();
        self.width = width;
        self.viewport_height = height;
        self.sync_layout(follow);
    }
    pub fn quit(&mut self) {
        self.running = false;
    }
    pub fn is_running(&self) -> bool {
        self.running
    }
    pub fn total_height(&self) -> usize {
        self.view().map_or(0, |s| s.layout.total_height())
    }
    pub fn max_offset(&self) -> usize {
        self.total_height()
            .saturating_sub(usize::from(self.viewport_height))
    }
    pub fn scroll_offset(&self) -> usize {
        self.view().map_or(0, |s| s.scroll_offset)
    }
    pub fn is_at_bottom(&self) -> bool {
        self.scroll_offset() >= self.max_offset()
    }
    pub fn set_scroll_offset(&mut self, offset: usize) {
        let max = self.max_offset();
        if let Some(s) = self.view_mut() {
            s.scroll_offset = offset.min(max);
        }
    }
    pub fn scroll_page_up(&mut self) {
        self.set_scroll_offset(
            self.scroll_offset()
                .saturating_sub(usize::from((self.viewport_height / 3).max(1))),
        );
    }
    pub fn scroll_page_down(&mut self) {
        self.set_scroll_offset(
            self.scroll_offset()
                .saturating_add(usize::from((self.viewport_height / 3).max(1))),
        );
    }
    pub fn scroll_lines(&mut self, delta: i32) {
        self.set_scroll_offset(self.scroll_offset().saturating_add_signed(delta as isize));
    }
    pub fn visible_lines(&self) -> Vec<LayoutLine> {
        self.view().map_or_else(Vec::new, |v| {
            v.layout.visible_lines(
                self.messages(),
                v.scroll_offset,
                usize::from(self.viewport_height),
            )
        })
    }
    pub fn selected(&self) -> Option<usize> {
        let id = self.view()?.selected?;
        self.messages().iter().position(|m| m.id == id)
    }
    pub fn hovered(&self) -> Option<usize> {
        let id = self.view()?.hovered?;
        self.messages().iter().position(|m| m.id == id)
    }
    pub fn selected_span(&self) -> Option<(usize, usize)> {
        self.view()?.layout.span(self.view()?.selected?)
    }
    pub fn hovered_span(&self) -> Option<(usize, usize)> {
        self.view()?.layout.span(self.view()?.hovered?)
    }
    pub fn set_hover_message(&mut self, index: Option<usize>) {
        let id = index
            .and_then(|i| self.messages().get(i))
            .filter(|m| m.selectable())
            .map(|m| m.id);
        if let Some(s) = self.view_mut() {
            s.hovered = id;
        }
    }
    pub fn message_at_row(&self, row: u16) -> Option<usize> {
        self.view()?
            .layout
            .message_at(self.scroll_offset() + usize::from(row))
    }
    pub fn click_message(&mut self, index: usize) {
        let id = self
            .messages()
            .get(index)
            .filter(|m| m.selectable())
            .map(|m| m.id);
        if let Some(s) = self.view_mut() {
            s.selected = id;
        }
        self.focus = Focus::Messages;
    }
    fn ensure_selection(&mut self) {
        if self.selected().is_some() {
            return;
        }
        let off = self.scroll_offset();
        let end = off + usize::from(self.viewport_height);
        let id = self
            .view()
            .and_then(|v| v.layout.last_fully_visible(off, end))
            .or_else(|| self.messages().back().map(|m| m.id))
            .and_then(|id| {
                let messages = self.messages();
                let position = messages.iter().position(|m| m.id == id)?;
                messages
                    .iter()
                    .take(position + 1)
                    .rev()
                    .find(|m| m.selectable())
                    .map(|m| m.id)
            });
        if let Some(s) = self.view_mut() {
            s.selected = id;
        }
    }
    fn reveal_selection(&mut self) {
        let Some((start, height)) = self.selected_span() else {
            return;
        };
        let top = start.saturating_sub(1);
        let bottom = start + height;
        let off = self.scroll_offset();
        if top < off {
            self.set_scroll_offset(top);
        } else if bottom >= off + usize::from(self.viewport_height) {
            self.set_scroll_offset((bottom + 1).saturating_sub(usize::from(self.viewport_height)));
        }
    }
    pub fn select_next(&mut self) {
        if self.focus != Focus::Messages {
            return;
        }
        self.ensure_selection();
        let next_id = if let Some(i) = self.selected() {
            self.messages()
                .iter()
                .skip(i + 1)
                .find(|m| m.selectable())
                .map(|m| m.id)
        } else {
            None
        };
        if let Some(id) = next_id {
            self.view_mut().unwrap().selected = Some(id);
        }
        self.reveal_selection();
    }
    pub fn select_prev(&mut self) {
        if self.focus != Focus::Messages {
            return;
        }
        self.ensure_selection();
        let prev_id = if let Some(i) = self.selected() {
            self.messages()
                .iter()
                .take(i)
                .rev()
                .find(|m| m.selectable())
                .map(|m| m.id)
        } else {
            None
        };
        if let Some(id) = prev_id {
            self.view_mut().unwrap().selected = Some(id);
        }
        self.reveal_selection();
    }
    pub fn focus(&self) -> Focus {
        self.focus
    }
    pub fn focus_messages(&mut self) {
        self.focus = Focus::Messages;
    }
    pub fn focus_composer(&mut self) {
        self.focus = Focus::Composer;
    }
    pub fn tab(&mut self) {
        self.focus = match self.focus {
            Focus::Composer => {
                if self.session.buffers.is_empty() {
                    Focus::Composer
                } else {
                    self.set_sidebar_cursor(self.active_sidebar_row());
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

    /// The hint bar's items for the current draft: `(name, catalog-key)`
    /// pairs in table order, capped at [`MAX_HINT_ROWS`]. Keys are resolved
    /// to description text at render time. Hidden unless the composer is
    /// focused on an opened buffer and the draft's leading word strictly
    /// extends at least one command name.
    pub fn command_hints(&self) -> Vec<(&'static str, &'static str)> {
        hint_matches(
            self.input(),
            self.focus == Focus::Composer && self.active_buffer().is_some(),
        )
    }

    pub fn is_hint_active(&self) -> bool {
        !self.command_hints().is_empty()
    }

    /// Rows the hint bar reserves between the message pane and the composer:
    /// one separator rule plus one row per visible item. Zero while hidden.
    pub fn hint_rows(&self) -> u16 {
        let items = self.command_hints();
        if items.is_empty() {
            return 0;
        }
        u16::try_from(items.len() + 1).unwrap_or(u16::MAX)
    }

    /// The highlighted hint row. Reading resets to the first row whenever
    /// the item list changed (a different leading word), and clamps to the
    /// last row so a shrunken list can never point past its end.
    pub fn hint_selected(&self) -> usize {
        let items = self.command_hints();
        if items.is_empty() {
            return 0;
        }
        if self.hint_selection_word == hint_word(self.input()).unwrap_or_default() {
            self.hint_selected.min(items.len() - 1)
        } else {
            0
        }
    }

    pub fn hint_up(&mut self) {
        self.move_hint_selection(false);
    }

    pub fn hint_down(&mut self) {
        self.move_hint_selection(true);
    }

    /// Clamp-move the hint highlight without wrapping; a no-op while the
    /// bar is hidden.
    fn move_hint_selection(&mut self, down: bool) {
        let items = self.command_hints();
        if items.is_empty() {
            return;
        }
        let word = hint_word(self.input()).unwrap_or_default();
        let current = if self.hint_selection_word == word {
            self.hint_selected.min(items.len() - 1)
        } else {
            0
        };
        let next = if down {
            (current + 1).min(items.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        self.hint_selection_word = word;
        self.hint_selected = next;
    }

    /// Complete the highlighted command into the draft: the leading word
    /// becomes `/name ` (trailing space) with the cursor right after it,
    /// which leaves the exact name and deactivates the bar. The whitespace
    /// run after the old word is swallowed so completion never doubles the
    /// separator the user already typed.
    pub fn complete_hint(&mut self) {
        let selected = self.hint_selected();
        let Some(&(name, _)) = self.command_hints().get(selected) else {
            return;
        };
        let text = self.input().to_string();
        let chars: Vec<char> = text.chars().collect();
        let start = chars.iter().position(|c| !c.is_whitespace()).unwrap_or(0);
        let word_end = (start..chars.len())
            .find(|&i| chars[i].is_whitespace())
            .unwrap_or(chars.len());
        let rest = (word_end..chars.len())
            .find(|&i| !chars[i].is_whitespace())
            .unwrap_or(chars.len());
        let replacement = format!("/{name} ");
        let mut composed = String::with_capacity(text.len() + replacement.len());
        composed.extend(chars[..start].iter());
        composed.push_str(&replacement);
        composed.extend(chars[rest..].iter());
        self.restore_input_at(composed, start + replacement.chars().count());
    }

    /// Adopt the draft's leading word as the highlight's home: a different
    /// word means a different item list, so the highlight restarts at the
    /// first row. Reading it back also resets on mismatch as a fallback for
    /// draft edits that bypass these wrappers (typing, submissions).
    fn observe_hint_word(&mut self) {
        let word = hint_word(self.input()).unwrap_or_default();
        if self.hint_selection_word != word {
            self.hint_selection_word = word;
            self.hint_selected = 0;
        }
    }

    /// Session draft restoration, re-based so a restored draft resets the
    /// hint highlight the same moment the item list changes.
    pub fn restore_input_at(&mut self, text: String, cursor: usize) {
        self.session.restore_input_at(text, cursor);
        self.observe_hint_word();
    }

    /// [`Self::restore_input_at`] for the cursor-at-end convenience form.
    pub fn restore_input(&mut self, text: String) {
        self.session.restore_input(text);
        self.observe_hint_word();
    }
    pub fn sidebar_rows(&self) -> Vec<SidebarRow> {
        let mut seen = Vec::new();
        let mut rows = Vec::new();
        for buffer in &self.session.buffers {
            if !seen.contains(&buffer.server) {
                seen.push(buffer.server.clone());
                let siblings: Vec<_> = self
                    .session
                    .buffers
                    .iter()
                    .filter(|candidate| candidate.server == buffer.server)
                    .collect();
                rows.push(SidebarRow {
                    server: buffer.server_label.clone(),
                    id: siblings
                        .iter()
                        .find(|candidate| candidate.kind == BufferKind::Server)
                        .map(|candidate| candidate.id),
                    kind: BufferKind::Server,
                });
                for query in [false, true] {
                    for sibling in &siblings {
                        if !sibling.hidden
                            && match sibling.kind {
                                BufferKind::Channel(_) => !query,
                                BufferKind::Query(_) => query,
                                BufferKind::Server => false,
                            }
                        {
                            rows.push(SidebarRow {
                                server: buffer.server_label.clone(),
                                id: Some(sibling.id),
                                kind: sibling.kind.clone(),
                            });
                        }
                    }
                }
            }
        }
        rows
    }
    fn active_sidebar_row(&self) -> usize {
        let Some(b) = self.active_buffer() else {
            return 0;
        };
        self.sidebar_rows()
            .iter()
            .position(|row| row.id == Some(b.id))
            .unwrap_or(0)
    }
    pub fn sidebar_cursor(&self) -> Option<usize> {
        self.sidebar_cursor
    }
    pub fn sidebar_hovered(&self) -> Option<usize> {
        self.sidebar_hovered
    }
    pub fn set_hover_sidebar(&mut self, row: Option<usize>) {
        self.sidebar_hovered = row.filter(|i| *i < self.sidebar_rows().len());
        self.sidebar_hovered_id = self
            .sidebar_hovered
            .and_then(|index| self.sidebar_rows().get(index).map(SidebarRow::identity));
    }
    pub fn sidebar_down(&mut self) {
        self.move_sidebar_cursor(true);
    }
    pub fn sidebar_up(&mut self) {
        self.move_sidebar_cursor(false);
    }
    fn move_sidebar_cursor(&mut self, down: bool) {
        self.reconcile_sidebar();
        let len = self.sidebar_rows().len();
        if self.focus != Focus::Sidebar || len == 0 {
            return;
        }
        let c = self.sidebar_cursor.unwrap_or(0);
        let next = if down {
            (c + 1).min(len - 1)
        } else {
            c.saturating_sub(1)
        };
        self.set_sidebar_cursor(next);
    }
    pub fn sidebar_enter(&mut self) {
        self.reconcile_sidebar();
        if self.focus == Focus::Sidebar
            && let Some(i) = self.sidebar_cursor
        {
            self.click_sidebar_row(i);
        }
    }
    pub fn click_sidebar_row(&mut self, index: usize) {
        let Some(row) = self.sidebar_rows().get(index).cloned() else {
            return;
        };
        let id = row
            .id
            .unwrap_or_else(|| self.session.open_server(&row.server));
        self.apply_submission_effect(SubmissionEffect::Activate(id));
    }
    fn set_sidebar_cursor(&mut self, index: usize) {
        if let Some(row) = self.sidebar_rows().get(index) {
            self.sidebar_cursor = Some(index);
            self.sidebar_cursor_id = Some(row.identity());
            self.reveal_sidebar_row(index);
        }
    }
    fn reconcile_sidebar(&mut self) {
        let rows = self.sidebar_rows();
        let previous = self.sidebar_cursor;
        if let Some(index) = previous {
            self.sidebar_cursor = (!rows.is_empty()).then(|| {
                self.sidebar_cursor_id
                    .as_ref()
                    .and_then(|identity| rows.iter().position(|row| &row.identity() == identity))
                    .unwrap_or(index.min(rows.len().saturating_sub(1)))
            });
            self.sidebar_cursor_id = self.sidebar_cursor.map(|index| rows[index].identity());
        }
        self.sidebar_hovered = self
            .sidebar_hovered_id
            .as_ref()
            .and_then(|identity| rows.iter().position(|row| &row.identity() == identity));
        self.sidebar_offset = self
            .sidebar_offset
            .min(rows.len().saturating_sub(self.sidebar_height));
        if self.sidebar_cursor != previous
            && let Some(index) = self.sidebar_cursor
        {
            self.reveal_sidebar_row(index);
        }
    }
    fn reveal_sidebar_row(&mut self, index: usize) {
        if self.sidebar_height == 0 {
            return;
        }
        if index < self.sidebar_offset {
            self.sidebar_offset = index;
        } else if index >= self.sidebar_offset + self.sidebar_height {
            self.sidebar_offset = index + 1 - self.sidebar_height;
        }
        self.sidebar_offset = self.sidebar_offset.min(
            self.sidebar_rows()
                .len()
                .saturating_sub(self.sidebar_height),
        );
    }
    pub fn set_sidebar_height(&mut self, height: u16) {
        let height = usize::from(height);
        if height != self.sidebar_height {
            self.sidebar_height = height;
            self.reconcile_sidebar();
            let index = if self.focus == Focus::Sidebar {
                self.sidebar_cursor.unwrap_or(0)
            } else {
                self.active_sidebar_row()
            };
            self.reveal_sidebar_row(index);
        }
    }
    pub fn sidebar_scroll_offset(&self) -> usize {
        self.sidebar_offset
    }
    pub fn scroll_sidebar(&mut self, delta: i32) {
        self.sidebar_offset = self
            .sidebar_offset
            .saturating_add_signed(delta as isize)
            .min(
                self.sidebar_rows()
                    .len()
                    .saturating_sub(self.sidebar_height),
            );
    }
}

#[cfg(test)]
mod tests;
