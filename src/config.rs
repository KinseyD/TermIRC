//! Configuration loading for termirc.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context as _;
use indexmap::IndexMap;
use serde::Deserialize;

use crate::core::{ServerId, valid_channel_name, valid_query_nickname};
use crate::i18n;
use crate::theme::ThemeTable;

/// Per-server connection settings, as written in the `[servers.<name>]` tables.
#[derive(Clone, Deserialize)]
pub struct ServerConfig {
    pub username: String,
    pub nickname: String,
    #[serde(default)]
    pub password: String,
    pub server: String,
    #[serde(default)]
    pub use_tls: bool,
    pub port: u16,
    pub channels: Vec<String>,
    #[serde(default)]
    pub sasl_username: Option<String>,
    #[serde(default)]
    pub sasl_password: Option<String>,
    #[serde(default)]
    pub queries: Vec<String>,
    /// Optional SOCKS5 proxy (e.g. a local Tor node); absent means direct.
    #[serde(default)]
    pub proxy: Option<ProxyConfig>,
}

/// Optional per-server SOCKS5 proxy settings, from `[servers.<name>.proxy]`.
#[derive(Clone, Deserialize)]
pub struct ProxyConfig {
    /// Only `"socks5"` is supported (covers Tor via `socks5://127.0.0.1:9050`).
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub host: String,
    /// `0` means "not configured"; validated to 1-65535 in `Config::parse`.
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

impl std::fmt::Debug for ProxyConfig {
    /// The proxy password is a credential — never include it in debug output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("type", &self.r#type)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl std::fmt::Debug for ServerConfig {
    /// The password is a credential — never include it in debug output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("username", &self.username)
            .field("nickname", &self.nickname)
            .field("password", &"<redacted>")
            .field("server", &self.server)
            .field("use_tls", &self.use_tls)
            .field("port", &self.port)
            .field("channels", &self.channels)
            .field("sasl_username", &self.sasl_username)
            .field("sasl_password", &"<redacted>")
            .field("queries", &self.queries)
            .field("proxy", &self.proxy)
            .finish()
    }
}

impl ServerConfig {
    /// The first channel in the configured list, if any.
    pub fn first_channel(&self) -> Option<&str> {
        self.channels.first().map(String::as_str)
    }
}

/// Root configuration: a map of named servers.
///
/// `IndexMap` (with toml's `preserve_order`) keeps the order in which servers
/// Persistent-history settings from the optional `[history]` table.
///
/// Persistence is opt-in (`persist = false` by default) so existing
/// configurations keep their in-memory-only behavior.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    pub persist: bool,
    pub retention_days: u64,
    /// Bare names exclude a target on every server; `"server/name"` entries
    /// are scoped to one server. Matching is ASCII case-insensitive, like
    /// `BufferKind::matches`.
    pub exclude: Vec<String>,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            persist: false,
            retention_days: 7,
            exclude: Vec::new(),
        }
    }
}

impl HistoryConfig {
    pub fn excludes(&self, server: &str, name: &str) -> bool {
        self.exclude
            .iter()
            .any(|entry| match entry.split_once('/') {
                Some((scoped, target)) => {
                    scoped.eq_ignore_ascii_case(server) && target.eq_ignore_ascii_case(name)
                }
                None => entry.eq_ignore_ascii_case(name),
            })
    }
}

/// Interface settings from the optional `[ui]` table.
///
/// Parsed apart from [`Config`] and consumed exactly once, by `main`, to
/// initialize the process-wide i18n catalog before anything renders: no
/// consumer ever needs the language afterwards, so it does not travel with
/// the connection model.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// UI language tag selecting a `locales/<language>.toml` overlay;
    /// `"en"` (the default) uses the embedded strings and loads no file.
    pub language: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            language: crate::i18n::DEFAULT_LANGUAGE.to_string(),
        }
    }
}

/// Read the `[ui]` table of the configuration file at `path`.
///
/// A missing file, missing table, or malformed value yields the default
/// language: locale problems degrade to English rather than fail startup
/// (a file too broken to parse here has already failed `Config::load`).
pub fn load_ui(path: &Path) -> UiConfig {
    std::fs::read_to_string(path)
        .map(|content| parse_ui(&content))
        .unwrap_or_default()
}

fn parse_ui(content: &str) -> UiConfig {
    #[derive(Deserialize)]
    struct UiTables {
        #[serde(default)]
        ui: UiConfig,
    }
    toml::from_str::<UiTables>(content)
        .map(|tables| tables.ui)
        .unwrap_or_default()
}

/// Root configuration: a map of named servers.
///
/// `IndexMap` (with toml's `preserve_order`) keeps the order in which servers
/// appear in the file, so `first_server` means "first written", not alphabetical.
#[derive(Debug, Deserialize)]
pub struct Config {
    pub servers: IndexMap<String, ServerConfig>,
    #[serde(default)]
    pub history: HistoryConfig,
    /// Optional color-theme overrides: role names mapped to colors.
    #[serde(default)]
    pub theme: ThemeTable,
}

impl Config {
    /// Parse configuration from a TOML string.
    pub fn parse(content: &str) -> anyhow::Result<Config> {
        let config: Config = toml::from_str(content)?;
        // Reject bad theme roles/colors here, at load time, so the errors are
        // machine-readable configuration errors rather than runtime surprises.
        config.theme.build()?;
        let mut names = HashMap::new();
        for name in config.servers.keys() {
            if let Some(previous) = names.insert(ServerId::new(name), name) {
                anyhow::bail!(
                    "{}",
                    i18n::format(
                        i18n::t("err.cfg_server_name_conflict"),
                        &[
                            ("previous", &format!("{previous:?}")),
                            ("name", &format!("{name:?}")),
                        ],
                    )
                );
            }
        }
        for server in config.servers.values() {
            anyhow::ensure!(
                server
                    .channels
                    .iter()
                    .all(|channel| valid_channel_name(channel)),
                "{}",
                i18n::t("err.cfg_channels")
            );
            anyhow::ensure!(
                server
                    .queries
                    .iter()
                    .all(|nickname| valid_query_nickname(nickname)),
                "{}",
                i18n::t("err.cfg_queries")
            );
            if let Some(proxy) = &server.proxy {
                anyhow::ensure!(
                    proxy.r#type == "socks5",
                    "{}",
                    i18n::format(
                        i18n::t("err.cfg_proxy_type"),
                        &[("type", &format!("{:?}", proxy.r#type))],
                    )
                );
                anyhow::ensure!(
                    !proxy.host.trim().is_empty(),
                    "{}",
                    i18n::t("err.cfg_proxy_host")
                );
                anyhow::ensure!(
                    (1..=65535).contains(&proxy.port),
                    "{}",
                    i18n::t("err.cfg_proxy_port")
                );
            }
            let sasl = match (&server.sasl_username, &server.sasl_password) {
                (Some(username), Some(password)) => Some((username, password)),
                (None, None) => None,
                _ => anyhow::bail!("{}", i18n::t("err.cfg_sasl_pair")),
            };
            if let Some((username, password)) = sasl {
                anyhow::ensure!(
                    !username.contains(['\0', '\r', '\n'])
                        && !password.contains(['\0', '\r', '\n']),
                    "{}",
                    i18n::t("err.cfg_sasl_characters")
                );
                // AUTHENTICATE carries at most 400 base64 characters per
                // line; refusing longer payloads keeps SASL PLAIN unchunked.
                let payload = 2 + username.len() + password.len();
                anyhow::ensure!(
                    4 * payload.div_ceil(3) <= 400,
                    "{}",
                    i18n::t("err.cfg_sasl_too_long")
                );
            }
        }
        Ok(config)
    }

    /// Load and parse configuration from a file path.
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        // Deliberately not localized (nor routed through the catalog with a
        // live override): the configured language lives inside this very
        // file, so when it cannot be read the locale is unknowable and the
        // embedded English default is the only honest rendering. The
        // catalog key is kept so the message stays with its siblings.
        let content = std::fs::read_to_string(path).with_context(|| {
            i18n::format(
                i18n::t("err.cfg_read"),
                &[("path", &path.display().to_string())],
            )
        })?;
        Config::parse(&content).with_context(|| {
            i18n::format(
                i18n::t("err.cfg_parse"),
                &[("path", &path.display().to_string())],
            )
        })
    }

    /// The first server in file order, with its name.
    pub fn first_server(&self) -> Option<(&str, &ServerConfig)> {
        self.servers
            .iter()
            .next()
            .map(|(name, server)| (name.as_str(), server))
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            servers: IndexMap::new(),
            history: HistoryConfig::default(),
            theme: ThemeTable::default(),
        }
    }
}

/// The `[servers.<name>]` keys the setup wizard owns. `save_server`
/// replaces exactly these inside the target section and leaves every
/// other key (password, queries, proxy, user comments) verbatim, so
/// re-running the wizard can never destroy what it does not model.
const WIZARD_KEYS: &[&str] = &[
    "username",
    "nickname",
    "server",
    "use_tls",
    "port",
    "channels",
    "sasl_username",
    "sasl_password",
];

/// Escape a TOML basic string: backslash, quote, and control characters
/// (as `\uXXXX`), so any user input round-trips through `Config::parse`.
fn escape_toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Emit one `[servers.<name>]` section in the style of
/// `config.example.toml`. The wizard-unmodeled keys (password, queries,
/// proxy) are omitted here; `save_server`'s merge keeps any existing
/// copies of them instead.
fn emit_server_table(name: &str, server: &ServerConfig) -> String {
    let mut out = format!("[servers.{name}]\n");
    let string = |key: &str, value: &str| format!("{key} = \"{}\"\n", escape_toml_string(value));
    out.push_str(&string("username", &server.username));
    out.push_str(&string("nickname", &server.nickname));
    out.push_str(&string("server", &server.server));
    out.push_str(&format!("use_tls = {}\n", server.use_tls));
    out.push_str(&format!("port = {}\n", server.port));
    let channels = server
        .channels
        .iter()
        .map(|channel| format!("\"{}\"", escape_toml_string(channel)))
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!("channels = [{channels}]\n"));
    if let (Some(username), Some(password)) = (&server.sasl_username, &server.sasl_password) {
        out.push_str(&string("sasl_username", username));
        out.push_str(&string("sasl_password", password));
    }
    out
}

/// The name inside a `[servers.<name>]` header line: a bare key or a
/// quoted basic string. `None` for any other line.
fn section_name(header: &str) -> Option<String> {
    let body = header.trim().strip_prefix("[servers.")?;
    let body = body.strip_suffix(']')?;
    let body = body.trim();
    if let Some(quoted) = body.strip_prefix('"') {
        let end = quoted.rfind('"')?;
        let inner = &quoted[..end];
        // Unescape the two escapes the emitter and TOML share.
        return Some(inner.replace("\\\"", "\"").replace("\\\\", "\\"));
    }
    Some(body.to_string())
}

/// Merge one emitted server section into existing config text, or
/// append it when no such section exists. Dotted-key spellings
/// (`servers.name.host = ...`) are not merged: they coexist as separate
/// keys, the post-merge re-parse then fails on the duplicated table,
/// and `save_server` refuses to write — never a half-updated file.
fn merge_server_section(existing: &str, name: &str, emitted: &str) -> String {
    let lines: Vec<&str> = existing.lines().collect();
    let mut section_start = None;
    let mut section_end = lines.len();
    let subtable_prefix = format!("[servers.{name}.");
    for (index, line) in lines.iter().enumerate() {
        let Some(found) = section_name(line) else {
            continue;
        };
        if found == name {
            section_start = Some(index);
            // The section runs until the next header that is not one of
            // its own sub-tables (`[servers.<name>.proxy]`, ...).
            section_end = lines[index + 1..]
                .iter()
                .position(|next| {
                    next.trim().starts_with('[') && !next.trim().starts_with(&subtable_prefix)
                })
                .map_or(lines.len(), |offset| index + 1 + offset);
            break;
        }
    }
    let Some(start) = section_start else {
        // No section yet: append at the end, keeping the file's own
        // trailing-newline shape.
        let separator = if existing.is_empty() { "" } else { "\n" };
        return format!("{existing}{separator}{emitted}");
    };
    let mut merged: Vec<String> = lines[..start].iter().map(|l| l.to_string()).collect();
    merged.push(lines[start].to_string());
    // The wizard-owned top-level keys live between the section header and
    // its first sub-table (proxy, ...); from that header on everything
    // stays verbatim, including keys that share a name with owned ones.
    let body_end = lines[start + 1..section_end]
        .iter()
        .position(|line| line.trim().starts_with('['))
        .map_or(section_end, |offset| start + 1 + offset);
    let mut replaced = false;
    for line in &lines[start + 1..body_end] {
        let key = line
            .split('=')
            .next()
            .map(str::trim)
            .filter(|key| !key.is_empty() && !key.contains('.'));
        if key.is_some_and(|key| WIZARD_KEYS.contains(&key)) {
            // The first owned key marks where the fresh block goes.
            if !replaced {
                merged.extend(emitted.lines().skip(1).map(str::to_string));
                replaced = true;
            }
        } else {
            merged.push(line.to_string());
        }
    }
    if !replaced {
        // A section with no owned keys yet (only unknown keys/comments):
        // insert the block right after the header.
        let block: Vec<String> = emitted.lines().skip(1).map(str::to_string).collect();
        let insert_at = merged.len();
        merged.splice(insert_at..insert_at, block);
    }
    merged.extend(lines[body_end..].iter().map(|l| l.to_string()));
    merged.join("\n") + "\n"
}

/// Persist one server entry at `path`, merging into any existing file.
///
/// The merged text is validated with [`Config::parse`] before anything
/// is written, so a wizard entry that contradicts the rest of the file
/// (a case-insensitive name clash, a broken proxy left behind, ...) is
/// rejected instead of producing a file the next start cannot load.
/// The write itself is atomic: a temporary file in the same directory,
/// then `rename` over the target.
pub fn save_server(path: &Path, name: &str, server: &ServerConfig) -> anyhow::Result<()> {
    // Only a genuinely missing file means "start from scratch". Treating
    // any other read failure (permissions, invalid UTF-8, a directory) as
    // an empty file would rewrite it with just the wizard's section and
    // silently lose everything else.
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| {
                i18n::format(
                    i18n::t("err.cfg_write"),
                    &[("path", &path.display().to_string())],
                )
            });
        }
    };
    let merged = merge_server_section(&existing, name, &emit_server_table(name, server));
    Config::parse(&merged).map_err(|error| {
        // Log only the top-level context: the toml error chain can embed
        // the offending source line, which may persist SASL passwords.
        tracing::warn!(target: "termirc::config", "rejected config merge: {}", error);
        anyhow::anyhow!("{}", i18n::t("err.cfg_write"))
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            i18n::format(
                i18n::t("err.cfg_write"),
                &[("path", &path.display().to_string())],
            )
        })?;
    }
    let temp = path.with_extension("toml.tmp");
    if let Err(error) = std::fs::write(&temp, &merged).and_then(|_| std::fs::rename(&temp, path)) {
        // Do not leave a partial or misleading `*.toml.tmp` behind when
        // the atomic write never landed.
        let _ = std::fs::remove_file(&temp);
        return Err(error).with_context(|| {
            i18n::format(
                i18n::t("err.cfg_write"),
                &[("path", &path.display().to_string())],
            )
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_toml(extra: &str) -> String {
        format!(
            "[servers.test]\nusername='me'\nnickname='me'\npassword=''\nserver='localhost'\nport=6667\nchannels=[]\n{extra}"
        )
    }

    #[test]
    fn sasl_credentials_must_be_set_together_and_bounded() {
        assert!(
            Config::parse(&server_toml(
                "sasl_username='jilles'\nsasl_password='sesame'\n"
            ))
            .is_ok()
        );
        for extra in [
            "sasl_username='jilles'\n",
            "sasl_password='sesame'\n",
            "sasl_username=\"a\\u0000b\"\nsasl_password=\"x\"\n",
            "sasl_username=\"a\"\nsasl_password=\"b\\u000d\"\n",
        ] {
            let error = Config::parse(&server_toml(extra)).unwrap_err().to_string();
            assert!(
                error.contains("sasl_requires_username_and_password")
                    || error.contains("NUL, CR, or LF"),
                "accepted {extra:?} ({error})"
            );
        }
        let long = "x".repeat(400);
        let error = Config::parse(&server_toml(&format!(
            "sasl_username='user'\nsasl_password='{long}'\n"
        )))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("sasl_credentials_too_long"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn history_table_is_optional_with_opt_in_defaults() {
        // Arrange/Act
        let default = Config::parse("[servers.s]\nusername=\"u\"\nnickname=\"n\"\npassword=\"\"\nserver=\"h\"\nport=1\nchannels=[]\n").unwrap();

        // Assert: opt-in persistence, seven-day retention, nothing excluded.
        assert_eq!(default.history, HistoryConfig::default());
        assert!(!default.history.persist);
        assert_eq!(default.history.retention_days, 7);
        assert!(default.history.exclude.is_empty());
    }

    #[test]
    fn history_table_parses_keys_and_excludes_match_case_insensitively() {
        // Arrange
        let config = Config::parse(
            "[servers.s]\nusername=\"u\"\nnickname=\"n\"\npassword=\"\"\nserver=\"h\"\nport=1\nchannels=[]\n\
             [history]\npersist=true\nretention_days=30\nexclude=[\"#Huge\", \"srv/alice\"]\n",
        )
        .unwrap();

        // Act/Assert: bare names exclude everywhere, scoped entries only
        // their server, both ASCII case-insensitively.
        assert!(config.history.persist);
        assert_eq!(config.history.retention_days, 30);
        assert!(config.history.excludes("other", "#huge"));
        assert!(config.history.excludes("SRV", "Alice"));
        assert!(!config.history.excludes("other", "alice"));
        assert!(!config.history.excludes("srv", "#other"));
    }

    #[test]
    fn debug_output_redacts_sasl_password() {
        let config = Config::parse(&server_toml(
            "sasl_username='jilles'\nsasl_password='sesame'\n",
        ))
        .unwrap();
        let rendered = format!("{:?}", config.servers["test"]);
        assert!(rendered.contains("sasl_username"));
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("sesame"));
    }

    #[test]
    fn rejects_unmanaged_configured_channels() {
        for channel in [
            "",
            "#",
            "room",
            "#one,#two",
            "#one key",
            "#a:b",
            "#bad\u{7}",
        ] {
            let content = format!(
                "[servers.test]\nusername='me'\nnickname='me'\npassword=''\nserver='localhost'\nport=6667\nchannels=[{channel:?}]\n"
            );
            assert!(Config::parse(&content).is_err(), "accepted {channel:?}");
        }
    }

    #[test]
    fn rejects_invalid_query_targets() {
        for nickname in [
            "",
            "alice bob",
            "alice,bob",
            "#room",
            "&room",
            "+room",
            "!room",
            "a\u{7}b",
        ] {
            let content = format!(
                "[servers.test]\nusername='me'\nnickname='me'\npassword='secret'\nserver='localhost'\nport=6667\nchannels=[]\nqueries=[{nickname:?}]\n"
            );
            assert!(Config::parse(&content).is_err(), "accepted {nickname:?}");
        }
    }

    #[test]
    fn first_server_returns_first_in_file_order_not_alphabetical() {
        // Arrange: "zeta" is written before "alpha" — alphabetical order would pick alpha.
        let content = r##"
[servers.zeta]
username = "u1"
nickname = "n1"
password = "p1"
server = "z.example.org"
port = 6667
channels = ["#z"]

[servers.alpha]
username = "u2"
nickname = "n2"
password = "p2"
server = "a.example.org"
port = 6667
channels = ["#a"]
"##;

        // Act
        let config = Config::parse(content).unwrap();

        // Assert
        let (name, server) = config.first_server().unwrap();
        assert_eq!(name, "zeta");
        assert_eq!(server.server, "z.example.org");
    }

    #[test]
    fn first_channel_returns_first_channel_of_server() {
        // Arrange
        let content = r##"
[servers.osu_irc]
username = "alice"
nickname = "alice"
password = "secret"
server = "irc.example.org"
port = 6667
channels = ["#osu", "#chinese"]
"##;
        let config = Config::parse(content).unwrap();

        // Act
        let channel = config.first_server().unwrap().1.first_channel();

        // Assert
        assert_eq!(channel, Some("#osu"));
    }

    #[test]
    fn parses_all_fields_of_test_schema() {
        // Arrange
        let content = r##"
[servers.osu_irc]
username = "alice"
nickname = "alice_"
password = "secret"
server = "irc.example.org"
use_tls = true
port = 6697
channels = ["#osu", "#chinese"]
"##;

        // Act
        let config = Config::parse(content).unwrap();

        // Assert
        let (name, srv) = config.first_server().unwrap();
        assert_eq!(name, "osu_irc");
        assert_eq!(srv.username, "alice");
        assert_eq!(srv.nickname, "alice_");
        assert_eq!(srv.password, "secret");
        assert_eq!(srv.server, "irc.example.org");
        assert!(srv.use_tls);
        assert_eq!(srv.port, 6697);
        assert_eq!(
            srv.channels,
            vec!["#osu".to_string(), "#chinese".to_string()]
        );
    }

    #[test]
    fn use_tls_defaults_to_false_when_absent() {
        // Arrange: no use_tls key.
        let content = r##"
[servers.plain]
username = "u"
nickname = "n"
password = "p"
server = "s.example.org"
port = 6667
channels = ["#c"]
"##;

        // Act
        let config = Config::parse(content).unwrap();

        // Assert
        assert!(!config.first_server().unwrap().1.use_tls);
    }

    #[test]
    fn first_channel_is_none_when_channels_empty() {
        // Arrange
        let content = r##"
[servers.empty]
username = "u"
nickname = "n"
password = "p"
server = "s.example.org"
port = 6667
channels = []
"##;
        let config = Config::parse(content).unwrap();

        // Act & Assert
        assert_eq!(config.first_server().unwrap().1.first_channel(), None);
    }

    #[test]
    fn first_server_is_none_when_no_servers() {
        // Arrange: servers table present but empty.
        let content = "[servers]\n";
        let config = Config::parse(content).unwrap();

        // Act & Assert
        assert!(config.first_server().is_none());
    }

    #[test]
    fn theme_table_is_absent_by_default() {
        // Arrange
        let content = server_toml("");

        // Act
        let config = Config::parse(&content).unwrap();

        // Assert
        assert!(config.theme.roles.is_empty());
        assert_eq!(
            config.theme.build().unwrap(),
            crate::theme::Theme::default()
        );
    }

    #[test]
    fn theme_table_parses_ansi_256_and_rgb_colors() {
        // Arrange
        let content =
            server_toml("\n[theme]\nnick = 'bright-cyan'\nstatus_ok = 40\nglobal_bg = '#0a0a0a'\n");

        // Act
        let config = Config::parse(&content).unwrap();
        let theme = config.theme.build().unwrap();

        // Assert: configured roles take the configured colors...
        use ratatui::style::Color;
        assert_eq!(theme.nick, Color::LightCyan);
        assert_eq!(theme.status_ok, Color::Indexed(40));
        assert_eq!(theme.global_bg, Color::Rgb(0x0a, 0x0a, 0x0a));
        // ...and unconfigured roles keep the default preset.
        assert_eq!(theme.hover_bg, crate::theme::Theme::default().hover_bg);
    }

    #[test]
    fn theme_table_rejects_unknown_roles_with_machine_readable_reason() {
        // Arrange
        let content = server_toml("\n[theme]\nsidebar_read = 'red'\n");

        // Act
        let error = Config::parse(&content).unwrap_err().to_string();

        // Assert
        assert!(
            error.contains("unknown_theme_role: sidebar_read"),
            "reason: {error}"
        );
    }

    #[test]
    fn theme_table_rejects_invalid_colors_with_machine_readable_reason() {
        // Arrange
        let content = server_toml("\n[theme]\naccent = 'not-a-color'\n");

        // Act
        let error = Config::parse(&content).unwrap_err().to_string();

        // Assert
        assert!(
            error.contains("invalid_theme_color: accent"),
            "reason: {error}"
        );
    }

    #[test]
    fn parse_errors_on_invalid_toml() {
        // Arrange
        let content = "this is [ not toml";
        let result = Config::parse(content);

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn load_errors_when_file_missing() {
        // Arrange
        let path = std::env::temp_dir()
            .join("termirc-no-such-dir")
            .join("config.toml");

        // Act
        let result = Config::load(&path);

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn debug_output_redacts_password() {
        // Arrange: the config holds a plaintext credential.
        let content = r##"
[servers.osu_irc]
username = "alice"
nickname = "alice"
password = "super-secret-token"
server = "irc.example.org"
port = 6667
channels = ["#osu"]
"##;
        let config = Config::parse(content).unwrap();

        // Act
        let debug = format!("{:?}", config.first_server().unwrap().1);

        // Assert: the token never appears; non-sensitive fields still do.
        assert!(
            !debug.contains("super-secret-token"),
            "password leaked: {debug}"
        );
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains("alice"));
        assert!(debug.contains("irc.example.org"));
    }

    #[test]
    fn parse_rejects_case_insensitive_server_name_collisions_without_credentials() {
        let content = r##"
[servers.Network]
username = "alice"
nickname = "alice"
password = "do-not-print-this-token"
server = "irc.example.org"
port = 6667
channels = ["#chat"]

[servers.network]
username = "bob"
nickname = "bob"
password = "second-private-token"
server = "other.example.org"
port = 6667
channels = ["#chat"]
"##;

        let error = Config::parse(content).expect_err("ambiguous server keys must be rejected");
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("Network"));
        assert!(diagnostic.contains("network"));
        assert!(!diagnostic.contains("do-not-print-this-token"));
        assert!(!diagnostic.contains("second-private-token"));
    }

    #[test]
    fn parses_valid_socks5_proxy_per_server() {
        // Arrange
        let content = server_toml(
            "\n[servers.test.proxy]\ntype = \"socks5\"\nhost = \"127.0.0.1\"\nport = 9050\n",
        );

        // Act
        let config = Config::parse(&content).unwrap();

        // Assert
        let proxy = config.servers["test"].proxy.as_ref().unwrap();
        assert_eq!(proxy.r#type, "socks5");
        assert_eq!(proxy.host, "127.0.0.1");
        assert_eq!(proxy.port, 9050);
        assert_eq!(proxy.username, None);
        assert_eq!(proxy.password, None);
    }

    #[test]
    fn server_without_proxy_table_parses_to_none() {
        // Arrange
        let content = server_toml("");

        // Act
        let config = Config::parse(&content).unwrap();

        // Assert
        assert!(config.servers["test"].proxy.is_none());
    }

    #[test]
    fn proxy_rejects_missing_host_unsupported_type_and_out_of_range_port() {
        // Arrange
        for extra in [
            "\n[servers.test.proxy]\ntype = \"socks5\"\nport = 9050\n",
            "\n[servers.test.proxy]\ntype = \"http\"\nhost = \"127.0.0.1\"\nport = 8080\n",
            "\n[servers.test.proxy]\ntype = \"socks5\"\nhost = \"127.0.0.1\"\nport = 0\n",
        ] {
            let content = server_toml(extra);

            // Act
            let result = Config::parse(&content);

            // Assert
            let error = result.expect_err("invalid proxy must be rejected");
            let reason = format!("{error:#}");
            assert!(
                reason.contains("unsupported_proxy_type")
                    || reason.contains("proxy_host_required")
                    || reason.contains("proxy_port_out_of_range"),
                "missing machine-readable reason: {reason}"
            );
        }
    }

    #[test]
    fn proxy_host_and_port_are_validated_with_exact_reasons() {
        // Arrange
        let missing_host =
            server_toml("\n[servers.test.proxy]\ntype = \"socks5\"\nhost = \"\"\nport = 9050\n");
        let bad_port = server_toml(
            "\n[servers.test.proxy]\ntype = \"socks5\"\nhost = \"127.0.0.1\"\nport = 65536\n",
        );
        let bad_type = server_toml(
            "\n[servers.test.proxy]\ntype = \"socks4\"\nhost = \"127.0.0.1\"\nport = 1080\n",
        );

        // Act & Assert
        let error = Config::parse(&missing_host).expect_err("empty proxy host");
        assert!(
            format!("{error:#}").contains("proxy_host_required"),
            "missing proxy_host_required"
        );
        // Ports beyond u16 fail TOML deserialization itself.
        assert!(Config::parse(&bad_port).is_err());
        let error = Config::parse(&bad_type).expect_err("socks4 proxy");
        assert!(
            format!("{error:#}").contains("unsupported_proxy_type"),
            "missing unsupported_proxy_type"
        );
    }

    #[test]
    fn debug_output_redacts_proxy_password() {
        // Arrange
        let content = server_toml(
            "\n[servers.test.proxy]\ntype = \"socks5\"\nhost = \"127.0.0.1\"\nport = 9050\nusername = \"tor\"\npassword = \"proxy-secret-token\"\n",
        );
        let config = Config::parse(&content).unwrap();

        // Act
        let debug = format!("{:?}", config.servers["test"].proxy.as_ref().unwrap());

        // Assert
        assert!(!debug.contains("proxy-secret-token"), "leaked: {debug}");
        assert!(debug.contains("<redacted>"));
    }

    /// A unique, cleaned temp path for writer tests.
    fn temp_path(tag: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("termirc-cfg-{}-{tag}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn wizard_server() -> ServerConfig {
        // Parsed (not hand-built) so the entry always matches the file
        // format exactly.
        Config::parse(
            r##"
[servers.new]
username = "me"
nickname = "me"
password = ""
server = "irc.example.org"
use_tls = true
port = 6697
channels = ["#a"]
"##,
        )
        .unwrap()
        .servers["new"]
            .clone()
    }

    #[test]
    fn the_shipped_example_configuration_parses() {
        // The example is the documented starting point; the wizard's
        // writer mirrors its formatting, so it must never drift into
        // something `Config::parse` rejects.
        let config = Config::parse(include_str!("../config.example.toml")).unwrap();
        assert!(config.servers.contains_key("example"));
        assert_eq!(config.servers["example"].port, 6697);
    }

    #[test]
    fn save_server_creates_a_parseable_file_and_round_trips() {
        // Arrange
        let path = temp_path("fresh");
        let server = wizard_server();

        // Act
        save_server(&path, "new", &server).unwrap();
        let saved = Config::load(&path).unwrap();

        // Assert
        assert_eq!(saved.servers["new"].server, "irc.example.org");
        assert_eq!(saved.servers["new"].port, 6697);
        assert!(saved.servers["new"].use_tls);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_server_replaces_only_wizard_keys_and_preserves_the_rest() {
        // Arrange: an existing file with keys the wizard does not model
        // (password, queries, proxy, comments) plus another server.
        let path = temp_path("merge");
        std::fs::write(
            &path,
            "# my setup\n[servers.old]\nusername = \"x\"\nnickname = \"x\"\npassword = \"keep\"\nserver = \"old.example\"\nport = 6667\nchannels = []\nqueries = [\"alice\"]\n\n[servers.old.proxy]\ntype = \"socks5\"\nhost = \"127.0.0.1\"\nport = 9050\n\n[ui]\nlanguage = \"zh-CN\"\n",
        )
        .unwrap();
        let mut server = wizard_server();
        server.nickname = "renamed".into();
        server.channels = vec!["#b".into(), "#c".into()];

        // Act
        save_server(&path, "old", &server).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();

        // Assert: the wizard's keys are the new ones, everything it does
        // not model survives verbatim, and the file still loads.
        let saved = Config::load(&path).unwrap();
        let merged = &saved.servers["old"];
        assert_eq!(merged.nickname, "renamed");
        assert_eq!(merged.username, "me");
        assert_eq!(merged.server, "irc.example.org");
        assert_eq!(merged.port, 6697);
        assert!(merged.use_tls);
        assert_eq!(merged.channels, vec!["#b".to_string(), "#c".to_string()]);
        assert_eq!(merged.password, "keep");
        assert_eq!(merged.queries, vec!["alice".to_string()]);
        assert!(merged.proxy.is_some());
        assert_eq!(saved.servers.len(), 1);
        assert!(text.contains("# my setup"));
        assert!(text.contains("language = \"zh-CN\""));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_server_creates_missing_parent_directories() {
        // Arrange: a nested path whose directories do not exist yet, so
        // the `create_dir_all` branch (and the same-directory temp rename)
        // is actually exercised — the other writer tests all use
        // `std::env::temp_dir()` directly.
        let root = std::env::temp_dir().join(format!("termirc-cfg-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let path = root.join("deep").join("config.toml");

        // Act
        save_server(&path, "new", &wizard_server()).unwrap();

        // Assert: the tree and file exist, the file reloads, and no temp
        // file was left behind.
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.servers["new"].server, "irc.example.org");
        assert!(path.exists());
        assert!(!path.with_extension("toml.tmp").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_server_refuses_an_unreadable_config_instead_of_rewriting_it() {
        // Arrange: an existing file that is not UTF-8 (the read fails with
        // InvalidData, as a permission error would with PermissionDenied)
        // and a directory where the config file is expected.
        let invalid = temp_path("invalid-utf8");
        std::fs::write(&invalid, [0xff, 0xfe, 0x00]).unwrap();
        let as_directory =
            std::env::temp_dir().join(format!("termirc-cfg-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&as_directory);
        std::fs::create_dir_all(&as_directory).unwrap();

        // Act
        let invalid_result = save_server(&invalid, "new", &wizard_server());
        let directory_result = save_server(&as_directory, "new", &wizard_server());

        // Assert: both report the write failure and leave the target as it
        // was instead of replacing it with just the wizard's section.
        let expected = |path: &std::path::Path| {
            i18n::format(
                i18n::t("err.cfg_write"),
                &[("path", &path.display().to_string())],
            )
        };
        let error = invalid_result.unwrap_err().to_string();
        assert_eq!(error, expected(&invalid));
        assert_eq!(std::fs::read(&invalid).unwrap(), [0xff, 0xfe, 0x00]);
        assert!(!invalid.with_extension("toml.tmp").exists());

        let error = directory_result.unwrap_err().to_string();
        assert_eq!(error, expected(&as_directory));
        assert!(as_directory.is_dir());
        assert!(!as_directory.with_extension("toml.tmp").exists());

        let _ = std::fs::remove_file(&invalid);
        let _ = std::fs::remove_dir_all(&as_directory);
    }

    #[test]
    fn save_server_appending_a_renamed_label_keeps_the_old_entry() {
        // Arrange
        let path = temp_path("rename");
        save_server(&path, "old", &wizard_server()).unwrap();
        let mut renamed = wizard_server();
        renamed.server = "irc2.example.org".into();

        // Act: the wizard saved under a different label.
        save_server(&path, "second", &renamed).unwrap();

        // Assert: both entries exist and the old one is untouched.
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.servers.len(), 2);
        assert_eq!(saved.servers["old"].server, "irc.example.org");
        assert_eq!(saved.servers["second"].server, "irc2.example.org");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_server_refuses_a_merge_that_would_not_reload() {
        // Arrange: an existing SERVER key whose ASCII-case-insensitive
        // identity clashes with the wizard's label.
        let path = temp_path("clash");
        save_server(&path, "Libera", &wizard_server()).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        // Act
        let result = save_server(&path, "libera", &wizard_server());

        // Assert: rejected, and the file on disk is unchanged.
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_server_escapes_toml_specials_in_emitted_strings() {
        // Arrange
        let path = temp_path("escape");
        let mut server = wizard_server();
        server.username = "a\"b\\c\u{1}".into();

        // Act
        save_server(&path, "esc", &server).unwrap();

        // Assert: the loaded values are the originals, byte for byte.
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.servers["esc"].username, "a\"b\\c\u{1}");
        let _ = std::fs::remove_file(&path);
    }
}
