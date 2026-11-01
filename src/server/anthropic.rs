//! Anthropic's Messages API, which Anthropic recommends for its models.

use super::http::Http;
use super::{Message, ModelInfo, ModelServer, Role};
use crate::config::Config;
use anyhow::{Result, bail};
use reqwest::RequestBuilder;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The API version git-agent speaks. It takes no sampling settings such as
/// `temperature`, on any model.
const VERSION: &str = "2026-10-21";

pub struct Anthropic {
    http: Http,
    base_url: String,
    api_key: Option<String>,
    max_tokens: usize,
}

impl Anthropic {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            http: Http::new(config)?,
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            api_key: config.api_key.clone(),
            max_tokens: config.max_output_tokens,
        })
    }

    async fn send<T: DeserializeOwned>(&self, request: RequestBuilder, url: &str) -> Result<T> {
        let request = request.header("anthropic-version", VERSION);
        let request = match &self.api_key {
            Some(key) => request.header("x-api-key", key),
            None => request,
        };
        self.http.send(request, url).await
    }
}

impl ModelServer for Anthropic {
    async fn models(&self) -> Result<Vec<ModelInfo>> {
        // The list comes in pages of 20 unless asked for more.
        let url = format!("{}/models?limit=1000", self.base_url);
        let list: ModelList = self.send(self.http.get(&url), &url).await?;
        let models = list.data.into_iter().map(|model| ModelInfo {
            context_length: model.max_input_tokens,
            ..ModelInfo::named(&model.id)
        });
        Ok(models.collect())
    }

    async fn complete(&self, model: &str, messages: &[Message]) -> Result<String> {
        let url = format!("{}/messages", self.base_url);
        let body = MessagesRequest::new(model, messages, self.max_tokens);
        let response: MessagesResponse = self.send(self.http.post(&url).json(&body), &url).await?;
        response.into_text(self.max_tokens)
    }
}

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: usize,
    /// This API takes the system prompt apart from the conversation.
    #[serde(skip_serializing_if = "String::is_empty")]
    system: String,
    messages: Vec<&'a Message>,
}

impl<'a> MessagesRequest<'a> {
    fn new(model: &'a str, messages: &'a [Message], max_tokens: usize) -> Self {
        let (system, turns): (Vec<&Message>, Vec<&Message>) = messages
            .iter()
            .partition(|message| message.role == Role::System);
        let system: Vec<&str> = system
            .iter()
            .map(|message| message.content.as_str())
            .collect();
        Self {
            model,
            max_tokens,
            system: system.join("\n\n"),
            messages: turns,
        }
    }
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ListedModel>,
}

#[derive(Deserialize)]
struct ListedModel {
    id: String,
    max_input_tokens: Option<usize>,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    stop_details: Option<StopDetails>,
}

/// A part of the reply. Only `text` parts hold the message; the others, such as
/// `thinking`, are skipped.
#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct StopDetails {
    category: Option<String>,
}

impl MessagesResponse {
    /// The reply's text. `max_tokens` is the limit the request was sent with.
    fn into_text(self, max_tokens: usize) -> Result<String> {
        // A declined request can hold partial text, so check before reading it.
        if self.stop_reason.as_deref() == Some("refusal") {
            let category = self.stop_details.and_then(|details| details.category);
            let category = category.map(|category| format!(" ({category})"));
            bail!(
                "the model declined to write the message{}; try another model with \
                 `git agent config`",
                category.unwrap_or_default()
            );
        }
        let text: String = self
            .content
            .into_iter()
            .filter(|block| block.kind == "text")
            .map(|block| block.text)
            .collect();
        if text.trim().is_empty() && self.stop_reason.as_deref() == Some("max_tokens") {
            bail!(
                "the model used all {max_tokens} output tokens before writing the message; \
                 raise the output limit with `git agent config` or --max-output-tokens"
            );
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(json: serde_json::Value) -> Result<String> {
        serde_json::from_value::<MessagesResponse>(json)
            .unwrap()
            .into_text(1000)
    }

    #[test]
    fn sends_the_system_prompt_apart_and_no_temperature() {
        let messages = [
            Message::system("write commit messages"),
            Message::user("the diff"),
            Message::assistant("feat: x"),
            Message::user("shorter"),
        ];
        let body = serde_json::to_value(MessagesRequest::new("claude", &messages, 2000)).unwrap();
        assert_eq!(
            body,
            json!({
                "model": "claude",
                "max_tokens": 2000,
                "system": "write commit messages",
                "messages": [
                    {"role": "user", "content": "the diff"},
                    {"role": "assistant", "content": "feat: x"},
                    {"role": "user", "content": "shorter"},
                ],
            })
        );
    }

    #[test]
    fn reads_the_text_and_skips_thinking() {
        let reply = response(json!({
            "content": [
                {"type": "thinking", "thinking": "", "signature": "abc"},
                {"type": "text", "text": "fix: handle empty input"},
            ],
            "stop_reason": "end_turn",
        }));
        assert_eq!(reply.unwrap(), "fix: handle empty input");
    }

    #[test]
    fn reports_refusals_and_cut_off_replies() {
        let refused = response(json!({
            "content": [{"type": "text", "text": "partial"}],
            "stop_reason": "refusal",
            "stop_details": {"type": "refusal", "category": "cyber"},
        }));
        assert!(
            refused
                .unwrap_err()
                .to_string()
                .contains("declined to write the message (cyber)")
        );
        let cut_off = response(json!({
            "content": [{"type": "thinking", "thinking": ""}],
            "stop_reason": "max_tokens",
        }));
        assert!(
            cut_off
                .unwrap_err()
                .to_string()
                .contains("all 1000 output tokens")
        );
    }

    #[test]
    fn reads_context_windows_from_the_model_list() {
        let list: ModelList = serde_json::from_value(json!({
            "data": [{"type": "model", "id": "claude", "display_name": "Claude",
                      "max_input_tokens": 1000000, "max_tokens": 128000}],
            "has_more": false,
        }))
        .unwrap();
        assert_eq!(list.data[0].max_input_tokens, Some(1000000));
    }
}
