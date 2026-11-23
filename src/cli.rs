//! Command-line interface: argument definitions and parsing only.
//!
//! `clap` owns the parsing (help, version, value validation) so the CLI
//! surface stays declarative as flags and subcommands grow; this module is
//! the only place that knows the option set. Errors and `--help` output are
//! English by design — localization initializes later, from the config file
//! this parsing may point at.

use std::path::PathBuf;

use clap::Parser;

/// A terminal IRC client.
#[derive(Debug, Parser)]
#[command(name = "termirc", version)]
pub struct Cli {
    /// Configuration file to load instead of ~/.config/termirc/config.toml.
    #[arg(short, long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Run headless in read-only listen mode: print incoming chat lines
    /// to stdout; nothing is ever sent (Ctrl+C exits)
    #[arg(long)]
    pub listen: bool,
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
}
