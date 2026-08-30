//! Chat message model and conversion from IRC protocol messages.

use irc::client::prelude::{Command, Message};

/// A single chat message to display, extracted from an IRC PRIVMSG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// Config key of the server the message came through.
    pub server: String,
    /// Channel the message was addressed to (target as received).
    pub channel: String,
    pub nick: String,
    pub text: String,
}

impl ChatMessage {
    /// Extract a chat message from a protocol message addressed to any of
    /// `channels` on `server`.
    ///
    /// Returns `None` for anything that is not a PRIVMSG targeted at one of
    /// the joined channels (DMs to our own nick, unconfigured channels),
    /// anything without an identifiable user as its source (server notices,
    /// numerics), and non-ACTION CTCP queries. `/me` actions render as
    /// `* <text>`.
    pub fn from_proto(msg: &Message, server: &str, channels: &[String]) -> Option<ChatMessage> {
        let Command::PRIVMSG(target, body) = &msg.command else {
            return None;
        };
        if !channels.iter().any(|c| target.eq_ignore_ascii_case(c)) {
            return None; // DMs to our nick and unconfigured channels are not channel chat
        }
        let nick = msg.source_nickname()?.to_string();
        let text = display_text(body)?;
        Some(ChatMessage {
            server: server.to_string(),
            channel: target.clone(),
            nick,
            text,
        })
    }

    /// Extract a raw protocol line for the server console from a message
    /// that is not channel chat.
    ///
    /// Server replies — numeric responses (welcome, MOTD, WHOIS results,
    /// errors) and NOTICEs — render verbatim as nick-less lines in the
    /// server's console view. Other users' presence traffic (JOIN/PART/…)
    /// and keepalive PINGs stay dropped: they would flood the console
    /// without ever being a reply to the user.
    pub fn raw_from_proto(msg: &Message, server: &str) -> Option<ChatMessage> {
        match &msg.command {
            Command::Response(..) | Command::NOTICE(..) => Some(ChatMessage {
                server: server.to_string(),
                channel: String::new(),
                nick: String::new(),
                text: msg.to_string().trim_end_matches(['\r', '\n']).to_string(),
            }),
            _ => None,
        }
    }
}

/// Convert a raw PRIVMSG body into display text.
///
/// Strips IRC line-ending cruft; CTCP `/me` actions render as `* <text>`;
/// any other CTCP query (VERSION, PING, ...) is not chat and yields `None`.
fn display_text(body: &str) -> Option<String> {
    let body = body.trim_end_matches(['\r', '\n']);
    if let Some(inner) = body.strip_prefix('\x01') {
        let inner = inner.trim_end_matches('\x01');
        let action = inner.strip_prefix("ACTION ")?;
        return Some(format!("* {action}"));
    }
    Some(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: &str = "osu_irc";

    fn channels() -> Vec<String> {
        vec!["#osu".to_string(), "#chinese".to_string()]
    }

    #[test]
    fn privmsg_with_full_nickmask_yields_nick_and_text() {
        // Arrange
        let msg: Message = ":alice!a@b PRIVMSG #osu :hello world".parse().unwrap();

        // Act
        let chat = ChatMessage::from_proto(&msg, SERVER, &channels());

        // Assert
        assert_eq!(
            chat,
            Some(ChatMessage {
                server: "osu_irc".to_string(),
                channel: "#osu".to_string(),
                nick: "alice".to_string(),
                text: "hello world".to_string(),
            })
        );
    }

    #[test]
    fn privmsg_with_bancho_style_prefix_yields_nick() {
        // Arrange: osu! Bancho uses Nick!Nick@cho.ppy.sh as the prefix.
        let msg: Message = ":Bubble_Shark!Bubble_Shark@cho.ppy.sh PRIVMSG #osu :hi"
            .parse()
            .unwrap();

        // Act
        let chat = ChatMessage::from_proto(&msg, SERVER, &channels());

        // Assert
        assert_eq!(chat.unwrap().nick, "Bubble_Shark");
    }

    #[test]
    fn channel_match_is_case_insensitive_and_keeps_received_case() {
        // Arrange: IRC channel names compare case-insensitively.
        let msg: Message = ":alice!a@b PRIVMSG #OSU :hi".parse().unwrap();

        // Act
        let chat = ChatMessage::from_proto(&msg, SERVER, &channels());

        // Assert
        let chat = chat.unwrap();
        assert_eq!(chat.channel, "#OSU");
        assert_eq!(chat.server, "osu_irc");
    }

    #[test]
    fn any_configured_channel_is_accepted() {
        // Arrange: a message to the second configured channel.
        let msg: Message = ":alice!a@b PRIVMSG #chinese :ni hao".parse().unwrap();

        // Act
        let chat = ChatMessage::from_proto(&msg, SERVER, &channels());

        // Assert
        assert_eq!(chat.unwrap().channel, "#chinese");
    }

    #[test]
    fn privmsg_to_private_nick_is_ignored() {
        // Arrange: a DM/whisper targets our nick, not a channel.
        let msg: Message = ":BanchoBot!bot@ppy.sh PRIVMSG Bubble_Shark :your rank is #1234"
            .parse()
            .unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg, SERVER, &channels()), None);
    }

    #[test]
    fn privmsg_to_unconfigured_channel_is_ignored() {
        // Arrange
        let msg: Message = ":alice!a@b PRIVMSG #other :hi".parse().unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg, SERVER, &channels()), None);
    }

    #[test]
    fn ctcp_action_renders_with_star_prefix() {
        // Arrange: /me arrives as a CTCP ACTION envelope.
        let msg: Message = ":alice!a@b PRIVMSG #osu :\x01ACTION dances\x01"
            .parse()
            .unwrap();

        // Act
        let chat = ChatMessage::from_proto(&msg, SERVER, &channels());

        // Assert
        assert_eq!(
            chat,
            Some(ChatMessage {
                server: "osu_irc".to_string(),
                channel: "#osu".to_string(),
                nick: "alice".to_string(),
                text: "* dances".to_string(),
            })
        );
    }

    #[test]
    fn non_action_ctcp_is_ignored() {
        // Arrange: e.g. a CTCP VERSION query is not chat.
        let msg: Message = ":alice!a@b PRIVMSG #osu :\x01VERSION\x01".parse().unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg, SERVER, &channels()), None);
    }

    #[test]
    fn notice_is_ignored() {
        // Arrange
        let msg: Message = ":alice!a@b NOTICE #osu :hi".parse().unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg, SERVER, &channels()), None);
    }

    #[test]
    fn server_message_without_nick_is_ignored() {
        // Arrange: server NOTICEs have a hostname prefix without a nick.
        let msg: Message = ":cho.ppy.sh NOTICE * :*** Looking up your hostname"
            .parse()
            .unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg, SERVER, &channels()), None);
    }

    #[test]
    fn privmsg_from_hostname_only_prefix_is_ignored() {
        // Arrange: a PRIVMSG whose source is a server name has no user nick.
        let msg: Message = ":cho.ppy.sh PRIVMSG #osu :hello".parse().unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg, SERVER, &channels()), None);
    }

    #[test]
    fn numeric_responses_are_ignored() {
        // Arrange
        let msg: Message = ":srv 001 me :Welcome".parse().unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg, SERVER, &channels()), None);
    }

    #[test]
    fn trailing_carriage_return_is_stripped() {
        // Arrange: IRC lines end with \r\n; a stray \r must not reach the UI.
        let msg: Message = ":alice!a@b PRIVMSG #osu :hello\r".parse().unwrap();

        // Act
        let chat = ChatMessage::from_proto(&msg, SERVER, &channels());

        // Assert
        assert_eq!(chat.unwrap().text, "hello");
    }

    // ----- raw console lines -----

    #[test]
    fn numeric_replies_become_raw_console_lines() {
        // Arrange: the welcome numeric every server sends on connect.
        let msg: Message = ":mock 001 test :Welcome to the Mock IRC Network"
            .parse()
            .unwrap();

        // Act
        let chat = ChatMessage::raw_from_proto(&msg, SERVER);

        // Assert: routed to the console (empty channel and nick), text is
        // the verbatim wire line without the CRLF.
        assert_eq!(
            chat,
            Some(ChatMessage {
                server: "osu_irc".to_string(),
                channel: String::new(),
                nick: String::new(),
                text: ":mock 001 test :Welcome to the Mock IRC Network".to_string(),
            })
        );
    }

    #[test]
    fn notices_become_raw_console_lines() {
        // Arrange: server notices (hostname lookups, auth hints).
        let msg: Message = ":cho.ppy.sh NOTICE * :*** Looking up your hostname"
            .parse()
            .unwrap();

        // Act
        let chat = ChatMessage::raw_from_proto(&msg, SERVER);

        // Assert
        assert_eq!(
            chat.unwrap().text,
            ":cho.ppy.sh NOTICE * :*** Looking up your hostname"
        );
    }

    #[test]
    fn joins_of_other_users_stay_out_of_the_console() {
        // Arrange: presence traffic of other users would flood the console
        // on busy channels.
        let msg: Message = ":bob!b@c JOIN #osu".parse().unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::raw_from_proto(&msg, SERVER), None);
    }

    #[test]
    fn server_ping_keepalives_stay_out_of_the_console() {
        // Arrange
        let msg: Message = "PING :mock".parse().unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::raw_from_proto(&msg, SERVER), None);
    }
}
