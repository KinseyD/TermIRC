//! Configuration loading for termirc.

use std::path::Path;

use indexmap::IndexMap;
use serde::Deserialize;

/// Per-server connection settings, as written in the `[servers.<name>]` tables.
#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub username: String,
    pub nickname: String,
    pub password: String,
    pub server: String,
    #[serde(default)]
    pub use_tls: bool,
    pub port: u16,
    pub channels: Vec<String>,
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
/// appear in the file, so `first_server` means "first written", not alphabetical.
#[derive(Debug, Deserialize)]
pub struct Config {
    pub servers: IndexMap<String, ServerConfig>,
}

impl Config {
    /// Parse configuration from a TOML string.
    pub fn parse(content: &str) -> anyhow::Result<Config> {
        Ok(toml::from_str(content)?)
    }

    /// Load and parse configuration from a file path.
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
        Config::parse(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))
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
    fn parse_errors_on_invalid_toml() {
        // Arrange
        let content = "this is [ not toml";

        // Act
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
}
