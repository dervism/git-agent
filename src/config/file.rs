//! The config file as the user wrote it: the settings they set, and only those.
//!
//! A setting the file leaves out gets the default of the running release, so a later
//! release can change a default for everyone who never chose a value.

use super::released::ReleasedDefaults;
use super::{Config, OnSecrets, Overrides, parse_error, render, write};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// API keys have no field here, so a file that holds one is refused.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub temperature: Option<f32>,
    pub timeout_secs: Option<u64>,
    pub max_output_tokens: Option<usize>,
    pub context_length: Option<usize>,
    pub on_secrets: Option<OnSecrets>,
    pub exclude: Option<Vec<String>>,
    pub system_prompt: Option<String>,
}

impl ConfigFile {
    /// The settings in the file at `path`, or `None` if there is no file.
    pub fn read(path: &Path) -> Result<Option<Self>> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        Self::parse(&text)
            .map(Some)
            .with_context(|| format!("invalid config file {}", path.display()))
    }

    /// The settings `text` sets. The settings that only repeat a default that a release
    /// has written into files are left out, and a blank model is no model.
    pub fn parse(text: &str) -> Result<Self> {
        let file =
            toml::from_str::<Self>(text).map_err(|error| parse_error::describe(text, &error))?;
        Ok(file.without_blank_model().without_released_defaults())
    }

    /// The names of the settings, in the order a file lists them.
    pub fn setting_names() -> [&'static str; 9] {
        Self::default().settings().map(|(name, _)| name)
    }

    /// The names of the settings this file sets.
    pub fn set_names(&self) -> Vec<&'static str> {
        self.settings()
            .into_iter()
            .filter_map(|(name, set)| set.then_some(name))
            .collect()
    }

    fn settings(&self) -> [(&'static str, bool); 9] {
        let Self {
            base_url,
            model,
            temperature,
            timeout_secs,
            max_output_tokens,
            context_length,
            on_secrets,
            exclude,
            system_prompt,
        } = self;
        [
            ("base_url", base_url.is_some()),
            ("model", model.is_some()),
            ("temperature", temperature.is_some()),
            ("timeout_secs", timeout_secs.is_some()),
            ("max_output_tokens", max_output_tokens.is_some()),
            ("context_length", context_length.is_some()),
            ("on_secrets", on_secrets.is_some()),
            ("exclude", exclude.is_some()),
            ("system_prompt", system_prompt.is_some()),
        ]
    }

    /// The settings `overrides` sets, in the file's shape. A blank model is none.
    pub fn from_overrides(overrides: &Overrides) -> Self {
        Self {
            base_url: overrides.base_url.clone(),
            model: overrides.model.clone(),
            temperature: overrides.temperature,
            max_output_tokens: overrides.max_output_tokens,
            context_length: overrides.context_length,
            on_secrets: overrides.on_secrets,
            ..Self::default()
        }
        .without_blank_model()
    }

    /// This file without a prompt or an exclude list that a release has written into files as
    /// the default, which says nothing about what the user chose.
    pub fn without_released_defaults(self) -> Self {
        ReleasedDefaults::ALL.forget(self)
    }

    fn without_blank_model(self) -> Self {
        Self {
            model: self.model.filter(|model| !model.trim().is_empty()),
            ..self
        }
    }

    /// This file, if the settings it sets are valid.
    pub fn validated(self) -> Result<Self> {
        self.clone().resolve().validate()?;
        Ok(self)
    }

    pub fn resolve(self) -> Config {
        Config::default().overlaid(self)
    }

    /// Writes the settings to `path` as a commented file, creating its directory. The
    /// settings that aren't set appear as comments with their defaults. A file that would
    /// not read back as these settings is not written.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = render::render(self);
        if !self.is_what_reads_back_from(&text) {
            bail!("these settings can't be written so that they read back the same");
        }
        write::replace_file(path, text.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))
    }

    /// Whether `text` sets exactly these settings, apart from the blanks around the prompt.
    fn is_what_reads_back_from(&self, text: &str) -> bool {
        let trimmed_prompt = |file: &Self| Self {
            system_prompt: file
                .system_prompt
                .as_deref()
                .map(|prompt| prompt.trim().to_owned()),
            ..file.clone()
        };
        toml::from_str::<Self>(text).is_ok_and(|read| trimmed_prompt(&read) == trimmed_prompt(self))
    }
}

impl Config {
    /// These settings, with the ones `file` sets in their place.
    pub(super) fn overlaid(self, file: ConfigFile) -> Self {
        let ConfigFile {
            base_url,
            model,
            temperature,
            timeout_secs,
            max_output_tokens,
            context_length,
            on_secrets,
            exclude,
            system_prompt,
        } = file;
        Self {
            base_url: base_url.unwrap_or(self.base_url),
            model: model.or(self.model),
            api_key: self.api_key,
            temperature: temperature.or(self.temperature),
            timeout_secs: timeout_secs.unwrap_or(self.timeout_secs),
            max_output_tokens: max_output_tokens.unwrap_or(self.max_output_tokens),
            context_length: context_length.or(self.context_length),
            on_secrets: on_secrets.unwrap_or(self.on_secrets),
            exclude: exclude.unwrap_or(self.exclude),
            system_prompt: system_prompt.unwrap_or(self.system_prompt),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        DEFAULT_BASE_URL, DEFAULT_EXCLUDE, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_SYSTEM_PROMPT,
        DEFAULT_TIMEOUT_SECS, LOCAL_TEMPERATURE,
    };
    use tempfile::TempDir;

    const V0_3_0: &str = include_str!("fixtures/v0.3.0.toml");
    const V0_4_0: &str = include_str!("fixtures/v0.4.0.toml");
    const V0_11_0: &str = include_str!("fixtures/v0.11.0.toml");

    fn parse(text: &str) -> ConfigFile {
        ConfigFile::parse(text).unwrap()
    }

    fn everything_at_its_default() -> ConfigFile {
        ConfigFile {
            base_url: Some(DEFAULT_BASE_URL.into()),
            model: None,
            temperature: Some(LOCAL_TEMPERATURE),
            timeout_secs: Some(DEFAULT_TIMEOUT_SECS),
            max_output_tokens: Some(DEFAULT_MAX_OUTPUT_TOKENS),
            context_length: None,
            on_secrets: Some(OnSecrets::default()),
            exclude: Some(DEFAULT_EXCLUDE.iter().map(|s| (*s).to_owned()).collect()),
            system_prompt: Some(DEFAULT_SYSTEM_PROMPT.to_owned()),
        }
    }

    fn only(set: impl FnOnce(&mut ConfigFile)) -> ConfigFile {
        let mut file = ConfigFile::default();
        set(&mut file);
        file
    }

    fn defaults_but(change: impl FnOnce(&mut Config)) -> Config {
        let mut config = Config::default();
        change(&mut config);
        config
    }

    fn assert_set_alone(text: &str, file: ConfigFile, config: Config) {
        assert_eq!(parse(text), file, "{text}");
        assert_eq!(file.resolve(), config, "{text}");
    }

    #[test]
    fn a_file_with_nothing_set_is_all_defaults() {
        for text in ["", "# model = \"x\"\n# timeout_secs = 12\n\n"] {
            let file = parse(text);
            assert_eq!(file, ConfigFile::default(), "{text:?}");
            assert_eq!(file.resolve(), Config::default());
        }
    }

    #[test]
    fn the_text_settings_set_alone_change_only_themselves() {
        let url = "https://example.com/v1";
        assert_set_alone(
            &format!("base_url = \"{url}\""),
            only(|file| file.base_url = Some(url.into())),
            defaults_but(|config| config.base_url = url.into()),
        );
        assert_set_alone(
            "model = \"llama3\"",
            only(|file| file.model = Some("llama3".into())),
            defaults_but(|config| config.model = Some("llama3".into())),
        );
        assert_set_alone(
            "system_prompt = \"Be brief.\"",
            only(|file| file.system_prompt = Some("Be brief.".into())),
            defaults_but(|config| config.system_prompt = "Be brief.".into()),
        );
    }

    #[test]
    fn the_number_settings_set_alone_change_only_themselves() {
        assert_set_alone(
            "temperature = 0.7",
            only(|file| file.temperature = Some(0.7)),
            defaults_but(|config| config.temperature = Some(0.7)),
        );
        assert_set_alone(
            "timeout_secs = 60",
            only(|file| file.timeout_secs = Some(60)),
            defaults_but(|config| config.timeout_secs = 60),
        );
        assert_set_alone(
            "max_output_tokens = 4096",
            only(|file| file.max_output_tokens = Some(4096)),
            defaults_but(|config| config.max_output_tokens = 4096),
        );
        assert_set_alone(
            "context_length = 8192",
            only(|file| file.context_length = Some(8192)),
            defaults_but(|config| config.context_length = Some(8192)),
        );
    }

    #[test]
    fn the_list_and_choice_settings_set_alone_change_only_themselves() {
        assert_set_alone(
            "on_secrets = \"warn\"",
            only(|file| file.on_secrets = Some(OnSecrets::Warn)),
            defaults_but(|config| config.on_secrets = OnSecrets::Warn),
        );
        assert_set_alone(
            "exclude = [\"*.min.js\"]",
            only(|file| file.exclude = Some(vec!["*.min.js".into()])),
            defaults_but(|config| config.exclude = vec!["*.min.js".into()]),
        );
    }

    #[test]
    fn a_setting_equal_to_its_default_stays_set_through_a_write_and_a_read() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let file = ConfigFile {
            exclude: None,
            system_prompt: None,
            ..everything_at_its_default()
        };
        file.save(&path).unwrap();
        assert_eq!(ConfigFile::read(&path).unwrap().unwrap(), file);
        let defaults = file.resolve();
        assert_eq!(defaults.temperature, Some(LOCAL_TEMPERATURE));
        assert_eq!(defaults.timeout_secs, DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn a_timeout_of_300_and_a_temperature_of_0_4_are_the_users_choice() {
        assert_eq!(parse("timeout_secs = 300").timeout_secs, Some(300));
        assert_eq!(parse("temperature = 0.4").temperature, Some(0.4));
    }

    #[test]
    fn the_prompt_older_files_hold_counts_as_unset() {
        let literal = format!("system_prompt = '''\n{DEFAULT_SYSTEM_PROMPT}\n'''");
        let escaped = toml::Value::String(DEFAULT_SYSTEM_PROMPT.to_owned()).to_string();
        let basic = format!("system_prompt = {escaped}");
        let padded = format!("system_prompt = '''\n\n{DEFAULT_SYSTEM_PROMPT}\n\n'''");
        let windows = format!(
            "system_prompt = '''\r\n{}\r\n'''\r\n",
            DEFAULT_SYSTEM_PROMPT.replace('\n', "\r\n")
        );
        for text in [literal, basic, padded, windows] {
            let file = parse(&text);
            assert_eq!(file.system_prompt, None, "{text}");
            assert_eq!(file.resolve().system_prompt, DEFAULT_SYSTEM_PROMPT);
        }
    }

    #[test]
    fn a_prompt_that_differs_in_one_inner_character_is_set() {
        let changed = DEFAULT_SYSTEM_PROMPT.replacen("Guidelines:", "Guidelinez:", 1);
        let file = parse(&format!("system_prompt = '''\n{changed}\n'''"));
        assert_eq!(
            file.system_prompt.as_deref(),
            Some(format!("{changed}\n").as_str())
        );
        assert_eq!(file.resolve().system_prompt, format!("{changed}\n"));
    }

    #[test]
    fn the_default_exclude_list_counts_as_unset_in_any_file() {
        let quoted: Vec<String> = DEFAULT_EXCLUDE
            .iter()
            .map(|pattern| format!("\"{pattern}\""))
            .collect();
        let patterns = quoted.join(", ");
        let file = parse(&format!("exclude = [{patterns}]"));
        assert_eq!(file.exclude, None);
        assert_eq!(file.resolve().exclude.len(), DEFAULT_EXCLUDE.len());
        assert_eq!(parse("exclude = []").exclude, Some(Vec::new()));
        assert!(parse("exclude = [\"Cargo.lock\"]").exclude.is_some());
    }

    #[test]
    fn a_file_written_by_v0_3_0_keeps_its_temperature_and_timeout() {
        let file = parse(V0_3_0);
        let expected = ConfigFile {
            base_url: Some(DEFAULT_BASE_URL.into()),
            temperature: Some(0.4),
            timeout_secs: Some(DEFAULT_TIMEOUT_SECS),
            ..ConfigFile::default()
        };
        assert_eq!(file, expected);
        assert_eq!(file.resolve().temperature, Some(0.4));
    }

    #[test]
    fn a_file_written_by_v0_4_0_has_only_its_own_choices() {
        let file = parse(V0_4_0);
        let expected = ConfigFile {
            base_url: Some(DEFAULT_BASE_URL.into()),
            timeout_secs: Some(DEFAULT_TIMEOUT_SECS),
            max_output_tokens: Some(DEFAULT_MAX_OUTPUT_TOKENS),
            ..ConfigFile::default()
        };
        assert_eq!(file, expected);
        assert_eq!(file.resolve(), Config::default());
    }

    #[test]
    fn a_file_written_by_v0_11_0_has_only_its_own_choices() {
        let file = parse(V0_11_0);
        let expected = ConfigFile {
            base_url: Some(DEFAULT_BASE_URL.into()),
            timeout_secs: Some(DEFAULT_TIMEOUT_SECS),
            max_output_tokens: Some(DEFAULT_MAX_OUTPUT_TOKENS),
            on_secrets: Some(OnSecrets::Reject),
            ..ConfigFile::default()
        };
        assert_eq!(file, expected);
        assert_eq!(file.resolve(), Config::default());
    }

    #[test]
    fn the_prompt_and_the_list_in_each_old_file_are_released_defaults() {
        for text in [V0_3_0, V0_4_0, V0_11_0] {
            let raw: ConfigFile = toml::from_str(text).unwrap();
            assert!(raw.system_prompt.is_some());
            assert!(raw.exclude.is_some());
            let read = parse(text);
            assert_eq!(read.system_prompt, None);
            assert_eq!(read.exclude, None);
        }
    }

    #[test]
    fn a_file_from_v0_2_0_is_refused_for_its_old_keys() {
        let text = "base_url = \"http://localhost:1234/v1\"\nmodel = \"google/gemma-4-e4b\"\n\
                    temperature = 0.4\ntimeout_secs = 300\nmax_diff_chars = 12000\n";
        let error = ConfigFile::parse(text).unwrap_err();
        assert!(error.to_string().contains("max_diff_chars"), "{error}");
        assert!(error.to_string().contains("line 5"), "{error}");
    }

    #[test]
    fn the_setting_names_are_the_ones_the_file_takes() {
        let error = toml::from_str::<ConfigFile>("x = 1").unwrap_err();
        let listed: Vec<&str> = error.message().split('`').skip(3).step_by(2).collect();
        assert_eq!(listed, ConfigFile::setting_names());
    }

    #[test]
    fn names_the_settings_a_file_sets() {
        assert!(ConfigFile::default().set_names().is_empty());
        let file = ConfigFile {
            model: Some("m".into()),
            on_secrets: Some(OnSecrets::Warn),
            system_prompt: Some("p".into()),
            ..ConfigFile::default()
        };
        assert_eq!(file.set_names(), ["model", "on_secrets", "system_prompt"]);
        assert_eq!(
            everything_at_its_default().set_names(),
            [
                "base_url",
                "temperature",
                "timeout_secs",
                "max_output_tokens",
                "on_secrets",
                "exclude",
                "system_prompt"
            ]
        );
    }

    #[test]
    fn never_reads_an_api_key_from_the_file() {
        assert!(ConfigFile::parse("api_key = \"sk-secret\"").is_err());
    }

    #[test]
    fn reads_nothing_from_a_missing_file_and_names_a_broken_one() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        assert_eq!(ConfigFile::read(&path).unwrap(), None);
        std::fs::write(&path, "timeout_secs = \"soon\"").unwrap();
        let error = format!("{:#}", ConfigFile::read(&path).unwrap_err());
        assert!(error.contains("invalid config file"), "{error}");
        assert!(error.contains("config.toml"), "{error}");
        assert!(error.contains("line 1, column 16"), "{error}");
    }

    #[test]
    fn saving_creates_the_folder() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a").join("b").join("config.toml");
        ConfigFile::default().save(&path).unwrap();
        assert_eq!(
            ConfigFile::read(&path).unwrap(),
            Some(ConfigFile::default())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_saved_file_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        ConfigFile::default().save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_file_that_would_not_read_back_the_same_is_not_written() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        ConfigFile::default().save(&path).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let not_a_number = ConfigFile {
            temperature: Some(f32::NAN),
            ..ConfigFile::default()
        };
        let error = not_a_number.save(&path).unwrap_err();
        assert!(error.to_string().contains("read back"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1);
    }

    #[test]
    fn text_with_control_characters_is_written_so_that_it_reads_back() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        for control in ['\u{1b}', '\0', '\u{c}', '\u{7f}', '\r'] {
            let file = ConfigFile {
                system_prompt: Some(format!("before {control} after")),
                model: Some(format!("m{control}")),
                ..ConfigFile::default()
            };
            file.save(&path).unwrap();
            assert_eq!(
                ConfigFile::read(&path).unwrap().unwrap(),
                file,
                "{control:?}"
            );
        }
    }

    #[test]
    fn flags_become_set_settings() {
        let overrides = Overrides {
            model: Some("some-model".into()),
            context_length: Some(8192),
            max_output_tokens: Some(4096),
            base_url: Some("https://example.com/v1".into()),
            api_key: Some("key".into()),
            temperature: Some(1.5),
            on_secrets: Some(OnSecrets::Warn),
            config: Some("elsewhere.toml".into()),
        };
        assert_eq!(
            ConfigFile::from_overrides(&overrides),
            ConfigFile {
                base_url: Some("https://example.com/v1".into()),
                model: Some("some-model".into()),
                temperature: Some(1.5),
                max_output_tokens: Some(4096),
                context_length: Some(8192),
                on_secrets: Some(OnSecrets::Warn),
                ..ConfigFile::default()
            }
        );
        assert_eq!(
            ConfigFile::from_overrides(&Overrides::default()),
            ConfigFile::default()
        );
    }

    #[test]
    fn a_blank_model_is_no_model_in_a_file_and_in_flags() {
        assert_eq!(parse("model = \"  \"").model, None);
        assert_eq!(parse("model = \"\"").model, None);
        let blank = Overrides {
            model: Some("  ".into()),
            ..Overrides::default()
        };
        assert_eq!(ConfigFile::from_overrides(&blank).model, None);
        let named = Overrides {
            model: Some("m".into()),
            ..Overrides::default()
        };
        assert_eq!(
            ConfigFile::from_overrides(&named).model.as_deref(),
            Some("m")
        );
    }

    #[test]
    fn validation_refuses_settings_the_config_would_refuse() {
        let bad = [
            only(|file| file.base_url = Some("nope".into())),
            only(|file| file.max_output_tokens = Some(10)),
            only(|file| file.context_length = Some(10)),
            only(|file| file.temperature = Some(3.0)),
        ];
        for file in bad {
            assert!(file.validated().is_err());
        }
        let good = everything_at_its_default();
        assert_eq!(good.clone().validated().unwrap(), good);
    }
}
