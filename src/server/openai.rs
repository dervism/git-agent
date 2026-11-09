//! The OpenAI-compatible API, offered by LM Studio, Ollama, llama.cpp, vLLM,
//! OpenAI and most other cloud providers.
//!
//! Its model list has only IDs, so git-agent also asks the APIs that LM Studio,
//! Ollama and llama.cpp offer next to it which model is loaded and how much
//! context it has. vLLM and some cloud providers put the context length in the
//! model list itself.

use super::http::Http;
use super::{Message, ModelInfo, ModelServer, Reply, Request};
use crate::config::Config;
use anyhow::{Result, anyhow};
use reqwest::RequestBuilder;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::time::Duration;

/// Longest wait for a server-specific endpoint, which the server may not have.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct OpenAiCompatible {
    http: Http,
    base_url: String,
    api_key: Option<String>,
    temperature: Option<f32>,
}

impl OpenAiCompatible {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            http: Http::new(config)?,
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            api_key: config.api_key.clone(),
            temperature: config.effective_temperature(),
        })
    }

    async fn send<T: DeserializeOwned>(&self, request: RequestBuilder, url: &str) -> Result<T> {
        let request = match &self.api_key {
            Some(key) => request.bearer_auth(key),
            None => request,
        };
        self.http.send(request, url).await
    }

    /// Adds what a server-specific endpoint reports. Returns false if the server
    /// doesn't have the endpoint.
    async fn add_details<D: ServerDetails>(&self, models: &mut Vec<ModelInfo>) -> bool {
        let root = self.base_url.strip_suffix("/v1").unwrap_or(&self.base_url);
        let url = format!("{root}/{}", D::PATH);
        let details = self.send::<D>(self.http.get(&url), &url);
        match tokio::time::timeout(PROBE_TIMEOUT, details).await {
            Ok(Ok(details)) => {
                details.apply(models);
                true
            }
            _ => false,
        }
    }
}

impl ModelServer for OpenAiCompatible {
    async fn models(&self) -> Result<Vec<ModelInfo>> {
        let url = format!("{}/models", self.base_url);
        let list: ModelList = self.send(self.http.get(&url), &url).await?;
        let mut models: Vec<ModelInfo> =
            list.data.into_iter().map(ListedModel::into_info).collect();
        if models.iter().all(|model| model.context_length.is_none()) {
            // The first of these endpoints that the server has fills in the rest.
            let _ = self.add_details::<LmStudioModels>(&mut models).await
                || self.add_details::<OllamaRunning>(&mut models).await
                || self.add_details::<LlamaCppProps>(&mut models).await;
        }
        Ok(models)
    }

    async fn complete(&self, request: &Request<'_>) -> Result<Reply> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = ChatRequest {
            model: request.model(),
            messages: request.messages(),
            temperature: self.temperature,
            stream: false,
        };
        let response: ChatResponse = self.send(self.http.post(&url).json(&body), &url).await?;
        let text = response
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .ok_or_else(|| anyhow!("the model returned no message"))?;
        Ok(Reply { text })
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
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
    data: Vec<ListedModel>,
}

#[derive(Deserialize)]
struct ListedModel {
    id: String,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl ListedModel {
    fn into_info(self) -> ModelInfo {
        // vLLM reports `max_model_len`; OpenRouter and others `context_length`.
        let context_length = ["max_model_len", "context_length"]
            .iter()
            .find_map(|key| self.extra.get(*key)?.as_u64())
            .and_then(|tokens| usize::try_from(tokens).ok());
        ModelInfo {
            context_length,
            ..ModelInfo::named(&self.id)
        }
    }
}

/// A server-specific endpoint next to the OpenAI-compatible API, and what it
/// adds to the model list.
trait ServerDetails: DeserializeOwned {
    /// The endpoint, relative to the server's root.
    const PATH: &'static str;

    fn apply(self, models: &mut Vec<ModelInfo>);
}

/// LM Studio's `api/v0/models`: every downloaded model, with its state.
#[derive(Deserialize)]
struct LmStudioModels {
    data: Vec<LmStudioModel>,
}

#[derive(Deserialize)]
struct LmStudioModel {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    state: String,
    loaded_context_length: Option<usize>,
}

impl ServerDetails for LmStudioModels {
    const PATH: &'static str = "api/v0/models";

    fn apply(self, models: &mut Vec<ModelInfo>) {
        let details = |id: &str| self.data.iter().find(|model| model.id == id);
        // Embedding models can't write commit messages.
        models.retain(|model| details(&model.id).is_none_or(|lm| lm.kind != "embeddings"));
        for model in models {
            if let Some(lm) = details(&model.id) {
                model.loaded = Some(lm.state == "loaded");
                model.context_length = lm.loaded_context_length;
            }
        }
    }
}

/// Ollama's `api/ps`: the models in memory.
#[derive(Deserialize)]
struct OllamaRunning {
    models: Vec<OllamaModel>,
}

#[derive(Deserialize)]
struct OllamaModel {
    name: String,
    #[serde(default)]
    context_length: usize,
}

impl ServerDetails for OllamaRunning {
    const PATH: &'static str = "api/ps";

    fn apply(self, models: &mut Vec<ModelInfo>) {
        for model in models {
            // Ollama accepts "llama3.2" for "llama3.2:latest".
            let running = self.models.iter().find(|running| {
                running.name == model.id || running.name == format!("{}:latest", model.id)
            });
            model.loaded = Some(running.is_some());
            model.context_length = running
                .map(|running| running.context_length)
                .filter(|&tokens| tokens > 0);
        }
    }
}

/// llama.cpp's `props`: the context of each request slot of its single model.
#[derive(Deserialize)]
struct LlamaCppProps {
    default_generation_settings: LlamaCppSettings,
}

#[derive(Deserialize)]
struct LlamaCppSettings {
    n_ctx: usize,
}

impl ServerDetails for LlamaCppProps {
    const PATH: &'static str = "props";

    fn apply(self, models: &mut Vec<ModelInfo>) {
        if let [model] = models.as_mut_slice() {
            model.loaded = Some(true);
            model.context_length = Some(self.default_generation_settings.n_ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(json: &str) -> Vec<ModelInfo> {
        let list: ModelList = serde_json::from_str(json).unwrap();
        list.data.into_iter().map(ListedModel::into_info).collect()
    }

    fn details<D: ServerDetails>(json: &str) -> D {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn sends_temperature_only_when_set() {
        let request = |temperature| {
            let body = ChatRequest {
                model: "m",
                messages: &[],
                temperature,
                stream: false,
            };
            serde_json::to_string(&body).unwrap()
        };
        assert!(!request(None).contains("temperature"));
        assert!(request(Some(0.4)).contains("\"temperature\":0.4"));
    }

    #[test]
    fn reads_context_lengths_from_the_model_list() {
        let models = listed(
            r#"{"data": [
                {"id": "vllm-model", "max_model_len": 32768},
                {"id": "hosted", "context_length": 200000},
                {"id": "plain", "object": "model", "context_length": null}
            ]}"#,
        );
        let lengths: Vec<_> = models.iter().map(|model| model.context_length).collect();
        assert_eq!(lengths, [Some(32768), Some(200000), None]);
    }

    #[test]
    fn applies_lm_studio_details() {
        let mut models = listed(r#"{"data": [{"id": "gemma"}, {"id": "qwen"}, {"id": "embed"}]}"#);
        details::<LmStudioModels>(
            r#"{"data": [
                {"id": "gemma", "type": "vlm", "state": "loaded",
                 "max_context_length": 131072, "loaded_context_length": 32768},
                {"id": "qwen", "type": "llm", "state": "not-loaded", "max_context_length": 262144},
                {"id": "embed", "type": "embeddings", "state": "loaded"}
            ]}"#,
        )
        .apply(&mut models);
        let qwen = ModelInfo {
            loaded: Some(false),
            ..ModelInfo::named("qwen")
        };
        let gemma = ModelInfo {
            loaded: Some(true),
            context_length: Some(32768),
            ..ModelInfo::named("gemma")
        };
        assert_eq!(models, [gemma, qwen]);
        // LM Studio answers unknown endpoints with an error body, which must not match.
        assert!(
            serde_json::from_str::<LmStudioModels>(r#"{"error": "Unexpected endpoint"}"#).is_err()
        );
    }

    #[test]
    fn applies_ollama_details() {
        let mut models = listed(r#"{"data": [{"id": "llama3.2"}, {"id": "qwen3:8b"}]}"#);
        details::<OllamaRunning>(
            r#"{"models": [{"name": "llama3.2:latest", "model": "llama3.2:latest",
                            "size": 1, "context_length": 8192}]}"#,
        )
        .apply(&mut models);
        assert_eq!(models[0].loaded, Some(true));
        assert_eq!(models[0].context_length, Some(8192));
        assert_eq!(models[1].loaded, Some(false));
        assert_eq!(models[1].context_length, None);
    }

    #[test]
    fn applies_llama_cpp_details() {
        let mut models = listed(r#"{"data": [{"id": "model.gguf"}]}"#);
        details::<LlamaCppProps>(
            r#"{"default_generation_settings": {"id": 0, "n_ctx": 16384}, "total_slots": 1}"#,
        )
        .apply(&mut models);
        assert_eq!(models[0].context_length, Some(16384));
    }
}
