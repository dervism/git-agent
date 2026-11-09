//! What goes to a model and what comes back.

use super::Message;
use crate::safe_text;

/// What goes to a model: which model, and the conversation, with its control and invisible
/// characters escaped so that a file can't hide instructions from the person reviewing its
/// diff. [`Request::escaped`] is the only way to make one, so nothing reaches a model
/// unescaped. Patches aren't made from this text, so commits are unchanged.
pub struct Request<'a> {
    model: &'a str,
    messages: Vec<Message>,
}

impl<'a> Request<'a> {
    pub fn escaped(model: &'a str, conversation: &[Message]) -> Self {
        let messages = conversation
            .iter()
            .map(|message| Message {
                role: message.role,
                content: safe_text::escaped(&message.content),
            })
            .collect();
        Self { model, messages }
    }

    pub fn model(&self) -> &str {
        self.model
    }

    /// The conversation as it goes to the model, escaped.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

/// A model's reply, as the server gave it.
pub struct Reply {
    pub text: String,
}

impl Reply {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_what_goes_to_the_model_and_keeps_the_rest() {
        let conversation = [
            Message::system("rules"),
            Message::user("+ blåbær\u{e0041}\r\n+ \u{1b}[2J Kjøre.txt\u{202e}"),
        ];
        let request = Request::escaped("m", &conversation);
        assert_eq!(request.model(), "m");
        let sent = request.messages();
        assert_eq!(sent[0].role, conversation[0].role);
        assert_eq!(sent[1].role, conversation[1].role);
        assert_eq!(sent[0].content, "rules");
        assert_eq!(
            sent[1].content,
            "+ blåbær\\363\\240\\201\\201\r\n+ \\033[2J Kjøre.txt\\342\\200\\256"
        );
    }

    #[test]
    fn sends_an_empty_conversation_as_it_is() {
        assert!(Request::escaped("m", &[]).messages().is_empty());
    }
}
