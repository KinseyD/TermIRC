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
        for input in ["/raw QUIT", "/join #new", "//hello", "/unknown"] {
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
