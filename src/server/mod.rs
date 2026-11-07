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

#[cfg(test)]
pub mod test_support {
    use super::{Message, ModelInfo, ModelServer};
    use crate::prompt;
    use anyhow::Result;
    use std::cell::{Cell, RefCell};

    /// A model that replies with fixed text, keeps what it was sent, and can do something
    /// while the user waits for its reply.
    pub struct FakeServer<'a> {
        pub plans: RefCell<Vec<&'a str>>,
        pub message: &'a str,
        pub message_requests: Cell<usize>,
        pub received: RefCell<Vec<Vec<Message>>>,
        during_plan: RefCell<Option<Box<dyn FnOnce() + 'a>>>,
        during_message: RefCell<Option<Box<dyn FnOnce() + 'a>>>,
    }

    impl<'a> FakeServer<'a> {
        pub fn new(plan: &'a str, message: &'a str) -> Self {
            Self {
                plans: RefCell::new(vec![plan]),
                message,
                message_requests: Cell::new(0),
                received: RefCell::new(Vec::new()),
                during_plan: RefCell::new(None),
                during_message: RefCell::new(None),
            }
        }

        pub fn during_plan(self, action: impl FnOnce() + 'a) -> Self {
            *self.during_plan.borrow_mut() = Some(Box::new(action));
            self
        }

        pub fn during_message(self, action: impl FnOnce() + 'a) -> Self {
            *self.during_message.borrow_mut() = Some(Box::new(action));
            self
        }
    }

    impl ModelServer for FakeServer<'_> {
        async fn models(&self) -> Result<Vec<ModelInfo>> {
            Ok(Vec::new())
        }

        async fn complete(&self, _model: &str, messages: &[Message]) -> Result<String> {
            self.received.borrow_mut().push(messages.to_vec());
            if messages[0].content == prompt::SPLIT_PROMPT {
                if let Some(action) = self.during_plan.borrow_mut().take() {
                    action();
                }
                let mut plans = self.plans.borrow_mut();
                let plan = if plans.len() > 1 {
                    plans.remove(0)
                } else {
                    plans[0]
                };
                return Ok(plan.to_owned());
            }
            self.message_requests.set(self.message_requests.get() + 1);
            if let Some(action) = self.during_message.borrow_mut().take() {
                action();
            }
            Ok(self.message.to_owned())
        }
    }
}
