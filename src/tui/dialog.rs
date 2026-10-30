//! The in-TUI server configuration wizard: a modal opened on first run
//! (no `config.toml` yet, or one without servers) and by `/config`.
//!
//! The wizard is a two-pane master-detail form: the left pane lists every
//! configured server plus a "new server" pseudo-row, the right pane edits
//! the selection across three sequential pages (connection, identity,
//! channels). Fields edit through an explicit edit mode, like the chat
//! composer: `Enter` opens the focused text field (a toggle flips in
//! place), typing lands only while it is open, `Enter`/`Tab` commit, and
//! `Esc` discards. Editing is WYSIWYG: committing or flipping asks the
//! caller to persist immediately, so what the screen shows is always the
//! saved configuration — there is no save action and no unsaved marker.
//! The dialog owns only form state and stays free of terminal I/O: `key`
//! maps a crossterm key code to a state change, the renderer reads the
//! fields, and `collect` turns them into a `ServerConfig` (validating
//! with the same rules `Config::parse` enforces). Saving is orchestrated
//! by `tui::events`, which knows the config path and the connection map.

use crate::config::{Config, ServerConfig};
use crate::core::Draft;
use ratatui::crossterm::event::KeyCode;

/// Which pane of the two-column wizard holds the keyboard focus. Tab
/// and the arrows cycle List ↔ Form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The left server list.
    List,
    /// The right, paged detail form.
    Form,
}

/// One of the three sequential pages of the detail form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// Label, host, port, TLS.
    Connection,
    /// Nickname, username, server password, SASL pair.
    Identity,
    /// Pre-join channels and query nicknames.
    Channels,
}

impl Page {
    /// All pages in display order, for the tab row.
    pub const ALL: [Page; 3] = [Page::Connection, Page::Identity, Page::Channels];

    /// The slice of `fields` this page shows.
    pub fn fields(self) -> std::ops::Range<usize> {
        match self {
            Page::Connection => F_LABEL..F_NICKNAME,
            Page::Identity => F_NICKNAME..F_CHANNELS,
            Page::Channels => F_CHANNELS..F_QUERIES + 1,
        }
    }

    /// The i18n key of the page's name in the tab row.
    pub fn tab_key(self) -> &'static str {
        match self {
            Page::Connection => "dlg.page_connection",
            Page::Identity => "dlg.page_identity",
            Page::Channels => "dlg.page_channels",
        }
    }

    /// The i18n key of the dim helper row shown under the fields.
    pub fn help_key(self) -> &'static str {
        match self {
            Page::Connection => "dlg.help_connection",
            Page::Identity => "dlg.help_identity",
            Page::Channels => "dlg.help_channels",
        }
    }

    fn next(self) -> Option<Page> {
        match self {
            Page::Connection => Some(Page::Identity),
            Page::Identity => Some(Page::Channels),
            Page::Channels => None,
        }
    }

    fn prev(self) -> Option<Page> {
        match self {
            Page::Connection => None,
            Page::Identity => Some(Page::Connection),
            Page::Channels => Some(Page::Identity),
        }
    }
}

// Field indices inside `fields`, in display order across the pages. The
// per-page slices are expressed through these so the layout has one home.
/// Page::Connection: the sidebar label of the entry.
pub const F_LABEL: usize = 0;
/// Page::Connection: the IRC server host.
pub const F_HOST: usize = 1;
/// Page::Connection: the TCP port.
pub const F_PORT: usize = 2;
/// Page::Connection: the TLS toggle.
pub const F_TLS: usize = 3;
/// Page::Identity: the nickname.
pub const F_NICKNAME: usize = 4;
/// Page::Identity: the username (blank defaults to the nickname).
pub const F_USERNAME: usize = 5;
/// Page::Identity: the server (PASS) password.
pub const F_PASSWORD: usize = 6;
/// Page::Identity: the SASL username, paired with the SASL password.
pub const F_SASL_USER: usize = 7;
/// Page::Identity: the SASL password.
pub const F_SASL_PASS: usize = 8;
/// Page::Channels: comma-separated channels to join at startup.
pub const F_CHANNELS: usize = 9;
/// Page::Channels: comma-separated nicknames to query at startup.
pub const F_QUERIES: usize = 10;

/// How each field edits: typed text or a boolean toggled in place.
/// Port, channel and query syntax are ordinary text here;
/// [`ConfigDialog::collect`] validates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Free single-line text.
    Text,
    /// A boolean toggled in place instead of typed.
    Toggle,
}

/// One editable form row.
#[derive(Debug, Clone)]
pub struct Field {
    /// The i18n key of the row's label.
    pub label_key: &'static str,
    pub kind: FieldKind,
    /// The typed value (ignored by [`FieldKind::Toggle`]).
    pub text: Draft,
    /// The toggle's value (only read by [`FieldKind::Toggle`]).
    pub flag: bool,
}

impl Field {
    fn text(label_key: &'static str, value: &str) -> Self {
        Self {
            label_key,
            kind: FieldKind::Text,
            // `restore` also places the cursor at the end of the value.
            text: Draft::default().restored(value),
            flag: false,
        }
    }

    fn toggle(label_key: &'static str, flag: bool) -> Self {
        Self {
            label_key,
            kind: FieldKind::Toggle,
            text: Draft::default(),
            flag,
        }
    }
}

/// A blank form with sane defaults for a first connection.
fn default_fields() -> Vec<Field> {
    vec![
        Field::text("dlg.field_label", "libera"),
        Field::text("dlg.field_host", ""),
        Field::text("dlg.field_port", "6697"),
        Field::toggle("dlg.field_tls", true),
        Field::text("dlg.field_nickname", ""),
        Field::text("dlg.field_username", ""),
        Field::text("dlg.field_password", ""),
        Field::text("dlg.field_sasl_user", ""),
        Field::text("dlg.field_sasl_pass", ""),
        Field::text("dlg.field_channels", ""),
        Field::text("dlg.field_queries", ""),
    ]
}

/// The form prefilled from one configured entry, every page included.
fn fields_from(name: &str, server: &ServerConfig) -> Vec<Field> {
    vec![
        Field::text("dlg.field_label", name),
        Field::text("dlg.field_host", &server.server),
        Field::text("dlg.field_port", &server.port.to_string()),
        Field::toggle("dlg.field_tls", server.use_tls),
        Field::text("dlg.field_nickname", &server.nickname),
        Field::text("dlg.field_username", &server.username),
        Field::text("dlg.field_password", &server.password),
        Field::text(
            "dlg.field_sasl_user",
            server.sasl_username.as_deref().unwrap_or_default(),
        ),
        Field::text(
            "dlg.field_sasl_pass",
            server.sasl_password.as_deref().unwrap_or_default(),
        ),
        Field::text("dlg.field_channels", &server.channels.join(", ")),
        Field::text("dlg.field_queries", &server.queries.join(", ")),
    ]
}

/// How long the first `d` press stays armed before the pending delete
/// cancels itself: long enough for a deliberate second press, short
/// enough that a stray `d` does not linger as a loaded trigger.
const DELETE_ARM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The modal wizard: the server list, the paged form for the selection,
/// and the validation error of the last failed save.
#[derive(Debug, Clone)]
pub struct ConfigDialog {
    /// Every configured entry in file order, snapshotted at open time:
    /// the dialog is modal, so the config cannot change underneath it.
    pub entries: Vec<(String, ServerConfig)>,
    /// The selected list row; `entries.len()` is the "new server" row.
    pub list_cursor: usize,
    /// The focused pane.
    pub pane: Pane,
    /// The form page shown (and edited) on the right.
    pub page: Page,
    /// All fields of all pages, laid out by the `F_*` constants.
    pub fields: Vec<Field>,
    /// The focused field, always inside `page.fields()`.
    pub cursor: usize,
    /// The i18n key of the failed `collect` validation, if any.
    pub error: Option<&'static str>,
    /// The configured server label the form is editing, when any: the
    /// save path replaces the entry (and its file section) when a
    /// commit changes the label.
    pub selected: Option<String>,
    /// Whether the focused text field is open for editing (entered
    /// with `Enter`, left through `Enter`/`Tab`/`Esc`). Implies
    /// [`Pane::Form`]: every pane switch from the form closes it.
    pub editing: bool,
    /// The focused field's value as it was when editing began, restored
    /// by an `Esc` discard.
    edit_backup: String,
    /// The deadline of a pending delete confirmation: the first `d`
    /// set it, a second `d` before the deadline confirms, anything
    /// else (including the deadline passing) cancels it. Public so
    /// tests can fabricate already-expired arms; `key` re-checks
    /// expiry with `Instant::now()` itself — the dialog stays pure
    /// state with no clock parameter on its key routing.
    pub delete_armed: Option<std::time::Instant>,
}

/// What one routed key press did to the dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogOutcome {
    /// Not consumed (or a pure edit): the dialog stays open.
    Edited,
    /// Close the dialog (Esc outside edit mode).
    Cancel,
    /// A commit (Enter/Tab) or toggle flip: persist the form now. The
    /// dialog stays open — what the user sees is the saved state.
    Save,
    /// A confirmed delete (the second `d`): remove this server's entry
    /// from the file, memory, and the list. The dialog stays open.
    Delete(String),
}

impl ConfigDialog {
    /// A wizard over no servers: a blank new entry, focus in the form.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            list_cursor: 0,
            pane: Pane::Form,
            page: Page::Connection,
            fields: default_fields(),
            cursor: F_LABEL,
            error: None,
            selected: None,
            editing: false,
            edit_backup: String::new(),
            delete_armed: None,
        }
    }

    /// A wizard over the config's servers, in file order. With at least
    /// one entry the list is focused on the first (its form prefilled as
    /// a preview); a serverless config lands directly in a blank form.
    pub fn from_config(config: &Config) -> Self {
        let mut dialog = Self::new();
        dialog.entries = config
            .servers
            .iter()
            .map(|(name, server)| (name.clone(), server.clone()))
            .collect();
        if dialog.entries.is_empty() {
            return dialog;
        }
        dialog.pane = Pane::List;
        dialog.load_selection();
        dialog
    }

    /// Whether the list cursor sits on the "new server" pseudo-row.
    pub fn on_new_row(&self) -> bool {
        self.list_cursor >= self.entries.len()
    }

    /// Replace the form with the entry under the list cursor: an
    /// existing entry prefills every page, the pseudo-row starts from
    /// defaults. The preview restarts from the first page.
    fn load_selection(&mut self) {
        match self.entries.get(self.list_cursor) {
            Some((name, server)) => {
                self.selected = Some(name.clone());
                self.fields = fields_from(name, server);
            }
            None => {
                self.selected = None;
                self.fields = default_fields();
            }
        }
        self.page = Page::Connection;
        self.cursor = F_LABEL;
        // A swapped form cannot stay open for editing: the edit would
        // apply to another entry's field.
        self.editing = false;
        self.edit_backup.clear();
        self.error = None;
    }

    /// The current text of the field with this label key (empty for
    /// toggles and unknown keys).
    pub fn field_text(&self, label_key: &str) -> &str {
        self.fields
            .iter()
            .find(|field| field.label_key == label_key)
            .map(|field| field.text.text())
            .unwrap_or_default()
    }

    /// Replace the text of the field with this label key, placing its
    /// cursor at the end.
    pub fn set_field_text(&mut self, label_key: &str, value: &str) {
        if let Some(field) = self
            .fields
            .iter_mut()
            .find(|field| field.label_key == label_key)
        {
            field.text.restore(value.to_string(), value.chars().count());
        }
    }

    /// Route one key press; the dialog consumes every key while open.
    /// PageUp/PageDown cycle the form's pages; `alt` carries the Alt
    /// modifier, whose keys are inert while the dialog is open.
    pub fn key(&mut self, code: KeyCode, ctrl: bool, alt: bool) -> DialogOutcome {
        if ctrl {
            // Ctrl+C closes the dialog instead of quitting: an accidental
            // interrupt must not take the whole client down.
            return if matches!(code, KeyCode::Char('c')) {
                DialogOutcome::Cancel
            } else {
                DialogOutcome::Edited
            };
        }
        // Lazily expire a pending delete so it can never confirm after
        // its deadline, even without a `tick` in between.
        self.delete_armed = self
            .delete_armed
            .take_if(|deadline| *deadline > std::time::Instant::now());
        // A pending delete is a modal confirm: the second `d` confirms,
        // every other key — Esc included — cancels the arm instead of
        // doing what it usually would (Esc must not close the dialog),
        // and nothing reaches the panes while armed.
        if self.delete_armed.is_some() {
            self.delete_armed = None;
            return match code {
                KeyCode::Char('d') => match self.entries.get(self.list_cursor) {
                    Some((name, _)) => DialogOutcome::Delete(name.clone()),
                    // Unreachable in practice — arming requires an
                    // existing row — but a vanished entry must not
                    // delete anything.
                    None => DialogOutcome::Edited,
                },
                _ => DialogOutcome::Edited,
            };
        }
        if matches!(code, KeyCode::Esc) {
            // While a field is open for editing, Esc discards the edit;
            // only outside edit mode does Esc close the dialog.
            if self.editing {
                self.discard_edit();
                return DialogOutcome::Edited;
            }
            return DialogOutcome::Cancel;
        }
        if alt {
            // Alt-modified keys are inert: swallowed, neither typed nor
            // routed to the panes.
            return DialogOutcome::Edited;
        }
        // PageUp/PageDown turn pages from either pane — but never while
        // a field is open for editing, whose only exits are Enter/Tab/
        // Esc (a page turn would retarget the open edit at another
        // field).
        if !self.editing {
            match code {
                KeyCode::PageUp => self.turn_page(false),
                KeyCode::PageDown => self.turn_page(true),
                _ => {}
            }
        }
        match self.pane {
            Pane::List => self.list_key(code),
            Pane::Form => self.form_key(code),
        }
    }

    /// Observe the passage of time: a pending delete whose deadline
    /// has passed is cancelled here, so the footer hint reverts without
    /// waiting for a key press. The event loop calls this once per
    /// iteration.
    pub fn tick(&mut self, now: std::time::Instant) {
        self.delete_armed = self.delete_armed.take_if(|deadline| *deadline > now);
    }

    /// Drop one entry from the list after its server was deleted: the
    /// cursor clamps onto a surviving row (or the pseudo-row), the form
    /// previews whatever lands under it, and any arm or error clears.
    pub fn remove_entry(&mut self, name: &str) {
        if let Some(index) = self.entries.iter().position(|(label, _)| label == name) {
            self.entries.remove(index);
            self.list_cursor = self.list_cursor.min(self.entries.len());
        }
        self.delete_armed = None;
        self.load_selection();
    }

    /// Keys while the server list is focused. Typing never reaches the
    /// form: only navigation, the new-entry shortcut, and pane cycling
    /// are meaningful here.
    fn list_key(&mut self, code: KeyCode) -> DialogOutcome {
        match code {
            KeyCode::Up => {
                self.list_cursor = self.list_cursor.saturating_sub(1);
                self.load_selection();
            }
            KeyCode::Down if !self.on_new_row() => {
                self.list_cursor += 1;
                self.load_selection();
            }
            // Enter edits the previewed entry; on the pseudo-row it
            // starts a new one.
            KeyCode::Enter => {
                self.load_selection();
                self.pane = Pane::Form;
            }
            // `n` jumps straight to a new entry from anywhere in the list.
            KeyCode::Char('n') => {
                self.list_cursor = self.entries.len();
                self.load_selection();
                self.pane = Pane::Form;
            }
            // → focuses the form (omp's arrows switch hub panes); there
            // is no pane left of the sidebar, so ← stays a no-op.
            KeyCode::Right | KeyCode::Tab => self.pane = Pane::Form,
            // BackTab wraps the cycle the other way: the form again,
            // mirroring ←/→'s two-pane semantics.
            KeyCode::BackTab => self.pane = Pane::Form,
            // `d` arms deletion of the selected entry — the second `d`
            // (routed through `key`'s arm gate) confirms. On the
            // pseudo-row there is nothing to delete, so it stays inert.
            KeyCode::Char('d') if !self.on_new_row() => {
                self.delete_armed = Some(std::time::Instant::now() + DELETE_ARM_TIMEOUT);
            }
            _ => {}
        }
        DialogOutcome::Edited
    }

    /// Keys while the paged form is focused. Idle: field navigation,
    /// pane switching, and `Enter` opening the focused field. While a
    /// field is open: [`ConfigDialog::editing_key`].
    fn form_key(&mut self, code: KeyCode) -> DialogOutcome {
        if self.editing {
            return self.editing_key(code);
        }
        let range = self.page.fields();
        match code {
            KeyCode::Tab => self.tab_forward(),
            // ← always returns to the sidebar (omp's hub arrows switch
            // panes); the text caret moves only inside edit mode.
            KeyCode::Left => self.pane = Pane::List,
            KeyCode::BackTab => self.tab_backward(),
            // Up/Down stay within the current page: pages are turned
            // explicitly (Tab at the edge, PgUp/PgDn from any pane).
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1).max(range.start),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(range.end - 1),
            KeyCode::Enter => match self.fields.get(self.cursor).map(|field| field.kind) {
                // A toggle flips in place — and a flip is a commit, so
                // it persists immediately; a text field opens for
                // editing instead of advancing the cursor.
                Some(FieldKind::Toggle) => {
                    self.fields[self.cursor].flag = !self.fields[self.cursor].flag;
                    return DialogOutcome::Save;
                }
                Some(FieldKind::Text) => self.begin_edit(),
                None => {}
            },
            // Space still flips (and saves) a toggle without opening
            // it; every other key is inert until Enter opens the field.
            KeyCode::Char(' ') if self.fields[self.cursor].kind == FieldKind::Toggle => {
                self.fields[self.cursor].flag = !self.fields[self.cursor].flag;
                return DialogOutcome::Save;
            }
            _ => {}
        }
        DialogOutcome::Edited
    }

    /// Keys while the focused text field is open for editing. Edit mode
    /// is exited only through `Enter` (commit and save), `Tab` (commit,
    /// advance, save) and `Esc` (discard, routed in
    /// [`ConfigDialog::key`]); every other key edits the value, so
    /// `Up`/`Down` and a pane-stealing `←` are deliberately ignored as
    /// exits.
    fn editing_key(&mut self, code: KeyCode) -> DialogOutcome {
        match code {
            // Enter commits and saves, keeping the field cursor on the
            // row.
            KeyCode::Enter => self.commit_edit(),
            // Tab commits and then takes the same step as outside edit
            // mode: next field, page turn at an edge.
            KeyCode::Tab => {
                let outcome = self.commit_edit();
                self.tab_forward();
                outcome
            }
            _ => {
                let Some(field) = self.fields.get_mut(self.cursor) else {
                    // The field vanished (page turned underneath);
                    // close the orphaned edit instead of writing into
                    // the void.
                    self.editing = false;
                    return DialogOutcome::Edited;
                };
                match code {
                    KeyCode::Char(c) if !c.is_control() => field.text.insert(c),
                    KeyCode::Backspace => field.text.backspace(),
                    KeyCode::Delete => field.text.delete(),
                    KeyCode::Left => field.text.left(),
                    KeyCode::Right => field.text.right(),
                    KeyCode::Home => field.text.home(),
                    KeyCode::End => field.text.end(),
                    _ => {}
                }
                DialogOutcome::Edited
            }
        }
    }

    /// Open the focused text field for editing: remember its value for
    /// an `Esc` discard and place the caret at the value's end, ready
    /// to append.
    fn begin_edit(&mut self) {
        if let Some(field) = self.fields.get_mut(self.cursor) {
            self.edit_backup = field.text.text().to_string();
            field.text.end();
            self.editing = true;
        }
    }

    /// Close the open field keeping its edited value. The commit is the
    /// save: what the user sees is the saved state, so the caller is
    /// asked to persist right away.
    fn commit_edit(&mut self) -> DialogOutcome {
        self.editing = false;
        self.edit_backup.clear();
        DialogOutcome::Save
    }

    /// Close the open field restoring the value it had when editing
    /// began. A discard is purely local: nothing is saved and the
    /// dialog stays exactly as it was.
    fn discard_edit(&mut self) {
        self.editing = false;
        let backup = std::mem::take(&mut self.edit_backup);
        if let Some(field) = self.fields.get_mut(self.cursor) {
            let caret = backup.chars().count();
            field.text.restore(backup, caret);
        }
    }

    /// Turn one page, keeping the field's offset within its page
    /// (clamped) so PgUp/PgDn feel like the same field position.
    fn turn_page(&mut self, forward: bool) {
        let target = if forward {
            self.page.next()
        } else {
            self.page.prev()
        };
        let Some(target) = target else {
            return;
        };
        let offset = self.cursor - self.page.fields().start;
        let range = target.fields();
        self.page = target;
        self.cursor = range.start + offset.min(range.len() - 1);
    }

    /// Tab off the last field of a page turns the page; off the last
    /// field of the last page it stops there — nothing follows the
    /// form, the list sits left of it.
    fn tab_forward(&mut self) {
        let range = self.page.fields();
        if self.cursor + 1 < range.end {
            self.cursor += 1;
        } else if let Some(next) = self.page.next() {
            self.page = next;
            self.cursor = next.fields().start;
        }
    }

    /// BackTab off the first field of a page returns to the previous
    /// page's last field; off the first page it reaches the list.
    fn tab_backward(&mut self) {
        let range = self.page.fields();
        if self.cursor > range.start {
            self.cursor -= 1;
        } else {
            match self.page.prev() {
                Some(prev) => {
                    self.page = prev;
                    self.cursor = prev.fields().end - 1;
                }
                None => self.pane = Pane::List,
            }
        }
    }

    /// Validate the form into `(label, ServerConfig)`, or the i18n key
    /// of the first violated rule. Every page is checked; `proxy` is the
    /// one key no page models, so it comes back `None` and the save path
    /// carries an existing entry's value across.
    pub fn collect(&self) -> Result<(String, ServerConfig), &'static str> {
        let label = self.fields[F_LABEL].text.text().trim();
        let host = self.fields[F_HOST].text.text().trim();
        let port = self.fields[F_PORT].text.text().trim();
        let nickname = self.fields[F_NICKNAME].text.text().trim();
        let username = self.fields[F_USERNAME].text.text().trim();
        let use_tls = self.fields[F_TLS].flag;
        let password = self.fields[F_PASSWORD].text.text();
        let sasl_user = self.fields[F_SASL_USER].text.text().trim();
        let sasl_pass = self.fields[F_SASL_PASS].text.text();
        let channels_field = self.fields[F_CHANNELS].text.text();
        let queries_field = self.fields[F_QUERIES].text.text();

        if label.is_empty()
            || label
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '[' | ']' | '"' | '\'' | '.' | '#' | '='))
        {
            return Err("dlg.err_label");
        }
        if host.is_empty() {
            return Err("dlg.err_host");
        }
        let port: u16 = port.parse().map_err(|_| "dlg.err_port")?;
        if port == 0 {
            return Err("dlg.err_port");
        }
        if nickname.is_empty() || nickname.chars().any(char::is_whitespace) {
            return Err("dlg.err_nickname");
        }
        let username = if username.is_empty() {
            nickname
        } else {
            username
        };
        let list = |text: &str| {
            text.split([',', ' '])
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let channels = list(channels_field);
        if !channels
            .iter()
            .all(|channel| crate::core::valid_channel_name(channel))
        {
            return Err("err.cfg_channels");
        }
        let queries = list(queries_field);
        if !queries
            .iter()
            .all(|nickname| crate::core::valid_query_nickname(nickname))
        {
            return Err("err.cfg_queries");
        }
        // SASL is all-or-nothing, mirroring `Config::parse`.
        let (sasl_username, sasl_password) = match (sasl_user.is_empty(), sasl_pass.is_empty()) {
            (false, false) => {
                if sasl_user.contains(['\0', '\r', '\n']) || sasl_pass.contains(['\0', '\r', '\n'])
                {
                    return Err("err.cfg_sasl_characters");
                }
                // Same bound as `Config::parse`: AUTHENTICATE must stay
                // within one 400-character base64 line.
                let payload = 2 + sasl_user.len() + sasl_pass.len();
                if 4 * payload.div_ceil(3) > 400 {
                    return Err("err.cfg_sasl_too_long");
                }
                (Some(sasl_user.to_string()), Some(sasl_pass.to_string()))
            }
            (true, true) => (None, None),
            _ => return Err("err.cfg_sasl_pair"),
        };
        Ok((
            label.to_string(),
            ServerConfig {
                username: username.to_string(),
                nickname: nickname.to_string(),
                password: password.to_string(),
                server: host.to_string(),
                use_tls,
                port,
                channels,
                sasl_username,
                sasl_password,
                queries,
                proxy: None,
            },
        ))
    }
}

impl Default for ConfigDialog {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Two configured servers, the first carrying every optional key.
    fn two_servers() -> Config {
        Config::parse(
            r##"
[servers.alpha]
username = "me"
nickname = "mynick"
password = "alpha-pass"
server = "irc.alpha.org"
use_tls = false
port = 6667
channels = ["#one", "#two"]
queries = ["alice"]
sasl_username = "acct"
sasl_password = "secret"

[servers.beta]
username = "you"
nickname = "yournick"
server = "irc.beta.org"
port = 7000
channels = ["#three"]
"##,
        )
        .unwrap()
    }

    /// A valid form with only the connection and identity essentials.
    fn filled() -> ConfigDialog {
        let mut dialog = ConfigDialog::new();
        dialog.set_field_text("dlg.field_label", "libera");
        dialog.set_field_text("dlg.field_host", "irc.libera.chat");
        dialog.set_field_text("dlg.field_nickname", "me");
        dialog.set_field_text("dlg.field_channels", "#a, #b");
        dialog
    }

    #[test]
    fn from_config_lists_every_server_and_prefills_the_first_into_all_pages() {
        // Arrange / Act
        let dialog = ConfigDialog::from_config(&two_servers());

        // Assert: the list holds both entries in file order, the first is
        // selected and focused, and the form carries its values across
        // all three pages — password, queries and SASL included.
        assert_eq!(dialog.pane, Pane::List);
        assert_eq!(dialog.list_cursor, 0);
        assert_eq!(dialog.selected.as_deref(), Some("alpha"));
        assert_eq!(dialog.field_text("dlg.field_label"), "alpha");
        assert_eq!(dialog.field_text("dlg.field_host"), "irc.alpha.org");
        assert_eq!(dialog.field_text("dlg.field_port"), "6667");
        assert!(!dialog.fields[F_TLS].flag);
        assert_eq!(dialog.field_text("dlg.field_nickname"), "mynick");
        assert_eq!(dialog.field_text("dlg.field_username"), "me");
        assert_eq!(dialog.field_text("dlg.field_password"), "alpha-pass");
        assert_eq!(dialog.field_text("dlg.field_sasl_user"), "acct");
        assert_eq!(dialog.field_text("dlg.field_sasl_pass"), "secret");
        assert_eq!(dialog.field_text("dlg.field_channels"), "#one, #two");
        assert_eq!(dialog.field_text("dlg.field_queries"), "alice");
    }

    #[test]
    fn from_config_without_servers_opens_a_blank_new_entry_in_the_form() {
        // Arrange / Act
        let dialog = ConfigDialog::from_config(&Config::default());

        // Assert: there is nothing to pick from, so focus starts in the
        // form on new-entry defaults and the pseudo-row is selected.
        assert_eq!(dialog.pane, Pane::Form);
        assert_eq!(dialog.selected, None);
        assert!(dialog.on_new_row());
        assert_eq!(dialog.field_text("dlg.field_label"), "libera");
        assert_eq!(dialog.field_text("dlg.field_host"), "");
        assert_eq!(dialog.field_text("dlg.field_port"), "6697");
        assert!(dialog.fields[F_TLS].flag, "TLS is on by default");
    }

    #[test]
    fn moving_the_list_selection_previews_the_entry_and_enter_focuses_the_form() {
        // Arrange
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert: moving the cursor swaps the previewed entry and
        // restarts it from the first page.
        dialog.key(KeyCode::Down, false, false);
        assert_eq!(dialog.selected.as_deref(), Some("beta"));
        assert_eq!(dialog.field_text("dlg.field_host"), "irc.beta.org");
        assert_eq!(dialog.field_text("dlg.field_password"), "");
        assert_eq!(dialog.page, Page::Connection);
        // Enter commits the preview and moves focus into the form.
        assert_eq!(
            dialog.key(KeyCode::Enter, false, false),
            DialogOutcome::Edited
        );
        assert_eq!(dialog.pane, Pane::Form);
        // Up clamps at the first row instead of wrapping.
        dialog.pane = Pane::List;
        dialog.key(KeyCode::Up, false, false);
        assert_eq!(dialog.list_cursor, 0);
        assert_eq!(dialog.selected.as_deref(), Some("alpha"));
    }

    #[test]
    fn the_new_server_row_and_the_n_key_start_a_blank_entry() {
        // Arrange: two servers, so the pseudo-row is list row 2.
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert: walking onto the pseudo-row previews defaults.
        dialog.key(KeyCode::Down, false, false);
        dialog.key(KeyCode::Down, false, false);
        assert!(dialog.on_new_row());
        assert_eq!(dialog.selected, None);
        assert_eq!(dialog.field_text("dlg.field_label"), "libera");
        assert_eq!(dialog.field_text("dlg.field_host"), "");
        // Enter there focuses the form for the new entry.
        dialog.key(KeyCode::Enter, false, false);
        assert_eq!(dialog.pane, Pane::Form);

        // `n` reaches the same state from anywhere in the list.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.key(KeyCode::Char('n'), false, false);
        assert_eq!(dialog.pane, Pane::Form);
        assert!(dialog.on_new_row());
        assert_eq!(dialog.selected, None);
    }

    #[test]
    fn tab_walks_fields_pages_and_panes_in_one_cycle() {
        // Arrange
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert: List → Form, then through every field and page.
        dialog.key(KeyCode::Tab, false, false);
        assert_eq!((dialog.pane, dialog.cursor), (Pane::Form, F_LABEL));
        dialog.key(KeyCode::Tab, false, false);
        assert_eq!(dialog.cursor, F_HOST);
        dialog.key(KeyCode::Tab, false, false);
        assert_eq!(dialog.cursor, F_PORT);
        dialog.key(KeyCode::Tab, false, false);
        assert_eq!(dialog.cursor, F_TLS);
        // Tab off the last field of a page turns the page.
        dialog.key(KeyCode::Tab, false, false);
        assert_eq!((dialog.page, dialog.cursor), (Page::Identity, F_NICKNAME));
        for expected in [F_USERNAME, F_PASSWORD, F_SASL_USER, F_SASL_PASS] {
            dialog.key(KeyCode::Tab, false, false);
            assert_eq!(dialog.cursor, expected);
        }
        dialog.key(KeyCode::Tab, false, false);
        assert_eq!((dialog.page, dialog.cursor), (Page::Channels, F_CHANNELS));
        dialog.key(KeyCode::Tab, false, false);
        assert_eq!(dialog.cursor, F_QUERIES);
        // Tab off the very last field stops there: no actions pane
        // follows the form anymore.
        dialog.key(KeyCode::Tab, false, false);
        assert_eq!((dialog.pane, dialog.cursor), (Pane::Form, F_QUERIES));

        // BackTab unwinds the exact same path, ending on the list.
        dialog.key(KeyCode::BackTab, false, false);
        assert_eq!((dialog.page, dialog.cursor), (Page::Channels, F_CHANNELS));
        dialog.key(KeyCode::BackTab, false, false);
        assert_eq!((dialog.page, dialog.cursor), (Page::Identity, F_SASL_PASS));
        for expected in [F_SASL_USER, F_PASSWORD, F_USERNAME, F_NICKNAME] {
            dialog.key(KeyCode::BackTab, false, false);
            assert_eq!(dialog.cursor, expected);
        }
        dialog.key(KeyCode::BackTab, false, false);
        assert_eq!((dialog.page, dialog.cursor), (Page::Connection, F_TLS));
        dialog.key(KeyCode::BackTab, false, false);
        assert_eq!(dialog.cursor, F_PORT);
        dialog.key(KeyCode::BackTab, false, false);
        assert_eq!(dialog.cursor, F_HOST);
        dialog.key(KeyCode::BackTab, false, false);
        assert_eq!(dialog.cursor, F_LABEL);
        dialog.key(KeyCode::BackTab, false, false);
        assert_eq!(dialog.pane, Pane::List);
    }

    #[test]
    fn page_keys_turn_pages_from_any_pane_and_alt_arrows_are_inert() {
        // Arrange: the list focused on the first page.
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert: PageUp/PageDown turn the form's pages from the
        // list pane too, without moving the focus. The ends clamp
        // without wrapping.
        dialog.key(KeyCode::PageDown, false, false);
        assert_eq!(dialog.page, Page::Identity, "list pane");
        assert_eq!(dialog.pane, Pane::List);
        dialog.key(KeyCode::PageDown, false, false);
        assert_eq!(dialog.page, Page::Channels);
        dialog.key(KeyCode::PageDown, false, false);
        assert_eq!(dialog.page, Page::Channels, "no wrap past the last page");
        dialog.key(KeyCode::PageUp, false, false);
        assert_eq!(dialog.page, Page::Identity);
        dialog.key(KeyCode::PageUp, false, false);
        assert_eq!(dialog.page, Page::Connection, "no wrap before the first");

        // From the form the same keys turn pages and carry the field
        // offset, clamped, into the new page — never the caret.
        dialog.pane = Pane::Form;
        dialog.page = Page::Identity;
        dialog.cursor = F_SASL_PASS; // offset 4 of a 5-field page
        dialog.key(KeyCode::PageDown, false, false);
        assert_eq!((dialog.page, dialog.cursor), (Page::Channels, F_QUERIES));
        dialog.key(KeyCode::PageUp, false, false);
        assert_eq!((dialog.page, dialog.cursor), (Page::Identity, F_USERNAME));

        // alt+←/alt+→ are inert everywhere: no page turn, no pane
        // change, no caret move.
        dialog.key(KeyCode::Right, false, true);
        dialog.key(KeyCode::Left, false, true);
        assert_eq!(
            (dialog.pane, dialog.page, dialog.cursor),
            (Pane::Form, Page::Identity, F_USERNAME)
        );
    }

    #[test]
    fn up_and_down_move_within_the_current_page_only() {
        // Arrange: the form focused on the first page.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;

        // Act / Assert
        for _ in 0..3 {
            dialog.key(KeyCode::Down, false, false);
        }
        assert_eq!(dialog.cursor, F_TLS);
        dialog.key(KeyCode::Down, false, false);
        assert_eq!(dialog.cursor, F_TLS, "Down clamps at the page's last field");
        dialog.key(KeyCode::Up, false, false);
        assert_eq!(dialog.cursor, F_PORT);
        dialog.key(KeyCode::Up, false, false);
        dialog.key(KeyCode::Up, false, false);
        dialog.key(KeyCode::Up, false, false);
        assert_eq!(
            dialog.cursor, F_LABEL,
            "Up clamps at the page's first field"
        );
    }

    #[test]
    fn enter_opens_a_text_field_and_flips_a_toggle_without_advancing() {
        // Arrange
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;

        // Act / Assert: Enter on a text field opens it for editing —
        // the cursor stays on the row and only Tab advances it.
        dialog.cursor = F_HOST;
        dialog.key(KeyCode::Enter, false, false);
        assert!(dialog.editing);
        dialog.key(KeyCode::Enter, false, false);
        assert_eq!(dialog.cursor, F_HOST);
        // ...while a toggle flips in place, without edit mode...
        dialog.cursor = F_TLS;
        let before = dialog.fields[F_TLS].flag;
        dialog.key(KeyCode::Enter, false, false);
        assert!(!dialog.editing);
        assert_eq!(dialog.fields[F_TLS].flag, !before);
        assert_eq!(dialog.cursor, F_TLS);
        // ...and Space flips (and saves) it the same way.
        dialog.key(KeyCode::Char(' '), false, false);
        assert_eq!(dialog.fields[F_TLS].flag, before);
        assert!(!dialog.editing);
        // Tab off the last field of the last page stops at the field.
        dialog.page = Page::Channels;
        dialog.cursor = F_QUERIES;
        dialog.key(KeyCode::Tab, false, false);
        assert_eq!((dialog.pane, dialog.cursor), (Pane::Form, F_QUERIES))
    }

    #[test]
    fn typing_while_the_list_is_focused_never_leaks_into_the_form() {
        // Arrange
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert: ordinary typing is swallowed by the list.
        dialog.key(KeyCode::Char('x'), false, false);
        dialog.key(KeyCode::Backspace, false, false);
        assert_eq!(dialog.field_text("dlg.field_label"), "alpha");
        assert_eq!(dialog.pane, Pane::List);
        // `n` is the list's own shortcut for a new entry.
        dialog.key(KeyCode::Char('n'), false, false);
        assert_eq!(dialog.pane, Pane::Form);
        assert_eq!(dialog.selected, None);
    }

    #[test]
    fn esc_or_ctrl_c_cancels_from_any_pane() {
        // Arrange
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert: Esc closes from the list...
        assert_eq!(
            dialog.key(KeyCode::Esc, false, false),
            DialogOutcome::Cancel
        );
        // ...and from the form, while Ctrl+C does the same from either
        // pane; every other ctrl chord is swallowed, not saved.
        dialog.pane = Pane::Form;
        assert_eq!(
            dialog.key(KeyCode::Char('c'), true, false),
            DialogOutcome::Cancel
        );
        assert_eq!(
            dialog.key(KeyCode::Char('x'), true, false),
            DialogOutcome::Edited
        );
    }

    #[test]
    fn left_from_the_form_always_returns_to_the_sidebar_when_not_editing() {
        // Arrange: the list focused on the first entry.
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert: → from the list focuses the form — omp's hub
        // idiom of arrows switching panes.
        dialog.key(KeyCode::Right, false, false);
        assert_eq!(dialog.pane, Pane::Form);
        // ← then returns to the sidebar from any field state: the
        // caret sits mid-text ("alpha", caret at its end) and still
        // the pane switch wins — the caret only moves inside edit
        // mode, never as a pane-stealing fallback.
        dialog.key(KeyCode::Left, false, false);
        assert_eq!(dialog.pane, Pane::List);
        // The caret is left where it was; re-entering the form keeps
        // the same field focused.
        dialog.key(KeyCode::Right, false, false);
        assert_eq!((dialog.pane, dialog.cursor), (Pane::Form, F_LABEL));
    }

    #[test]
    fn enter_opens_a_text_field_with_the_caret_at_the_value_end() {
        // Arrange: the form on the label field, its caret parked at
        // the value's start.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;
        dialog.fields[F_LABEL].text.home();

        // Act
        assert_eq!(
            dialog.key(KeyCode::Enter, false, false),
            DialogOutcome::Edited
        );

        // Assert: the field is open for editing with the caret at the
        // end, ready to append.
        assert!(dialog.editing);
        assert_eq!(dialog.fields[F_LABEL].text.cursor(), "alpha".len());
    }

    #[test]
    fn typing_lands_only_while_a_field_is_open_for_editing() {
        // Arrange: the form focused on the label field.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;

        // Act / Assert: outside edit mode, typing keys are no-ops.
        for code in [
            KeyCode::Char('x'),
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Home,
            KeyCode::End,
        ] {
            dialog.key(code, false, false);
        }
        assert_eq!(dialog.field_text("dlg.field_label"), "alpha");
        assert_eq!(dialog.fields[F_LABEL].text.cursor(), 5);

        // Inside edit mode the same keys edit the value.
        dialog.key(KeyCode::Enter, false, false);
        dialog.key(KeyCode::Char('!'), false, false);
        assert_eq!(dialog.field_text("dlg.field_label"), "alpha!");
        assert!(dialog.editing, "typing alone does not commit");
    }

    #[test]
    fn enter_commits_the_edit_and_keeps_the_field_cursor_on_the_row() {
        // Arrange: the label field open for editing with a change.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;
        dialog.key(KeyCode::Enter, false, false);
        dialog.key(KeyCode::Char('2'), false, false);

        // Act: Enter commits — and the commit asks for an immediate
        // save.
        assert_eq!(
            dialog.key(KeyCode::Enter, false, false),
            DialogOutcome::Save
        );

        // Assert: the text stays, edit mode ends, and the cursor stays
        // on the row.
        assert!(!dialog.editing);
        assert_eq!(dialog.field_text("dlg.field_label"), "alpha2");
        assert_eq!(dialog.cursor, F_LABEL);
    }

    #[test]
    fn esc_while_editing_discards_the_edit_instead_of_cancelling() {
        // Arrange: one committed edit ("alpha2"), then the field
        // reopens and the edit goes further before Esc.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;
        dialog.key(KeyCode::Enter, false, false);
        dialog.key(KeyCode::Char('2'), false, false);
        dialog.key(KeyCode::Enter, false, false);
        dialog.key(KeyCode::Enter, false, false);
        dialog.key(KeyCode::Backspace, false, false);
        dialog.key(KeyCode::Char('!'), false, false);

        // Act: Esc discards the open edit, not the dialog.
        assert_eq!(
            dialog.key(KeyCode::Esc, false, false),
            DialogOutcome::Edited
        );

        // Assert: the value snaps back to what the last commit left
        // and edit mode ends.
        assert!(!dialog.editing);
        assert_eq!(dialog.field_text("dlg.field_label"), "alpha2");

        // Outside edit mode, Esc cancels the dialog as before.
        assert_eq!(
            dialog.key(KeyCode::Esc, false, false),
            DialogOutcome::Cancel
        );
    }

    #[test]
    fn an_escaped_edit_stays_local_without_saving() {
        // Arrange / Act: open, type, discard — nothing is committed.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;
        dialog.key(KeyCode::Enter, false, false);
        dialog.key(KeyCode::Char('x'), false, false);

        // Assert: Esc restores the value and reports a plain edit, so
        // nothing reaches the save path.
        assert_eq!(
            dialog.key(KeyCode::Esc, false, false),
            DialogOutcome::Edited
        );
        assert!(!dialog.editing);
        assert_eq!(dialog.field_text("dlg.field_label"), "alpha");
    }

    #[test]
    fn up_down_and_page_keys_stay_in_the_field_while_editing() {
        // Arrange: the label field open for editing.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;
        dialog.key(KeyCode::Enter, false, false);

        // Act / Assert: edit mode is exited only via Enter/Tab/Esc, so
        // Up/Down do not move the field cursor and PageUp/PageDown do
        // not turn the page (that would retarget the open edit at
        // another field).
        dialog.key(KeyCode::Up, false, false);
        dialog.key(KeyCode::Down, false, false);
        assert_eq!(dialog.cursor, F_LABEL);
        dialog.key(KeyCode::PageUp, false, false);
        dialog.key(KeyCode::PageDown, false, false);
        assert_eq!(dialog.page, Page::Connection);
        // Left stays a caret key while editing — it must not steal
        // the pane focus back to the sidebar.
        dialog.key(KeyCode::Left, false, false);
        assert_eq!((dialog.pane, dialog.editing), (Pane::Form, true));
        assert_eq!(dialog.fields[F_LABEL].text.cursor(), 4);
    }

    #[test]
    fn alt_arrows_are_inert_from_any_pane() {
        // Arrange: the list focused (a preview on the first page).
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert: alt+←/alt+→ do nothing anywhere — pages turn
        // only with PageUp/PageDown now — and other alt keys stay
        // swallowed, not typed.
        dialog.key(KeyCode::Right, false, true);
        dialog.key(KeyCode::Left, false, true);
        assert_eq!((dialog.pane, dialog.page), (Pane::List, Page::Connection));
        // From the form the same keys leave pane, page, and caret put.
        dialog.pane = Pane::Form;
        dialog.cursor = F_LABEL;
        dialog.key(KeyCode::Right, false, true);
        dialog.key(KeyCode::Left, false, true);
        assert_eq!((dialog.page, dialog.cursor), (Page::Connection, F_LABEL));
        dialog.key(KeyCode::Char('x'), false, true);
        assert_eq!(dialog.field_text("dlg.field_label"), "alpha");
    }

    #[test]
    fn collect_validates_every_rule_across_the_pages() {
        for (key, text, expected) in [
            ("dlg.field_label", "", "dlg.err_label"),
            ("dlg.field_label", "a b", "dlg.err_label"),
            ("dlg.field_label", "a.b", "dlg.err_label"),
            ("dlg.field_label", "a\"b", "dlg.err_label"),
            ("dlg.field_host", "", "dlg.err_host"),
            ("dlg.field_port", "", "dlg.err_port"),
            ("dlg.field_port", "70000", "dlg.err_port"),
            ("dlg.field_port", "0", "dlg.err_port"),
            ("dlg.field_nickname", "", "dlg.err_nickname"),
            ("dlg.field_nickname", "two words", "dlg.err_nickname"),
            ("dlg.field_channels", "chan", "err.cfg_channels"),
            ("dlg.field_channels", "#a password", "err.cfg_channels"),
            ("dlg.field_queries", "#a", "err.cfg_queries"),
            ("dlg.field_sasl_user", "acc", "err.cfg_sasl_pair"),
            ("dlg.field_sasl_pass", "secret", "err.cfg_sasl_pair"),
        ] {
            let mut dialog = filled();
            dialog.set_field_text(key, text);
            assert_eq!(dialog.collect().unwrap_err(), expected, "{key} = {text:?}");
        }
    }

    #[test]
    fn collect_round_trips_the_form_including_password_and_queries() {
        // Arrange: nickname only (username defaults to it), a server
        // password, channels and queries.
        let mut dialog = filled();
        dialog.set_field_text("dlg.field_password", "hunter2");
        dialog.set_field_text("dlg.field_queries", "alice, bob");

        // Act
        let (label, server) = dialog.collect().unwrap();

        // Assert: every page's values round-trip; the blank username
        // defaults to the nickname, and proxy stays unmodeled (the save
        // path carries an existing entry's proxy across).
        assert_eq!(label, "libera");
        assert_eq!(server.username, "me");
        assert_eq!(server.server, "irc.libera.chat");
        assert_eq!(server.port, 6697);
        assert!(server.use_tls);
        assert_eq!(server.password, "hunter2");
        assert_eq!(server.channels, vec!["#a".to_string(), "#b".to_string()]);
        assert_eq!(server.queries, vec!["alice".to_string(), "bob".to_string()]);
        assert!(server.sasl_username.is_none());
        assert!(server.proxy.is_none());
    }

    #[test]
    fn collect_accepts_a_full_sasl_pair_and_rejects_oversized_credentials() {
        // Arrange / Act / Assert: the pair passes together...
        let mut dialog = filled();
        dialog.set_field_text("dlg.field_sasl_user", "account");
        dialog.set_field_text("dlg.field_sasl_pass", "secret");
        let (_, server) = dialog.collect().unwrap();
        assert_eq!(server.sasl_username.as_deref(), Some("account"));

        // ...until the encoded PLAIN payload would not fit one line.
        let mut oversized = filled();
        oversized.set_field_text("dlg.field_sasl_user", &"u".repeat(280));
        oversized.set_field_text("dlg.field_sasl_pass", &"p".repeat(280));
        assert_eq!(oversized.collect().unwrap_err(), "err.cfg_sasl_too_long");
    }

    #[test]
    fn editing_keys_touch_only_the_focused_field() {
        // Arrange: the form focused on the host field, opened for
        // editing with Enter.
        let mut dialog = filled();
        dialog.pane = Pane::Form;
        dialog.cursor = F_HOST;
        dialog.key(KeyCode::Enter, false, false);
        assert!(dialog.editing);

        // Act / Assert: End + insert appends, Backspace removes the
        // tail — none of it reaches the save path before a commit.
        dialog.key(KeyCode::End, false, false);
        dialog.key(KeyCode::Char('s'), false, false);
        dialog.key(KeyCode::Backspace, false, false);
        assert_eq!(dialog.field_text("dlg.field_host"), "irc.libera.chat");
        assert!(dialog.editing, "typing alone does not commit");
        // Left moves the caret while editing (not the pane); Delete
        // then removes the character under it.
        dialog.key(KeyCode::Left, false, false);
        assert_eq!(dialog.pane, Pane::Form);
        dialog.key(KeyCode::Delete, false, false);
        assert_eq!(dialog.field_text("dlg.field_host"), "irc.libera.cha");
        assert_eq!(dialog.field_text("dlg.field_label"), "libera");
    }

    #[test]
    fn moving_the_list_cursor_reloads_the_saved_entry_preview() {
        // Arrange: the form on the first entry, one field committed (a
        // save the dialog itself only requests — the entries list is
        // the save path's business).
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;
        dialog.key(KeyCode::Enter, false, false);
        dialog.key(KeyCode::Char('x'), false, false);
        assert_eq!(
            dialog.key(KeyCode::Enter, false, false),
            DialogOutcome::Save
        );
        assert_eq!(dialog.field_text("dlg.field_label"), "alphax");

        // Act / Assert: moving away and back restarts the preview from
        // the entry the list holds — under auto-save the form shows the
        // persisted state, so an unsaved preview concept cannot leak.
        dialog.pane = Pane::List;
        dialog.key(KeyCode::Down, false, false);
        assert_eq!(dialog.selected.as_deref(), Some("beta"));
        dialog.key(KeyCode::Up, false, false);
        assert_eq!(dialog.selected.as_deref(), Some("alpha"));
        assert_eq!(dialog.field_text("dlg.field_label"), "alpha");
    }

    #[test]
    fn committing_a_text_edit_auto_saves_and_keeps_the_form_open() {
        // Arrange: the label field of the first entry open with a change.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;
        dialog.key(KeyCode::Enter, false, false);
        dialog.key(KeyCode::Char('2'), false, false);

        // Act: Enter commits — and committing is saving now, so the key
        // asks the caller to persist instead of just editing.
        assert_eq!(
            dialog.key(KeyCode::Enter, false, false),
            DialogOutcome::Save
        );

        // Assert: the value stays committed and edit mode ended; the
        // dialog itself stays open for further browsing.
        assert!(!dialog.editing);
        assert_eq!(dialog.field_text("dlg.field_label"), "alpha2");
        assert_eq!(dialog.cursor, F_LABEL);
    }

    #[test]
    fn flipping_a_toggle_auto_saves_in_place() {
        // Arrange: the form focused on the TLS toggle.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;
        dialog.cursor = F_TLS;
        let before = dialog.fields[F_TLS].flag;

        // Act / Assert: Enter flips (and saves) without edit mode...
        assert_eq!(
            dialog.key(KeyCode::Enter, false, false),
            DialogOutcome::Save
        );
        assert!(!dialog.editing);
        assert_eq!(dialog.fields[F_TLS].flag, !before);
        // ...and Space flips (and saves) the same way.
        assert_eq!(
            dialog.key(KeyCode::Char(' '), false, false),
            DialogOutcome::Save
        );
        assert_eq!(dialog.fields[F_TLS].flag, before);
    }

    #[test]
    fn tab_while_editing_commits_advances_and_auto_saves() {
        // Arrange: the host field open with a change.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;
        dialog.cursor = F_HOST;
        dialog.key(KeyCode::Enter, false, false);
        dialog.key(KeyCode::Char('!'), false, false);

        // Act: Tab commits, advances, and saves in one step.
        assert_eq!(dialog.key(KeyCode::Tab, false, false), DialogOutcome::Save);

        // Assert
        assert!(!dialog.editing);
        assert_eq!(dialog.field_text("dlg.field_host"), "irc.alpha.org!");
        assert_eq!(dialog.cursor, F_PORT);

        // Tab off the very last field commits and then stops there:
        // there is no actions pane to fall into anymore.
        dialog.page = Page::Channels;
        dialog.cursor = F_QUERIES;
        dialog.key(KeyCode::Enter, false, false);
        dialog.key(KeyCode::Char('!'), false, false);
        assert_eq!(dialog.key(KeyCode::Tab, false, false), DialogOutcome::Save);
        assert_eq!((dialog.pane, dialog.cursor), (Pane::Form, F_QUERIES));
    }

    #[test]
    fn tab_cycles_only_the_list_and_form_panes() {
        // Arrange
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert: Tab from the list focuses the form; BackTab
        // from the list wraps to the form as well — the arrows' ←/→
        // semantics (← Form→List, → List→Form) mirrored.
        dialog.key(KeyCode::Tab, false, false);
        assert_eq!(dialog.pane, Pane::Form);
        dialog.pane = Pane::List;
        dialog.key(KeyCode::BackTab, false, false);
        assert_eq!(dialog.pane, Pane::Form);
        // BackTab off the form's first field reaches the list.
        dialog.cursor = F_LABEL;
        dialog.key(KeyCode::BackTab, false, false);
        assert_eq!(dialog.pane, Pane::List);
    }

    #[test]
    fn d_arms_deletion_on_an_existing_row_and_stays_inert_on_the_new_row() {
        // Arrange / Act
        let mut dialog = ConfigDialog::from_config(&two_servers());
        assert_eq!(
            dialog.key(KeyCode::Char('d'), false, false),
            DialogOutcome::Edited
        );

        // Assert: the arm carries a future deadline.
        let deadline = dialog.delete_armed.expect("d must arm the delete");
        assert!(deadline > std::time::Instant::now());

        // On the pseudo-row the same key does nothing.
        dialog.delete_armed = None;
        dialog.key(KeyCode::Char('n'), false, false);
        dialog.pane = Pane::List;
        assert!(dialog.on_new_row());
        dialog.key(KeyCode::Char('d'), false, false);
        assert!(
            dialog.delete_armed.is_none(),
            "no entry under the cursor, no arm"
        );
    }

    #[test]
    fn an_armed_delete_swallows_every_key_but_the_second_d_and_disarms_on_esc() {
        // Arrange
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert: any other key cancels the arm, is swallowed,
        // and leaves the dialog exactly as it was — `n` must not jump
        // to the form, and Esc must not close the dialog.
        for code in [
            KeyCode::Char('n'),
            KeyCode::Up,
            KeyCode::Enter,
            KeyCode::Tab,
        ] {
            dialog.key(KeyCode::Char('d'), false, false);
            assert_eq!(
                dialog.key(code, false, false),
                DialogOutcome::Edited,
                "{code:?}"
            );
            assert!(dialog.delete_armed.is_none(), "{code:?} must disarm");
            assert_eq!(dialog.pane, Pane::List, "{code:?} must not reach the panes");
            assert_eq!(dialog.list_cursor, 0, "{code:?} must not move the cursor");
        }
        dialog.key(KeyCode::Char('d'), false, false);
        assert_eq!(
            dialog.key(KeyCode::Esc, false, false),
            DialogOutcome::Edited
        );
        assert!(dialog.delete_armed.is_none());
        assert_eq!(
            dialog.selected.as_deref(),
            Some("alpha"),
            "Esc disarmed the delete, it did not close the dialog"
        );
    }

    #[test]
    fn the_second_d_within_the_timeout_confirms_and_deletes_the_entry() {
        // Arrange / Act
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.key(KeyCode::Char('d'), false, false);

        // Assert: the confirm carries the selected entry's label and
        // re-arms nothing.
        assert!(matches!(
            dialog.key(KeyCode::Char('d'), false, false),
            DialogOutcome::Delete(ref name) if name == "alpha"
        ));
        assert!(dialog.delete_armed.is_none());
    }

    #[test]
    fn an_expired_arm_cannot_confirm_and_tick_clears_it() {
        // Arrange: an arm whose deadline already passed — fabricated
        // directly because `key` takes no clock parameter.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.key(KeyCode::Char('d'), false, false);
        dialog.delete_armed = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));

        // Act / Assert: the second `d` no longer confirms; expiry drops
        // the arm and the press re-arms instead.
        assert_eq!(
            dialog.key(KeyCode::Char('d'), false, false),
            DialogOutcome::Edited
        );
        assert!(
            dialog.delete_armed.is_some(),
            "the press re-armed a fresh delete"
        );

        // `tick` clears an expired arm outright (the footer hint
        // reverts without a key press) and keeps a live one.
        dialog.delete_armed = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        dialog.tick(std::time::Instant::now());
        assert!(dialog.delete_armed.is_none());
        dialog.delete_armed = Some(std::time::Instant::now() + DELETE_ARM_TIMEOUT);
        dialog.tick(std::time::Instant::now());
        assert!(
            dialog.delete_armed.is_some(),
            "a live arm survives its tick"
        );
    }
}
