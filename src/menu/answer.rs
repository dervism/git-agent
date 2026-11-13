//! What the user chose in one step of the menu, and how it changes the config file.
//!
//! A step changes only the settings it asks about. Every other setting stays as the file has
//! it, set or unset, so a default that a later release changes still reaches the file.

use crate::config::{ConfigFile, OnSecrets};

/// A model picked in the menu, with the context length to save for it.
pub struct ModelChoice {
    pub model: Option<String>,
    pub context_length: Option<usize>,
}

/// A provider picked in the menu, with the model that goes with it.
pub struct ProviderChoice {
    pub base_url: String,
    pub model: Option<String>,
    pub context_length: Option<usize>,
    /// The user agreed to stop sending the temperature that the file sets.
    pub drops_temperature: bool,
}

/// An `Option` is the new value of a setting that can be unset: `None` leaves it unset, so
/// it gets its default.
pub enum Answer {
    Provider(ProviderChoice),
    Model(ModelChoice),
    ContextLength(Option<usize>),
    OutputLimit(Option<usize>),
    Temperature(Option<f32>),
    Timeout(Option<u64>),
    Secrets(Option<OnSecrets>),
    Exclude(Option<Vec<String>>),
    SystemPrompt(Option<String>),
}

impl Answer {
    /// Writes this answer into `file`. Returns whether the file changed.
    pub fn write_into(self, file: &mut ConfigFile) -> bool {
        let written = self.applied(file.clone());
        let changed = written != *file;
        *file = written;
        changed
    }

    /// `file` with this answer written into it. A prompt or an exclude list that is a released
    /// default is no choice, so it stays unset, as it would when the file is read again.
    pub fn applied(self, file: ConfigFile) -> ConfigFile {
        self.set_in(file).without_released_defaults()
    }

    fn set_in(self, file: ConfigFile) -> ConfigFile {
        match self {
            Answer::Provider(choice) => ConfigFile {
                base_url: Some(choice.base_url),
                model: choice.model,
                context_length: choice.context_length,
                temperature: file.temperature.filter(|_| !choice.drops_temperature),
                ..file
            },
            Answer::Model(choice) => ConfigFile {
                model: choice.model,
                context_length: choice.context_length,
                ..file
            },
            Answer::ContextLength(tokens) => ConfigFile {
                context_length: tokens,
                ..file
            },
            Answer::OutputLimit(tokens) => ConfigFile {
                max_output_tokens: tokens,
                ..file
            },
            Answer::Temperature(temperature) => ConfigFile {
                temperature,
                ..file
            },
            Answer::Timeout(seconds) => ConfigFile {
                timeout_secs: seconds,
                ..file
            },
            Answer::Secrets(on_secrets) => ConfigFile { on_secrets, ..file },
            Answer::Exclude(exclude) => ConfigFile { exclude, ..file },
            Answer::SystemPrompt(system_prompt) => ConfigFile {
                system_prompt,
                ..file
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        DEFAULT_BASE_URL, DEFAULT_EXCLUDE, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_SYSTEM_PROMPT,
        DEFAULT_TIMEOUT_SECS,
    };
    use tempfile::TempDir;

    const V0_11_0: &str = include_str!("../config/fixtures/v0.11.0.toml");

    fn nothing_set() -> ConfigFile {
        ConfigFile::default()
    }

    fn everything_set() -> ConfigFile {
        ConfigFile {
            base_url: Some("https://example.com/v1".into()),
            model: Some("some-model".into()),
            temperature: Some(0.9),
            timeout_secs: Some(12),
            max_output_tokens: Some(2048),
            context_length: Some(9000),
            on_secrets: Some(OnSecrets::Warn),
            exclude: Some(vec!["*.min.js".into()]),
            system_prompt: Some("my own prompt".into()),
        }
    }

    fn model_choice() -> ModelChoice {
        ModelChoice {
            model: Some("chosen".into()),
            context_length: Some(4000),
        }
    }

    fn provider_choice(drops_temperature: bool) -> ProviderChoice {
        ProviderChoice {
            base_url: "http://localhost:11434/v1".into(),
            model: Some("chosen".into()),
            context_length: None,
            drops_temperature,
        }
    }

    fn full_but(change: impl FnOnce(&mut ConfigFile)) -> ConfigFile {
        let mut file = everything_set();
        change(&mut file);
        file
    }

    fn assert_changes_only(answer: Answer, expected: ConfigFile) {
        assert_eq!(answer.applied(everything_set()), expected);
    }

    #[test]
    fn a_provider_and_a_model_change_only_their_own_settings() {
        assert_changes_only(
            Answer::Provider(provider_choice(true)),
            full_but(|file| {
                file.base_url = Some("http://localhost:11434/v1".into());
                file.model = Some("chosen".into());
                file.context_length = None;
                file.temperature = None;
            }),
        );
        assert_changes_only(
            Answer::Model(model_choice()),
            full_but(|file| {
                file.model = Some("chosen".into());
                file.context_length = Some(4000);
            }),
        );
    }

    #[test]
    fn a_single_setting_answer_changes_only_that_setting() {
        assert_changes_only(
            Answer::ContextLength(None),
            full_but(|file| file.context_length = None),
        );
        assert_changes_only(
            Answer::OutputLimit(Some(5000)),
            full_but(|file| file.max_output_tokens = Some(5000)),
        );
        assert_changes_only(
            Answer::Temperature(None),
            full_but(|file| file.temperature = None),
        );
        assert_changes_only(
            Answer::Timeout(Some(99)),
            full_but(|file| file.timeout_secs = Some(99)),
        );
        assert_changes_only(
            Answer::Secrets(Some(OnSecrets::Reject)),
            full_but(|file| file.on_secrets = Some(OnSecrets::Reject)),
        );
        assert_changes_only(Answer::Exclude(None), full_but(|file| file.exclude = None));
        assert_changes_only(
            Answer::SystemPrompt(None),
            full_but(|file| file.system_prompt = None),
        );
    }

    #[test]
    fn choosing_the_default_unsets_the_setting() {
        assert_changes_only(
            Answer::Timeout(None),
            full_but(|file| file.timeout_secs = None),
        );
        assert_changes_only(
            Answer::OutputLimit(None),
            full_but(|file| file.max_output_tokens = None),
        );
        assert_changes_only(
            Answer::Secrets(None),
            full_but(|file| file.on_secrets = None),
        );
        assert_eq!(Answer::Timeout(None).applied(nothing_set()), nothing_set());
    }

    fn settings_set(file: &ConfigFile) -> usize {
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
        [
            base_url.is_some(),
            model.is_some(),
            temperature.is_some(),
            timeout_secs.is_some(),
            max_output_tokens.is_some(),
            context_length.is_some(),
            on_secrets.is_some(),
            exclude.is_some(),
            system_prompt.is_some(),
        ]
        .into_iter()
        .filter(|set| *set)
        .count()
    }

    #[test]
    fn an_answer_leaves_every_other_setting_unset() {
        let answers = [
            Answer::Provider(provider_choice(false)),
            Answer::Model(model_choice()),
            Answer::ContextLength(Some(5000)),
            Answer::OutputLimit(Some(5000)),
            Answer::Temperature(Some(0.5)),
            Answer::Timeout(Some(99)),
            Answer::Secrets(Some(OnSecrets::Warn)),
            Answer::Exclude(Some(vec!["a".into()])),
            Answer::SystemPrompt(Some("mine".into())),
        ];
        let counts: Vec<usize> = answers
            .into_iter()
            .map(|answer| settings_set(&answer.applied(nothing_set())))
            .collect();
        assert_eq!(counts, [2, 2, 1, 1, 1, 1, 1, 1, 1]);
    }

    #[test]
    fn a_value_equal_to_the_default_that_the_user_sets_is_a_choice() {
        let timeout = Answer::Timeout(Some(DEFAULT_TIMEOUT_SECS)).applied(nothing_set());
        assert_eq!(timeout.timeout_secs, Some(DEFAULT_TIMEOUT_SECS));
        let output = Answer::OutputLimit(Some(DEFAULT_MAX_OUTPUT_TOKENS)).applied(nothing_set());
        assert_eq!(output.max_output_tokens, Some(DEFAULT_MAX_OUTPUT_TOKENS));
        let secrets = Answer::Secrets(Some(OnSecrets::Reject)).applied(nothing_set());
        assert_eq!(secrets.on_secrets, Some(OnSecrets::Reject));
    }

    #[test]
    fn a_prompt_or_a_list_that_is_a_released_default_stays_unset() {
        let prompt = Answer::SystemPrompt(Some(DEFAULT_SYSTEM_PROMPT.into()));
        assert_eq!(prompt.applied(everything_set()).system_prompt, None);
        let patterns = DEFAULT_EXCLUDE.iter().map(|s| (*s).to_owned()).collect();
        let list = Answer::Exclude(Some(patterns)).applied(everything_set());
        assert_eq!(list.exclude, None);
    }

    #[test]
    fn a_provider_at_the_default_address_sets_the_address() {
        let choice = ProviderChoice {
            base_url: DEFAULT_BASE_URL.into(),
            model: None,
            context_length: None,
            drops_temperature: false,
        };
        let file = Answer::Provider(choice).applied(nothing_set());
        assert_eq!(file.base_url.as_deref(), Some(DEFAULT_BASE_URL));
    }

    #[test]
    fn a_provider_keeps_the_temperature_unless_told_to_drop_it() {
        let kept = Answer::Provider(provider_choice(false)).applied(everything_set());
        assert_eq!(kept.temperature, Some(0.9));
        let dropped = Answer::Provider(provider_choice(true)).applied(everything_set());
        assert_eq!(dropped.temperature, None);
    }

    #[test]
    fn writing_reports_whether_the_file_changed() {
        let mut file = nothing_set();
        assert!(Answer::Timeout(Some(99)).write_into(&mut file));
        assert_eq!(file.timeout_secs, Some(99));
        assert!(!Answer::Timeout(Some(99)).write_into(&mut file));
        assert!(Answer::Timeout(None).write_into(&mut file));
        assert!(!Answer::Timeout(None).write_into(&mut file));
        assert!(Answer::Timeout(Some(DEFAULT_TIMEOUT_SECS)).write_into(&mut file));
        assert!(!Answer::Exclude(None).write_into(&mut file));
        assert!(!Answer::ContextLength(None).write_into(&mut file));
    }

    #[test]
    fn a_change_through_the_menu_leaves_the_other_settings_as_the_file_had_them() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let written = ConfigFile {
            timeout_secs: Some(DEFAULT_TIMEOUT_SECS),
            exclude: Some(vec!["*.min.js".into()]),
            ..nothing_set()
        };
        written.save(&path).unwrap();

        let file = ConfigFile::read(&path).unwrap().unwrap();
        let changed = Answer::OutputLimit(Some(32_000)).applied(file);
        changed.save(&path).unwrap();

        let saved = ConfigFile::read(&path).unwrap().unwrap();
        let expected = ConfigFile {
            max_output_tokens: Some(32_000),
            ..written
        };
        assert_eq!(saved, expected);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# temperature = 0.4"), "{text}");
        assert!(text.contains("\ntimeout_secs = 300\n"), "{text}");
        assert!(
            text.contains("# base_url = \"http://localhost:1234/v1\""),
            "{text}"
        );
    }

    #[test]
    fn a_change_to_a_file_from_v0_11_0_sets_only_that_setting_and_what_the_file_chose() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, V0_11_0).unwrap();

        let file = ConfigFile::read(&path).unwrap().unwrap();
        let changed = Answer::OutputLimit(Some(32_000)).applied(file);
        changed.save(&path).unwrap();

        let saved = ConfigFile::read(&path).unwrap().unwrap();
        let expected = ConfigFile {
            base_url: Some(DEFAULT_BASE_URL.into()),
            timeout_secs: Some(DEFAULT_TIMEOUT_SECS),
            max_output_tokens: Some(32_000),
            on_secrets: Some(OnSecrets::Reject),
            ..nothing_set()
        };
        assert_eq!(saved, expected);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\n# system_prompt = '''\n"), "{text}");
        assert!(text.contains("\n# exclude = [\n"), "{text}");
        assert!(!text.contains("\nsystem_prompt = "), "{text}");
        assert!(!text.contains("\nexclude = "), "{text}");
    }
}
