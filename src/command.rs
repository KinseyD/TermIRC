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
