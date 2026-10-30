//! The in-TUI server configuration wizard: a modal opened on first run
//! (no `config.toml` yet, or one without servers) and by `/config`.
//!
//! The wizard is a two-pane master-detail form: the left pane lists every
//! configured server plus a "new server" pseudo-row, the right pane edits
//! the selection across three sequential pages (connection, identity,
//! channels). The dialog owns only form state and stays free of terminal
//! I/O: `key` maps a crossterm key code to a state change, the renderer
//! reads the fields, and `collect` turns them into a `ServerConfig`
//! (validating with the same rules `Config::parse` enforces). Saving is
//! orchestrated by `tui::events`, which knows the config path and the
//! connection map.

use crate::config::{Config, ServerConfig};
use crate::core::Draft;
use ratatui::crossterm::event::KeyCode;

/// Which pane of the two-column wizard holds the keyboard focus. Tab
/// cycles List → Form → Actions → List; BackTab unwinds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The left server list.
    List,
    /// The right, paged detail form.
    Form,
    /// The footer's Save/Cancel actions.
    Actions,
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
    /// The configured server label the form is editing, when any: a
    /// saved dialog with a different label adds a new entry and keeps
    /// the old one untouched.
    pub selected: Option<String>,
    /// The entry whose form edits have not been saved: the list shows a
    /// trailing `*` on that row so moving the cursor away (which
    /// silently discards the preview's edits) stays visible. Cleared by
    /// a save and by landing back on the row.
    pub dirty_row: Option<String>,
}

/// What one routed key press did to the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogOutcome {
    /// Not consumed (or a pure edit): the dialog stays open.
    Edited,
    /// Close without saving.
    Cancel,
    /// Validate and persist; the caller performs the actual save.
    Save,
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
            dirty_row: None,
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
        self.error = None;
        // Landing back on the edited row clears its marker: the user
        // has now seen that the preview restarted from the saved values.
        if self.dirty_row == self.selected {
            self.dirty_row = None;
        }
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
    /// `alt` carries the Alt modifier, which omp's hubs use for page
    /// cycling (`alt+left`/`alt+right`).
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
        if matches!(code, KeyCode::Esc) {
            return DialogOutcome::Cancel;
        }
        if alt {
            // omp's ModelHub turns its kind tabs with alt+left/right
            // from anywhere in the hub; ours cycles the form's pages.
            // Any other alt key is swallowed, not typed.
            match code {
                KeyCode::Left => self.turn_page(false),
                KeyCode::Right => self.turn_page(true),
                _ => {}
            }
            return DialogOutcome::Edited;
        }
        match self.pane {
            Pane::List => self.list_key(code),
            Pane::Form => self.form_key(code),
            Pane::Actions => self.actions_key(code),
        }
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
            KeyCode::Right => self.pane = Pane::Form,
            KeyCode::Tab => self.pane = Pane::Form,
            KeyCode::BackTab => self.pane = Pane::Actions,
            _ => {}
        }
        DialogOutcome::Edited
    }

    /// Keys while the paged form is focused: field navigation and
    /// editing, page turns at the edges.
    fn form_key(&mut self, code: KeyCode) -> DialogOutcome {
        let range = self.page.fields();
        match code {
            KeyCode::Tab => self.tab_forward(),
            // ← off the start of a field returns to the sidebar (omp's
            // hub arrows switch panes); mid-field it still moves the
            // text caret via the field arms below.
            KeyCode::Left if self.fields[self.cursor].text.cursor() == 0 => self.pane = Pane::List,
            KeyCode::BackTab => self.tab_backward(),
            // Up/Down stay within the current page: pages are turned
            // explicitly (Tab at the edge, alt+←/alt+→ from any pane).
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1).max(range.start),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(range.end - 1),
            KeyCode::Enter => {
                if let Some(field) = self.fields.get_mut(self.cursor) {
                    if field.kind == FieldKind::Toggle {
                        field.flag = !field.flag;
                        self.mark_dirty();
                    } else {
                        // Enter advances; off the last field of a page it
                        // flows on, reaching the actions after the last
                        // page so Save is one key away.
                        match self.page.next() {
                            Some(next) if self.cursor + 1 >= range.end => {
                                self.page = next;
                                self.cursor = next.fields().start;
                            }
                            _ if self.cursor + 1 < range.end => self.cursor += 1,
                            _ => self.pane = Pane::Actions,
                        }
                    }
                }
            }
            _ => {
                if let Some(field) = self.fields.get_mut(self.cursor) {
                    match (field.kind, code) {
                        (FieldKind::Toggle, KeyCode::Char(' ')) => {
                            field.flag = !field.flag;
                            self.mark_dirty();
                        }
                        (FieldKind::Toggle, _) => {}
                        (FieldKind::Text, KeyCode::Char(c)) if !c.is_control() => {
                            field.text.insert(c);
                            self.mark_dirty();
                        }
                        (FieldKind::Text, KeyCode::Backspace) => {
                            field.text.backspace();
                            self.mark_dirty();
                        }
                        (FieldKind::Text, KeyCode::Delete) => {
                            field.text.delete();
                            self.mark_dirty();
                        }
                        (FieldKind::Text, KeyCode::Left) => field.text.left(),
                        (FieldKind::Text, KeyCode::Right) => field.text.right(),
                        (FieldKind::Text, KeyCode::Home) => field.text.home(),
                        (FieldKind::Text, KeyCode::End) => field.text.end(),
                        (FieldKind::Text, _) => {}
                    }
                }
            }
        }
        DialogOutcome::Edited
    }
    /// Keys while the footer actions are focused.
    fn actions_key(&mut self, code: KeyCode) -> DialogOutcome {
        match code {
            KeyCode::Enter => {
                self.error = None;
                // A successful save persists the form, so no row keeps
                // an unsaved-edits marker (the dialog closes anyway).
                self.dirty_row = None;
                DialogOutcome::Save
            }
            KeyCode::Left => {
                self.pane = Pane::List;
                DialogOutcome::Edited
            }
            KeyCode::Right => {
                self.pane = Pane::Form;
                DialogOutcome::Edited
            }
            KeyCode::Tab => {
                self.pane = Pane::List;
                DialogOutcome::Edited
            }
            KeyCode::BackTab => {
                self.pane = Pane::Form;
                DialogOutcome::Edited
            }
            _ => DialogOutcome::Edited,
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
    /// field of the last page it reaches the actions.
    fn tab_forward(&mut self) {
        let range = self.page.fields();
        if self.cursor + 1 < range.end {
            self.cursor += 1;
        } else {
            match self.page.next() {
                Some(next) => {
                    self.page = next;
                    self.cursor = next.fields().start;
                }
                None => self.pane = Pane::Actions,
            }
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

    /// Attribute the next text edit or toggle flip to the entry being
    /// edited: the marker follows the most recently edited row, since a
    /// new edit supersedes what the marker said about an older one.
    fn mark_dirty(&mut self) {
        if let Some(selected) = &self.selected {
            self.dirty_row = Some(selected.clone());
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
        // Tab off the very last field lands on the actions, then the
        // cycle closes back to the list.
        dialog.key(KeyCode::Tab, false, false);
        assert_eq!(dialog.pane, Pane::Actions);
        dialog.key(KeyCode::Tab, false, false);
        assert_eq!(dialog.pane, Pane::List);

        // BackTab unwinds the exact same path.
        dialog.key(KeyCode::BackTab, false, false);
        assert_eq!(dialog.pane, Pane::Actions);
        dialog.key(KeyCode::BackTab, false, false);
        assert_eq!((dialog.pane, dialog.cursor), (Pane::Form, F_QUERIES));
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
    fn page_keys_are_inert_and_only_alt_arrows_turn_pages() {
        // Arrange: the list focused on the first page.
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert: PageUp/PageDown no longer turn pages in any
        // pane — pages turn only through alt+←/alt+→ (and Tab at a
        // field edge) — so both keys are swallowed where they land.
        dialog.key(KeyCode::PageDown, false, false);
        assert_eq!(dialog.page, Page::Connection, "list pane");
        dialog.pane = Pane::Form;
        dialog.key(KeyCode::PageDown, false, false);
        assert_eq!(dialog.page, Page::Connection, "form pane");
        dialog.key(KeyCode::PageUp, false, false);
        assert_eq!(dialog.page, Page::Connection);

        // alt+←/alt+→ still turn from the form, clamping at the ends
        // without wrapping and carrying the field offset, clamped,
        // into the new page.
        dialog.key(KeyCode::Right, false, true);
        assert_eq!(dialog.page, Page::Identity);
        dialog.key(KeyCode::Right, false, true);
        assert_eq!(dialog.page, Page::Channels);
        dialog.key(KeyCode::Right, false, true);
        assert_eq!(dialog.page, Page::Channels, "no wrap past the last page");
        dialog.page = Page::Identity;
        dialog.cursor = F_SASL_PASS; // offset 4 of a 5-field page
        dialog.key(KeyCode::Right, false, true);
        assert_eq!((dialog.page, dialog.cursor), (Page::Channels, F_QUERIES));
        dialog.key(KeyCode::Left, false, true);
        assert_eq!((dialog.page, dialog.cursor), (Page::Identity, F_USERNAME));
        dialog.key(KeyCode::Left, false, true);
        dialog.key(KeyCode::Left, false, true);
        assert_eq!(dialog.page, Page::Connection, "no wrap before the first");
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
    fn enter_flips_a_toggle_advances_a_field_and_reaches_the_actions() {
        // Arrange
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;

        // Act / Assert: Enter steps down a text field...
        dialog.cursor = F_HOST;
        dialog.key(KeyCode::Enter, false, false);
        assert_eq!(dialog.cursor, F_PORT);
        // ...flips a toggle in place...
        dialog.cursor = F_TLS;
        let before = dialog.fields[F_TLS].flag;
        dialog.key(KeyCode::Enter, false, false);
        assert_eq!(dialog.fields[F_TLS].flag, !before);
        assert_eq!(dialog.cursor, F_TLS);
        // ...and Space toggles too.
        dialog.key(KeyCode::Char(' '), false, false);
        assert_eq!(dialog.fields[F_TLS].flag, before);
        // Enter off the last field of the last page reaches the actions.
        dialog.page = Page::Channels;
        dialog.cursor = F_QUERIES;
        dialog.key(KeyCode::Enter, false, false);
        assert_eq!(dialog.pane, Pane::Actions);
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
    fn the_actions_row_saves_and_esc_or_ctrl_c_cancel_from_any_pane() {
        // Arrange
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert
        dialog.pane = Pane::Actions;
        assert_eq!(
            dialog.key(KeyCode::Enter, false, false),
            DialogOutcome::Save
        );
        assert_eq!(
            dialog.key(KeyCode::Esc, false, false),
            DialogOutcome::Cancel
        );
        assert_eq!(
            dialog.key(KeyCode::Char('c'), true, false),
            DialogOutcome::Cancel
        );
        assert_eq!(
            dialog.key(KeyCode::Char('x'), true, false),
            DialogOutcome::Edited
        );
        dialog.pane = Pane::List;
        assert_eq!(
            dialog.key(KeyCode::Esc, false, false),
            DialogOutcome::Cancel
        );
        dialog.pane = Pane::Form;
        assert_eq!(
            dialog.key(KeyCode::Char('c'), true, false),
            DialogOutcome::Cancel
        );
    }

    #[test]
    fn left_and_right_move_pane_focus_without_disturbing_the_caret() {
        // Arrange: the list focused on the first entry.
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert: → from the list focuses the form — omp's hub
        // idiom of arrows switching panes.
        dialog.key(KeyCode::Right, false, false);
        assert_eq!(dialog.pane, Pane::Form);
        // ← mid-field still moves the text caret; only ← off the start
        // of a field returns to the sidebar.
        dialog.key(KeyCode::Left, false, false);
        assert_eq!(dialog.pane, Pane::Form, "mid-field left moves the caret");
        dialog.key(KeyCode::Home, false, false);
        dialog.key(KeyCode::Left, false, false);
        assert_eq!(dialog.pane, Pane::List);
        // From the actions row the arrows pick a side.
        dialog.pane = Pane::Actions;
        dialog.key(KeyCode::Left, false, false);
        assert_eq!(dialog.pane, Pane::List);
        dialog.pane = Pane::Actions;
        dialog.key(KeyCode::Right, false, false);
        assert_eq!(dialog.pane, Pane::Form);
    }

    #[test]
    fn alt_arrows_turn_pages_from_any_pane() {
        // Arrange: the list focused (a preview on the first page).
        let mut dialog = ConfigDialog::from_config(&two_servers());

        // Act / Assert: alt+→/alt+← cycle the form's pages from any
        // pane — omp's ModelHub cycles its kind tabs the same way —
        // without moving the pane focus or the caret.
        dialog.key(KeyCode::Right, false, true);
        assert_eq!(dialog.page, Page::Identity);
        assert_eq!(dialog.pane, Pane::List);
        dialog.key(KeyCode::Left, false, true);
        assert_eq!(dialog.page, Page::Connection);
        // From the form the same keys turn pages, never the caret.
        dialog.pane = Pane::Form;
        dialog.cursor = F_LABEL;
        dialog.key(KeyCode::Right, false, true);
        assert_eq!(dialog.page, Page::Identity);
        assert_eq!(dialog.cursor, F_NICKNAME);
        // Alt with any other key is swallowed, not typed.
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
        // Arrange: the form focused on the host field.
        let mut dialog = filled();
        dialog.pane = Pane::Form;
        dialog.cursor = F_HOST;

        // Act / Assert: End + insert appends, Backspace removes the tail.
        dialog.key(KeyCode::End, false, false);
        dialog.key(KeyCode::Char('s'), false, false);
        dialog.key(KeyCode::Backspace, false, false);
        assert_eq!(dialog.field_text("dlg.field_host"), "irc.libera.chat");
        // Left + Delete removes the character under the cursor.
        dialog.key(KeyCode::Left, false, false);
        dialog.key(KeyCode::Delete, false, false);
        assert_eq!(dialog.field_text("dlg.field_host"), "irc.libera.cha");
        assert_eq!(dialog.field_text("dlg.field_label"), "libera");
    }

    #[test]
    fn editing_marks_the_edited_entry_until_save_or_returning_to_it() {
        // Arrange: the form focused on the first entry's label.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;

        // Act / Assert: caret moves alone do not dirty the entry...
        dialog.key(KeyCode::End, false, false);
        assert_eq!(dialog.dirty_row, None);
        // ...but text edits and toggle flips do.
        dialog.key(KeyCode::Char('x'), false, false);
        assert_eq!(dialog.dirty_row.as_deref(), Some("alpha"));
        dialog.cursor = F_TLS;
        dialog.key(KeyCode::Char(' '), false, false);
        assert_eq!(dialog.dirty_row.as_deref(), Some("alpha"));

        // Moving the list cursor away keeps the marker on the row whose
        // edits were just silently discarded.
        dialog.pane = Pane::List;
        dialog.key(KeyCode::Down, false, false);
        assert_eq!(dialog.selected.as_deref(), Some("beta"));
        assert_eq!(dialog.dirty_row.as_deref(), Some("alpha"));

        // A save clears it: the dialog closes on success anyway.
        dialog.pane = Pane::Actions;
        assert_eq!(
            dialog.key(KeyCode::Enter, false, false),
            DialogOutcome::Save
        );
        assert_eq!(dialog.dirty_row, None);

        // Re-landing on the edited row also clears it: the user has now
        // seen that the preview restarted from the saved values.
        let mut dialog = ConfigDialog::from_config(&two_servers());
        dialog.pane = Pane::Form;
        dialog.key(KeyCode::Char('x'), false, false);
        assert_eq!(dialog.dirty_row.as_deref(), Some("alpha"));
        dialog.pane = Pane::List;
        dialog.key(KeyCode::Down, false, false);
        dialog.key(KeyCode::Up, false, false);
        assert_eq!(dialog.selected.as_deref(), Some("alpha"));
        assert_eq!(dialog.dirty_row, None);
    }
}
