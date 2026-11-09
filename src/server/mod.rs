//! Model servers: what git-agent needs from one, and the APIs that provide it.
//!
//! [`ModelServer`] is the interface. `OpenAiCompatible` implements it for the
//! OpenAI-compatible API that local servers and most cloud providers offer, and
//! `Anthropic` for Anthropic's Messages API. [`Server`] connects to the one a
//! config points at. Both share one HTTP transport (`http.rs`).

mod anthropic;
mod http;
mod openai;

use crate::config::Config;
use crate::{providers, safe_text};
use anthropic::Anthropic;
use anyhow::Result;
use openai::OpenAiCompatible;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

/// A model, as its server reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    /// Whether the model is in memory, if the server says.
    pub loaded: Option<bool>,
    /// Context window in tokens, if the server says.
    pub context_length: Option<usize>,
}

impl ModelInfo {
    /// A model the server said nothing about.
    pub fn named(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            loaded: None,
            context_length: None,
        }
    }
}

/// What goes to a model: which model, and the conversation, with its control and invisible
/// characters escaped so that a file can't hide instructions from the person reviewing its
/// diff. [`Request::new`] is the only way to make one, so nothing reaches a model unescaped.
/// Patches aren't made from this text, so commits are unchanged.
pub struct Request<'a> {
    model: &'a str,
    messages: Vec<Message>,
}

impl<'a> Request<'a> {
    pub fn new(model: &'a str, conversation: &[Message]) -> Self {
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

/// What git-agent needs from a model server.
pub trait ModelServer {
    /// The server's models, with what it reports about each.
    async fn models(&self) -> Result<Vec<ModelInfo>>;

    /// Sends `request` to its model and returns the reply.
    async fn complete(&self, request: &Request<'_>) -> Result<Reply>;
}

/// The model server a config points at, speaking the API it offers.
pub enum Server {
    OpenAi(OpenAiCompatible),
    Anthropic(Anthropic),
}

impl Server {
    pub fn connect(config: &Config) -> Result<Self> {
        Ok(if providers::is_anthropic(&config.base_url) {
            Self::Anthropic(Anthropic::new(config)?)
        } else {
            Self::OpenAi(OpenAiCompatible::new(config)?)
        })
    }

    /// Whether the API takes a temperature. Anthropic's sets its own.
    pub fn takes_temperature(&self) -> bool {
        matches!(self, Self::OpenAi(_))
    }
}

impl ModelServer for Server {
    async fn models(&self) -> Result<Vec<ModelInfo>> {
        match self {
            Self::OpenAi(server) => server.models().await,
            Self::Anthropic(server) => server.models().await,
        }
    }

    async fn complete(&self, request: &Request<'_>) -> Result<Reply> {
        match self {
            Self::OpenAi(server) => server.complete(request).await,
            Self::Anthropic(server) => server.complete(request).await,
        }
    }
}

#[cfg(test)]
pub mod test_support {
    use super::{Message, ModelInfo, ModelServer, Reply, Request};
    use anyhow::Result;
    use std::cell::RefCell;

    /// A model that always gives the same reply, and keeps each conversation it was sent.
    pub struct FakeServer<'a> {
        reply: &'a str,
        pub received: RefCell<Vec<Vec<Message>>>,
    }

    impl<'a> FakeServer<'a> {
        pub fn replying(reply: &'a str) -> Self {
            Self {
                reply,
                received: RefCell::new(Vec::new()),
            }
        }
    }

    impl ModelServer for FakeServer<'_> {
        async fn models(&self) -> Result<Vec<ModelInfo>> {
            Ok(Vec::new())
        }

        async fn complete(&self, request: &Request<'_>) -> Result<Reply> {
            self.received.borrow_mut().push(request.messages().to_vec());
            Ok(Reply {
                text: self.reply.to_owned(),
            })
        }
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
        let request = Request::new("m", &conversation);
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
}
