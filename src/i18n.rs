//! User-visible string catalog: every translatable text with its compiled-in
//! default and an optional file overlay per configured language.
//!
//! The default language (English) is embedded in [`REGISTRY`] so the catalog
//! works with no files on disk and the default rendering is byte-identical to
//! the pre-i18n literals, which the test suite pins. Shipped translations are
//! likewise embedded via [`BUILTIN_LOCALES`] (`include_str!`), so a released
//! binary needs no locale files at all. A `[ui] language` other than `"en"`
//! loads the embedded locale when one matches; a `locales/<language>.toml`
//! file found on the disk search paths (beside the binary, in the working
//! directory, and in the config directory) OVERRIDES the embedded copy, so
//! power users can customize a translation without rebuilding. Any unknown,
//! missing, malformed, or partial overlay falls back to the embedded
//! defaults with one warning per problem, logged when the catalog is
//! initialized — lookups on the render path never warn and never panic.
//!
//! Templates use named brace parameters (`{server}`), substituted by
//! [`format`]. An overlay entry is only accepted when its parameter set
//! matches the default's, so a translation can neither drop a parameter nor
//! invent one; everything else is a load-time warning plus fallback.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::OnceLock;

/// The language whose strings are embedded in [`REGISTRY`]. Configured
/// `language` values equal to it load no file at all.
pub const DEFAULT_LANGUAGE: &str = "en";

/// Every translatable string as `(key, default text)`, grouped by surface
/// (`cmd.` hint descriptions, `hint.` chrome, `sys.` system notes, `status.`
/// composer status, `statusbar.` the bottom status bar, `err.` errors).
/// The order is the canonical key order and must stay grouped and stable:
/// missing-key warnings and the completeness test report keys in it.
const REGISTRY: &[(&str, &str)] = &[
    // cmd.* — the slash-command hint bar descriptions, one per command name.
    ("cmd.join", "Join or focus a channel on the current server"),
    (
        "cmd.part",
        "Leave the named or current channel, keeping its buffer",
    ),
    (
        "cmd.query",
        "Open or reopen a private conversation and focus it",
    ),
    ("cmd.close", "Hide the current private conversation"),
    ("cmd.msg", "Send a message without switching conversation"),
    ("cmd.me", "Send a CTCP ACTION to the current conversation"),
    (
        "cmd.nick",
        "Request a nickname change on the current server",
    ),
    (
        "cmd.topic",
        "View the topic of the named or current channel",
    ),
    ("cmd.names", "List members of the named or current channel"),
    (
        "cmd.mode",
        "Query or change channel or user modes, passed through",
    ),
    ("cmd.whois", "Query who a nickname is on the current server"),
    ("cmd.away", "Set an away message; no argument toggles"),
    ("cmd.back", "Clear away status on the current server"),
    (
        "cmd.connect",
        "Connect a stopped server, defaulting to the current one",
    ),
    (
        "cmd.reconnect",
        "Restart the connection, or connect if stopped",
    ),
    (
        "cmd.disconnect",
        "Disconnect the current server and stop retries",
    ),
    (
        "cmd.quit",
        "Alias for disconnect; the application stays open",
    ),
    ("cmd.config", "Open the server configuration wizard"),
    // hint.* — standing TUI chrome. Key names (Esc, PgUp/PgDn) stay verbatim
    // in every language.
    ("hint.tips", "Esc quit · Enter send · PgUp/PgDn scroll"),
    // welcome.* — the empty welcome page (no buffer viewed).
    ("welcome.config_hint", "Press c to configure a server"),
    // dlg.* — the modal setup wizard (first run and /config).
    ("dlg.title", "Server configuration"),
    ("dlg.new_server", "new server"),
    ("dlg.page_connection", "connection"),
    ("dlg.page_identity", "identity"),
    ("dlg.page_channels", "channels"),
    (
        "dlg.hub_hint",
        "enter save · ↑/↓ fields · ←/→ panes · pgup/pgdn page · esc cancel",
    ),
    ("dlg.field_label", "Name"),
    ("dlg.field_host", "Server"),
    ("dlg.field_port", "Port"),
    ("dlg.field_tls", "TLS"),
    ("dlg.field_nickname", "Nickname"),
    ("dlg.field_username", "Username"),
    ("dlg.field_password", "Server password"),
    ("dlg.field_sasl_user", "SASL username"),
    ("dlg.field_sasl_pass", "SASL password"),
    ("dlg.field_channels", "Channels"),
    ("dlg.field_queries", "Queries"),
    ("dlg.on", "on"),
    ("dlg.off", "off"),
    ("dlg.help_connection", "Name, host, port, and TLS"),
    ("dlg.help_identity", "Nickname, username, password, SASL"),
    ("dlg.help_channels", "Channels and queries at startup"),
    (
        "dlg.err_label",
        "the name must be non-empty without spaces or . \" ' [ ] # =",
    ),
    ("dlg.err_host", "the server address must not be empty"),
    (
        "dlg.err_port",
        "the port must be a number between 1 and 65535",
    ),
    (
        "dlg.err_nickname",
        "the nickname must be non-empty without spaces",
    ),
    // sys.* — system notes appended to buffers.
    ("sys.connected", "Connected to {server}"),
    (
        "sys.connection_lost",
        "Connection lost, reconnecting to {server}",
    ),
    ("sys.disconnected", "Disconnected from {server}"),
    ("sys.restored_one", "Restored {n} message through {cutoff}"),
    (
        "sys.restored_many",
        "Restored {n} messages through {cutoff}",
    ),
    ("sys.kicked", "Kicked from {channel}: {reason}"),
    (
        "sys.kicked_no_reason",
        "Kicked from {channel}: no reason given",
    ),
    (
        "sys.channel_timeout",
        "{op} timed out; channel membership is unconfirmed",
    ),
    // status.* — composer status messages (shown on the tips row).
    ("status.send_failed", "failed to send ({reason})"),
    (
        "status.send_failed_disconnected",
        "failed to send ({server} disconnected or busy)",
    ),
    (
        "status.query_target_changed",
        "query target changed; use /query to select a nickname",
    ),
    ("status.config_saved", "configuration saved to {path}"),
    // statusbar.* — the bottom status bar. The server and channel blocks
    // show the raw names (pure data); only the input block's standing
    // label is localized.
    ("statusbar.input", "type in"),
    // err.send_* — SendError variants, surfaced via status.send_failed.
    (
        "err.send_invalid_characters",
        "message contains an invalid control character",
    ),
    ("err.send_invalid_command", "invalid IRC command"),
    (
        "err.send_too_long",
        "message exceeds the 512-byte IRC line limit",
    ),
    // err.cfg_* — configuration load/validation errors.
    (
        "err.cfg_server_name_conflict",
        "server names {previous} and {name} conflict (ASCII case-insensitive)",
    ),
    (
        "err.cfg_channels",
        "channels must contain individual # or & channel names without passwords",
    ),
    (
        "err.cfg_queries",
        "queries must contain individual nicknames, not channels or multiple targets",
    ),
    (
        "err.cfg_proxy_type",
        "unsupported_proxy_type: only \"socks5\" is supported, got {type}",
    ),
    (
        "err.cfg_proxy_host",
        "proxy_host_required: proxy.host must be a non-empty hostname",
    ),
    (
        "err.cfg_proxy_port",
        "proxy_port_out_of_range: proxy.port must be between 1 and 65535",
    ),
    (
        "err.cfg_sasl_pair",
        "sasl_requires_username_and_password: set both sasl_username and sasl_password, or neither",
    ),
    (
        "err.cfg_sasl_characters",
        "sasl credentials must not contain NUL, CR, or LF characters",
    ),
    (
        "err.cfg_sasl_too_long",
        "sasl_credentials_too_long: encoded SASL PLAIN payload would exceed 400 bytes",
    ),
    ("err.cfg_read", "failed to read {path}"),
    ("err.cfg_parse", "failed to parse {path}"),
    ("err.cfg_write", "failed to write {path}"),
    // err.startup_* — startup failures printed before the TUI exists.
    ("err.startup_home", "could not resolve the home directory"),
    (
        "err.startup_config_load",
        "failed to load config from {path} - copy your config file there (e.g. config.example.toml)",
    ),
    ("err.startup_mouse", "failed to enable mouse capture"),
];

/// Shipped translations embedded in the binary with `include_str!`, so a
/// released binary needs no locale files on disk. Each entry's key set must
/// exactly equal [`REGISTRY`]'s, which the test suite pins. A locale file
/// found on the disk search paths overrides the embedded copy.
const BUILTIN_LOCALES: &[(&str, &str)] = &[("zh-CN", include_str!("../locales/zh-CN.toml"))];
static CATALOG: OnceLock<Catalog> = OnceLock::new();

/// A validated locale overlay: accepted entries only. Entries are keyed and
/// borrowed as `&'static str` through the [`OnceLock`], so rendering an
/// overlaid string allocates nothing beyond what the caller's own formatting
/// needs.
#[derive(Debug, Default)]
pub struct Catalog {
    overrides: HashMap<String, String>,
}

impl Catalog {
    /// Resolve the overlay for `language` by searching the locale paths.
    ///
    /// Never fails: any problem (unknown language, unreadable file, parse
    /// error, partial coverage) yields an empty-or-partial catalog plus one
    /// warning per problem for [`init`] to log.
    pub fn load(language: &str) -> (Catalog, Vec<String>) {
        Self::load_with_roots(language, locale_paths(language))
    }

    /// [`Self::load`] against explicit roots; the seam tests use to isolate
    /// the catalog from the real disk search paths.
    fn load_with_roots(language: &str, paths: Vec<PathBuf>) -> (Catalog, Vec<String>) {
        if language == DEFAULT_LANGUAGE {
            return (Catalog::default(), Vec::new());
        }
        // The language doubles as a file name; refuse anything that could
        // escape the locales directory rather than guess at intent.
        if language.is_empty()
            || language.contains(['/', '\\', '\0'])
            || language == "."
            || language == ".."
        {
            return (
                Catalog::default(),
                vec![format!(
                    "language {language:?} is not a locale tag; using {DEFAULT_LANGUAGE}"
                )],
            );
        }
        for path in paths {
            match std::fs::read_to_string(&path) {
                // A file on disk overrides the embedded copy: power users
                // can customize a shipped translation without rebuilding.
                Ok(content) => {
                    let (catalog, warnings) = Self::parse(language, &content);
                    return (catalog, warnings);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return (
                        Catalog::default(),
                        vec![format!(
                            "locale file {} could not be read ({e}); using {DEFAULT_LANGUAGE}",
                            path.display()
                        )],
                    );
                }
            }
        }
        // No file on disk: fall back to the embedded translation when the
        // language ships with the binary.
        if let Some((_, content)) = BUILTIN_LOCALES.iter().find(|(tag, _)| *tag == language) {
            return Self::parse(language, content);
        }
        (
            Catalog::default(),
            vec![format!(
                "no locale file {language}.toml found; using {DEFAULT_LANGUAGE}"
            )],
        )
    }

    /// Parse and validate one locale file's contents against [`REGISTRY`].
    pub(super) fn parse(language: &str, content: &str) -> (Catalog, Vec<String>) {
        let mut flat = BTreeMap::new();
        let mut warnings = Vec::new();
        match toml::from_str::<toml::Table>(content) {
            Ok(table) => flatten_table("", table, &mut flat, &mut warnings),
            Err(e) => {
                return (
                    Catalog::default(),
                    vec![format!(
                        "locale {language} is not valid TOML ({e}); using {DEFAULT_LANGUAGE}"
                    )],
                );
            }
        }
        let mut overrides = HashMap::new();
        for (key, value) in &flat {
            let Some(default) = default_text(key) else {
                warnings.push(format!(
                    "locale {language} has unknown key {key:?}; it is ignored"
                ));
                continue;
            };
            if parameter_names(default) == parameter_names(value) {
                overrides.insert(key.clone(), value.clone());
            } else {
                warnings.push(format!(
                    "locale {language} key {key:?} changes the template parameters; using {DEFAULT_LANGUAGE}"
                ));
            }
        }
        for (key, _) in REGISTRY {
            if !flat.contains_key(*key) {
                warnings.push(format!(
                    "locale {language} is missing key {key:?}; using {DEFAULT_LANGUAGE}"
                ));
            }
        }
        (Catalog { overrides }, warnings)
    }

    /// The overlay text for `key`, if this catalog accepted one.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.overrides.get(key).map(String::as_str)
    }
}

/// Directories searched for `locales/<language>.toml`, in order: beside the
/// running binary (installed layout), the working directory (running from a
/// checkout), and the config directory (user-supplied translations).
fn locale_paths(language: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        roots.push(dir.join("locales"));
    }
    roots.push(PathBuf::from("locales"));
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".config").join("termirc").join("locales"));
    }
    roots
        .into_iter()
        .map(|dir| dir.join(format!("{language}.toml")))
        .collect()
}

/// Flatten nested TOML tables into dotted keys; non-string leaves are warned
/// about and dropped, keeping the accepted set simply `key -> text`.
fn flatten_table(
    prefix: &str,
    table: toml::Table,
    out: &mut BTreeMap<String, String>,
    warnings: &mut Vec<String>,
) {
    for (name, value) in table {
        let key = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}.{name}")
        };
        match value {
            toml::Value::String(text) => {
                out.insert(key, text);
            }
            toml::Value::Table(inner) => flatten_table(&key, inner, out, warnings),
            _ => warnings.push(format!(
                "locale value at {key:?} is not a string; it is ignored"
            )),
        }
    }
}

/// The compiled-in default for `key`, or `None` for keys the registry does
/// not know. Linear scan: the registry is small and this runs once per
/// rendered string at most.
fn default_text(key: &str) -> Option<&'static str> {
    REGISTRY
        .iter()
        .find(|(registered, _)| *registered == key)
        .map(|(_, text)| *text)
}

/// The brace-parameter names of `template` in sorted order, so templates
/// compare by parameter set rather than by wording.
fn parameter_names(template: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{')
        && let Some(rel) = rest[open..].find('}')
    {
        let close = open + rel;
        let name = rest[open + 1..close].to_string();
        if !names.contains(&name) {
            names.push(name);
        }
        rest = &rest[close + 1..];
    }
    names.sort();
    names
}

/// Resolve `key` to the text to display: the overlay entry when the catalog
/// has one, otherwise the embedded default. Unknown keys render as the empty
/// string (they cannot occur from this crate's call sites, which use
/// registry literals only).
pub fn t(key: &str) -> &'static str {
    if let Some(catalog) = CATALOG.get()
        && let Some(text) = catalog.get(key)
    {
        return text;
    }
    default_text(key).unwrap_or_default()
}

/// Substitute named brace parameters (`{server}`) in `template`. Parameters
/// absent from `params` stay verbatim; load-time validation already warned
/// about such cases, so rendering stays silent.
pub fn format(template: &str, params: &[(&str, &str)]) -> String {
    if params.is_empty() {
        return template.to_string();
    }
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    'scan: while let Some(open) = rest.find('{') {
        let Some(rel) = rest[open..].find('}') else {
            break;
        };
        let close = open + rel;
        let name = &rest[open + 1..close];
        for &(key, value) in params {
            if key == name {
                out.push_str(&rest[..open]);
                out.push_str(value);
                rest = &rest[close + 1..];
                continue 'scan;
            }
        }
        out.push_str(&rest[..close + 1]);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Load the overlay for `language` into the process-wide catalog and log one
/// warning per problem. Called once from `main` before the TUI starts.
pub fn init(language: &str) {
    let (catalog, warnings) = Catalog::load(language);
    for warning in &warnings {
        tracing::warn!(target: "termirc::i18n", "{warning}");
    }
    let _ = CATALOG.set(catalog);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry is the single source of default strings; this table is a
    /// deliberate copy of the literals as they appear at their original call
    /// sites, pinning the byte-identity the rest of the suite relies on.
    #[test]
    fn default_registry_renders_the_pinned_literals() {
        let pinned: &[(&str, &str)] = &[
            ("cmd.join", "Join or focus a channel on the current server"),
            (
                "cmd.part",
                "Leave the named or current channel, keeping its buffer",
            ),
            (
                "cmd.query",
                "Open or reopen a private conversation and focus it",
            ),
            ("cmd.close", "Hide the current private conversation"),
            ("cmd.msg", "Send a message without switching conversation"),
            ("cmd.me", "Send a CTCP ACTION to the current conversation"),
            (
                "cmd.nick",
                "Request a nickname change on the current server",
            ),
            (
                "cmd.topic",
                "View the topic of the named or current channel",
            ),
            ("cmd.names", "List members of the named or current channel"),
            (
                "cmd.mode",
                "Query or change channel or user modes, passed through",
            ),
            ("cmd.whois", "Query who a nickname is on the current server"),
            ("cmd.away", "Set an away message; no argument toggles"),
            ("cmd.back", "Clear away status on the current server"),
            (
                "cmd.connect",
                "Connect a stopped server, defaulting to the current one",
            ),
            (
                "cmd.reconnect",
                "Restart the connection, or connect if stopped",
            ),
            (
                "cmd.disconnect",
                "Disconnect the current server and stop retries",
            ),
            (
                "cmd.quit",
                "Alias for disconnect; the application stays open",
            ),
            ("cmd.config", "Open the server configuration wizard"),
            ("hint.tips", "Esc quit · Enter send · PgUp/PgDn scroll"),
            ("welcome.config_hint", "Press c to configure a server"),
            ("dlg.title", "Server configuration"),
            ("dlg.new_server", "new server"),
            ("dlg.page_connection", "connection"),
            ("dlg.page_identity", "identity"),
            ("dlg.page_channels", "channels"),
            (
                "dlg.hub_hint",
                "enter save · ↑/↓ fields · ←/→ panes · pgup/pgdn page · esc cancel",
            ),
            ("dlg.field_label", "Name"),
            ("dlg.field_host", "Server"),
            ("dlg.field_port", "Port"),
            ("dlg.field_tls", "TLS"),
            ("dlg.field_nickname", "Nickname"),
            ("dlg.field_username", "Username"),
            ("dlg.field_password", "Server password"),
            ("dlg.field_sasl_user", "SASL username"),
            ("dlg.field_sasl_pass", "SASL password"),
            ("dlg.field_channels", "Channels"),
            ("dlg.field_queries", "Queries"),
            ("dlg.on", "on"),
            ("dlg.off", "off"),
            ("dlg.help_connection", "Name, host, port, and TLS"),
            ("dlg.help_identity", "Nickname, username, password, SASL"),
            ("dlg.help_channels", "Channels and queries at startup"),
            (
                "dlg.err_label",
                "the name must be non-empty without spaces or . \" ' [ ] # =",
            ),
            ("dlg.err_host", "the server address must not be empty"),
            (
                "dlg.err_port",
                "the port must be a number between 1 and 65535",
            ),
            (
                "dlg.err_nickname",
                "the nickname must be non-empty without spaces",
            ),
            ("sys.connected", "Connected to {server}"),
            (
                "sys.connection_lost",
                "Connection lost, reconnecting to {server}",
            ),
            ("sys.disconnected", "Disconnected from {server}"),
            ("sys.restored_one", "Restored {n} message through {cutoff}"),
            (
                "sys.restored_many",
                "Restored {n} messages through {cutoff}",
            ),
            ("sys.kicked", "Kicked from {channel}: {reason}"),
            (
                "sys.kicked_no_reason",
                "Kicked from {channel}: no reason given",
            ),
            (
                "sys.channel_timeout",
                "{op} timed out; channel membership is unconfirmed",
            ),
            ("status.send_failed", "failed to send ({reason})"),
            (
                "status.send_failed_disconnected",
                "failed to send ({server} disconnected or busy)",
            ),
            (
                "status.query_target_changed",
                "query target changed; use /query to select a nickname",
            ),
            ("status.config_saved", "configuration saved to {path}"),
            ("statusbar.input", "type in"),
            (
                "err.send_invalid_characters",
                "message contains an invalid control character",
            ),
            ("err.send_invalid_command", "invalid IRC command"),
            (
                "err.send_too_long",
                "message exceeds the 512-byte IRC line limit",
            ),
            (
                "err.cfg_server_name_conflict",
                "server names {previous} and {name} conflict (ASCII case-insensitive)",
            ),
            (
                "err.cfg_channels",
                "channels must contain individual # or & channel names without passwords",
            ),
            (
                "err.cfg_queries",
                "queries must contain individual nicknames, not channels or multiple targets",
            ),
            (
                "err.cfg_proxy_type",
                "unsupported_proxy_type: only \"socks5\" is supported, got {type}",
            ),
            (
                "err.cfg_proxy_host",
                "proxy_host_required: proxy.host must be a non-empty hostname",
            ),
            (
                "err.cfg_proxy_port",
                "proxy_port_out_of_range: proxy.port must be between 1 and 65535",
            ),
            (
                "err.cfg_sasl_pair",
                "sasl_requires_username_and_password: set both sasl_username and sasl_password, or neither",
            ),
            (
                "err.cfg_sasl_characters",
                "sasl credentials must not contain NUL, CR, or LF characters",
            ),
            (
                "err.cfg_sasl_too_long",
                "sasl_credentials_too_long: encoded SASL PLAIN payload would exceed 400 bytes",
            ),
            ("err.cfg_read", "failed to read {path}"),
            ("err.cfg_parse", "failed to parse {path}"),
            ("err.cfg_write", "failed to write {path}"),
            ("err.startup_home", "could not resolve the home directory"),
            (
                "err.startup_config_load",
                "failed to load config from {path} - copy your config file there (e.g. config.example.toml)",
            ),
            ("err.startup_mouse", "failed to enable mouse capture"),
        ];
        assert_eq!(REGISTRY.len(), pinned.len(), "registry and pin table sizes");
        for (key, literal) in pinned {
            assert_eq!(default_text(key), Some(*literal), "default for {key}");
            // Without an initialized catalog `t` borrows the same default,
            // which is how every other test module sees the default locale.
            assert_eq!(t(key), *literal, "t() default for {key}");
        }
    }

    #[test]
    fn registry_keys_are_unique() {
        let mut keys: Vec<_> = REGISTRY.iter().map(|(key, _)| *key).collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "duplicate registry keys");
    }

    /// The fullscreen hub retired the modal's header/footer rows; their
    /// keys must stay gone so no dead catalog entries accumulate.
    #[test]
    fn the_registry_carries_no_keys_the_hub_retired() {
        for dead in ["dlg.servers", "dlg.save", "dlg.cancel", "dlg.page_hint"] {
            assert!(
                REGISTRY.iter().all(|(key, _)| *key != dead),
                "retired key {dead} is back in the registry"
            );
        }
        // The hub's footer hint is pinned in both locales.
        assert_eq!(
            REGISTRY
                .iter()
                .find(|(key, _)| *key == "dlg.hub_hint")
                .map(|(_, value)| *value),
            Some("enter save · ↑/↓ fields · ←/→ panes · pgup/pgdn page · esc cancel")
        );
        let zh = BUILTIN_LOCALES
            .iter()
            .find(|(tag, _)| *tag == "zh-CN")
            .unwrap()
            .1;
        assert!(
            zh.contains(
                "hub_hint = \"enter 保存 · ↑/↓ 字段 · ←/→ 切栏 · pgup/pgdn 翻页 · esc 取消\""
            ),
            "zh-CN hub hint literal changed"
        );
    }

    /// The command table's second column must reference exactly the `cmd.*`
    /// registry keys — every command keeps a description and no key goes
    /// stale when either list changes.
    #[test]
    fn command_table_keys_mirror_the_cmd_registry() {
        let table: Vec<_> = crate::command::COMMANDS
            .iter()
            .map(|(name, key)| (*name, *key))
            .collect();
        let registry: Vec<_> = REGISTRY
            .iter()
            .filter(|(key, _)| key.starts_with("cmd."))
            .map(|(key, _)| *key)
            .collect();
        assert_eq!(
            table.iter().map(|(_, key)| *key).collect::<Vec<_>>(),
            registry,
            "COMMANDS keys must equal the cmd.* registry keys in order"
        );
        for (name, key) in &table {
            assert!(!t(key).is_empty(), "command {name} has no text for {key}");
        }
    }

    #[test]
    fn format_substitutes_named_parameters() {
        assert_eq!(
            format("Connected to {server}", &[("server", "srv")]),
            "Connected to srv"
        );
        assert_eq!(
            format(
                "Restored {n} messages through {cutoff}",
                &[("n", "3"), ("cutoff", "2026-10-22T20:14:43Z")]
            ),
            "Restored 3 messages through 2026-10-22T20:14:43Z"
        );
        // Parameter order in the call does not matter.
        assert_eq!(format("a {x} b {y}", &[("y", "2"), ("x", "1")]), "a 1 b 2");
    }

    #[test]
    fn format_keeps_unknown_and_unmatched_braces_verbatim() {
        assert_eq!(format("{keep}", &[("other", "v")]), "{keep}");
        assert_eq!(format("trailing {open", &[("open", "v")]), "trailing {open");
        assert_eq!(format("{}", &[]), "{}");
        assert_eq!(format("no params", &[("unused", "v")]), "no params");
    }

    #[test]
    fn format_without_parameters_returns_the_template() {
        assert_eq!(
            format("Esc quit · Enter send · PgUp/PgDn scroll", &[]),
            "Esc quit · Enter send · PgUp/PgDn scroll"
        );
    }

    fn warnings_matching(warnings: &[String], needle: &str) -> usize {
        warnings.iter().filter(|w| w.contains(needle)).count()
    }

    #[test]
    fn full_overlay_is_accepted_without_warnings() {
        let content =
            "[sys]\nconnected = \"已连接到 {server}\"\ndisconnected = \"已从 {server} 断开\"\n";
        // One group only: every other registry key is missing, so parse must
        // report exactly those, and none about the two accepted entries.
        let (catalog, warnings) = Catalog::parse("zh-CN", content);
        assert_eq!(catalog.get("sys.connected"), Some("已连接到 {server}"));
        assert_eq!(catalog.get("sys.disconnected"), Some("已从 {server} 断开"));
        assert_eq!(catalog.get("cmd.join"), None);
        assert_eq!(warnings.len(), REGISTRY.len() - 2);
        assert_eq!(
            warnings_matching(&warnings, "missing key"),
            REGISTRY.len() - 2
        );
        assert_eq!(warnings_matching(&warnings, "unknown key"), 0);
    }

    #[test]
    fn unknown_and_malformed_entries_are_dropped_with_one_warning_each() {
        let content = "\
[sys]
connected = \"已连接到 {server}\"
kicked = \"被踢出 {channel}（{reason}）\"
kicked_no_reason = \"被踢出（{reason}）\"
[hint]
missing_key_here = \"x\"
";
        let (catalog, warnings) = Catalog::parse("xx", content);
        // `sys.kicked_no_reason` has no parameters; renaming one to take a
        // parameter changes the template contract and must fall back.
        assert_eq!(catalog.get("sys.kicked_no_reason"), None);
        assert_eq!(
            warnings_matching(&warnings, "unknown key \"hint.missing_key_here\""),
            1
        );
        assert_eq!(
            warnings_matching(
                &warnings,
                "key \"sys.kicked_no_reason\" changes the template parameters"
            ),
            1
        );
    }

    #[test]
    fn invalid_toml_falls_back_wholly_with_one_warning() {
        let (catalog, warnings) = Catalog::parse("xx", "not [ valid toml");
        assert_eq!(catalog.get("sys.connected"), None);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not valid TOML"));
    }

    #[test]
    fn non_string_leaves_are_warned_and_ignored() {
        let content = "[sys]\nconnected = 7\n";
        let (catalog, warnings) = Catalog::parse("xx", content);
        assert_eq!(catalog.get("sys.connected"), None);
        assert_eq!(warnings_matching(&warnings, "is not a string"), 1);
    }

    #[test]
    fn default_language_loads_no_file_and_warns_about_nothing() {
        let (catalog, warnings) = Catalog::load(DEFAULT_LANGUAGE);
        assert_eq!(catalog.get("sys.connected"), None);
        assert!(warnings.is_empty());
    }

    #[test]
    fn path_traversal_languages_are_refused() {
        for language in ["", ".", "..", "../en", "a/b", "a\\b"] {
            let (catalog, warnings) = Catalog::load(language);
            assert_eq!(catalog.get("sys.connected"), None, "{language:?}");
            assert_eq!(warnings.len(), 1, "{language:?}: {warnings:?}");
            assert!(warnings[0].contains("not a locale tag"), "{language:?}");
        }
    }

    #[test]
    fn unknown_language_reports_one_warning() {
        // No locales file can exist for a tag this improbable; the loader
        // must not panic and must warn exactly once.
        let (catalog, warnings) = Catalog::load("zz-Nowhere-9999");
        assert!(catalog.get("sys.connected").is_none());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("no locale file"));
    }

    #[test]
    fn locale_file_is_read_from_disk_and_validated() {
        let dir = std::env::temp_dir().join(format!("termirc-i18n-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("xx-test.toml");
        std::fs::write(&path, "[sys]\ndisconnected = \"wylogowano z {server}\"\n").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let (catalog, _warnings) = Catalog::parse("xx-test", &content);
        assert_eq!(
            catalog.get("sys.disconnected"),
            Some("wylogowano z {server}")
        );
        assert_eq!(
            format(
                catalog.get("sys.disconnected").unwrap(),
                &[("server", "srv")]
            ),
            "wylogowano z srv"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The shipped zh-CN translation must cover the whole registry with no
    /// warnings: every key present, no unknown keys, and every entry's
    /// parameter set matching its default. This is the completeness
    /// contract that keeps the file from drifting behind the registry, and
    /// it now applies to every [`BUILTIN_LOCALES`] entry the binary embeds.
    #[test]
    fn zh_cn_locale_covers_the_whole_registry_without_warnings() {
        assert!(!BUILTIN_LOCALES.is_empty(), "at least one builtin locale");
        for (language, content) in BUILTIN_LOCALES {
            let (catalog, warnings) = Catalog::parse(language, content);
            assert!(
                warnings.is_empty(),
                "{language} load warnings: {warnings:?}"
            );
            for (key, _) in REGISTRY {
                assert!(catalog.get(key).is_some(), "{language} is missing {key}");
                assert!(
                    !catalog.get(key).is_some_and(str::is_empty),
                    "{language} has empty text for {key}"
                );
            }
        }
        // The loader must also find the file through its real search path
        // (cargo runs tests from the package root, where locales/ lives).
        let (loaded, load_warnings) = Catalog::load("zh-CN");
        assert!(load_warnings.is_empty(), "load warnings: {load_warnings:?}");
        assert!(loaded.get("sys.connected").is_some());
    }

    /// A released binary ships without locale files: the embedded zh-CN
    /// must activate even when every disk search root is empty, with no
    /// warning at all.
    #[test]
    fn embedded_locale_activates_without_any_disk_file() {
        let empty = std::env::temp_dir().join(format!("termirc-i18n-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty); // deliberately nonexistent
        let (catalog, warnings) =
            Catalog::load_with_roots("zh-CN", vec![empty.join("locales").join("zh-CN.toml")]);
        assert!(warnings.is_empty(), "embedded load warnings: {warnings:?}");
        assert_eq!(catalog.get("sys.connected"), Some("已连接到 {server}"));
        assert_eq!(
            catalog.get("cmd.join"),
            Some("加入或聚焦当前服务器上的频道")
        );
    }

    /// A locale file on a disk search path overrides the embedded copy for
    /// the same language: on-disk files are per-install customization, not
    /// a requirement.
    #[test]
    fn disk_locale_file_overrides_the_embedded_copy() {
        let dir = std::env::temp_dir().join(format!("termirc-i18n-ovr-{}", std::process::id()));
        let locales = dir.join("locales");
        std::fs::create_dir_all(&locales).unwrap();
        let customized = BUILTIN_LOCALES
            .iter()
            .find(|(tag, _)| *tag == "zh-CN")
            .map(|(_, content)| {
                content.replace(
                    "connected = \"已连接到 {server}\"",
                    "connected = \"已连接到服务器 {server}\"",
                )
            })
            .unwrap();
        std::fs::write(locales.join("zh-CN.toml"), customized).unwrap();

        let (catalog, warnings) =
            Catalog::load_with_roots("zh-CN", vec![locales.join("zh-CN.toml")]);
        assert!(warnings.is_empty(), "override load warnings: {warnings:?}");
        assert_eq!(
            catalog.get("sys.connected"),
            Some("已连接到服务器 {server}")
        );
        // Every other key still comes from the (customized copy of the)
        // file, and unknown text cannot appear out of nowhere.
        assert_eq!(
            catalog.get("cmd.join"),
            Some("加入或聚焦当前服务器上的频道")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Translated templates substitute parameters exactly like the defaults:
    /// the {server} slot takes the label and the surrounding CJK text stays
    /// intact (width-aware rendering is the layout layer's concern).
    #[test]
    fn zh_cn_parameters_substitute_into_translated_templates() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales/zh-CN.toml");
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let (catalog, _warnings) = Catalog::parse("zh-CN", &content);
        let connected = catalog.get("sys.connected").unwrap();
        let rendered = format(connected, &[("server", "srv")]);
        assert!(rendered.contains("srv"), "rendered: {rendered:?}");
        assert!(rendered.contains('连'), "rendered: {rendered:?}");
        assert!(!rendered.contains('{') && !rendered.contains('}'));

        let kicked = catalog.get("sys.kicked").unwrap();
        assert_eq!(
            format(kicked, &[("channel", "#osu"), ("reason", "spam")]),
            "被踢出 #osu：spam"
        );
    }
}
