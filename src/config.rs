//! Configuration loading for termirc.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context as _;
use indexmap::IndexMap;
use serde::Deserialize;

use crate::core::{ServerId, valid_channel_name, valid_query_nickname};
use crate::theme::ThemeTable;

/// Per-server connection settings, as written in the `[servers.<name>]` tables.
#[derive(Clone, Deserialize)]
pub struct ServerConfig {
    pub username: String,
    pub nickname: String,
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
                    "server names {previous:?} and {name:?} conflict (ASCII case-insensitive)"
                );
            }
        }
        for server in config.servers.values() {
            anyhow::ensure!(
                server
                    .channels
                    .iter()
                    .all(|channel| valid_channel_name(channel)),
                "channels must contain individual # or & channel names without passwords"
            );
            anyhow::ensure!(
                server
                    .queries
                    .iter()
                    .all(|nickname| valid_query_nickname(nickname)),
                "queries must contain individual nicknames, not channels or multiple targets"
            );
            if let Some(proxy) = &server.proxy {
                anyhow::ensure!(
                    proxy.r#type == "socks5",
                    "unsupported_proxy_type: only \"socks5\" is supported, got {:?}",
                    proxy.r#type
                );
                anyhow::ensure!(
                    !proxy.host.trim().is_empty(),
                    "proxy_host_required: proxy.host must be a non-empty hostname"
                );
                anyhow::ensure!(
                    (1..=65535).contains(&proxy.port),
                    "proxy_port_out_of_range: proxy.port must be between 1 and 65535"
                );
            }
            let sasl = match (&server.sasl_username, &server.sasl_password) {
                (Some(username), Some(password)) => Some((username, password)),
                (None, None) => None,
                _ => anyhow::bail!(
                    "sasl_requires_username_and_password: set both sasl_username and sasl_password, or neither"
                ),
            };
            if let Some((username, password)) = sasl {
                anyhow::ensure!(
                    !username.contains(['\0', '\r', '\n'])
                        && !password.contains(['\0', '\r', '\n']),
                    "sasl credentials must not contain NUL, CR, or LF characters"
                );
                // AUTHENTICATE carries at most 400 base64 characters per
                // line; refusing longer payloads keeps SASL PLAIN unchunked.
                let payload = 2 + username.len() + password.len();
                anyhow::ensure!(
                    4 * payload.div_ceil(3) <= 400,
                    "sasl_credentials_too_long: encoded SASL PLAIN payload would exceed 400 bytes"
                );
            }
        }
        Ok(config)
    }

    /// Load and parse configuration from a file path.
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Config::parse(&content).with_context(|| format!("failed to parse {}", path.display()))
    }

    /// The first server in file order, with its name.
    pub fn first_server(&self) -> Option<(&str, &ServerConfig)> {
        self.servers
            .iter()
            .next()
            .map(|(name, server)| (name.as_str(), server))
    }
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
}
