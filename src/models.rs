//! Choosing the model, and the context to fit the prompt into.

use crate::config::Config;
use crate::server::{ModelInfo, ModelServer};
use anyhow::{Context, Result, bail};

/// Context length assumed when neither the config nor the server gives one.
/// It fits the defaults of common local servers; Ollama, for one, silently
/// cuts prompts that are longer than its context.
pub const FALLBACK_CONTEXT_LENGTH: usize = 4096;

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
pub async fn resolve(server: &impl ModelServer, config: &Config) -> Result<Model> {
    let (info, picked) = match (&config.model, config.context_length) {
        (Some(id), Some(_)) => (ModelInfo::named(id), None),
        (Some(id), None) => {
            // Without the list the context length is unknown, which isn't worth failing for.
            let listed = server.models().await.unwrap_or_default();
            let info = listed.into_iter().find(|model| model.id == *id);
            (info.unwrap_or_else(|| ModelInfo::named(id)), None)
        }
        (None, _) => {
            let listed = server.models().await.context(
                "could not get the server's models to choose one; \
                 choose one with `git agent config` or --model",
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
    const HOW: &str = "choose one with `git agent config` or --model";

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::Message;

    fn info(id: &str, loaded: Option<bool>) -> ModelInfo {
        ModelInfo {
            loaded,
            ..ModelInfo::named(id)
        }
    }

    /// A server that lists `models`, or fails to if there are none.
    struct FakeServer(Vec<ModelInfo>);

    impl ModelServer for FakeServer {
        async fn models(&self) -> Result<Vec<ModelInfo>> {
            match self.0.as_slice() {
                [] => bail!("no model list"),
                models => Ok(models.to_vec()),
            }
        }

        async fn complete(&self, _model: &str, _messages: &[Message]) -> Result<String> {
            unreachable!("choosing a model sends no conversation")
        }
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
        let error = |models| choose(models).unwrap_err().to_string();
        assert!(error(vec![info("a", Some(true)), info("b", Some(true))]).contains("a, b"));
        let none_loaded = vec![info("a", Some(false)), info("b", Some(false))];
        assert!(error(none_loaded).contains("no model is loaded"));
        assert!(error(vec![info("a", None), info("b", None)]).contains("2 models"));
        assert!(choose(vec![]).is_err());
    }

    #[tokio::test]
    async fn takes_the_context_from_the_config_then_the_server() {
        let server = FakeServer(vec![ModelInfo {
            context_length: Some(32768),
            ..info("gemma", Some(true))
        }]);
        let auto = resolve(&server, &Config::default()).await.unwrap();
        assert_eq!(auto.id, "gemma");
        assert_eq!(auto.picked, Some(Pick::Loaded));
        assert_eq!(
            (auto.context_length, auto.context_source),
            (32768, ContextSource::Server)
        );

        let configured = Config {
            model: Some("gemma".into()),
            context_length: Some(8192),
            ..Config::default()
        };
        let model = resolve(&server, &configured).await.unwrap();
        assert_eq!(model.picked, None);
        assert_eq!(model.context_source, ContextSource::Config);
    }

    #[tokio::test]
    async fn assumes_a_small_context_when_nothing_reports_one() {
        let config = Config {
            model: Some("unlisted".into()),
            ..Config::default()
        };
        let model = resolve(&FakeServer(Vec::new()), &config).await.unwrap();
        assert_eq!(model.context_length, FALLBACK_CONTEXT_LENGTH);
        assert_eq!(model.context_source, ContextSource::Assumed);
        // Without a configured model, a server that can't list its models is an error.
        assert!(
            resolve(&FakeServer(Vec::new()), &Config::default())
                .await
                .is_err()
        );
    }
}
