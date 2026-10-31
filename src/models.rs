//! What the server says about its models: which one is loaded and how much
//! context it has.
//!
//! The OpenAI-compatible model list only has IDs, so git-agent also asks the
//! APIs that LM Studio, Ollama and llama.cpp offer next to it. vLLM and some
//! hosted services put the context length in the model list itself.

use crate::config::Config;
use crate::llm::Client;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::time::Duration;

/// Context length assumed when neither the config nor the server gives one.
/// It fits the defaults of common local servers; Ollama, for one, silently
/// cuts prompts that are longer than its context.
pub const FALLBACK_CONTEXT_LENGTH: usize = 4096;

/// Longest wait for a server-specific endpoint, which the server may not have.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    /// Whether the model is in memory, if the server says.
    pub loaded: Option<bool>,
    /// Context window in tokens, if the server says.
    pub context_length: Option<usize>,
}

impl ModelInfo {
    fn named(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            loaded: None,
            context_length: None,
        }
    }
}

/// The model to use and the context to fit the prompt into.
pub struct Model {
    pub id: String,
    /// Why git-agent picked the model, or `None` if it was configured.
    pub picked: Option<Pick>,
    pub context_length: usize,
    pub context_source: ContextSource,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pick {
    /// The only model loaded on the server.
    Loaded,
    /// The only model the server has.
    Only,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContextSource {
    /// `context_length` in the config file, or its flag or environment variable.
    Config,
    Server,
    /// [`FALLBACK_CONTEXT_LENGTH`]
    Assumed,
}

/// Finds the model to use: the configured one, or else the one loaded on the server.
pub async fn resolve(client: &Client, config: &Config) -> Result<Model> {
    let (info, picked) = match (&config.model, config.context_length) {
        (Some(id), Some(_)) => (ModelInfo::named(id), None),
        (Some(id), None) => {
            // Without the list the context length is unknown, which isn't worth failing for.
            let listed = list(client).await.unwrap_or_default();
            let info = listed.into_iter().find(|model| model.id == *id);
            (info.unwrap_or_else(|| ModelInfo::named(id)), None)
        }
        (None, _) => {
            let listed = list(client).await.context(
                "could not get the server's models to choose one; \
                 set `model` in the config file or pass --model",
            )?;
            let info = choose(listed)?;
            let pick = match info.loaded {
                Some(true) => Pick::Loaded,
                _ => Pick::Only,
            };
            (info, Some(pick))
        }
    };
    let (context_length, context_source) = match (config.context_length, info.context_length) {
        (Some(tokens), _) => (tokens, ContextSource::Config),
        (None, Some(tokens)) => (tokens, ContextSource::Server),
        (None, None) => (FALLBACK_CONTEXT_LENGTH, ContextSource::Assumed),
    };
    Ok(Model {
        id: info.id,
        picked,
        context_length,
        context_source,
    })
}

/// The model to use when none is configured: the only loaded one, or the only one there is.
pub fn choose(models: Vec<ModelInfo>) -> Result<ModelInfo> {
    const HOW: &str = "choose one with --model or set `model` in the config file";

    let loaded: Vec<&ModelInfo> = models
        .iter()
        .filter(|model| model.loaded == Some(true))
        .collect();
    match loaded.as_slice() {
        [model] => return Ok((*model).clone()),
        [] => {}
        several => {
            let ids: Vec<&str> = several.iter().map(|model| model.id.as_str()).collect();
            bail!(
                "several models are loaded on the server ({}); {HOW}",
                ids.join(", ")
            );
        }
    }
    match models.as_slice() {
        [model] => Ok(model.clone()),
        [] => bail!("the server has no models"),
        all if all.iter().any(|model| model.loaded.is_some()) => bail!(
            "no model is loaded on the server; load one, or {HOW} \
             (`git agent models` lists them)"
        ),
        all => bail!(
            "the server has {} models; {HOW} (`git agent models` lists them)",
            all.len()
        ),
    }
}

/// The server's models, with load state and context length where the server reports them.
pub async fn list(client: &Client) -> Result<Vec<ModelInfo>> {
    let list: ModelList = client.get(&client.api_url("models")).await?;
    let mut models: Vec<ModelInfo> = list.data.into_iter().map(ListedModel::into_info).collect();
    if models.iter().all(|model| model.context_length.is_none()) {
        add_server_details(client, &mut models).await;
    }
    Ok(models)
}

async fn add_server_details(client: &Client, models: &mut Vec<ModelInfo>) {
    if let Some(lm_studio) = probe::<LmStudioModels>(client, "api/v0/models").await {
        lm_studio.apply(models);
    } else if let Some(ollama) = probe::<OllamaRunning>(client, "api/ps").await {
        ollama.apply(models);
    } else if let Some(llama_cpp) = probe::<LlamaCppProps>(client, "props").await {
        llama_cpp.apply(models);
    }
}

/// Fetches a server-specific endpoint, or `None` if the server doesn't have it.
async fn probe<T: DeserializeOwned>(client: &Client, path: &str) -> Option<T> {
    let url = client.server_url(path);
    tokio::time::timeout(PROBE_TIMEOUT, client.get(&url))
        .await
        .ok()?
        .ok()
}

/// The OpenAI-compatible model list.
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
            id: self.id,
            loaded: None,
            context_length,
        }
    }
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

impl LmStudioModels {
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

impl OllamaRunning {
    fn apply(self, models: &mut [ModelInfo]) {
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

impl LlamaCppProps {
    fn apply(self, models: &mut [ModelInfo]) {
        if let [model] = models {
            model.loaded = Some(true);
            model.context_length = Some(self.default_generation_settings.n_ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, loaded: Option<bool>) -> ModelInfo {
        ModelInfo {
            loaded,
            ..ModelInfo::named(id)
        }
    }

    fn listed(json: &str) -> Vec<ModelInfo> {
        let list: ModelList = serde_json::from_str(json).unwrap();
        list.data.into_iter().map(ListedModel::into_info).collect()
    }

    #[test]
    fn chooses_the_only_loaded_model() {
        let models = vec![info("a", Some(false)), info("b", Some(true))];
        assert_eq!(choose(models).unwrap().id, "b");
        // A single model is used even when the server doesn't say it's loaded.
        assert_eq!(choose(vec![info("a", None)]).unwrap().id, "a");
    }

    #[test]
    fn refuses_to_guess_between_models() {
        let several = vec![info("a", Some(true)), info("b", Some(true))];
        assert!(choose(several).unwrap_err().to_string().contains("a, b"));
        let none_loaded = vec![info("a", Some(false)), info("b", Some(false))];
        assert!(
            choose(none_loaded)
                .unwrap_err()
                .to_string()
                .contains("no model is loaded")
        );
        let unknown = vec![info("a", None), info("b", None)];
        assert!(
            choose(unknown)
                .unwrap_err()
                .to_string()
                .contains("2 models")
        );
        assert!(choose(vec![]).is_err());
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
        let lm_studio: LmStudioModels = serde_json::from_str(
            r#"{"data": [
                {"id": "gemma", "type": "vlm", "state": "loaded",
                 "max_context_length": 131072, "loaded_context_length": 32768},
                {"id": "qwen", "type": "llm", "state": "not-loaded", "max_context_length": 262144},
                {"id": "embed", "type": "embeddings", "state": "loaded"}
            ]}"#,
        )
        .unwrap();
        lm_studio.apply(&mut models);
        assert_eq!(
            models,
            [
                ModelInfo {
                    id: "gemma".into(),
                    loaded: Some(true),
                    context_length: Some(32768),
                },
                info("qwen", Some(false)),
            ]
        );
        // LM Studio answers unknown endpoints with an error body, which must not match.
        assert!(
            serde_json::from_str::<LmStudioModels>(r#"{"error": "Unexpected endpoint"}"#).is_err()
        );
    }

    #[test]
    fn applies_ollama_details() {
        let mut models = listed(r#"{"data": [{"id": "llama3.2"}, {"id": "qwen3:8b"}]}"#);
        let ollama: OllamaRunning = serde_json::from_str(
            r#"{"models": [{"name": "llama3.2:latest", "model": "llama3.2:latest",
                            "size": 1, "context_length": 8192}]}"#,
        )
        .unwrap();
        ollama.apply(&mut models);
        assert_eq!(models[0].loaded, Some(true));
        assert_eq!(models[0].context_length, Some(8192));
        assert_eq!(models[1], info("qwen3:8b", Some(false)));
    }

    #[test]
    fn applies_llama_cpp_details() {
        let mut models = listed(r#"{"data": [{"id": "model.gguf"}]}"#);
        let props: LlamaCppProps = serde_json::from_str(
            r#"{"default_generation_settings": {"id": 0, "n_ctx": 16384}, "total_slots": 1}"#,
        )
        .unwrap();
        props.apply(&mut models);
        assert_eq!(models[0].context_length, Some(16384));
    }
}
