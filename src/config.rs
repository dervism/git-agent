//! Settings for the model and the prompt.
//!
//! Later sources win: built-in defaults, then the config file, then
//! environment variables and command-line flags (both parsed by clap, see `cli.rs`).
//! API keys never come from the file: they come from a flag, the environment,
//! or the macOS keychain (see `secrets.rs`).

use crate::cli::SettingArgs;
use crate::{providers, secrets};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub const DEFAULT_BASE_URL: &str = "http://localhost:1234/v1";
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

/// What git-agent does when the changes it would commit, or the commits it
/// would push or open a pull request for, add what look like secrets. Either
/// way, the model gets them replaced (see `redact.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
    pub fn load(args: &SettingArgs) -> Result<Loaded> {
        let path = resolve_path(args);
        let file = match &path {
            Some(path) => Self::read_file(path)?,
            None => None,
        };
        if file.is_none() && args.config.is_some() {
            bail!("config file {} does not exist", path.unwrap().display());
        }
        let file_found = file.is_some();
        let mut config = file.unwrap_or_default();
        config.apply(args);
        if config.api_key.is_none() {
            config.api_key = secrets::get(&config.base_url)?;
        }
        config.validate()?;
        Ok(Loaded {
            config,
            path,
            file_found,
        })
    }

    /// The settings in the file at `path`, or `None` if there is no file.
    pub fn read_file(path: &Path) -> Result<Option<Self>> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let config = toml::from_str(&text)
            .with_context(|| format!("invalid config file {}", path.display()))?;
        Ok(Some(config))
    }

    pub fn apply(&mut self, args: &SettingArgs) {
        if let Some(base_url) = &args.base_url {
            self.base_url = base_url.clone();
        }
        if let Some(model) = &args.model {
            self.model = Some(model.clone());
        }
        if let Some(api_key) = &args.api_key {
            self.api_key = Some(api_key.clone());
        }
        if let Some(temperature) = args.temperature {
            self.temperature = Some(temperature);
        }
        if let Some(context_length) = args.context_length {
            self.context_length = Some(context_length);
        }
        if let Some(tokens) = args.max_output_tokens {
            self.max_output_tokens = tokens;
        }
        if let Some(on_secrets) = args.on_secrets {
            self.on_secrets = on_secrets;
        }
        self.model = self.model.take().filter(|model| !model.trim().is_empty());
        self.api_key = self.api_key.take().filter(|key| !key.trim().is_empty());
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
            bail!(
                "base_url must start with http:// or https:// (got {:?})",
                self.base_url
            );
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

    /// Writes the settings to `path` as a commented file, creating its directory.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }
        fs::write(path, template(self))
            .with_context(|| format!("failed to write {}", path.display()))
    }
}

/// The config file to use: `--config`/`GIT_AGENT_CONFIG`, else the per-user default location.
pub fn resolve_path(args: &SettingArgs) -> Option<PathBuf> {
    args.config.clone().or_else(default_path)
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

/// A commented config file containing `config`'s values.
pub fn template(config: &Config) -> String {
    let exclude: String = config
        .exclude
        .iter()
        .map(|pattern| format!("    {},\n", toml_string(pattern)))
        .collect();
    let system_prompt = if config.system_prompt.contains("'''") {
        toml_string(&config.system_prompt)
    } else {
        format!("'''\n{}\n'''", config.system_prompt.trim())
    };
    let model = match &config.model {
        Some(model) => format!("model = {}", toml_string(model)),
        None => "# model = \"qwen/qwen3-8b\"".to_owned(),
    };
    let context_length = match config.context_length {
        Some(tokens) => format!("context_length = {tokens}"),
        None => "# context_length = 32768".to_owned(),
    };
    let temperature = match config.temperature {
        Some(temperature) => format!("temperature = {temperature}"),
        None => "# temperature = 0.4".to_owned(),
    };
    format!(
        r#"# git-agent configuration
#
# Change these settings with `git agent config`, or edit this file. Command-line
# flags and environment variables override these values.

# Base URL of an OpenAI-compatible API, including the /v1 suffix.
#   LM Studio:  http://localhost:1234/v1
#   Ollama:     http://localhost:11434/v1
#   OpenAI:     https://api.openai.com/v1
# `git agent config` lists more providers.
# Overridden by --base-url / GIT_AGENT_BASE_URL.
base_url = {base_url}

# Model ID as listed by `git-agent models`. When it isn't set, git-agent uses
# the model that is loaded on the server, or the only model the server has.
# Overridden by --model / GIT_AGENT_MODEL.
{model}

# API keys are never stored in this file. On macOS, `git agent config` saves
# them in the keychain. Otherwise, set the GIT_AGENT_API_KEY environment variable.

# Sampling temperature from 0.0 to 2.0. Lower values give more predictable
# messages. When it isn't set, git-agent uses {local_temperature} on local servers and
# leaves it to the provider in the cloud, since many cloud models accept only
# their default. Anthropic's API takes no temperature at all.
{temperature}

# Seconds to wait for the model to answer.
timeout_secs = {timeout_secs}

# Most tokens the model may write in one reply, thinking included. git-agent
# sends it to Anthropic, whose API needs a limit; OpenAI-compatible servers use
# their own. It is also the most room git-agent keeps free for the reply. Much
# higher values can make a reply take longer than `timeout_secs`.
# Overridden by --max-output-tokens / GIT_AGENT_MAX_OUTPUT_TOKENS.
max_output_tokens = {max_output_tokens}

# The model's context window in tokens. git-agent shortens large diffs to fit
# it, leaving room for the reply. When it isn't set, git-agent asks the server
# (LM Studio, Ollama, llama.cpp, vLLM, Anthropic and OpenRouter report it) or
# assumes {fallback} tokens. Set it if your server doesn't report it, or lower it to keep prompts
# small and fast.
# Overridden by --context-length / GIT_AGENT_CONTEXT_LENGTH.
{context_length}

# What git-agent does when the changes it would commit, or the commits it would
# push or open a pull request for, add what look like secrets, such as API keys,
# private keys or passwords:
#   "reject"  don't commit, push or create the pull request
#   "warn"    list them and carry on
# Either way, the model gets [REDACTED] in their place.
# Overridden by --on-secrets / GIT_AGENT_ON_SECRETS.
on_secrets = {on_secrets}

# Files whose content is left out of the diff sent to the model. They still
# appear in the list of changed files. Patterns without a "/" match the file
# name in any directory; "*" wildcards are allowed.
exclude = [
{exclude}]

# Instructions given to the model before the diff.
system_prompt = {system_prompt}
"#,
        base_url = toml_string(&config.base_url),
        timeout_secs = config.timeout_secs,
        max_output_tokens = config.max_output_tokens,
        on_secrets = toml_string(config.on_secrets.as_str()),
        local_temperature = LOCAL_TEMPERATURE,
        fallback = crate::models::FALLBACK_CONTEXT_LENGTH,
    )
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(config: &Config) -> Config {
        let mut parsed: Config = toml::from_str(&template(config)).unwrap();
        parsed.system_prompt = parsed.system_prompt.trim().to_owned();
        parsed
    }

    #[test]
    fn template_round_trips() {
        let mut config = Config::default();
        config.system_prompt = config.system_prompt.trim().to_owned();
        assert_eq!(round_trip(&config), config);

        let config = Config {
            model: Some("qwen/qwen3-8b".into()),
            temperature: Some(0.4),
            context_length: Some(32768),
            max_output_tokens: 32000,
            on_secrets: OnSecrets::Warn,
            exclude: vec!["*.min.js".into(), "docs/generated/**".into()],
            ..config
        };
        assert_eq!(round_trip(&config), config);
    }

    #[test]
    fn rejects_secrets_unless_told_to_warn() {
        assert_eq!(Config::default().on_secrets, OnSecrets::Reject);
        let config: Config = toml::from_str("on_secrets = \"warn\"").unwrap();
        assert_eq!(config.on_secrets, OnSecrets::Warn);
        assert!(toml::from_str::<Config>("on_secrets = \"ignore\"").is_err());

        let mut config = Config::default();
        config.apply(&SettingArgs {
            on_secrets: Some(OnSecrets::Warn),
            ..SettingArgs::default()
        });
        assert_eq!(config.on_secrets, OnSecrets::Warn);
    }

    #[test]
    fn never_reads_or_writes_api_keys() {
        assert!(toml::from_str::<Config>("api_key = \"sk-secret\"").is_err());
        let config = Config {
            api_key: Some("sk-secret".into()),
            ..Config::default()
        };
        assert!(!template(&config).contains("sk-secret"));
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
        let config: Config = toml::from_str("model = \"llama3\"").unwrap();
        assert_eq!(config.model.as_deref(), Some("llama3"));
        assert_eq!(config.base_url, DEFAULT_BASE_URL);
        assert_eq!(config.context_length, None);
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("modle = \"llama3\"").is_err());
    }

    #[test]
    fn flags_override_file_values() {
        let mut config = Config {
            model: Some("from-file".into()),
            ..Config::default()
        };
        config.apply(&SettingArgs {
            model: Some("other".into()),
            temperature: Some(1.0),
            context_length: Some(8192),
            max_output_tokens: Some(4096),
            api_key: Some("  ".into()),
            ..SettingArgs::default()
        });
        assert_eq!(config.model.as_deref(), Some("other"));
        assert_eq!(config.temperature, Some(1.0));
        assert_eq!(config.context_length, Some(8192));
        assert_eq!(config.max_output_tokens, 4096);
        assert_eq!(config.api_key, None);
        assert_eq!(config.base_url, DEFAULT_BASE_URL);

        let mut blank = Config::default();
        blank.apply(&SettingArgs {
            model: Some(" ".into()),
            ..SettingArgs::default()
        });
        assert_eq!(blank.model, None);
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
