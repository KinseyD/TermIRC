//! Chat message model and conversion from IRC protocol messages.

use irc::client::prelude::{Command, Message};

/// A single chat message to display, extracted from an IRC PRIVMSG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub nick: String,
    pub text: String,
}

impl ChatMessage {
    /// Extract a chat message from a protocol message.
    ///
    /// Returns `None` for anything that is not a PRIVMSG from an identifiable
    /// user (NOTICEs, server numerics, hostname-only prefixes, ...).
    pub fn from_proto(msg: &Message) -> Option<ChatMessage> {
        if let Command::PRIVMSG(_, body) = &msg.command {
            let nick = msg.source_nickname()?.to_string();
            let text = body.trim_end_matches(['\r', '\n']).to_string();
            Some(ChatMessage { nick, text })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privmsg_with_full_nickmask_yields_nick_and_text() {
        // Arrange
        let msg: Message = ":alice!a@b PRIVMSG #osu :hello world".parse().unwrap();

        // Act
        let chat = ChatMessage::from_proto(&msg);

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
        let chat = ChatMessage::from_proto(&msg);

        // Assert
        assert_eq!(chat.unwrap().nick, "Bubble_Shark");
    }

    #[test]
    fn notice_is_ignored() {
        // Arrange
        let msg: Message = ":alice!a@b NOTICE #osu :hi".parse().unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg), None);
    }

    #[test]
    fn server_message_without_nick_is_ignored() {
        // Arrange: server NOTICEs have a hostname prefix without a nick.
        let msg: Message = ":cho.ppy.sh NOTICE * :*** Looking up your hostname"
            .parse()
            .unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg), None);
    }

    #[test]
    fn numeric_responses_are_ignored() {
        // Arrange
        let msg: Message = ":srv 001 me :Welcome".parse().unwrap();

        // Act & Assert
        assert_eq!(ChatMessage::from_proto(&msg), None);
    }

    #[test]
    fn trailing_carriage_return_is_stripped() {
        // Arrange: IRC lines end with \r\n; a stray \r must not reach the UI.
        let msg: Message = ":alice!a@b PRIVMSG #osu :hello\r".parse().unwrap();

        // Act
        let chat = ChatMessage::from_proto(&msg);

        // Assert
        assert_eq!(chat.unwrap().text, "hello");
    }
}
