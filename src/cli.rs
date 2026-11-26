//! Command-line interface: argument definitions and parsing only.
//!
//! `clap` owns the parsing (help, version, value validation) so the CLI
//! surface stays declarative as flags and subcommands grow; this module is
//! the only place that knows the option set and subcommand set. Errors and
//! `--help` output are
//! English by design — localization initializes later, from the config file
//! this parsing may point at.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// A terminal IRC client.
#[derive(Debug, Parser)]
#[command(name = "termirc", version)]
pub struct Cli {
    /// Configuration file to load instead of ~/.config/termirc/config.toml.
    #[arg(short, long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Run headless in read-only listen mode: print incoming chat lines
    /// to stdout; nothing is ever sent (Ctrl+C exits)
    #[arg(long)]
    pub listen: bool,
    /// Run headless in interactive stdio mode: one send per stdin line,
    /// one rendered event line per stdout line (Ctrl+C exits)
    #[arg(long, conflicts_with = "listen")]
    pub stdio: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Headless subcommands: they run without a terminal and exit when done.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Send one message headlessly to `server/target`, then exit
    Send {
        /// Conversation to send to: a config server name, `/`, then a
        /// `#channel` or a nickname
        #[arg(value_name = "SERVER/TARGET")]
        buffer: String,
        /// Message text; `-` reads it from stdin
        #[arg(value_name = "MESSAGE")]
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_flag_accepts_long_short_and_equals_forms() {
        for args in [
            vec!["termirc", "--config", "a.toml"],
            vec!["termirc", "-c", "a.toml"],
            vec!["termirc", "--config=a.toml"],
        ] {
            let cli = Cli::try_parse_from(args.clone()).expect("args parse");
            assert_eq!(cli.config, Some(PathBuf::from("a.toml")), "args: {args:?}");
        }
    }

    #[test]
    fn config_flag_without_a_value_is_rejected() {
        // Must hard-fail rather than fall back to the default config path:
        // a typo'd invocation must never start the wrong profile.
        assert!(Cli::try_parse_from(["termirc", "--config"]).is_err());
    }

    #[test]
    fn unknown_arguments_are_rejected() {
        assert!(Cli::try_parse_from(["termirc", "--nope"]).is_err());
    }

    #[test]
    fn listen_flag_parses_true_alone() {
        let cli = Cli::try_parse_from(["termirc", "--listen"]).expect("args parse");
        assert!(cli.listen);
        assert_eq!(cli.config, None);
    }

    #[test]
    fn listen_flag_combines_with_the_config_flag() {
        let cli = Cli::try_parse_from(["termirc", "--listen", "-c", "a.toml"]).expect("args parse");
        assert!(cli.listen);
        assert_eq!(cli.config, Some(PathBuf::from("a.toml")));
    }

    #[test]
    fn listen_flag_stays_false_when_absent() {
        let cli = Cli::try_parse_from(["termirc", "--config", "a.toml"]).expect("args parse");
        assert!(!cli.listen);
    }

    #[test]
    fn without_arguments_the_config_path_stays_unset() {
        assert_eq!(Cli::try_parse_from(["termirc"]).unwrap().config, None);
    }

    #[test]
    fn send_subcommand_parses_buffer_and_message() {
        // Arrange/Act
        let cli = Cli::try_parse_from(["termirc", "send", "libera/#termirc", "deploy done"])
            .expect("args parse");
        // Assert
        assert_eq!(
            cli.command,
            Some(Command::Send {
                buffer: "libera/#termirc".into(),
                message: "deploy done".into(),
            })
        );
    }

    #[test]
    fn config_flag_is_accepted_before_and_after_the_send_subcommand() {
        for args in [
            vec!["termirc", "-c", "a.toml", "send", "s/#c", "hi"],
            vec!["termirc", "send", "-c", "a.toml", "s/#c", "hi"],
        ] {
            let cli = Cli::try_parse_from(args.clone()).expect("args parse");
            assert_eq!(cli.config, Some(PathBuf::from("a.toml")), "args: {args:?}");
            assert_eq!(
                cli.command,
                Some(Command::Send {
                    buffer: "s/#c".into(),
                    message: "hi".into(),
                }),
                "args: {args:?}"
            );
        }
    }

    #[test]
    fn listen_flag_and_send_subcommand_parse_together_so_main_can_reject_them() {
        // clap 4.6.7 cannot express this conflict: `args_conflicts_with_subcommands`
        // would also reject the global `--config` next to `send`, so `main` rejects
        // the combination instead (usage error, exit code 2).
        let cli =
            Cli::try_parse_from(["termirc", "--listen", "send", "s/#c", "hi"]).expect("args parse");
        assert!(cli.listen);
        assert!(cli.command.is_some());
    }

    #[test]
    fn send_requires_both_a_buffer_and_a_message() {
        // Missing message
        assert!(Cli::try_parse_from(["termirc", "send", "s/#c"]).is_err());
        // Extra positional
        assert!(Cli::try_parse_from(["termirc", "send", "s/#c", "hi", "extra"]).is_err());
    }

    #[test]
    fn send_treats_a_leading_dash_message_as_plain_text() {
        // A lone `-` is a message value, not the stdin marker position alone
        let cli = Cli::try_parse_from(["termirc", "send", "s/#c", "-"]).expect("args parse");
        assert_eq!(
            cli.command,
            Some(Command::Send {
                buffer: "s/#c".into(),
                message: "-".into(),
            })
        );
        // `--` lets an option-like message through as plain text
        let cli =
            Cli::try_parse_from(["termirc", "send", "s/#c", "--", "-x hi"]).expect("args parse");
        assert_eq!(
            cli.command,
            Some(Command::Send {
                buffer: "s/#c".into(),
                message: "-x hi".into(),
            })
        );
    }

    #[test]
    fn without_a_subcommand_the_command_field_is_none() {
        let cli = Cli::try_parse_from(["termirc"]).expect("args parse");
        assert_eq!(cli.command, None);
        let cli = Cli::try_parse_from(["termirc", "--listen"]).expect("args parse");
        assert!(cli.listen);
        assert_eq!(cli.command, None);
    }

    #[test]
    fn stdio_flag_parses_and_combines_with_the_config_flag() {
        for args in [
            vec!["termirc", "--stdio"],
            vec!["termirc", "--stdio", "-c", "a.toml"],
            vec!["termirc", "-c", "a.toml", "--stdio"],
        ] {
            let cli = Cli::try_parse_from(args.clone()).expect("args parse");
            assert!(cli.stdio, "args: {args:?}");
            assert!(!cli.listen, "args: {args:?}");
        }
    }

    #[test]
    fn stdio_flag_stays_false_when_absent() {
        let cli = Cli::try_parse_from(["termirc", "--config", "a.toml"]).expect("args parse");
        assert!(!cli.stdio);
    }

    #[test]
    fn stdio_and_listen_flags_are_rejected_together() {
        // The flags contradict each other at the parser, so clap owns the
        // usage error and its exit code 2.
        assert!(Cli::try_parse_from(["termirc", "--stdio", "--listen"]).is_err());
        assert!(Cli::try_parse_from(["termirc", "--listen", "--stdio"]).is_err());
    }
}
