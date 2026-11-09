//! Choosing the model, and the context to fit the prompt into.

use crate::config::{self, Config};
use crate::server::{ModelInfo, ModelServer};
use anyhow::{Context, Result, bail};

/// The model to use and the context to fit the prompt into.
pub struct Model {
    pub id: String,
    /// Why git-agent picked the model, or `None` if it was configured.
    pub picked: Option<Pick>,
    pub context_length: usize,
    pub context_source: ContextSource,
    /// The most tokens a reply may take, which the prompt leaves room for.
    pub max_output_tokens: usize,
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
    /// [`config::FALLBACK_CONTEXT_LENGTH`]
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
        (None, None) => (config::FALLBACK_CONTEXT_LENGTH, ContextSource::Assumed),
    };
    Ok(Model {
        id: info.id,
        picked,
        context_length,
        context_source,
        max_output_tokens: config.max_output_tokens,
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
pub mod test_support {
    use super::{ContextSource, Model};
    use crate::config;

    /// A configured model, "fake", with a context of `context_length` tokens.
    pub fn configured(context_length: usize) -> Model {
        Model {
            id: "fake".to_owned(),
            picked: None,
            context_length,
            context_source: ContextSource::Config,
            max_output_tokens: config::DEFAULT_MAX_OUTPUT_TOKENS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::test_support::FakeServer;

    fn info(id: &str, loaded: Option<bool>) -> ModelInfo {
        ModelInfo {
            loaded,
            ..ModelInfo::named(id)
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
        let server = FakeServer::listing(vec![ModelInfo {
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
        let model = resolve(&FakeServer::unlisted(), &config).await.unwrap();
        assert_eq!(model.context_length, config::FALLBACK_CONTEXT_LENGTH);
        assert_eq!(model.context_source, ContextSource::Assumed);
        // Without a configured model, a server that can't list its models is an error.
        assert!(
            resolve(&FakeServer::unlisted(), &Config::default())
                .await
                .is_err()
        );
    }
}
