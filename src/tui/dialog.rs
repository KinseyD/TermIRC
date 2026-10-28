//! The in-TUI server configuration wizard: a modal form opened on first
//! run (no `config.toml` yet, or one without servers) and by `/config`.
//!
//! The dialog owns only form state — plain `Draft` fields plus a cursor —
//! and stays free of terminal I/O: `key` maps a crossterm key code to a
//! state change, the renderer reads the fields, and `collect` turns them
//! into a `ServerConfig` (validating with the same rules `Config::parse`
//! enforces). Saving is orchestrated by `tui::events`, which knows the
//! config path and the connection map.

use crate::config::{Config, ServerConfig};
use crate::core::Draft;
use ratatui::crossterm::event::KeyCode;

/// Index of the pseudo-row below the last field: the Save action.
pub const SAVE_ROW: usize = 9;

/// How each field edits: typed text or a boolean toggled in place.
/// Port and channel syntax are ordinary text here; [`ConfigDialog::collect`]
/// validates them.
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
}

/// The modal configuration form plus its cursor and validation error.
#[derive(Debug, Clone)]
pub struct ConfigDialog {
    /// Rows in display order; `SAVE_ROW` indexes the Save action.
    pub fields: Vec<Field>,
    /// The focused row: `0..fields.len()` is a field, `SAVE_ROW` is Save.
    pub cursor: usize,
    /// The i18n key of the failed `collect` validation, if any.
    pub error: Option<&'static str>,
    /// The configured server label this dialog was opened on, when any:
    /// a saved dialog with a different label adds a new entry and keeps
    /// the old one untouched.
    pub editing: Option<String>,
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
    /// A blank form with sane defaults for a first connection.
    pub fn new() -> Self {
        Self {
            fields: vec![
                Field::text("dlg.field_label", "libera"),
                Field::text("dlg.field_host", ""),
                Field::text("dlg.field_port", "6697"),
                Field::text("dlg.field_nickname", ""),
                Field::text("dlg.field_username", ""),
                Field {
                    label_key: "dlg.field_tls",
                    kind: FieldKind::Toggle,
                    text: Draft::default(),
                    flag: true,
                },
                Field::text("dlg.field_channels", ""),
                Field::text("dlg.field_sasl_user", ""),
                Field::text("dlg.field_sasl_pass", ""),
            ],
            cursor: 0,
            error: None,
            editing: None,
        }
    }
    /// A form prefilled from the config's first server (the wizard edits
    /// one entry at a time; multi-server management is out of scope).
    /// Keys the form does not model — password, queries, proxy — are
    /// preserved verbatim on save by the config writer.
    pub fn from_config(config: &Config) -> Self {
        let mut dialog = Self::new();
        if let Some((name, server)) = config.first_server() {
            dialog.editing = Some(name.to_string());
            dialog.fields[0] = Field::text("dlg.field_label", name);
            dialog.fields[1] = Field::text("dlg.field_host", &server.server);
            dialog.fields[2] = Field::text("dlg.field_port", &server.port.to_string());
            dialog.fields[3] = Field::text("dlg.field_nickname", &server.nickname);
            dialog.fields[4] = Field::text("dlg.field_username", &server.username);
            dialog.fields[5].flag = server.use_tls;
            let channels = server.channels.join(", ");
            dialog.fields[6] = Field::text("dlg.field_channels", &channels);
            let sasl_user = server.sasl_username.clone().unwrap_or_default();
            dialog.fields[7] = Field::text("dlg.field_sasl_user", &sasl_user);
            let sasl_pass = server.sasl_password.clone().unwrap_or_default();
            dialog.fields[8] = Field::text("dlg.field_sasl_pass", &sasl_pass);
        }
        dialog
    }
    fn field(&mut self) -> Option<&mut Field> {
        self.fields.get_mut(self.cursor)
    }

    /// Route one key press; the dialog consumes every key while open.
    pub fn key(&mut self, code: KeyCode, ctrl: bool) -> DialogOutcome {
        if ctrl {
            // Ctrl+C closes the dialog instead of quitting: an accidental
            // interrupt must not take the whole client down.
            return if matches!(code, KeyCode::Char('c')) {
                DialogOutcome::Cancel
            } else {
                DialogOutcome::Edited
            };
        }
        match code {
            KeyCode::Esc => DialogOutcome::Cancel,
            KeyCode::Tab | KeyCode::Down | KeyCode::Enter if self.cursor == SAVE_ROW => {
                if matches!(code, KeyCode::Enter) {
                    self.error = None;
                    DialogOutcome::Save
                } else {
                    self.cursor = 0;
                    DialogOutcome::Edited
                }
            }
            KeyCode::Tab | KeyCode::Down => {
                // Navigation keys always advance, even on the toggle.
                self.cursor += 1;
                DialogOutcome::Edited
            }
            KeyCode::Enter => {
                // Enter on a toggle flips it; on other fields it steps
                // down, matching common form behavior.
                if let Some(field) = self.field() {
                    if field.kind == FieldKind::Toggle {
                        field.flag = !field.flag;
                    } else {
                        self.cursor += 1;
                    }
                }
                DialogOutcome::Edited
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                DialogOutcome::Edited
            }
            _ => {
                if let Some(field) = self.field() {
                    match (field.kind, code) {
                        (FieldKind::Toggle, KeyCode::Char(' ')) => field.flag = !field.flag,
                        (FieldKind::Toggle, _) => {}
                        (FieldKind::Text, KeyCode::Char(c)) if !c.is_control() => {
                            field.text.insert(c);
                        }
                        (FieldKind::Text, KeyCode::Backspace) => field.text.backspace(),
                        (FieldKind::Text, KeyCode::Delete) => field.text.delete(),
                        (FieldKind::Text, KeyCode::Left) => field.text.left(),
                        (FieldKind::Text, KeyCode::Right) => field.text.right(),
                        (FieldKind::Text, KeyCode::Home) => field.text.home(),
                        (FieldKind::Text, KeyCode::End) => field.text.end(),
                        (FieldKind::Text, _) => {}
                    }
                }
                DialogOutcome::Edited
            }
        }
    }

    /// Validate the form into `(label, ServerConfig)`, or the i18n key of
    /// the first violated rule. The returned entry carries empty password
    /// and queries: the config writer's merge preserves those keys from
    /// an existing section instead of erasing them.
    pub fn collect(&self) -> Result<(String, ServerConfig), &'static str> {
        let label = self.fields[0].text.text().trim();
        let host = self.fields[1].text.text().trim();
        let port = self.fields[2].text.text().trim();
        let nickname = self.fields[3].text.text().trim();
        let username = self.fields[4].text.text().trim();
        let use_tls = self.fields[5].flag;
        let channels_field = self.fields[6].text.text();
        let sasl_user = self.fields[7].text.text().trim();
        let sasl_pass = self.fields[8].text.text();

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
        let channels: Vec<String> = channels_field
            .split([',', ' '])
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect();
        if !channels
            .iter()
            .all(|channel| crate::core::valid_channel_name(channel))
        {
            return Err("err.cfg_channels");
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
                password: String::new(),
                server: host.to_string(),
                use_tls,
                port,
                channels,
                sasl_username,
                sasl_password,
                queries: Vec::new(),
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

    fn filled_dialog() -> ConfigDialog {
        let mut dialog = ConfigDialog::new();
        dialog.fields[0].text.restore("libera".into(), 6);
        dialog.fields[1].text.restore("irc.libera.chat".into(), 15);
        dialog.fields[3].text.restore("me".into(), 2);
        dialog.fields[6].text.restore("#a, #b".into(), 6);
        dialog
    }

    #[test]
    fn collect_applies_defaults_and_validates_every_rule() {
        for (index, text, expected) in [
            (0, "", "dlg.err_label"),
            (0, "a b", "dlg.err_label"),
            (0, "a.b", "dlg.err_label"),
            (0, "a\"b", "dlg.err_label"),
            (1, "", "dlg.err_host"),
            (2, "", "dlg.err_port"),
            (2, "70000", "dlg.err_port"),
            (2, "0", "dlg.err_port"),
            (3, "", "dlg.err_nickname"),
            (3, "two words", "dlg.err_nickname"),
            (6, "chan", "err.cfg_channels"),
            (6, "#a password", "err.cfg_channels"),
            (7, "acc", "err.cfg_sasl_pair"),
            (8, "secret", "err.cfg_sasl_pair"),
        ] {
            let mut dialog = filled_dialog();
            dialog.fields[index]
                .text
                .restore(text.to_string(), text.chars().count());
            assert_eq!(
                dialog.collect().unwrap_err(),
                expected,
                "field {index} = {text:?}"
            );
        }
    }

    #[test]
    fn collect_fills_username_from_nickname_and_keeps_defaults() {
        // Arrange: nickname only; username left blank.
        let dialog = filled_dialog();

        // Act
        let (label, server) = dialog.collect().unwrap();

        // Assert: the blank username defaults to the nickname, TLS stays
        // on, and unmodeled keys come back empty for the merge to fill.
        assert_eq!(label, "libera");
        assert_eq!(server.username, "me");
        assert_eq!(server.server, "irc.libera.chat");
        assert_eq!(server.port, 6697);
        assert!(server.use_tls);
        assert_eq!(server.channels, vec!["#a".to_string(), "#b".to_string()]);
        assert_eq!(server.password, "");
        assert!(server.sasl_username.is_none());
    }

    #[test]
    fn collect_accepts_a_full_sasl_pair_and_rejects_oversized_credentials() {
        let mut dialog = filled_dialog();
        dialog.fields[7].text.restore("account".into(), 7);
        dialog.fields[8].text.restore("secret".into(), 6);
        let (_, server) = dialog.collect().unwrap();
        assert_eq!(server.sasl_username.as_deref(), Some("account"));

        let mut oversized = filled_dialog();
        oversized.fields[7].text.restore("u".repeat(280), 280);
        oversized.fields[8].text.restore("p".repeat(280), 280);
        assert_eq!(oversized.collect().unwrap_err(), "err.cfg_sasl_too_long");
    }

    #[test]
    fn key_routes_navigation_edits_and_actions() {
        // Arrange
        let mut dialog = ConfigDialog::new();

        // Act / Assert: typing edits the first field.
        assert_eq!(dialog.key(KeyCode::Char('x'), false), DialogOutcome::Edited);
        assert_eq!(dialog.fields[0].text.text(), "liberax");
        // Tab steps through the fields to the Save row.
        for _ in 0..9 {
            dialog.key(KeyCode::Tab, false);
        }
        assert_eq!(dialog.cursor, SAVE_ROW);
        // Tab past Save wraps to the first field.
        dialog.key(KeyCode::Tab, false);
        assert_eq!(dialog.cursor, 0);
        // Up at the first field stays put (movement clamps, no wrap).
        dialog.key(KeyCode::Up, false);
        assert_eq!(dialog.cursor, 0);
        // The toggle flips with Space instead of typing.
        dialog.cursor = 5;
        dialog.key(KeyCode::Char(' '), false);
        assert!(!dialog.fields[5].flag);
        // Enter on Save requests a save.
        dialog.cursor = SAVE_ROW;
        assert_eq!(dialog.key(KeyCode::Enter, false), DialogOutcome::Save);
        // Esc and Ctrl+C cancel; other ctrl keys are ignored.
        assert_eq!(dialog.key(KeyCode::Esc, false), DialogOutcome::Cancel);
        assert_eq!(dialog.key(KeyCode::Char('c'), true), DialogOutcome::Cancel);
        assert_eq!(dialog.key(KeyCode::Char('x'), true), DialogOutcome::Edited);
    }

    #[test]
    fn key_edits_the_focused_text_field_without_touching_the_others() {
        let mut dialog = filled_dialog();
        dialog.cursor = 1;
        dialog.key(KeyCode::End, false);
        dialog.key(KeyCode::Char('s'), false);
        dialog.key(KeyCode::Backspace, false);
        assert_eq!(dialog.fields[1].text.text(), "irc.libera.chat");
        dialog.key(KeyCode::Left, false);
        dialog.key(KeyCode::Delete, false);
        assert_eq!(dialog.fields[1].text.text(), "irc.libera.cha");
        assert_eq!(dialog.fields[0].text.text(), "libera");
    }

    #[test]
    fn from_config_prefills_the_first_server_and_remembers_its_label() {
        let config = crate::config::Config::parse(
            r##"
[servers.Existing]
username = "me"
nickname = "mynick"
password = "keepme"
server = "irc.example.org"
use_tls = false
port = 6667
channels = ["#one", "#two"]
queries = ["alice"]
"##,
        )
        .unwrap();

        let dialog = ConfigDialog::from_config(&config);

        assert_eq!(dialog.editing.as_deref(), Some("Existing"));
        assert_eq!(dialog.fields[0].text.text(), "Existing");
        assert_eq!(dialog.fields[1].text.text(), "irc.example.org");
        assert_eq!(dialog.fields[2].text.text(), "6667");
        assert_eq!(dialog.fields[3].text.text(), "mynick");
        assert!(!dialog.fields[5].flag);
        assert_eq!(dialog.fields[6].text.text(), "#one, #two");
        // collect round-trips what the form models.
        let (_, server) = dialog.collect().unwrap();
        assert_eq!(server.port, 6667);
        assert!(!server.use_tls);
        assert_eq!(
            server.channels,
            vec!["#one".to_string(), "#two".to_string()]
        );
    }
}
