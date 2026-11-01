//! JSON over HTTP, with the error messages git-agent gives for every server.

use crate::config::Config;
use anyhow::{Context, Result, anyhow, bail};
use reqwest::header::CONTENT_TYPE;
use reqwest::{RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use std::fmt::Write;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest part of a non-JSON error body to show.
const MAX_DETAIL_CHARS: usize = 300;

pub struct Http {
    client: reqwest::Client,
    timeout: Duration,
}

impl Http {
    pub fn new(config: &Config) -> Result<Self> {
        let timeout = Duration::from_secs(config.timeout_secs);
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(timeout)
            // Requests are separated by the user reviewing the message, long enough for
            // servers to close idle keep-alive connections. Reusing one fails with
            // "connection closed before message completed", so open a new one each time.
            .pool_max_idle_per_host(0)
            .build()
            .context("failed to create HTTP client")?;
        Ok(Self { client, timeout })
    }

    pub fn get(&self, url: &str) -> RequestBuilder {
        self.client.get(url)
    }

    pub fn post(&self, url: &str) -> RequestBuilder {
        self.client.post(url)
    }

    /// Sends `request` to `url` and decodes the JSON it answers with.
    pub async fn send<T: DeserializeOwned>(&self, request: RequestBuilder, url: &str) -> Result<T> {
        let response = request.send().await.map_err(|err| {
            if err.is_connect() {
                anyhow!(
                    "could not connect to {url}; is the model server running? \
                     Choose another server with `git agent config` or pass --base-url"
                )
            } else if err.is_timeout() {
                anyhow!(
                    "no response from {url} within {}s; raise the timeout with `git agent config`",
                    self.timeout.as_secs()
                )
            } else {
                anyhow!(err).context(format!("request to {url} failed"))
            }
        })?;

        let status = response.status();
        let is_html = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/html"));
        let body = response
            .text()
            .await
            .with_context(|| format!("failed to read the response from {url}"))?;
        if !status.is_success() {
            return Err(status_error(url, status, &body, is_html));
        }
        // Some servers (LM Studio, for one) answer unknown endpoints with a
        // success status and an error body.
        serde_json::from_str(&body).or_else(|err| match json_error(&body) {
            Some(message) => bail!("{url} returned an error: {message}"),
            None => Err(err).with_context(|| format!("unexpected response from {url}")),
        })
    }
}

/// Explains a failed request, with a hint for the failures a setting can fix.
fn status_error(url: &str, status: StatusCode, body: &str, is_html: bool) -> anyhow::Error {
    let detail = error_detail(body, is_html);
    let mut message = format!("{url} returned {status}");
    if let Some(detail) = &detail {
        let _ = write!(message, ": {detail}");
    }
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        message.push_str(
            "\nThe server needs a valid API key. Save one with `git agent config` (macOS) \
             or set GIT_AGENT_API_KEY.",
        );
    } else if detail.as_deref().is_some_and(mentions_context) {
        message.push_str(
            "\nThe prompt may be larger than the model's context. Set its real size with \
             `git agent config` or --context-length.",
        );
    }
    anyhow!(message)
}

/// Whether an error message is about the context window, as servers word it
/// ("context length", "context size", "n_ctx", "prompt is too long", ...).
fn mentions_context(detail: &str) -> bool {
    let detail = detail.to_ascii_lowercase();
    ["context", "n_ctx", "too long"]
        .iter()
        .any(|word| detail.contains(word))
}

/// The message from a JSON error body, as OpenAI-compatible servers and
/// Anthropic send them.
fn json_error(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let message = json
        .pointer("/error/message")
        .or_else(|| json.get("error"))?;
    Some(message.as_str()?.trim().to_owned())
}

/// Explains an error response: the message from a JSON error body, a hint if
/// the server sent a web page, or the start of any other body.
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
            detail(r#"{"type":"error","error":{"type":"not_found_error","message":"no model"}}"#)
                .unwrap(),
            "no model"
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
    fn adds_hints_for_keys_and_context() {
        let error = |status, body| status_error("u", status, body, false).to_string();
        assert!(error(StatusCode::UNAUTHORIZED, "").contains("valid API key"));
        let too_long = r#"{"error":{"message":"prompt is too long: 5000 tokens > 4096"}}"#;
        assert!(error(StatusCode::BAD_REQUEST, too_long).contains("larger than the model's"));
        assert!(!error(StatusCode::BAD_REQUEST, "model not found").contains('\n'));
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
