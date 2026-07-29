//! Chat message model and conversion from IRC protocol messages.

use irc::client::prelude::{Command, Message};

/// A single chat message to display, extracted from an IRC PRIVMSG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub nick: String,
    pub text: String,
}

impl ChatMessage {
    /// Extract a chat message from a protocol message addressed to `channel`.
    ///
    /// Returns `None` for anything that is not a PRIVMSG targeted at the
    /// channel we are viewing (DMs to our own nick, other channels), anything
    /// without an identifiable user as its source (server notices, numerics),
    /// and non-ACTION CTCP queries. `/me` actions render as `* <text>`.
    pub fn from_proto(msg: &Message, channel: &str) -> Option<ChatMessage> {
        let Command::PRIVMSG(target, body) = &msg.command else {
            return None;
        };
        if !target.eq_ignore_ascii_case(channel) {
            return None; // DMs to our nick and other channels are not channel chat
        }
        let nick = msg.source_nickname()?.to_string();
        let text = display_text(body)?;
        Some(ChatMessage { nick, text })
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

    const CHANNEL: &str = "#osu";

    #[test]
    fn privmsg_with_full_nickmask_yields_nick_and_text() {
        // Arrange
        let msg: Message = ":alice!a@b PRIVMSG #osu :hello world".parse().unwrap();

        // Act
        let chat = ChatMessage::from_proto(&msg, CHANNEL);

        // Assert
        assert_eq!(
            chat,
            Some(ChatMessage {
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
        let chat = ChatMessage::from_proto(&msg, CHANNEL);

        // Assert
        assert_eq!(chat.unwrap().nick, "Bubble_Shark");
    }

    #[test]
    fn channel_target_match_is_case_insensitive() {
        // Arrange: IRC channel names compare case-insensitively.
        let msg: Message = ":alice!a@b PRIVMSG #OSU :hi".parse().unwrap();

        // Act
        let chat = ChatMessage::from_proto(&msg, "#osu");

        // Assert
        assert!(chat.is_some());
    }

    #[test]
    fn privmsg_to_a_private_nick_is_ignored() {
        // Arrange: a DM/whisper targets our nick, not the channel.
        let msg: Message = ":BanchoBot!bot@ppy.sh PRIVMSG Bubble_Shark :your rank is #1234"
            .parse()
            .unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg, CHANNEL), None);
    }

    #[test]
    fn privmsg_to_another_channel_is_ignored() {
        // Arrange
        let msg: Message = ":alice!a@b PRIVMSG #chinese :ni hao".parse().unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg, CHANNEL), None);
    }

    #[test]
    fn ctcp_action_renders_with_star_prefix() {
        // Arrange: /me arrives as a CTCP ACTION envelope.
        let msg: Message = ":alice!a@b PRIVMSG #osu :\x01ACTION dances\x01"
            .parse()
            .unwrap();

        // Act
        let chat = ChatMessage::from_proto(&msg, CHANNEL);

        // Assert
        assert_eq!(
            chat,
            Some(ChatMessage {
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
        assert_eq!(ChatMessage::from_proto(&msg, CHANNEL), None);
    }

    #[test]
    fn notice_is_ignored() {
        // Arrange
        let msg: Message = ":alice!a@b NOTICE #osu :hi".parse().unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg, CHANNEL), None);
    }

    #[test]
    fn server_message_without_nick_is_ignored() {
        // Arrange: server NOTICEs have a hostname prefix without a nick.
        let msg: Message = ":cho.ppy.sh NOTICE * :*** Looking up your hostname"
            .parse()
            .unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg, CHANNEL), None);
    }

    #[test]
    fn privmsg_from_hostname_only_prefix_is_ignored() {
        // Arrange: a PRIVMSG whose source is a server name has no user nick.
        let msg: Message = ":cho.ppy.sh PRIVMSG #osu :hello".parse().unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg, CHANNEL), None);
    }

    #[test]
    fn numeric_responses_are_ignored() {
        // Arrange
        let msg: Message = ":srv 001 me :Welcome".parse().unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg, CHANNEL), None);
    }

    #[test]
    fn trailing_carriage_return_is_stripped() {
        // Arrange: IRC lines end with \r\n; a stray \r must not reach the UI.
        let msg: Message = ":alice!a@b PRIVMSG #osu :hello\r".parse().unwrap();

        // Act
        let chat = ChatMessage::from_proto(&msg, CHANNEL);

        // Assert
        assert_eq!(chat.unwrap().text, "hello");
    }
}
