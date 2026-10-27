//! Color theming: the renderer's color roles, configurable via `[theme]`.
//!
//! A [`Theme`] is a complete palette covering every color role the renderer
//! uses; the default preset is exactly the palette that was previously
//! hardcoded in `render.rs`, so a config without a `[theme]` table renders
//! byte-identically. Roles are addressed by snake_case names in TOML and each
//! value is either a named ANSI-16 color (`"red"`, `"bright-black"`, ...), a
//! 256-color index (`0-255`), or an `"#rrggbb"` hex string.

use ratatui::style::Color;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Every theme role name, in documentation order.
pub const ROLE_NAMES: &[&str] = &[
    "nick",
    "dim",
    "separator",
    "global_bg",
    "input_bg",
    "input_cursor_fg",
    "input_cursor_bg",
    "accent",
    "accent_dim",
    "sidebar_active_bg",
    "sidebar_cursor_bg",
    "message_select_bg",
    "hover_bg",
    "status_ok",
    "status_err",
    "status_pending",
    "status_bar_highlight",
];

/// A complete color palette for the renderer: one [`Color`] per role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Message author prefix ("alice: ").
    pub nick: Color,
    /// Secondary text: channel names, query labels, hint descriptions.
    pub dim: Color,
    /// The thin `│` line separating the sidebar from the main column, and
    /// the hint bar's rule.
    pub separator: Color,
    /// Global background of the whole screen.
    pub global_bg: Color,
    /// Background of the composer panel.
    pub input_bg: Color,
    /// Foreground of the composer's text-cursor block.
    pub input_cursor_fg: Color,
    /// Background of the composer's text-cursor block.
    pub input_cursor_bg: Color,
    /// The composer's `┃` accent (and the welcome logo) while focused.
    pub accent: Color,
    /// The accent while another region holds the focus.
    pub accent_dim: Color,
    /// Background of the sidebar's active channel row.
    pub sidebar_active_bg: Color,
    /// Background of the sidebar row under the cursor.
    pub sidebar_cursor_bg: Color,
    /// Background highlight of the selected message.
    pub message_select_bg: Color,
    /// Background of the hovered (pre-selected) message or sidebar row.
    pub hover_bg: Color,
    /// Sidebar status dot: connected.
    pub status_ok: Color,
    /// Sidebar status dot: stopped / not joined / uncertain.
    pub status_err: Color,
    /// Sidebar status dot: connecting / joining / parting.
    pub status_pending: Color,
    /// Background of the focused segment in the bottom status bar.
    pub status_bar_highlight: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            nick: Color::Cyan,
            dim: Color::Rgb(0x70, 0x70, 0x70),
            separator: Color::Rgb(0x55, 0x55, 0x55),
            global_bg: Color::Rgb(0x0a, 0x0a, 0x0a),
            input_bg: Color::Rgb(0x1e, 0x1e, 0x1e),
            input_cursor_fg: Color::Rgb(0x1e, 0x1e, 0x1e),
            input_cursor_bg: Color::White,
            accent: Color::Rgb(0x9a, 0xd9, 0x9a),
            accent_dim: Color::Rgb(0x4a, 0x6a, 0x4a),
            sidebar_active_bg: Color::Rgb(0x2e, 0x2e, 0x2e),
            sidebar_cursor_bg: Color::Rgb(0x20, 0x20, 0x20),
            message_select_bg: Color::Rgb(0x24, 0x24, 0x24),
            hover_bg: Color::Rgb(0x1a, 0x1a, 0x1a),
            status_ok: Color::Green,
            status_err: Color::Red,
            status_pending: Color::Gray,
            status_bar_highlight: Color::Rgb(0x9a, 0xd9, 0x9a),
        }
    }
}

impl Theme {
    /// Set the role `name` to `color`; `false` when the name is not a role.
    pub fn set(&mut self, name: &str, color: Color) -> bool {
        match name {
            "nick" => self.nick = color,
            "dim" => self.dim = color,
            "separator" => self.separator = color,
            "global_bg" => self.global_bg = color,
            "input_bg" => self.input_bg = color,
            "input_cursor_fg" => self.input_cursor_fg = color,
            "input_cursor_bg" => self.input_cursor_bg = color,
            "accent" => self.accent = color,
            "accent_dim" => self.accent_dim = color,
            "sidebar_active_bg" => self.sidebar_active_bg = color,
            "sidebar_cursor_bg" => self.sidebar_cursor_bg = color,
            "message_select_bg" => self.message_select_bg = color,
            "hover_bg" => self.hover_bg = color,
            "status_ok" => self.status_ok = color,
            "status_err" => self.status_err = color,
            "status_pending" => self.status_pending = color,
            "status_bar_highlight" => self.status_bar_highlight = color,
            _ => return false,
        }
        true
    }

    /// The color of role `name`, or `None` when the name is not a role.
    pub fn get(&self, name: &str) -> Option<Color> {
        match name {
            "nick" => Some(self.nick),
            "dim" => Some(self.dim),
            "separator" => Some(self.separator),
            "global_bg" => Some(self.global_bg),
            "input_bg" => Some(self.input_bg),
            "input_cursor_fg" => Some(self.input_cursor_fg),
            "input_cursor_bg" => Some(self.input_cursor_bg),
            "accent" => Some(self.accent),
            "accent_dim" => Some(self.accent_dim),
            "sidebar_active_bg" => Some(self.sidebar_active_bg),
            "sidebar_cursor_bg" => Some(self.sidebar_cursor_bg),
            "message_select_bg" => Some(self.message_select_bg),
            "hover_bg" => Some(self.hover_bg),
            "status_ok" => Some(self.status_ok),
            "status_err" => Some(self.status_err),
            "status_pending" => Some(self.status_pending),
            "status_bar_highlight" => Some(self.status_bar_highlight),
            _ => None,
        }
    }
}

/// One raw `[theme]` value from configuration: a string or a 256-color index.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ThemeColor {
    Text(String),
    Index(i64),
}

impl ThemeColor {
    /// Parse into a ratatui [`Color`], with a machine-readable reason on
    /// failure: `invalid_theme_color: <role> = <value>: ...`.
    pub fn parse(&self, role: &str) -> anyhow::Result<Color> {
        let invalid = |what: &str| {
            anyhow::anyhow!(
                "invalid_theme_color: {role} = {what}: expected \"#rrggbb\", \
                 a 256-color index (0-255), or an ANSI-16 name such as \"red\", \
                 \"bright-black\", or \"white\""
            )
        };
        match self {
            ThemeColor::Index(index) => u8::try_from(*index)
                .map(Color::Indexed)
                .map_err(|_| invalid(&format!("{index}: 256-color index must be 0-255"))),
            ThemeColor::Text(text) => {
                let name = text.trim();
                if let Some(hex) = name.strip_prefix('#') {
                    return parse_hex(hex)
                        .map(|(r, g, b)| Color::Rgb(r, g, b))
                        .ok_or_else(|| invalid(&format!("\"{text}\"")));
                }
                parse_ansi16(name).ok_or_else(|| invalid(&format!("\"{text}\"")))
            }
        }
    }
}

/// Parse six hex digits into an RGB triple.
fn parse_hex(hex: &str) -> Option<(u8, u8, u8)> {
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let pair = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).ok();
    Some((pair(0..2)?, pair(2..4)?, pair(4..6)?))
}

/// Map an ANSI-16 color name onto a ratatui color.
fn parse_ansi16(name: &str) -> Option<Color> {
    let color = match name.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::Gray,
        "bright-black" => Color::DarkGray,
        "bright-red" => Color::LightRed,
        "bright-green" => Color::LightGreen,
        "bright-yellow" => Color::LightYellow,
        "bright-blue" => Color::LightBlue,
        "bright-magenta" => Color::LightMagenta,
        "bright-cyan" => Color::LightCyan,
        "bright-white" => Color::White,
        _ => return None,
    };
    Some(color)
}

/// The `[theme]` config table: a map of role names to color values. Absent
/// roles keep their default.
#[derive(Debug, Default, Deserialize)]
pub struct ThemeTable {
    #[serde(flatten)]
    pub roles: BTreeMap<String, ThemeColor>,
}

impl ThemeTable {
    /// Resolve the table into a complete [`Theme`]: start from the default
    /// preset and apply each configured role. Unknown role names and invalid
    /// color values fail with machine-readable reasons.
    pub fn build(&self) -> anyhow::Result<Theme> {
        let mut theme = Theme::default();
        for (role, raw) in &self.roles {
            let color = raw.parse(role)?;
            anyhow::ensure!(
                theme.set(role, color),
                "unknown_theme_role: {role} (valid roles: {})",
                ROLE_NAMES.join(", ")
            );
        }
        Ok(theme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_rgb_colors() {
        assert_eq!(
            ThemeColor::Text("#0a0a0a".into())
                .parse("global_bg")
                .unwrap(),
            Color::Rgb(0x0a, 0x0a, 0x0a)
        );
        assert_eq!(
            ThemeColor::Text("#9AD99A".into()).parse("accent").unwrap(),
            Color::Rgb(0x9a, 0xd9, 0x9a)
        );
    }

    #[test]
    fn parses_ansi16_names_including_bright_forms() {
        let cases = [
            ("red", Color::Red),
            ("cyan", Color::Cyan),
            ("white", Color::Gray),
            ("black", Color::Black),
            ("bright-black", Color::DarkGray),
            ("bright-white", Color::White),
            ("BRIGHT-GREEN", Color::LightGreen),
        ];
        for (name, expected) in cases {
            assert_eq!(
                ThemeColor::Text(name.into()).parse("dim").unwrap(),
                expected,
                "name {name}"
            );
        }
    }

    #[test]
    fn parses_256_color_indices() {
        assert_eq!(
            ThemeColor::Index(0).parse("dim").unwrap(),
            Color::Indexed(0)
        );
        assert_eq!(
            ThemeColor::Index(255).parse("dim").unwrap(),
            Color::Indexed(255)
        );
    }

    #[test]
    fn rejects_invalid_colors_with_machine_readable_reasons() {
        for (raw, fragment) in [
            (ThemeColor::Text("#12345".into()), "#12345"),
            (ThemeColor::Text("#12345g".into()), "#12345g"),
            (ThemeColor::Text("redd".into()), "redd"),
            (ThemeColor::Text("".into()), "\"\""),
            (ThemeColor::Index(-1), "-1"),
            (ThemeColor::Index(256), "256"),
        ] {
            let error = raw.parse("hover_bg").unwrap_err().to_string();
            assert!(
                error.starts_with("invalid_theme_color: hover_bg = "),
                "reason for {fragment:?}: {error}"
            );
            assert!(error.contains(fragment), "reason: {error}");
        }
    }

    #[test]
    fn build_rejects_unknown_roles_with_machine_readable_reason() {
        let table = table(&[("sidebar_read", ThemeColor::Text("red".into()))]);
        let error = table.build().unwrap_err().to_string();
        assert!(
            error.starts_with("unknown_theme_role: sidebar_read"),
            "reason: {error}"
        );
        assert!(error.contains("sidebar_active_bg"), "reason: {error}");
    }

    #[test]
    fn build_applies_overrides_on_top_of_the_default_preset() {
        let table = ThemeTable {
            roles: [
                ("accent".to_string(), ThemeColor::Text("#3366ff".into())),
                ("status_ok".to_string(), ThemeColor::Index(40)),
                (
                    "status_err".to_string(),
                    ThemeColor::Text("bright-red".into()),
                ),
            ]
            .into_iter()
            .collect(),
        };
        let theme = table.build().unwrap();
        assert_eq!(theme.accent, Color::Rgb(0x33, 0x66, 0xff));
        assert_eq!(theme.status_ok, Color::Indexed(40));
        assert_eq!(theme.status_err, Color::LightRed);
        // Untouched roles keep the default preset.
        assert_eq!(theme.global_bg, Theme::default().global_bg);
    }

    #[test]
    fn default_preset_matches_the_previous_hardcoded_render_palette() {
        let theme = Theme::default();
        let expected: [(&str, Color); ROLE_NAMES.len()] = [
            ("nick", Color::Cyan),
            ("dim", Color::Rgb(0x70, 0x70, 0x70)),
            ("separator", Color::Rgb(0x55, 0x55, 0x55)),
            ("global_bg", Color::Rgb(0x0a, 0x0a, 0x0a)),
            ("input_bg", Color::Rgb(0x1e, 0x1e, 0x1e)),
            ("input_cursor_fg", Color::Rgb(0x1e, 0x1e, 0x1e)),
            ("input_cursor_bg", Color::White),
            ("accent", Color::Rgb(0x9a, 0xd9, 0x9a)),
            ("accent_dim", Color::Rgb(0x4a, 0x6a, 0x4a)),
            ("sidebar_active_bg", Color::Rgb(0x2e, 0x2e, 0x2e)),
            ("sidebar_cursor_bg", Color::Rgb(0x20, 0x20, 0x20)),
            ("message_select_bg", Color::Rgb(0x24, 0x24, 0x24)),
            ("hover_bg", Color::Rgb(0x1a, 0x1a, 0x1a)),
            ("status_ok", Color::Green),
            ("status_err", Color::Red),
            ("status_pending", Color::Gray),
            ("status_bar_highlight", Color::Rgb(0x9a, 0xd9, 0x9a)),
        ];
        assert_eq!(expected.len(), ROLE_NAMES.len());
        for (name, color) in expected {
            assert_eq!(theme.get(name), Some(color), "role {name}");
        }
    }

    #[test]
    fn every_role_name_round_trips_through_set_and_get() {
        for name in ROLE_NAMES {
            let mut theme = Theme::default();
            assert!(theme.set(name, Color::Indexed(200)), "role {name}");
            assert_eq!(theme.get(name), Some(Color::Indexed(200)));
        }
        assert!(!theme_roundtrip_helper("no_such_role"));
    }

    fn theme_roundtrip_helper(name: &str) -> bool {
        let mut theme = Theme::default();
        theme.set(name, Color::Indexed(200))
    }

    fn table(entries: &[(&str, ThemeColor)]) -> ThemeTable {
        ThemeTable {
            roles: entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }
}
