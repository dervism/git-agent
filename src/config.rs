//! Settings for the model and the prompt.
//!
//! Later sources win: built-in defaults, then the config file, then
//! environment variables and command-line flags, which `cli.rs` reads into [`Overrides`].
//! API keys never come from the file: they come from a flag, the environment,
//! or the macOS keychain (see `api_keys.rs`).

mod file;
mod parse_error;
mod released;
mod render;
mod write;

pub use file::ConfigFile;

use crate::{api_keys, providers};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::{env, mem};

pub const DEFAULT_BASE_URL: &str = "http://localhost:1234/v1";
/// Context length assumed when neither the config nor the server gives one.
/// It fits the defaults of common local servers; Ollama, for one, silently
/// cuts prompts that are longer than its context.
pub const FALLBACK_CONTEXT_LENGTH: usize = 4096;
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Most tokens a model may write in one reply, thinking included. Enough for the
/// thinking current models do before a commit message, and small enough for a
/// reply that isn't streamed.
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 16_000;
/// Smallest output limit: a commit message and a little thinking.
pub const MIN_OUTPUT_TOKENS: usize = 1024;
/// git-agent's temperature on local servers: low, for predictable messages.
pub const LOCAL_TEMPERATURE: f32 = 0.4;
/// Smallest usable context: the system prompt, a little of the diff, and the reply.
pub const MIN_CONTEXT_LENGTH: usize = 2048;

/// Files whose diffs are noise for a commit message. They still appear in the file list.
pub const DEFAULT_EXCLUDE: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "poetry.lock",
    "uv.lock",
    "Pipfile.lock",
    "composer.lock",
    "Gemfile.lock",
    "go.sum",
];

pub const DEFAULT_SYSTEM_PROMPT: &str = r#"You write git commit messages for changes a developer is about to commit.

Format:
- A subject line in the imperative mood, at most 72 characters, with no trailing period. Start it with a Conventional Commits type (feat, fix, refactor, perf, docs, test, build, ci or chore) and an optional scope, e.g. "fix(parser): handle empty input".
- Then a blank line and a body explaining what changed and why, wrapped at 72 characters. Use "- " bullets for several distinct changes. Omit the body for trivial changes.

Guidelines:
- Describe the intent and effect of the change, not a file-by-file narration of the diff.
- Mention lockfiles and generated files only if they are the main change.
- Do not invent motivation that the diff and the author's notes do not support.

Reply with the commit message only: no introduction, no Markdown headings, no code fences."#;

/// Settings given for one run, on the command line or in the environment, which win over
/// the config file.
#[derive(Debug, Default)]
pub struct Overrides {
    /// The config file to read instead of the usual one.
    pub config: Option<PathBuf>,
    pub model: Option<String>,
    pub context_length: Option<usize>,
    pub max_output_tokens: Option<usize>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub temperature: Option<f32>,
    pub on_secrets: Option<OnSecrets>,
}

/// What git-agent does when the changes it would commit, or the commits it
/// would push or open a pull request for, add what look like secrets. Either
/// way, the model gets them replaced (see `redact.rs`).
///
/// The default applies to every config file that doesn't set `on_secrets`, so changing it
/// changes what git-agent does for all of those users.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnSecrets {
    /// Don't commit, push or create the pull request
    #[default]
    Reject,
    /// List them and carry on
    Warn,
}

impl OnSecrets {
    pub fn as_str(self) -> &'static str {
        match self {
            OnSecrets::Reject => "reject",
            OnSecrets::Warn => "warn",
        }
    }
}

/// The settings in force: what the config file, the environment and the flags say, and the
/// defaults for the rest. [`ConfigFile`] holds only what the file sets.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Config {
    pub base_url: String,
    /// `None` uses the model loaded on the server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// From `--api-key`, `GIT_AGENT_API_KEY` or the keychain, never from the file.
    #[serde(skip)]
    pub api_key: Option<String>,
    /// `None` means [`LOCAL_TEMPERATURE`] on local servers and the provider's
    /// default in the cloud (see [`Config::effective_temperature`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    pub timeout_secs: u64,
    /// Sent to Anthropic, whose API needs a limit. OpenAI-compatible servers use their own.
    pub max_output_tokens: usize,
    /// The model's context window in tokens. `None` asks the server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<usize>,
    pub on_secrets: OnSecrets,
    pub exclude: Vec<String>,
    pub system_prompt: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: None,
            api_key: None,
            temperature: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            context_length: None,
            on_secrets: OnSecrets::default(),
            exclude: DEFAULT_EXCLUDE.iter().map(|s| s.to_string()).collect(),
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_owned(),
        }
    }
}

/// A resolved configuration and the file it was read from, if any.
pub struct Loaded {
    pub config: Config,
    pub path: Option<PathBuf>,
    pub file_found: bool,
}

impl Config {
    pub fn load(overrides: &Overrides) -> Result<Loaded> {
        let path = resolve_path(overrides);
        let file = match &path {
            Some(path) => ConfigFile::read(path)?,
            None => None,
        };
        if file.is_none() && overrides.config.is_some() {
            bail!("config file {} does not exist", path.unwrap().display());
        }
        let file_found = file.is_some();
        let mut config = file.map(ConfigFile::resolve).unwrap_or_default();
        config.apply(overrides);
        if config.api_key.is_none() {
            config.api_key = api_keys::get(&config.base_url)?;
        }
        config.validate()?;
        Ok(Loaded {
            config,
            path,
            file_found,
        })
    }

    /// Applies flags and `GIT_AGENT_*` variables. A blank model among them clears the model, so
    /// git-agent uses the one loaded on the server whatever the file says.
    pub fn apply(&mut self, overrides: &Overrides) {
        let api_key = overrides.api_key.clone().or_else(|| self.api_key.take());
        let clears_model = overrides
            .model
            .as_deref()
            .is_some_and(|model| model.trim().is_empty());
        let applied = mem::take(self).overlaid(ConfigFile::from_overrides(overrides));
        *self = Self {
            api_key: api_key.filter(|key| !key.trim().is_empty()),
            model: applied.model.filter(|_| !clears_model),
            ..applied
        };
    }

    fn validate(&self) -> Result<()> {
        if let Some(tokens) = self.context_length
            && tokens < MIN_CONTEXT_LENGTH
        {
            bail!("context_length must be at least {MIN_CONTEXT_LENGTH} tokens (got {tokens})");
        }
        if self.max_output_tokens < MIN_OUTPUT_TOKENS {
            bail!(
                "max_output_tokens must be at least {MIN_OUTPUT_TOKENS} tokens (got {})",
                self.max_output_tokens
            );
        }
        if !(self.base_url.starts_with("http://") || self.base_url.starts_with("https://")) {
            bail!("base_url must start with http:// or https://");
        }
        if let Some(temperature) = self.temperature
            && !(0.0..=2.0).contains(&temperature)
        {
            bail!("temperature must be between 0.0 and 2.0 (got {temperature})");
        }
        Ok(())
    }

    /// The temperature to send: the configured one, else git-agent's own on a
    /// local server. Cloud models get their provider's default, which many of
    /// them require.
    pub fn effective_temperature(&self) -> Option<f32> {
        let local = providers::is_local(&self.base_url).then_some(LOCAL_TEMPERATURE);
        self.temperature.or(local)
    }
}

/// The config file to use: `--config`/`GIT_AGENT_CONFIG`, else the per-user default location.
pub fn resolve_path(overrides: &Overrides) -> Option<PathBuf> {
    overrides.config.clone().or_else(default_path)
}

fn default_path() -> Option<PathBuf> {
    let non_empty = |name| env::var_os(name).filter(|value| !value.is_empty());
    let config_dir = non_empty("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            cfg!(windows)
                .then(|| non_empty("APPDATA"))
                .flatten()
                .map(PathBuf::from)
        })
        .or_else(|| non_empty("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config_dir.join("git-agent").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config> {
        ConfigFile::parse(text).map(ConfigFile::resolve)
    }

    #[test]
    fn rejects_secrets_unless_told_to_warn() {
        assert_eq!(Config::default().on_secrets, OnSecrets::Reject);
        assert_eq!(
            parse("on_secrets = \"warn\"").unwrap().on_secrets,
            OnSecrets::Warn
        );
        assert!(parse("on_secrets = \"ignore\"").is_err());

        let mut config = Config::default();
        config.apply(&Overrides {
            on_secrets: Some(OnSecrets::Warn),
            ..Overrides::default()
        });
        assert_eq!(config.on_secrets, OnSecrets::Warn);
    }

    #[test]
    fn never_reads_or_writes_api_keys() {
        assert!(ConfigFile::parse("api_key = \"sk-secret\"").is_err());
        let overrides = Overrides {
            api_key: Some("sk-secret".into()),
            ..Overrides::default()
        };
        let file = ConfigFile::from_overrides(&overrides);
        assert!(!render::render(&file).contains("sk-secret"));

        let mut config = Config::default();
        config.apply(&overrides);
        assert_eq!(config.api_key.as_deref(), Some("sk-secret"));
        assert!(
            !toml::to_string_pretty(&config)
                .unwrap()
                .contains("sk-secret")
        );
    }

    #[test]
    fn local_servers_get_git_agents_temperature() {
        let cloud = |temperature| Config {
            base_url: "https://api.openai.com/v1".into(),
            temperature,
            ..Config::default()
        };
        assert_eq!(
            Config::default().effective_temperature(),
            Some(LOCAL_TEMPERATURE)
        );
        assert_eq!(cloud(None).effective_temperature(), None);
        assert_eq!(cloud(Some(0.7)).effective_temperature(), Some(0.7));
    }

    #[test]
    fn missing_keys_fall_back_to_defaults() {
        let config = parse("model = \"llama3\"").unwrap();
        assert_eq!(config.model.as_deref(), Some("llama3"));
        assert_eq!(config.base_url, DEFAULT_BASE_URL);
        assert_eq!(config.context_length, None);
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(parse("modle = \"llama3\"").is_err());
    }

    #[test]
    fn flags_override_file_values() {
        let mut config = Config {
            model: Some("from-file".into()),
            ..Config::default()
        };
        config.apply(&Overrides {
            model: Some("other".into()),
            temperature: Some(1.0),
            context_length: Some(8192),
            max_output_tokens: Some(4096),
            api_key: Some("  ".into()),
            ..Overrides::default()
        });
        assert_eq!(config.model.as_deref(), Some("other"));
        assert_eq!(config.temperature, Some(1.0));
        assert_eq!(config.context_length, Some(8192));
        assert_eq!(config.max_output_tokens, 4096);
        assert_eq!(config.api_key, None);
        assert_eq!(config.base_url, DEFAULT_BASE_URL);

        let mut blank = Config::default();
        blank.apply(&Overrides {
            model: Some(" ".into()),
            ..Overrides::default()
        });
        assert_eq!(blank.model, None);
    }

    #[test]
    fn a_blank_model_flag_clears_the_files_model() {
        let from_file = || Config {
            model: Some("from-file".into()),
            ..Config::default()
        };
        for blank in ["", " "] {
            let mut config = from_file();
            config.apply(&Overrides {
                model: Some(blank.into()),
                ..Overrides::default()
            });
            assert_eq!(config.model, None, "{blank:?}");
        }
        let mut untouched = from_file();
        untouched.apply(&Overrides::default());
        assert_eq!(untouched.model.as_deref(), Some("from-file"));
    }

    #[test]
    fn flags_leave_what_they_do_not_name_alone() {
        let before = Config {
            base_url: "https://example.com/v1".into(),
            model: Some("from-file".into()),
            api_key: Some("key".into()),
            temperature: Some(0.9),
            timeout_secs: 12,
            max_output_tokens: 2048,
            context_length: Some(9000),
            on_secrets: OnSecrets::Warn,
            exclude: vec!["*.min.js".into()],
            system_prompt: "mine".into(),
        };
        let mut after = before.clone();
        after.apply(&Overrides::default());
        assert_eq!(after, before);
    }

    fn load_file(text: &str, overrides: Overrides) -> Result<Loaded> {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        Config::load(&Overrides {
            config: Some(path),
            api_key: Some("test-key".into()),
            ..overrides
        })
    }

    #[test]
    fn loads_the_file_over_the_defaults_and_the_flags_over_the_file() {
        let text = "timeout_secs = 60\nmodel = \"from-file\"\n";
        let loaded = load_file(text, Overrides::default()).unwrap();
        assert!(loaded.file_found);
        assert_eq!(loaded.config.timeout_secs, 60);
        assert_eq!(loaded.config.model.as_deref(), Some("from-file"));
        assert_eq!(loaded.config.system_prompt, DEFAULT_SYSTEM_PROMPT);

        let flags = Overrides {
            model: Some("from-flag".into()),
            ..Overrides::default()
        };
        let loaded = load_file(text, flags).unwrap();
        assert_eq!(loaded.config.model.as_deref(), Some("from-flag"));
        assert_eq!(loaded.config.timeout_secs, 60);
    }

    #[test]
    fn loads_a_file_an_older_release_wrote_with_the_default_prompt() {
        let text =
            format!("timeout_secs = 300\nsystem_prompt = '''\n{DEFAULT_SYSTEM_PROMPT}\n'''\n");
        let loaded = load_file(&text, Overrides::default()).unwrap();
        assert_eq!(
            loaded.config,
            Config {
                api_key: Some("test-key".into()),
                ..Config::default()
            }
        );
    }

    #[test]
    fn refuses_a_missing_file_that_was_asked_for_and_a_bad_setting_in_a_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let missing = Overrides {
            config: Some(dir.path().join("config.toml")),
            api_key: Some("test-key".into()),
            ..Overrides::default()
        };
        assert!(Config::load(&missing).is_err());
        assert!(load_file("max_output_tokens = 10", Overrides::default()).is_err());
    }

    #[test]
    fn a_bad_base_url_is_refused_without_being_shown() {
        let password = format!("pw{}", "K4".repeat(6));
        for url in [
            format!("user:{password}@host/v1"),
            format!("ftp://user:{password}@host/v1"),
            password.clone(),
        ] {
            let config = Config {
                base_url: url,
                ..Config::default()
            };
            let error = config.validate().unwrap_err().to_string();
            assert!(error.contains("http:// or https://"), "{error}");
            assert!(!error.contains(&password), "{error}");
        }
    }

    #[test]
    fn validates_settings() {
        let bad_temperature = Config {
            temperature: Some(3.0),
            ..Config::default()
        };
        assert!(bad_temperature.validate().is_err());
        let bad_url = Config {
            base_url: "localhost:1234".into(),
            ..Config::default()
        };
        assert!(bad_url.validate().is_err());
        let tiny_context = Config {
            context_length: Some(512),
            ..Config::default()
        };
        assert!(tiny_context.validate().is_err());
        let tiny_output = Config {
            max_output_tokens: 100,
            ..Config::default()
        };
        assert!(tiny_output.validate().is_err());
        assert!(Config::default().validate().is_ok());
    }
}
