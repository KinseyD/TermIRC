//! IRC wire adaptation and validation. Core messages never expose IRC crate types.

use std::{fmt, time::SystemTime};

use irc::proto::{Command, Message as WireMessage, Response};

use crate::core::{
    BufferKind, ConnectionCommand, DeliveryState, Direction, MessageContent, MessageKind,
    OutgoingMessage, RoutedMessage, ServerId,
};

/// Baseline IRC line limit, including the terminating CRLF.
pub const MAX_WIRE_BYTES: usize = 512;

/// Safe to display or log: it never contains the user's input or parser source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    InvalidCharacters,
    InvalidCommand,
    TooLong,
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidCharacters => "message contains an invalid control character",
            Self::InvalidCommand => "invalid IRC command",
            Self::TooLong => "message exceeds the 512-byte IRC line limit",
        })
    }
}

impl std::error::Error for SendError {}

fn validate_text(text: &str) -> Result<(), SendError> {
    if text.contains(['\r', '\n', '\0']) {
        Err(SendError::InvalidCharacters)
    } else {
        Ok(())
    }
}

fn validate_wire(message: WireMessage) -> Result<WireMessage, SendError> {
    if message.to_string().len() > MAX_WIRE_BYTES {
        Err(SendError::TooLong)
    } else {
        Ok(message)
    }
}

/// Encode once before queueing; the worker repeats validation for direct callers.
pub fn encode_outgoing(message: &OutgoingMessage) -> Result<WireMessage, SendError> {
    let wire = match message {
        OutgoingMessage::Privmsg { target, text, .. } => {
            validate_text(target)?;
            validate_text(text)?;
            if target.is_empty() || target.contains(char::is_whitespace) || target.starts_with(':')
            {
                return Err(SendError::InvalidCommand);
            }
            Command::PRIVMSG(target.clone(), text.clone()).into()
        }
        OutgoingMessage::Raw { line, .. } => {
            validate_text(line)?;
            let wire: WireMessage = line.parse().map_err(|_| SendError::InvalidCommand)?;
            if matches!(&wire.command, Command::Raw(command, _) if command.is_empty() || !command.bytes().all(|c| c.is_ascii_alphanumeric()))
            {
                return Err(SendError::InvalidCommand);
            }
            wire
        }
    };
    validate_wire(wire)
}

/// Validate user-supplied controls before any state changes or queue writes.
pub fn validate_control(command: &ConnectionCommand) -> Result<(), SendError> {
    let wire = match command {
        ConnectionCommand::Connect | ConnectionCommand::Reconnect => return Ok(()),
        ConnectionCommand::Disconnect(reason) => {
            validate_text(reason)?;
            Command::QUIT(Some(reason.clone()))
        }
        ConnectionCommand::Nick(nick) => {
            validate_text(nick)?;
            if nick.is_empty() || nick.contains(char::is_whitespace) || nick.starts_with(':') {
                return Err(SendError::InvalidCommand);
            }
            Command::NICK(nick.clone())
        }
        ConnectionCommand::Away(reason) => {
            if let Some(reason) = reason {
                validate_text(reason)?;
            }
            Command::AWAY(reason.clone())
        }
        ConnectionCommand::Back => Command::AWAY(None),
    };
    validate_wire(wire.into()).map(|_| ())
}

// Presence and automatic registration floods stay hidden. User-requested
// replies and all 4xx/5xx errors remain available to the application.
const IGNORED_NUMERICS: &[Response] = &[
    Response::RPL_ISUPPORT,
    Response::RPL_LUSERCLIENT,
    Response::RPL_LUSEROP,
    Response::RPL_LUSERUNKNOWN,
    Response::RPL_LUSERCHANNELS,
    Response::RPL_LUSERME,
    Response::RPL_LOCALUSERS,
    Response::RPL_GLOBALUSERS,
    Response::RPL_NAMREPLY,
    Response::RPL_ENDOFNAMES,
];

/// Decode one incoming wire message into one typed destination and payload.
pub fn decode_message(
    message: &WireMessage,
    server: &str,
    channels: &[String],
) -> Option<RoutedMessage> {
    let (target, nick, text, kind) = match &message.command {
        Command::PRIVMSG(target, body) => {
            if !channels
                .iter()
                .any(|channel| channel.eq_ignore_ascii_case(target))
            {
                return None;
            }
            let nick = message.source_nickname()?.to_owned();
            let body = body.trim_end_matches(['\r', '\n']);
            let (text, kind) = if let Some(ctcp) = body.strip_prefix('\x01') {
                let action = ctcp.trim_end_matches('\x01').strip_prefix("ACTION ")?;
                (format!("* {action}"), MessageKind::Action)
            } else {
                (body.to_owned(), MessageKind::Chat)
            };
            (BufferKind::Channel(target.clone()), nick, text, kind)
        }
        Command::Response(response, args) if (400..600).contains(&(*response as u16)) => {
            error_payload(*response as u16, args, channels)
        }
        // The IRC dependency treats unrecognized numerics as raw commands.
        Command::Raw(command, args)
            if command
                .parse::<u16>()
                .is_ok_and(|code| (400..600).contains(&code)) =>
        {
            error_payload(command.parse().ok()?, args, channels)
        }
        Command::Response(response, _) if IGNORED_NUMERICS.contains(response) => return None,
        Command::Response(_, args) => {
            if args.len() <= 1 || args.iter().any(|arg| is_channel(arg)) {
                return None;
            }
            (
                BufferKind::Server,
                String::new(),
                args[1..].join(" "),
                MessageKind::Console,
            )
        }
        Command::NOTICE(target, text) if !is_channel(target) => (
            BufferKind::Server,
            String::new(),
            text.clone(),
            MessageKind::Console,
        ),
        _ => return None,
    };
    let text = if kind == MessageKind::Console {
        text.trim().to_owned()
    } else {
        text
    };
    if text.is_empty() && kind == MessageKind::Console {
        return None;
    }
    Some(RoutedMessage {
        server: ServerId::new(server),
        target,
        content: MessageContent {
            nick,
            text,
            kind,
            direction: Direction::Incoming,
            received_at: SystemTime::now(),
            tags: message
                .tags
                .iter()
                .flatten()
                .map(|tag| (tag.0.clone(), tag.1.clone()))
                .collect(),
            delivery: DeliveryState::Received,
        },
    })
}

fn is_channel(value: &str) -> bool {
    value.starts_with('#') || value.starts_with('&')
}

fn error_payload(
    code: u16,
    args: &[String],
    channels: &[String],
) -> (BufferKind, String, String, MessageKind) {
    // The first argument names us, the final one is the explanatory text.
    // Prefer a channel when present, but retain nick/command targets too.
    let parameters = args
        .get(1..args.len().saturating_sub(1))
        .unwrap_or_default();
    let target = parameters
        .iter()
        .find(|arg| is_channel(arg))
        .or_else(|| parameters.first())
        .cloned();
    let destination = target
        .as_ref()
        .filter(|target| {
            channels
                .iter()
                .any(|channel| channel.eq_ignore_ascii_case(target))
        })
        .map_or(BufferKind::Server, |target| {
            BufferKind::Channel(target.clone())
        });
    let reason = args
        .last()
        .filter(|_| args.len() > 1)
        .cloned()
        .unwrap_or_default();
    let details = args.get(1..).unwrap_or_default().join(" ");
    let text = if details.is_empty() {
        code.to_string()
    } else {
        format!("{code} {details}")
    };
    (
        destination,
        String::new(),
        text,
        MessageKind::Error {
            code,
            target,
            reason,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(line: &str) -> Option<RoutedMessage> {
        decode_message(
            &line.parse().unwrap(),
            "OSU_IRC",
            &["#osu".into(), "#chinese".into()],
        )
    }

    fn outgoing(target: &str, text: &str) -> OutgoingMessage {
        OutgoingMessage::Privmsg {
            server: "srv".into(),
            target: target.into(),
            text: text.into(),
        }
    }

    fn raw(line: &str) -> OutgoingMessage {
        OutgoingMessage::Raw {
            server: "srv".into(),
            line: line.into(),
        }
    }

    #[test]
    fn privmsg_with_full_nickmask_yields_nick_and_text() {
        let message = decode(":alice!a@b PRIVMSG #osu :hello world").unwrap();
        assert_eq!(message.server, ServerId::new("osu_irc"));
        assert_eq!(message.target, BufferKind::Channel("#osu".into()));
        assert_eq!(message.content.nick, "alice");
        assert_eq!(message.content.text, "hello world");
        assert_eq!(message.content.kind, MessageKind::Chat);
        assert_eq!(message.content.delivery, DeliveryState::Received);
    }

    #[test]
    fn privmsg_with_bancho_style_prefix_yields_nick() {
        assert_eq!(
            decode(":Bubble_Shark!Bubble_Shark@cho.ppy.sh PRIVMSG #osu :hi")
                .unwrap()
                .content
                .nick,
            "Bubble_Shark"
        );
    }

    #[test]
    fn channel_match_is_case_insensitive_and_keeps_received_case() {
        assert_eq!(
            decode(":alice!a@b PRIVMSG #OSU :hi").unwrap().target,
            BufferKind::Channel("#OSU".into())
        );
    }

    #[test]
    fn any_configured_channel_is_accepted() {
        assert_eq!(
            decode(":alice!a@b PRIVMSG #chinese :hi").unwrap().target,
            BufferKind::Channel("#chinese".into())
        );
    }

    #[test]
    fn privmsg_to_private_nick_is_ignored() {
        assert!(decode(":bot!u@h PRIVMSG me :hello").is_none());
    }

    #[test]
    fn privmsg_to_unconfigured_channel_is_ignored() {
        assert!(decode(":alice!u@h PRIVMSG #other :hello").is_none());
    }

    #[test]
    fn ctcp_action_renders_with_star_prefix() {
        let message = decode(":alice!a@b PRIVMSG #osu :\x01ACTION dances\x01").unwrap();
        assert_eq!(message.content.text, "* dances");
        assert_eq!(message.content.kind, MessageKind::Action);
    }

    #[test]
    fn non_action_ctcp_is_ignored() {
        assert!(decode(":alice!a@b PRIVMSG #osu :\x01VERSION\x01").is_none());
    }

    #[test]
    fn channel_notice_is_ignored() {
        assert!(decode(":alice!a@b NOTICE #osu :hi").is_none());
    }

    #[test]
    fn server_message_without_nick_becomes_console_message() {
        let message = decode(":cho.ppy.sh NOTICE * :*** Looking up your hostname").unwrap();
        assert_eq!(message.target, BufferKind::Server);
        assert!(message.content.nick.is_empty());
    }

    #[test]
    fn privmsg_from_hostname_only_prefix_is_ignored() {
        assert!(decode(":cho.ppy.sh PRIVMSG #osu :hello").is_none());
    }

    #[test]
    fn trailing_carriage_return_is_stripped() {
        assert_eq!(
            decode(":alice!a@b PRIVMSG #osu :hello\r")
                .unwrap()
                .content
                .text,
            "hello"
        );
    }

    #[test]
    fn numeric_replies_become_console_payload_lines() {
        let message = decode(":mock 001 test :Welcome to the Mock IRC Network").unwrap();
        assert_eq!(message.target, BufferKind::Server);
        assert!(message.content.nick.is_empty());
        assert_eq!(message.content.text, "Welcome to the Mock IRC Network");
        assert_eq!(message.content.kind, MessageKind::Console);
    }

    #[test]
    fn notices_become_console_payload_lines() {
        assert_eq!(
            decode(":cho.ppy.sh NOTICE * :*** Looking up your hostname")
                .unwrap()
                .content
                .text,
            "*** Looking up your hostname"
        );
    }

    #[test]
    fn multi_param_replies_drop_own_nick_and_join_the_rest() {
        assert_eq!(
            decode(":mock 311 smoke alice ~a host * :real name")
                .unwrap()
                .content
                .text,
            "alice ~a host * real name"
        );
        assert_eq!(
            decode(":mock 433 smoke BadNick :Nickname is already in use")
                .unwrap()
                .content
                .text,
            "433 BadNick Nickname is already in use"
        );
    }

    #[test]
    fn replies_without_a_payload_are_dropped() {
        assert!(decode(":mock 300 smoke").is_none());
    }

    #[test]
    fn channel_param_replies_are_blocked_from_the_console() {
        for line in [
            ":mock 332 smoke #osu :Welcome to #osu",
            ":mock NOTICE #osu :spam",
            ":mock 353 smoke = #osu :a b",
        ] {
            assert!(decode(line).is_none(), "{line}");
        }
    }

    #[test]
    fn joins_of_other_users_stay_out_of_the_console() {
        assert!(decode(":bob!b@c JOIN #osu").is_none());
    }

    #[test]
    fn server_ping_keepalives_stay_out_of_the_console() {
        assert!(decode("PING :mock").is_none());
    }

    #[test]
    fn names_replies_are_blocked_from_the_console() {
        assert!(decode(":mock 353 smoke = #test :smoke alice bob").is_none());
        assert!(decode(":mock 366 smoke #test :End of /NAMES list").is_none());
    }

    #[test]
    fn lusers_statistics_are_blocked_from_the_console() {
        for code in [250, 251, 252, 253, 254, 255, 265, 266] {
            assert!(decode(&format!(":mock {code} smoke 3 :population statistics")).is_none());
        }
    }

    #[test]
    fn isupport_tokens_are_blocked_from_the_console() {
        assert!(
            decode(":mock 005 smoke PREFIX=(ov)@+ CHANTYPES=# NICKLEN=30 :supported by server")
                .is_none()
        );
    }

    #[test]
    fn whois_motd_and_error_replies_stay_visible() {
        for line in [
            ":mock 311 smoke alice ~a host * :real name",
            ":mock 372 smoke :- some motd line",
            ":mock 401 smoke nobody :No such nick/channel",
        ] {
            assert!(decode(line).is_some(), "{line}");
        }
    }

    #[test]
    fn channel_errors_retain_code_target_reason_and_have_no_nickname() {
        for (code, reason) in [
            (475, "Cannot join channel (+k)"),
            (404, "Cannot send to channel"),
        ] {
            let message = decode(&format!(":mock {code} smoke #OSU :{reason}")).unwrap();
            assert_eq!(message.target, BufferKind::Channel("#OSU".into()));
            assert!(message.content.nick.is_empty());
            assert_eq!(message.content.text, format!("{code} #OSU {reason}"));
            assert_eq!(
                message.content.kind,
                MessageKind::Error {
                    code,
                    target: Some("#OSU".into()),
                    reason: reason.into()
                }
            );
        }
    }

    #[test]
    fn unknown_channel_errors_fall_back_to_server_without_losing_target() {
        let message = decode(":mock 475 smoke #unknown :Need a key").unwrap();
        assert_eq!(message.target, BufferKind::Server);
        assert_eq!(
            message.content.kind,
            MessageKind::Error {
                code: 475,
                target: Some("#unknown".into()),
                reason: "Need a key".into()
            }
        );
        assert_eq!(message.content.text, "475 #unknown Need a key");
    }

    #[test]
    fn nickname_errors_retain_the_requested_target() {
        let message = decode(":mock 401 smoke nobody :No such nick/channel").unwrap();
        assert_eq!(message.target, BufferKind::Server);
        assert_eq!(
            message.content.kind,
            MessageKind::Error {
                code: 401,
                target: Some("nobody".into()),
                reason: "No such nick/channel".into(),
            }
        );
    }

    #[test]
    fn errors_unknown_to_the_dependency_remain_visible() {
        let message = decode(":mock 599 smoke #osu :Custom failure").unwrap();
        assert!(matches!(
            message.content.kind,
            MessageKind::Error { code: 599, .. }
        ));
    }

    #[test]
    fn tags_are_retained_on_chat_console_and_error_messages() {
        for body in [
            ":alice!u@h PRIVMSG #osu :hello",
            ":mock NOTICE * :hello",
            ":mock 404 me #osu :blocked",
        ] {
            let message = decode(&format!(
                "@time=2026-09-17T00:00:00Z;custom;escaped=one\\stwo {body}"
            ))
            .unwrap();
            assert_eq!(
                message.content.tags,
                vec![
                    ("time".into(), Some("2026-09-17T00:00:00Z".into())),
                    ("custom".into(), None),
                    ("escaped".into(), Some("one two".into()))
                ]
            );
        }
    }

    #[test]
    fn encoded_length_includes_target_command_and_crlf() {
        // "PRIVMSG #x " is 11 bytes, and CRLF adds 2.
        let accepted = encode_outgoing(&outgoing("#x", &"a".repeat(499))).unwrap();
        assert_eq!(accepted.to_string().len(), 512);
        assert_eq!(
            encode_outgoing(&outgoing("#x", &"a".repeat(500))),
            Err(SendError::TooLong)
        );
        assert_eq!(
            encode_outgoing(&outgoing("#longer", &"a".repeat(499))),
            Err(SendError::TooLong)
        );
    }

    #[test]
    fn unicode_limit_uses_utf8_bytes_and_trailing_colon_overhead() {
        let text = format!("{}a", "中".repeat(166));
        assert_eq!(
            encode_outgoing(&outgoing("#x", &text))
                .unwrap()
                .to_string()
                .len(),
            512
        );
        assert_eq!(
            encode_outgoing(&outgoing("#x", &format!("{text}中"))),
            Err(SendError::TooLong)
        );
        assert_eq!(
            encode_outgoing(&outgoing("#x", &format!("{} ", "a".repeat(498)))),
            Err(SendError::TooLong)
        );
    }

    #[test]
    fn raw_encoding_preserves_trailing_space_and_colon_semantics() {
        for (line, expected) in [
            ("PING :smoke", Command::PING("smoke".into(), None)),
            ("QUIT :bye", Command::QUIT(Some("bye".into()))),
            (
                "PRIVMSG #x :hello  world  ",
                Command::PRIVMSG("#x".into(), "hello  world  ".into()),
            ),
        ] {
            let encoded = encode_outgoing(&raw(line)).unwrap();
            let roundtrip: WireMessage = encoded.to_string().parse().unwrap();
            assert_eq!(roundtrip.command, expected);
        }
    }

    #[test]
    fn raw_commands_and_controls_enforce_actual_wire_limit() {
        assert_eq!(
            encode_outgoing(&raw(&format!("QUIT :{}", "a".repeat(505))))
                .unwrap()
                .to_string()
                .len(),
            512
        );
        assert_eq!(
            encode_outgoing(&raw(&format!("QUIT :{}", "a".repeat(506)))),
            Err(SendError::TooLong)
        );
        for command in [
            ConnectionCommand::Nick("a".repeat(506)),
            ConnectionCommand::Away(Some("中".repeat(169))),
            ConnectionCommand::Disconnect("a".repeat(506)),
        ] {
            assert_eq!(validate_control(&command), Err(SendError::TooLong));
        }
        for command in [
            ConnectionCommand::Nick("a".repeat(505)),
            ConnectionCommand::Away(Some("a".repeat(505))),
            ConnectionCommand::Disconnect("a".repeat(505)),
            ConnectionCommand::Connect,
            ConnectionCommand::Reconnect,
            ConnectionCommand::Back,
        ] {
            assert!(validate_control(&command).is_ok());
        }
    }

    #[test]
    fn invalid_input_errors_never_include_original_contents() {
        for invalid in ["secret\r", "secret\n", "secret\0"] {
            assert_eq!(
                encode_outgoing(&outgoing("#x", invalid)),
                Err(SendError::InvalidCharacters)
            );
            assert_eq!(
                encode_outgoing(&raw(invalid)),
                Err(SendError::InvalidCharacters)
            );
            for command in [
                ConnectionCommand::Nick(invalid.into()),
                ConnectionCommand::Away(Some(invalid.into())),
                ConnectionCommand::Disconnect(invalid.into()),
            ] {
                let error = validate_control(&command).unwrap_err();
                assert!(!error.to_string().contains("secret"));
                assert!(!format!("{error:?}").contains("secret"));
            }
        }
        assert_eq!(encode_outgoing(&raw("")), Err(SendError::InvalidCommand));
        assert_eq!(encode_outgoing(&raw(" ")), Err(SendError::InvalidCommand));
    }
}
