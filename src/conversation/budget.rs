//! The model's context as a budget: what text costs in tokens, what's kept free for the
//! reply, and how much room that leaves for the conversation.

use crate::config::{MIN_CONTEXT_LENGTH, MIN_OUTPUT_TOKENS};
use crate::server::Message;

/// Rough number of bytes per token. Common tokenizers average 3 to 4 bytes per
/// token for code and English, and about 3 for most other scripts.
const BYTES_PER_TOKEN: usize = 3;

/// Room for the headings around the changes and the chat template's markup.
const FRAMING_BYTES: usize = 256;

// The smallest context keeps as many tokens for the conversation as for the smallest reply.
const _: () = assert!(2 * MIN_OUTPUT_TOKENS <= MIN_CONTEXT_LENGTH);

/// The tokens the conversation takes, estimated from its size.
pub fn tokens_in(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| message.content.len().div_ceil(BYTES_PER_TOKEN))
        .sum()
}

/// The tokens a conversation of `bytes` takes once it's framed for the model.
pub fn framed_tokens(bytes: usize) -> usize {
    (bytes + FRAMING_BYTES).div_ceil(BYTES_PER_TOKEN)
}

/// What a conversation has to fit in: the model's context, with room kept free for a reply
/// of up to the model's output limit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    context_length: usize,
    max_output_tokens: usize,
}

impl Budget {
    pub(super) fn new(context_length: usize, max_output_tokens: usize) -> Self {
        Self {
            context_length,
            max_output_tokens,
        }
    }

    /// Bytes the user message may take after `system_prompt`.
    pub fn user_message_bytes(self, system_prompt: &str) -> usize {
        let conversation_bytes = self.conversation_tokens() * BYTES_PER_TOKEN;
        conversation_bytes.saturating_sub(system_prompt.trim().len() + FRAMING_BYTES)
    }

    /// Whether `messages` leave room in the context for another reply: half of the reply's
    /// reserve, and at least [`MIN_OUTPUT_TOKENS`].
    pub fn has_room_for_a_reply(self, messages: &[Message]) -> bool {
        let reply_tokens = (self.reply_reserve() / 2).max(MIN_OUTPUT_TOKENS);
        tokens_in(messages) + reply_tokens <= self.context_length
    }

    /// Whether escaping made `conversation` too large for the context, with the reply's reserve
    /// kept free. What's sent is sized before it's escaped, so only text full of such characters
    /// can get here.
    pub fn outgrown_by_escaping(self, conversation: &[Message], outgoing: &[Message]) -> bool {
        let room = self.conversation_tokens();
        tokens_in(conversation) <= room && tokens_in(outgoing) > room
    }

    /// Tokens kept free for the reply, including any reasoning, and for a round of
    /// feedback: up to the output limit, but at most a quarter of the context, so a
    /// small context still has room for the changes, and never less than
    /// [`MIN_OUTPUT_TOKENS`].
    fn reply_reserve(self) -> usize {
        (self.context_length / 4)
            .min(self.max_output_tokens)
            .max(MIN_OUTPUT_TOKENS)
    }

    /// The most tokens the conversation may take, with the reply's reserve kept free.
    fn conversation_tokens(self) -> usize {
        self.context_length.saturating_sub(self.reply_reserve())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_MAX_OUTPUT_TOKENS as REPLY;
    use crate::server::Request;

    fn budget(context_length: usize) -> Budget {
        Budget::new(context_length, REPLY)
    }

    #[test]
    fn counts_every_started_token_of_every_message() {
        assert_eq!(tokens_in(&[]), 0);
        assert_eq!(tokens_in(&[Message::user("")]), 0);
        assert_eq!(tokens_in(&[Message::user("abc")]), 1);
        assert_eq!(
            tokens_in(&[Message::system("abcd"), Message::user("é")]),
            2 + 1
        );
        assert_eq!(framed_tokens(0), 86);
        assert_eq!(framed_tokens(1), 86);
        assert_eq!(framed_tokens(2), 86);
        assert_eq!(framed_tokens(3), 87);
    }

    #[test]
    fn keeps_a_quarter_of_the_context_for_the_reply_within_its_limits() {
        assert_eq!(budget(4096).reply_reserve(), 1024);
        assert_eq!(budget(32_768).reply_reserve(), 8192);
        assert_eq!(budget(100_000).reply_reserve(), REPLY);
        assert_eq!(Budget::new(100_000, 2000).reply_reserve(), 2000);
        assert_eq!(budget(1000).reply_reserve(), MIN_OUTPUT_TOKENS);
        assert_eq!(Budget::new(1000, 10).reply_reserve(), MIN_OUTPUT_TOKENS);
    }

    #[test]
    fn leaves_the_user_message_the_room_after_the_reply_and_system_prompt() {
        assert_eq!(budget(4096).user_message_bytes(""), (4096 - 1024) * 3 - 256);
        assert_eq!(
            budget(4096).user_message_bytes(" \n rules \n"),
            (4096 - 1024) * 3 - 256 - 5
        );
        assert_eq!(
            budget(32_768).user_message_bytes(""),
            (32_768 - 8192) * 3 - 256
        );
        assert_eq!(
            Budget::new(1_000_000, 4096).user_message_bytes(""),
            (1_000_000 - 4096) * 3 - 256
        );
        // A context smaller than the reply's reserve leaves no room at all.
        assert_eq!(budget(1000).user_message_bytes(""), 0);
        assert_eq!(budget(4096).user_message_bytes(&"x".repeat(10_000)), 0);
    }

    #[test]
    fn asks_again_only_while_a_reply_still_fits() {
        // A 4,096-token context keeps 1,024 tokens for the next reply.
        let fits = [Message::user("x".repeat(3072 * 3))];
        assert!(budget(4096).has_room_for_a_reply(&fits));
        let over = [Message::user("x".repeat(3072 * 3 + 1))];
        assert!(!budget(4096).has_room_for_a_reply(&over));
        // A large context keeps half the reply's reserve.
        let large = [Message::user("x".repeat((100_000 - 8000) * 3))];
        assert!(budget(100_000).has_room_for_a_reply(&large));
        assert!(!budget(99_999).has_room_for_a_reply(&large));
        assert!(budget(1024).has_room_for_a_reply(&[]));
        assert!(!budget(1023).has_room_for_a_reply(&[]));
    }

    #[test]
    fn notices_only_escaping_that_outgrows_the_context() {
        let outgrown = |conversation: &[Message], context_length| {
            let outgoing = Request::escaped("m", conversation).messages().to_vec();
            budget(context_length).outgrown_by_escaping(conversation, &outgoing)
        };
        let plain = [Message::user("x".repeat(4000))];
        assert!(!outgrown(&plain, 10));

        let hidden = [Message::user("\u{1b}".repeat(4000))];
        assert!(outgrown(&hidden, 3000));
        assert!(!outgrown(&hidden, 100_000));
        // 1,000 tokens become 4,000 escaped: they fit in 5,000, but not with 1,250 kept for
        // the reply.
        let fewer = [Message::user("\u{1b}".repeat(3000))];
        assert!(outgrown(&fewer, 5000));
        assert!(!outgrown(&fewer, 5333));
        // 3,999 tokens become 4,000 escaped, one more than the room of 3,999.
        let edge = [Message::user(format!("{}\u{1b}", "x".repeat(11_996)))];
        assert!(outgrown(&edge, 5332));
        // A conversation over the room already is the server's to judge.
        let long = [Message::user(format!("{}\u{1b}", "x".repeat(40_000)))];
        assert!(!outgrown(&long, 5000));
    }
}
