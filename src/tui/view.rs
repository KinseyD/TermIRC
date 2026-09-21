//! Terminal-only view state. Session data and histories live in application/history.
use crate::application::{Session, SubmissionEffect};
use crate::core::{BufferId, BufferKind, MessageId, RoutedMessage, ServerId};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
const MAX_MESSAGES: usize = 5000;
use super::layout::{LayoutCache, LayoutLine};

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
            view.selected = messages.front().map(|m| m.id);
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
        self.connecting_dot_visible = (elapsed.as_millis() / 500).is_multiple_of(2);
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
        let id = index.and_then(|i| self.messages().get(i)).map(|m| m.id);
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
        let id = self.messages().get(index).map(|m| m.id);
        if let Some(id) = id
            && let Some(s) = self.view_mut()
        {
            s.selected = Some(id);
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
            .or_else(|| self.messages().back().map(|m| m.id));
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
        if let Some(i) = self.selected() {
            let next = (i + 1).min(self.messages().len() - 1);
            let id = self.messages()[next].id;
            self.view_mut().unwrap().selected = Some(id);
        }
        self.reveal_selection();
    }
    pub fn select_prev(&mut self) {
        if self.focus != Focus::Messages {
            return;
        }
        self.ensure_selection();
        if let Some(i) = self.selected() {
            let id = self.messages()[i.saturating_sub(1)].id;
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
