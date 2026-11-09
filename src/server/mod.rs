//! Model servers: what git-agent needs from one, and the APIs that provide it.
//!
//! [`ModelServer`] is the interface. `OpenAiCompatible` implements it for the
//! OpenAI-compatible API that local servers and most cloud providers offer, and
//! `Anthropic` for Anthropic's Messages API. [`Server`] connects to the one a
//! config points at. Both take a [`Request`] and give back a [`Reply`], and share one
//! HTTP transport (`http.rs`).

mod anthropic;
mod http;
mod openai;
mod request;

use crate::config::Config;
use crate::providers;
use anthropic::Anthropic;
use anyhow::Result;
use openai::OpenAiCompatible;
pub use request::{Reply, Request};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
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
    use anyhow::{Result, anyhow};
    use std::cell::RefCell;

    /// How a fake model answers a conversation.
    type Answer<'a> = Box<dyn Fn(&[Message]) -> String + 'a>;

    /// A model server for tests. It answers each conversation from the conversation as
    /// sent, keeps every conversation it was sent, and lists its models if it was given
    /// some.
    pub struct FakeServer<'a> {
        models: Option<Vec<ModelInfo>>,
        answer: Answer<'a>,
        pub received: RefCell<Vec<Vec<Message>>>,
    }

    impl<'a> FakeServer<'a> {
        /// A model that gives the same reply to everything.
        pub fn replying(reply: &'a str) -> Self {
            Self::answering(move |_| reply.to_owned())
        }

        /// A model that answers with what `answer` makes of the conversation.
        pub fn answering(answer: impl Fn(&[Message]) -> String + 'a) -> Self {
            Self {
                models: None,
                answer: Box::new(answer),
                received: RefCell::new(Vec::new()),
            }
        }

        /// A server that lists `models`, and is sent no conversation.
        pub fn listing(models: Vec<ModelInfo>) -> Self {
            Self {
                models: Some(models),
                ..Self::unlisted()
            }
        }

        /// A server that can't list its models, and is sent no conversation.
        pub fn unlisted() -> Self {
            Self::answering(|_| unreachable!("this server is only asked for its models"))
        }
    }

    impl ModelServer for FakeServer<'_> {
        async fn models(&self) -> Result<Vec<ModelInfo>> {
            self.models
                .clone()
                .ok_or_else(|| anyhow!("the server can't list its models"))
        }

        async fn complete(&self, request: &Request<'_>) -> Result<Reply> {
            self.received.borrow_mut().push(request.messages().to_vec());
            Ok(Reply::new((self.answer)(request.messages())))
        }
    }
}
