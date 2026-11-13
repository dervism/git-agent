//! Writes the config file: each setting under its explanation, as a value if it is set and as
//! a comment if it isn't. The comment shows the default, or an example for a setting that has
//! none. Removing the leading "#" from a commented setting makes it a set one.

use super::{
    ConfigFile, DEFAULT_BASE_URL, DEFAULT_EXCLUDE, DEFAULT_MAX_OUTPUT_TOKENS,
    DEFAULT_SYSTEM_PROMPT, DEFAULT_TIMEOUT_SECS, FALLBACK_CONTEXT_LENGTH, LOCAL_TEMPERATURE,
    OnSecrets,
};

const HEADER: &str = "\
# git-agent configuration
#
# Change these settings with `git agent config`, or edit this file. Command-line
# flags and environment variables override these values.
#
# A line that starts with # is not set. git-agent uses the value the line shows, unless
# the text above it calls it an example, and a new release may change that value.
# Remove the # to set it.";

const EXAMPLE_MODEL: &str = "qwen/qwen3-8b";
const EXAMPLE_TEMPERATURE: f32 = 0.4;
const EXAMPLE_CONTEXT_LENGTH: usize = 32768;

/// The file's text for `file`.
pub fn render(file: &ConfigFile) -> String {
    let sections = [
        base_url(file),
        model(file),
        api_key_note(),
        temperature(file),
        timeout_secs(file),
        max_output_tokens(file),
        context_length(file),
        on_secrets(file),
        exclude(file),
        system_prompt(file),
    ];
    format!("{HEADER}\n\n{}\n", sections.join("\n\n"))
}

fn base_url(file: &ConfigFile) -> String {
    let help = "\
# Base URL of an OpenAI-compatible API, including the /v1 suffix.
#   LM Studio:  http://localhost:1234/v1
#   Ollama:     http://localhost:11434/v1
#   OpenAI:     https://api.openai.com/v1
# `git agent config` lists more providers.
# Overridden by --base-url / GIT_AGENT_BASE_URL.";
    let value = file.base_url.as_deref().map(toml_string);
    section(help, "base_url", value, toml_string(DEFAULT_BASE_URL))
}

fn model(file: &ConfigFile) -> String {
    let help = "\
# Model ID as listed by `git-agent models`. When it isn't set, git-agent uses
# the model that is loaded on the server, or the only model the server has.
# The commented line is an example.
# Overridden by --model / GIT_AGENT_MODEL.";
    let value = file.model.as_deref().map(toml_string);
    section(help, "model", value, toml_string(EXAMPLE_MODEL))
}

fn api_key_note() -> String {
    "\
# API keys are never stored in this file. On macOS, `git agent config` saves
# them in the keychain. Otherwise, set the GIT_AGENT_API_KEY environment variable."
        .to_owned()
}

fn temperature(file: &ConfigFile) -> String {
    let help = format!(
        "\
# Sampling temperature from 0.0 to 2.0. Lower values give more predictable
# messages. When it isn't set, git-agent uses {LOCAL_TEMPERATURE} on local servers and
# leaves it to the provider in the cloud, since many cloud models accept only
# their default. Anthropic's API takes no temperature at all.
# The commented line is an example."
    );
    let value = file
        .temperature
        .filter(|temperature| temperature.is_finite())
        .map(|temperature| temperature.to_string());
    section(&help, "temperature", value, EXAMPLE_TEMPERATURE.to_string())
}

fn timeout_secs(file: &ConfigFile) -> String {
    let help = "# Seconds to wait for the model to answer.";
    let value = file.timeout_secs.map(|seconds| seconds.to_string());
    section(
        help,
        "timeout_secs",
        value,
        DEFAULT_TIMEOUT_SECS.to_string(),
    )
}

fn max_output_tokens(file: &ConfigFile) -> String {
    let help = "\
# Most tokens the model may write in one reply, thinking included. git-agent
# sends it to Anthropic, whose API needs a limit; OpenAI-compatible servers use
# their own. It is also the most room git-agent keeps free for the reply. Much
# higher values can make a reply take longer than `timeout_secs`.
# Overridden by --max-output-tokens / GIT_AGENT_MAX_OUTPUT_TOKENS.";
    let value = file.max_output_tokens.map(|tokens| tokens.to_string());
    let default = DEFAULT_MAX_OUTPUT_TOKENS.to_string();
    section(help, "max_output_tokens", value, default)
}

fn context_length(file: &ConfigFile) -> String {
    let help = format!(
        "\
# The model's context window in tokens. git-agent shortens large diffs to fit
# it, leaving room for the reply. When it isn't set, git-agent asks the server
# (LM Studio, Ollama, llama.cpp, vLLM, Anthropic and OpenRouter report it) or
# assumes {FALLBACK_CONTEXT_LENGTH} tokens. Set it if your server doesn't report it, or
# lower it to keep prompts small and fast. The commented line is an example.
# Overridden by --context-length / GIT_AGENT_CONTEXT_LENGTH."
    );
    let value = file.context_length.map(|tokens| tokens.to_string());
    let example = EXAMPLE_CONTEXT_LENGTH.to_string();
    section(&help, "context_length", value, example)
}

fn on_secrets(file: &ConfigFile) -> String {
    let help = "\
# What git-agent does when the changes it would commit, or the commits it would
# push or open a pull request for, add what look like secrets, such as API keys,
# private keys or passwords:
#   \"reject\"  don't commit, push or create the pull request
#   \"warn\"    list them and carry on
# Either way, the model gets [REDACTED] in their place.
# Overridden by --on-secrets / GIT_AGENT_ON_SECRETS.";
    let value = file
        .on_secrets
        .map(|on_secrets| toml_string(on_secrets.as_str()));
    let default = toml_string(OnSecrets::default().as_str());
    section(help, "on_secrets", value, default)
}

fn exclude(file: &ConfigFile) -> String {
    let help = "\
# Files whose content is left out of the diff sent to the model. They still
# appear in the list of changed files. Patterns without a \"/\" match the file
# name in any directory; \"*\" wildcards are allowed.";
    let value = file.exclude.as_deref().map(string_array);
    section(help, "exclude", value, string_array(DEFAULT_EXCLUDE))
}

fn system_prompt(file: &ConfigFile) -> String {
    let help = "# Instructions given to the model before the diff.";
    let value = file.system_prompt.as_deref().map(multi_line_string);
    section(
        help,
        "system_prompt",
        value,
        multi_line_string(DEFAULT_SYSTEM_PROMPT),
    )
}

/// `help`, then the setting: `key = value` if it has a value, else `default` as a comment.
fn section(help: &str, key: &str, value: Option<String>, default: String) -> String {
    let setting = match value {
        Some(value) => format!("{key} = {value}"),
        None => comment_out(&format!("{key} = {default}")),
    };
    format!("{help}\n{setting}")
}

fn comment_out(text: &str) -> String {
    text.lines()
        .map(|line| match line {
            "" => "#".to_owned(),
            line => format!("# {line}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

/// One pattern per line, so a long list stays readable and a line can be commented out.
fn string_array<S: AsRef<str>>(items: &[S]) -> String {
    let lines: String = items
        .iter()
        .map(|item| format!("    {},\n", toml_string(item.as_ref())))
        .collect();
    format!("[\n{lines}]")
}

/// A literal block of lines, unless the text holds `'''` or a control character other than a
/// tab or a line break, which a literal block refuses and only an escaped string can hold.
fn multi_line_string(text: &str) -> String {
    let refused = |c: char| c.is_control() && !matches!(c, '\n' | '\t');
    if text.contains("'''") || text.chars().any(refused) {
        toml_string(text)
    } else {
        format!("'''\n{}\n'''", text.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, DEFAULT_BASE_URL};

    const KEYS: [&str; 9] = [
        "base_url",
        "model",
        "temperature",
        "timeout_secs",
        "max_output_tokens",
        "context_length",
        "on_secrets",
        "exclude",
        "system_prompt",
    ];

    fn read(text: &str) -> ConfigFile {
        ConfigFile::parse(text).unwrap_or_else(|error| panic!("{error:#}\n{text}"))
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
            exclude: Some(vec!["*.min.js".into(), "docs/generated/**".into()]),
            system_prompt: Some("Be brief.\n\nUse \"quotes\" and \\ slashes.\n".into()),
        }
    }

    /// The names of the settings the text writes as values, or as comments.
    fn settings_written(text: &str, as_value: bool) -> Vec<String> {
        let prefix = if as_value { "" } else { "# " };
        text.lines()
            .filter_map(|line| line.strip_prefix(prefix))
            .filter_map(|line| line.split_once(" = "))
            .map(|(key, _)| key)
            .filter(|key| KEYS.contains(key))
            .map(str::to_owned)
            .collect()
    }

    /// The text with the `#` removed from the commented setting `key` and its lines.
    fn uncommented(text: &str, key: &str) -> String {
        let marker = format!("# {key} = ");
        let mut lines = text.lines();
        let mut kept: Vec<String> = Vec::new();
        for line in lines.by_ref() {
            if line.starts_with(&marker) {
                kept.push(line[2..].to_owned());
                break;
            }
            kept.push(line.to_owned());
        }
        for line in lines {
            match line.strip_prefix('#') {
                Some(rest) => kept.push(rest.strip_prefix(' ').unwrap_or(rest).to_owned()),
                None => {
                    kept.push(line.to_owned());
                    break;
                }
            }
        }
        kept.join("\n")
    }

    #[test]
    fn a_file_with_nothing_set_writes_every_setting_as_a_comment() {
        let text = render(&ConfigFile::default());
        assert!(settings_written(&text, true).is_empty(), "{text}");
        assert_eq!(settings_written(&text, false), KEYS);
        for line in [
            "# base_url = \"http://localhost:1234/v1\"",
            "# model = \"qwen/qwen3-8b\"",
            "# temperature = 0.4",
            "# timeout_secs = 300",
            "# max_output_tokens = 16000",
            "# context_length = 32768",
            "# on_secrets = \"reject\"",
        ] {
            assert!(
                text.lines().any(|written| written == line),
                "{line}\n{text}"
            );
        }
        assert_eq!(read(&text), ConfigFile::default());
    }

    #[test]
    fn each_setting_set_alone_is_the_only_one_written_as_a_value() {
        let full = everything_set();
        let alone = |keep: &str| ConfigFile {
            base_url: full.base_url.clone().filter(|_| keep == "base_url"),
            model: full.model.clone().filter(|_| keep == "model"),
            temperature: full.temperature.filter(|_| keep == "temperature"),
            timeout_secs: full.timeout_secs.filter(|_| keep == "timeout_secs"),
            max_output_tokens: full
                .max_output_tokens
                .filter(|_| keep == "max_output_tokens"),
            context_length: full.context_length.filter(|_| keep == "context_length"),
            on_secrets: full.on_secrets.filter(|_| keep == "on_secrets"),
            exclude: full.exclude.clone().filter(|_| keep == "exclude"),
            system_prompt: full
                .system_prompt
                .clone()
                .filter(|_| keep == "system_prompt"),
        };
        for key in KEYS {
            let file = alone(key);
            let text = render(&file);
            assert_eq!(settings_written(&text, true), [key], "{text}");
            assert_eq!(read(&text), file, "{key}\n{text}");
        }
    }

    #[test]
    fn every_setting_set_reads_back_as_written() {
        let file = everything_set();
        let text = render(&file);
        assert!(settings_written(&text, false).is_empty(), "{text}");
        assert_eq!(read(&text), file);
    }

    #[test]
    fn a_setting_equal_to_its_default_is_written_as_a_value() {
        let file = ConfigFile {
            base_url: Some(DEFAULT_BASE_URL.into()),
            timeout_secs: Some(300),
            max_output_tokens: Some(16_000),
            on_secrets: Some(OnSecrets::Reject),
            ..ConfigFile::default()
        };
        let text = render(&file);
        assert!(text.contains("\ntimeout_secs = 300\n"), "{text}");
        assert!(text.contains("\nbase_url = \"http://localhost:1234/v1\"\n"));
        assert!(text.contains("\nmax_output_tokens = 16000\n"));
        assert!(text.contains("\non_secrets = \"reject\"\n"));
        assert_eq!(read(&text), file);
    }

    #[test]
    fn removing_the_hash_from_a_commented_setting_sets_it_to_what_the_line_shows() {
        let defaults = Config::default();
        let text = render(&ConfigFile::default());
        let set = |key| read(&uncommented(&text, key));
        let raw = |key| -> ConfigFile { toml::from_str(&uncommented(&text, key)).unwrap() };
        assert_eq!(set("base_url").base_url.as_deref(), Some(DEFAULT_BASE_URL));
        assert_eq!(set("model").model.as_deref(), Some(EXAMPLE_MODEL));
        assert_eq!(set("temperature").temperature, Some(EXAMPLE_TEMPERATURE));
        assert_eq!(
            set("timeout_secs").timeout_secs,
            Some(defaults.timeout_secs)
        );
        assert_eq!(
            set("max_output_tokens").max_output_tokens,
            Some(defaults.max_output_tokens)
        );
        let example = Some(EXAMPLE_CONTEXT_LENGTH);
        assert_eq!(set("context_length").context_length, example);
        assert_eq!(set("on_secrets").on_secrets, Some(defaults.on_secrets));
        assert_eq!(raw("exclude").exclude, Some(defaults.exclude));
    }

    #[test]
    fn the_commented_prompt_is_the_default_prompt_when_uncommented() {
        let text = render(&ConfigFile::default());
        let raw: ConfigFile = toml::from_str(&uncommented(&text, "system_prompt")).unwrap();
        assert_eq!(
            raw.system_prompt.as_deref().map(str::trim),
            Some(DEFAULT_SYSTEM_PROMPT.trim())
        );
        let commented: Vec<&str> = text
            .lines()
            .skip_while(|line| !line.starts_with("# system_prompt = "))
            .collect();
        assert!(commented.iter().all(|line| line.starts_with('#')), "{text}");
        assert!(commented.len() > 10);
    }

    #[test]
    fn every_commented_setting_uncomments_to_a_valid_file_alone() {
        let text = render(&ConfigFile::default());
        for key in KEYS {
            let uncommented = uncommented(&text, key);
            let file: ConfigFile = toml::from_str(&uncommented)
                .unwrap_or_else(|error| panic!("{key}: {error}\n{uncommented}"));
            assert_ne!(file, ConfigFile::default(), "{key}");
        }
    }

    #[test]
    fn a_prompt_with_triple_quotes_is_written_escaped_and_reads_back() {
        let file = ConfigFile {
            system_prompt: Some("It's '''odd''' text".into()),
            ..ConfigFile::default()
        };
        assert_eq!(read(&render(&file)), file);
    }

    #[test]
    fn the_three_settings_that_have_no_default_are_marked_as_examples() {
        let text = render(&ConfigFile::default());
        for key in ["model", "temperature", "context_length"] {
            let above: Vec<&str> = text
                .lines()
                .take_while(|line| !line.starts_with(&format!("# {key} = ")))
                .collect();
            let help_start = above.iter().rposition(|line| line.is_empty()).unwrap();
            let help = above[help_start..].join("\n");
            assert!(
                help.contains("The commented line is an example."),
                "{key}\n{help}"
            );
        }
        assert_eq!(text.matches("The commented line is an example.").count(), 3);
    }

    #[test]
    fn text_with_a_control_character_is_written_as_an_escaped_string() {
        for control in ['\u{1b}', '\0', '\u{c}', '\u{7f}', '\r'] {
            let prompt = format!("before {control} after");
            let file = ConfigFile {
                system_prompt: Some(prompt.clone()),
                ..ConfigFile::default()
            };
            let text = render(&file);
            assert!(!text.contains(control), "{control:?}");
            assert!(!text.contains("system_prompt = '''"), "{control:?}");
            assert_eq!(read(&text).system_prompt, Some(prompt), "{control:?}");
        }
    }

    #[test]
    fn a_tab_and_line_breaks_keep_the_literal_block() {
        let file = ConfigFile {
            system_prompt: Some("one\n\ttwo\nthree".into()),
            ..ConfigFile::default()
        };
        let text = render(&file);
        assert!(
            text.contains("system_prompt = '''\none\n\ttwo\nthree\n'''"),
            "{text}"
        );
        assert_eq!(
            read(&text).system_prompt.as_deref().map(str::trim),
            Some("one\n\ttwo\nthree")
        );
    }

    #[test]
    fn a_temperature_that_is_not_finite_is_not_written_as_a_value() {
        for temperature in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let file = ConfigFile {
                temperature: Some(temperature),
                ..ConfigFile::default()
            };
            let text = render(&file);
            assert!(!text.contains("\ntemperature = "), "{temperature}");
            assert!(text.parse::<toml::Table>().is_ok(), "{temperature}");
        }
    }

    #[test]
    fn a_prompt_is_written_trimmed() {
        let file = ConfigFile {
            system_prompt: Some("\n  Be brief.\n\n".into()),
            ..ConfigFile::default()
        };
        let text = render(&file);
        assert!(
            text.contains("system_prompt = '''\nBe brief.\n'''"),
            "{text}"
        );
    }

    #[test]
    fn writes_a_temperature_so_that_it_reads_back() {
        for temperature in [0.0, 0.4, 1.0, 2.0, 0.7] {
            let file = ConfigFile {
                temperature: Some(temperature),
                ..ConfigFile::default()
            };
            assert_eq!(read(&render(&file)), file, "{temperature}");
        }
    }

    #[test]
    fn an_empty_exclude_list_reads_back_as_set() {
        let file = ConfigFile {
            exclude: Some(Vec::new()),
            ..ConfigFile::default()
        };
        assert_eq!(read(&render(&file)), file);
    }

    #[test]
    fn explains_what_a_commented_setting_is() {
        let text = render(&ConfigFile::default());
        assert!(
            text.contains("A line that starts with # is not set."),
            "{text}"
        );
        assert!(
            text.contains("unless\n# the text above it calls it an example"),
            "{text}"
        );
        assert!(text.starts_with("# git-agent configuration\n"));
    }

    #[test]
    fn keeps_every_setting_under_its_explanation() {
        let text = render(&ConfigFile::default());
        for help in [
            "# Base URL of an OpenAI-compatible API",
            "# Model ID as listed by `git-agent models`",
            "# API keys are never stored in this file.",
            "# Sampling temperature from 0.0 to 2.0.",
            "# Seconds to wait for the model to answer.",
            "# Most tokens the model may write in one reply",
            "# The model's context window in tokens.",
            "# What git-agent does when the changes it would commit",
            "# Files whose content is left out of the diff",
            "# Instructions given to the model before the diff.",
        ] {
            assert!(text.contains(help), "{help}");
        }
    }
}
