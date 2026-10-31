//! Settings for the model and the prompt.
//!
//! Later sources win: built-in defaults, then the config file, then
//! environment variables and command-line flags (both parsed by clap, see `cli.rs`).

use crate::cli::SettingArgs;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub const DEFAULT_BASE_URL: &str = "http://localhost:1234/v1";
pub const DEFAULT_MODEL: &str = "google/gemma-4-e4b";
pub const DEFAULT_TEMPERATURE: f32 = 0.4;
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;
pub const DEFAULT_MAX_DIFF_CHARS: usize = 16_000;

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub base_url: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    pub temperature: f32,
    pub timeout_secs: u64,
    pub max_diff_chars: usize,
    pub exclude: Vec<String>,
    pub system_prompt: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: DEFAULT_MODEL.to_owned(),
            api_key: None,
            temperature: DEFAULT_TEMPERATURE,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_diff_chars: DEFAULT_MAX_DIFF_CHARS,
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
            Some(path) => Self::read(path)?,
            None => None,
        };
        if file.is_none() && args.config.is_some() {
            bail!("config file {} does not exist", path.unwrap().display());
        }
        let file_found = file.is_some();
        let mut config = file.unwrap_or_default();
        config.apply(args);
        config.validate()?;
        Ok(Loaded {
            config,
            path,
            file_found,
        })
    }

    fn read(path: &Path) -> Result<Option<Self>> {
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
            self.model = model.clone();
        }
        if let Some(api_key) = &args.api_key {
            self.api_key = Some(api_key.clone());
        }
        if let Some(temperature) = args.temperature {
            self.temperature = temperature;
        }
        self.api_key = self.api_key.take().filter(|key| !key.trim().is_empty());
    }

    fn validate(&self) -> Result<()> {
        if self.model.trim().is_empty() {
            bail!("no model configured; set `model` in the config file or pass --model");
        }
        if !(self.base_url.starts_with("http://") || self.base_url.starts_with("https://")) {
            bail!(
                "base_url must start with http:// or https:// (got {:?})",
                self.base_url
            );
        }
        if !(0.0..=2.0).contains(&self.temperature) {
            bail!(
                "temperature must be between 0.0 and 2.0 (got {})",
                self.temperature
            );
        }
        Ok(())
    }

    /// A copy that is safe to print.
    pub fn redacted(&self) -> Self {
        Self {
            api_key: self.api_key.as_ref().map(|_| "<redacted>".to_owned()),
            ..self.clone()
        }
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

/// A commented config file containing `config`'s values. The API key is never written.
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
    format!(
        r#"# git-agent configuration
#
# Command-line flags and environment variables override these values.

# Base URL of an OpenAI-compatible API, including the /v1 suffix.
#   LM Studio:  http://localhost:1234/v1
#   Ollama:     http://localhost:11434/v1
#   llama.cpp:  http://localhost:8080/v1
# Overridden by --base-url / GIT_AGENT_BASE_URL.
base_url = {base_url}

# Model ID as listed by `git-agent models`.
# Overridden by --model / GIT_AGENT_MODEL.
model = {model}

# API key sent as a Bearer token. Local servers usually don't need one.
# Prefer the GIT_AGENT_API_KEY environment variable to storing it here.
# api_key = ""

# Sampling temperature from 0.0 to 2.0. Lower values give more predictable
# messages, higher values more varied ones when regenerating.
temperature = {temperature}

# Seconds to wait for the model to answer.
timeout_secs = {timeout_secs}

# Diffs longer than this many characters are cut off before being sent to the
# model. The list of changed files is always sent in full. Lower this if your
# model has a small context window.
max_diff_chars = {max_diff_chars}

# Files whose content is left out of the diff sent to the model. They still
# appear in the list of changed files. Patterns without a "/" match the file
# name in any directory; "*" wildcards are allowed.
exclude = [
{exclude}]

# Instructions given to the model before the diff.
system_prompt = {system_prompt}
"#,
        base_url = toml_string(&config.base_url),
        model = toml_string(&config.model),
        temperature = config.temperature,
        timeout_secs = config.timeout_secs,
        max_diff_chars = config.max_diff_chars,
    )
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_round_trips() {
        let mut config = Config {
            model: "qwen/qwen3-8b".into(),
            exclude: vec!["*.min.js".into(), "docs/generated/**".into()],
            ..Config::default()
        };
        let mut parsed: Config = toml::from_str(&template(&config)).unwrap();
        parsed.system_prompt = parsed.system_prompt.trim().to_owned();
        config.system_prompt = config.system_prompt.trim().to_owned();
        assert_eq!(parsed, config);
    }

    #[test]
    fn missing_keys_fall_back_to_defaults() {
        let config: Config = toml::from_str("model = \"llama3\"").unwrap();
        assert_eq!(config.model, "llama3");
        assert_eq!(config.base_url, DEFAULT_BASE_URL);
        assert_eq!(config.max_diff_chars, DEFAULT_MAX_DIFF_CHARS);
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("modle = \"llama3\"").is_err());
    }

    #[test]
    fn flags_override_file_values() {
        let mut config = Config::default();
        config.apply(&SettingArgs {
            model: Some("other".into()),
            temperature: Some(1.0),
            api_key: Some("  ".into()),
            ..SettingArgs::default()
        });
        assert_eq!(config.model, "other");
        assert_eq!(config.temperature, 1.0);
        assert_eq!(config.api_key, None);
        assert_eq!(config.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn validates_temperature_and_url() {
        let bad_temperature = Config {
            temperature: 3.0,
            ..Config::default()
        };
        assert!(bad_temperature.validate().is_err());
        let bad_url = Config {
            base_url: "localhost:1234".into(),
            ..Config::default()
        };
        assert!(bad_url.validate().is_err());
        assert!(Config::default().validate().is_ok());
    }
}
