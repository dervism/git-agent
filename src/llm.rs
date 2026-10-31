//! Client for OpenAI-compatible chat completion APIs
//! (LM Studio, Ollama, llama.cpp, vLLM, OpenAI, ...).

use crate::config::Config;
use anyhow::{Context, Result, anyhow, bail};
use reqwest::{RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, Serialize)]
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

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    temperature: f32,
    stream: bool,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

pub struct Client {
    http: reqwest::Client,
    base_url: String,
    model: String,
    api_key: Option<String>,
    temperature: f32,
    timeout: Duration,
}

impl Client {
    pub fn new(config: &Config) -> Result<Self> {
        let timeout = Duration::from_secs(config.timeout_secs);
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(timeout)
            // Requests are separated by the user reviewing the message, long enough for
            // servers to close idle keep-alive connections. Reusing one fails with
            // "connection closed before message completed", so open a new one each time.
            .pool_max_idle_per_host(0)
            .build()
            .context("failed to create HTTP client")?;
        Ok(Self {
            http,
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            model: config.model.clone(),
            api_key: config.api_key.clone(),
            temperature: config.temperature,
            timeout,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Sends a conversation and returns the model's raw reply.
    pub async fn complete(&self, messages: &[Message]) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = ChatRequest {
            model: &self.model,
            messages,
            temperature: self.temperature,
            stream: false,
        };
        let response: ChatResponse = self
            .send(self.http.post(&url).json(&body), &url)
            .await?
            .json()
            .await
            .with_context(|| format!("unexpected response from {url}"))?;
        response
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .ok_or_else(|| anyhow!("the model returned no message"))
    }

    /// IDs of the models the server offers.
    pub async fn models(&self) -> Result<Vec<String>> {
        let url = format!("{}/models", self.base_url);
        let list: ModelList = self
            .send(self.http.get(&url), &url)
            .await?
            .json()
            .await
            .with_context(|| format!("unexpected response from {url}"))?;
        Ok(list.data.into_iter().map(|model| model.id).collect())
    }

    async fn send(&self, request: RequestBuilder, url: &str) -> Result<Response> {
        let request = match &self.api_key {
            Some(key) => request.bearer_auth(key),
            None => request,
        };
        let response = request.send().await.map_err(|err| {
            if err.is_connect() {
                anyhow!(
                    "could not connect to {url}; is the model server running? \
                     Set `base_url` in the config file or pass --base-url"
                )
            } else if err.is_timeout() {
                anyhow!(
                    "no response from {url} within {}s; raise `timeout_secs` in the config file",
                    self.timeout.as_secs()
                )
            } else {
                anyhow!(err).context(format!("request to {url} failed"))
            }
        })?;

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        bail!("{url} returned {status}: {}", error_detail(&body));
    }
}

/// The error message from an OpenAI-style error body, or the body itself.
fn error_detail(body: &str) -> String {
    let json: Option<serde_json::Value> = serde_json::from_str(body).ok();
    json.as_ref()
        .and_then(|json| json.pointer("/error/message").or_else(|| json.get("error")))
        .and_then(|message| message.as_str())
        .unwrap_or(body)
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_error_messages() {
        assert_eq!(
            error_detail(r#"{"error":{"message":"model not found","type":"x"}}"#),
            "model not found"
        );
        assert_eq!(
            error_detail(r#"{"error":"no model loaded"}"#),
            "no model loaded"
        );
        assert_eq!(error_detail("Bad Gateway\n"), "Bad Gateway");
    }
}
