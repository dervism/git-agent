//! The model's context as a budget: what text costs in tokens, what's kept free for the
//! reply, and how much room that leaves for the conversation.

use crate::server::Message;

/// Rough number of bytes per token. Common tokenizers average 3 to 4 bytes per
/// token for code and English, and about 3 for most other scripts.
const BYTES_PER_TOKEN: usize = 3;

/// Room for the headings around the changes and the chat template's markup.
const FRAMING_BYTES: usize = 256;

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

/// Tokens kept free for the reply, including any reasoning, and for a round of
/// feedback: up to the output limit, but at most a quarter of the context, so a
/// small context still has room for the changes.
fn reply_reserve(context_length: usize, max_output_tokens: usize) -> usize {
    (context_length / 4).min(max_output_tokens).max(1024)
}

/// Tokens the conversation may take in a context of `context_length` tokens, with room
/// for a reply of up to `max_output_tokens`.
fn conversation_room(context_length: usize, max_output_tokens: usize) -> usize {
    context_length.saturating_sub(reply_reserve(context_length, max_output_tokens))
}

/// Bytes the user message may take in a context of `context_length` tokens,
/// with room for a reply of up to `max_output_tokens`.
pub fn room(system_prompt: &str, context_length: usize, max_output_tokens: usize) -> usize {
    let budget = conversation_room(context_length, max_output_tokens) * BYTES_PER_TOKEN;
    budget.saturating_sub(system_prompt.trim().len() + FRAMING_BYTES)
}

/// Whether `messages` leave room in the context for another reply.
pub fn has_room(messages: &[Message], context_length: usize, max_output_tokens: usize) -> bool {
    let reply = (reply_reserve(context_length, max_output_tokens) / 2).max(1024);
    tokens_in(messages) + reply <= context_length
}

/// Whether escaping made `conversation` too large for the model's context, with room left
/// for its reply. What's sent is sized before it's escaped, so only text full of such
/// characters can get here.
pub fn outgrew_context(
    conversation: &[Message],
    outgoing: &[Message],
    context_length: usize,
    max_output_tokens: usize,
) -> bool {
    let room = conversation_room(context_length, max_output_tokens);
    tokens_in(conversation) <= room && tokens_in(outgoing) > room
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_MAX_OUTPUT_TOKENS as REPLY;
    use crate::server::Request;

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
        assert_eq!(reply_reserve(4096, REPLY), 1024);
        assert_eq!(reply_reserve(100_000, REPLY), REPLY);
        assert_eq!(reply_reserve(100_000, 2000), 2000);
        assert_eq!(reply_reserve(1000, REPLY), 1024);
        assert_eq!(reply_reserve(1000, 10), 1024);
    }

    #[test]
    fn leaves_the_user_message_the_room_after_the_reply_and_system_prompt() {
        assert_eq!(room("", 4096, REPLY), (4096 - 1024) * 3 - 256);
        assert_eq!(
            room(" \n rules \n", 4096, REPLY),
            (4096 - 1024) * 3 - 256 - 5
        );
        assert_eq!(room("", 1_000_000, 4096), (1_000_000 - 4096) * 3 - 256);
        // A context smaller than the reply's reserve leaves no room at all.
        assert_eq!(room("", 1000, REPLY), 0);
        assert_eq!(room(&"x".repeat(10_000), 4096, REPLY), 0);
    }

    #[test]
    fn asks_again_only_while_a_reply_still_fits() {
        // A 4,096-token context keeps 1,024 tokens for the next reply.
        let fits = [Message::user("x".repeat(3072 * 3))];
        assert!(has_room(&fits, 4096, REPLY));
        let over = [Message::user("x".repeat(3072 * 3 + 1))];
        assert!(!has_room(&over, 4096, REPLY));
        // A large context keeps half the reply's reserve.
        let large = [Message::user("x".repeat((100_000 - 8000) * 3))];
        assert!(has_room(&large, 100_000, REPLY));
        assert!(!has_room(&large, 99_999, REPLY));
        assert!(has_room(&[], 1024, REPLY));
        assert!(!has_room(&[], 1023, REPLY));
    }

    #[test]
    fn notices_only_escaping_that_outgrows_the_context() {
        let escaped =
            |conversation: &[Message]| Request::escaped("m", conversation).messages().to_vec();
        let plain = [Message::user("x".repeat(4000))];
        assert!(!outgrew_context(&plain, &escaped(&plain), 10, REPLY));

        let hidden = [Message::user("\u{1b}".repeat(4000))];
        let outgoing = escaped(&hidden);
        assert!(outgrew_context(&hidden, &outgoing, 3000, REPLY));
        assert!(!outgrew_context(&hidden, &outgoing, 100_000, REPLY));
        // 4,000 tokens fit in 5,000, but not with 1,250 kept for the reply.
        assert!(outgrew_context(&hidden, &outgoing, 5000, REPLY));
        // A conversation over the room already is the server's to judge.
        let long = [Message::user(format!("{}\u{1b}", "x".repeat(40_000)))];
        assert!(!outgrew_context(&long, &escaped(&long), 5000, REPLY));
    }
}
