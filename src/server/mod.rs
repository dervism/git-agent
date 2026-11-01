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
use crate::providers;
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

/// What git-agent needs from a model server.
pub trait ModelServer {
    /// The server's models, with what it reports about each.
    async fn models(&self) -> Result<Vec<ModelInfo>>;

    /// Sends `messages` to `model` and returns the text of its reply.
    async fn complete(&self, model: &str, messages: &[Message]) -> Result<String>;
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

    async fn complete(&self, model: &str, messages: &[Message]) -> Result<String> {
        match self {
            Self::OpenAi(server) => server.complete(model, messages).await,
            Self::Anthropic(server) => server.complete(model, messages).await,
        }
    }
}
