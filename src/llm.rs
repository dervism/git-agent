//! Client for OpenAI-compatible chat completion APIs
//! (LM Studio, Ollama, llama.cpp, vLLM, OpenAI, ...).

use crate::config::Config;
use anyhow::{Context, Result, anyhow, bail};
use reqwest::header::CONTENT_TYPE;
use reqwest::{RequestBuilder, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest part of a non-JSON error body to show.
const MAX_DETAIL_CHARS: usize = 300;

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

pub struct Client {
    http: reqwest::Client,
    base_url: String,
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
            api_key: config.api_key.clone(),
            temperature: config.temperature,
            timeout,
        })
    }

    /// URL of an endpoint of the OpenAI-compatible API, e.g. `models`.
    pub fn api_url(&self, path: &str) -> String {
        format!("{}/{path}", self.base_url)
    }

    /// URL of a server-specific endpoint next to the OpenAI-compatible API,
    /// e.g. LM Studio's `api/v0/models`.
    pub fn server_url(&self, path: &str) -> String {
        let root = self.base_url.strip_suffix("/v1").unwrap_or(&self.base_url);
        format!("{root}/{path}")
    }

    /// Sends a conversation to `model` and returns its raw reply.
    pub async fn complete(&self, model: &str, messages: &[Message]) -> Result<String> {
        let url = self.api_url("chat/completions");
        let body = ChatRequest {
            model,
            messages,
            temperature: self.temperature,
            stream: false,
        };
        let response = self.send(self.http.post(&url).json(&body), &url).await?;
        let response: ChatResponse = parse(response, &url).await?;
        response
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .ok_or_else(|| anyhow!("the model returned no message"))
    }

    /// Fetches and decodes a JSON document.
    pub async fn get<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let response = self.send(self.http.get(url), url).await?;
        parse(response, url).await
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
        let is_html = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/html"));
        let body = response.text().await.unwrap_or_default();
        match error_detail(&body, is_html) {
            Some(detail) if mentions_context(&detail) => bail!(
                "{url} returned {status}: {detail}\n\
                 The prompt may be larger than the model's context. Set `context_length` \
                 in the config file (or pass --context-length) to the context the model \
                 is loaded with."
            ),
            Some(detail) => bail!("{url} returned {status}: {detail}"),
            None => bail!("{url} returned {status}"),
        }
    }
}

/// Whether an error message is about the context window, as servers word it
/// ("context length", "context size", "n_ctx", ...).
fn mentions_context(detail: &str) -> bool {
    let detail = detail.to_ascii_lowercase();
    detail.contains("context") || detail.contains("n_ctx")
}

/// Decodes a successful response. Some servers (LM Studio, for one) also answer
/// unknown endpoints with a success status and an error body.
async fn parse<T: DeserializeOwned>(response: Response, url: &str) -> Result<T> {
    let body = response
        .text()
        .await
        .with_context(|| format!("failed to read the response from {url}"))?;
    serde_json::from_str(&body).or_else(|err| match json_error(&body) {
        Some(message) => bail!("{url} returned an error: {message}"),
        None => Err(err).with_context(|| format!("unexpected response from {url}")),
    })
}

/// The message from an OpenAI-style error body.
fn json_error(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let message = json
        .pointer("/error/message")
        .or_else(|| json.get("error"))?;
    Some(message.as_str()?.trim().to_owned())
}

/// Explains an error response: the message from an OpenAI-style error body, a hint
/// if the server sent a web page, or the start of any other body.
fn error_detail(body: &str, is_html: bool) -> Option<String> {
    if let Some(message) = json_error(body) {
        return Some(message);
    }

    let body = body.trim();
    let start = body.get(..14).unwrap_or(body).to_ascii_lowercase();
    if is_html || start.starts_with("<!doctype html") || start.starts_with("<html") {
        return Some(
            "the server sent a web page instead of an API response; \
             check that `base_url` points to your model server's /v1 endpoint"
                .to_owned(),
        );
    }
    match body.char_indices().nth(MAX_DETAIL_CHARS) {
        _ if body.is_empty() => None,
        Some((end, _)) => Some(format!("{}…", &body[..end])),
        None => Some(body.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_error_messages() {
        let detail = |body: &str| error_detail(body, false);
        assert_eq!(
            detail(r#"{"error":{"message":"model not found","type":"x"}}"#).unwrap(),
            "model not found"
        );
        assert_eq!(
            detail(r#"{"error":"no model loaded"}"#).unwrap(),
            "no model loaded"
        );
        assert_eq!(detail("Bad Gateway\n").unwrap(), "Bad Gateway");
        assert_eq!(detail("  \n"), None);
    }

    #[test]
    fn replaces_web_pages_with_a_hint() {
        let page = "<!DOCTYPE html><html><body><p>Example Domain</p></body></html>";
        for detail in [error_detail(page, false), error_detail("Not found", true)] {
            let detail = detail.unwrap();
            assert!(detail.contains("web page") && detail.contains("base_url"));
        }
    }

    #[test]
    fn recognizes_context_errors() {
        assert!(mentions_context(
            "This model's maximum context length is 4096 tokens."
        ));
        assert!(mentions_context(
            "request (5000 tokens) exceeds the available Context Size"
        ));
        assert!(mentions_context("n_ctx is too small"));
        assert!(!mentions_context("model not found"));
    }

    #[test]
    fn shortens_long_bodies() {
        let detail = error_detail(&"x".repeat(1000), false).unwrap();
        assert_eq!(detail.chars().count(), MAX_DETAIL_CHARS + 1);
        assert!(detail.ends_with('…'));
    }
}
