//! Local slash-command parsing, independent of connections and execution.

/// A command token and its argument body, without interpreting their meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashParseError {
    MissingName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandAction {
    Join(String),
    Part {
        channel: Option<String>,
        reason: Option<String>,
    },
    Query(String),
    Close,
    Nick(String),
    Away(Option<String>),
    Back,
    Connect(Option<String>),
    Reconnect(Option<String>),
    Disconnect(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandError {
    Unsupported,
    InvalidArguments,
}

impl SlashCommand {
    pub fn action(&self) -> Result<CommandAction, CommandError> {
        use CommandAction::*;
        let args = self.arguments.as_str();
        if args.contains(['\r', '\n', '\0']) {
            return Err(CommandError::InvalidArguments);
        }
        let single = !args.is_empty() && !args.chars().any(char::is_whitespace);
        let optional = || (!args.is_empty()).then(|| args.to_string());
        match self.name.as_str() {
            "join" if crate::core::valid_channel_name(args) => Ok(Join(args.into())),
            "join" => Err(CommandError::InvalidArguments),
            "part" => {
                let end = args.find(char::is_whitespace).unwrap_or(args.len());
                let first = &args[..end];
                if first.starts_with(['#', '&']) {
                    if !crate::core::valid_channel_name(first) {
                        return Err(CommandError::InvalidArguments);
                    }
                    let reason = args[end..].trim_start();
                    Ok(Part {
                        channel: Some(first.into()),
                        reason: (!reason.is_empty()).then(|| reason.into()),
                    })
                } else {
                    Ok(Part {
                        channel: None,
                        reason: optional(),
                    })
                }
            }
            "query" if crate::core::valid_query_nickname(args) => Ok(Query(args.into())),
            "close" if args.is_empty() => Ok(Close),
            "query" | "close" => Err(CommandError::InvalidArguments),
            "nick"
                if single
                    && !args.contains([',', '*', '?', '!', '@', '.'])
                    && !args.starts_with(['$', ':', '#', '&', '+', '%', '~']) =>
            {
                Ok(Nick(args.into()))
            }
            "away" => Ok(Away(optional())),
            "back" if args.is_empty() => Ok(Back),
            "connect" if args.is_empty() || single => Ok(Connect(optional())),
            "reconnect" if args.is_empty() || single => Ok(Reconnect(optional())),
            "disconnect" | "quit" => Ok(Disconnect(args.into())),
            "nick" | "back" | "connect" | "reconnect" => Err(CommandError::InvalidArguments),
            _ => Err(CommandError::Unsupported),
        }
    }
}

/// Classify trimmed input and split a slash command without executing it.
///
/// `None` denotes ordinary input. Only the first slash is removed, so `//`
/// stays on the command path. Arguments retain internal whitespace and quotes;
/// command names are ASCII-lowercased but not checked against a command list.
pub fn parse_slash_command(input: &str) -> Option<Result<SlashCommand, SlashParseError>> {
    let body = input.trim().strip_prefix('/')?;
    let name_end = body.find(char::is_whitespace).unwrap_or(body.len());
    let name = &body[..name_end];
    if name.is_empty() {
        return Some(Err(SlashParseError::MissingName));
    }
    Some(Ok(SlashCommand {
        name: name.to_ascii_lowercase(),
        arguments: body[name_end..].trim_start().to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_and_part_are_channel_commands() {
        for input in [
            "/join #new",
            "/JOIN &local",
            "/part",
            "/part #new bye  all",
            "/part bye all",
        ] {
            assert!(
                parse_slash_command(input)
                    .unwrap()
                    .unwrap()
                    .action()
                    .is_ok(),
                "{input}"
            );
        }
    }

    #[test]
    fn channel_commands_preserve_reason_and_reject_invalid_names() {
        for (input, expected) in [
            ("/join #Room", CommandAction::Join("#Room".into())),
            (
                "/part",
                CommandAction::Part {
                    channel: None,
                    reason: None,
                },
            ),
            (
                "/part bye  all",
                CommandAction::Part {
                    channel: None,
                    reason: Some("bye  all".into()),
                },
            ),
            (
                "/part &local bye  all",
                CommandAction::Part {
                    channel: Some("&local".into()),
                    reason: Some("bye  all".into()),
                },
            ),
            (
                "/part #Room",
                CommandAction::Part {
                    channel: Some("#Room".into()),
                    reason: None,
                },
            ),
        ] {
            assert_eq!(
                parse_slash_command(input).unwrap().unwrap().action(),
                Ok(expected),
                "{input}"
            );
        }
        for input in [
            "/join",
            "/join #",
            "/join &",
            "/join room",
            "/join 0",
            "/join #a,#b",
            "/join #a key",
            "/join #a #b",
            "/join #a:b",
            "/join #a\u{7}",
            "/part #",
            "/part #a,#b bye",
            "/part #a:b",
            "/part hi\nQUIT",
        ] {
            assert_eq!(
                parse_slash_command(input).unwrap().unwrap().action(),
                Err(CommandError::InvalidArguments),
                "{input:?}"
            );
        }
    }

    #[test]
    fn query_and_close_are_local_commands() {
        assert!(
            parse_slash_command("/query Alice")
                .unwrap()
                .unwrap()
                .action()
                .is_ok()
        );
        assert!(
            parse_slash_command("/close")
                .unwrap()
                .unwrap()
                .action()
                .is_ok()
        );
        for input in [
            "/query",
            "/query a b",
            "/query a,b",
            "/query #room",
            "/query !room",
            "/close alice",
        ] {
            assert!(
                parse_slash_command(input)
                    .unwrap()
                    .unwrap()
                    .action()
                    .is_err(),
                "{input}"
            );
        }
    }

    #[test]
    fn identity_and_connection_commands_have_typed_arguments() {
        for (input, expected) in [
            ("/nick Alice", CommandAction::Nick("Alice".into())),
            (
                "/away lunch  break",
                CommandAction::Away(Some("lunch  break".into())),
            ),
            ("/away", CommandAction::Away(None)),
            ("/back", CommandAction::Back),
            ("/connect", CommandAction::Connect(None)),
            (
                "/connect Libera",
                CommandAction::Connect(Some("Libera".into())),
            ),
            (
                "/reconnect srv",
                CommandAction::Reconnect(Some("srv".into())),
            ),
            (
                "/disconnect bye all",
                CommandAction::Disconnect("bye all".into()),
            ),
            ("/quit", CommandAction::Disconnect(String::new())),
        ] {
            assert_eq!(
                parse_slash_command(input).unwrap().unwrap().action(),
                Ok(expected)
            );
        }
    }

    #[test]
    fn unsupported_or_malformed_commands_cannot_be_executed() {
        for input in [
            "/nick",
            "/nick a b",
            "/nick #chan",
            "/nick :bob",
            "/nick a,b",
            "/back now",
            "/connect a b",
            "/reconnect a b",
            "/away hi\r\nQUIT",
            "/quit a\0b",
        ] {
            assert_eq!(
                parse_slash_command(input).unwrap().unwrap().action(),
                Err(CommandError::InvalidArguments),
                "{input:?}"
            );
        }
        for input in ["/raw QUIT", "//hello", "/unknown"] {
            assert_eq!(
                parse_slash_command(input).unwrap().unwrap().action(),
                Err(CommandError::Unsupported)
            );
        }
    }

    #[test]
    fn parses_name_and_preserves_argument_body() {
        assert_eq!(
            parse_slash_command("  /MSG   alice hello  世界  "),
            Some(Ok(SlashCommand {
                name: "msg".into(),
                arguments: "alice hello  世界".into(),
            }))
        );
    }

    #[test]
    fn ordinary_text_is_not_a_command() {
        for input in ["", "  ", "hello /join", "WHOIS nick"] {
            assert_eq!(parse_slash_command(input), None, "{input:?}");
        }
    }

    #[test]
    fn missing_names_are_parse_errors() {
        for input in ["/", "/   ", "/ join", "/\tjoin"] {
            assert_eq!(
                parse_slash_command(input),
                Some(Err(SlashParseError::MissingName)),
                "{input:?}"
            );
        }
    }

    #[test]
    fn command_names_are_not_validated_or_executed() {
        for (input, name, arguments) in [
            ("/quit", "quit", ""),
            ("/unknown a b", "unknown", "a b"),
            ("/format-msg\talice hi", "format-msg", "alice hi"),
            ("//hello", "/hello", ""),
            ("/你好 世界", "你好", "世界"),
            ("/msg alice \"hello world\"", "msg", "alice \"hello world\""),
        ] {
            assert_eq!(
                parse_slash_command(input),
                Some(Ok(SlashCommand {
                    name: name.into(),
                    arguments: arguments.into(),
                })),
                "{input:?}"
            );
        }
    }
}
