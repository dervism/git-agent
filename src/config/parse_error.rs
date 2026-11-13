//! Says what is wrong with a config file without repeating what it holds: everything here
//! comes from the position of the error, the name of the setting on that line, and the kind of
//! mistake.

use super::ConfigFile;
use anyhow::{Error, anyhow};
use std::ops::Range;

/// The error for `text`, which failed to parse as `error`.
pub fn describe(text: &str, error: &toml::de::Error) -> Error {
    let span = error.span();
    let written = span
        .as_ref()
        .and_then(|span| name_on_the_line_of(text, span.start));
    let what = match &written {
        Some(name) if looks_like_an_api_key(name) => API_KEY_ADVICE.to_owned(),
        name => mistake(error.message(), name.as_deref()),
    };
    match span {
        Some(span) => {
            let (line, column) = position(text, &span);
            anyhow!("line {line}, column {column}: {what}")
        }
        None => anyhow!("{what}"),
    }
}

const API_KEY_ADVICE: &str = "API keys don't go in the config file; save one with \
    `git agent config` (the macOS keychain) or set the GIT_AGENT_API_KEY environment variable";

/// What is wrong, from serde's `message` and the `written` name on the line. The name is only
/// shown when it is one of ours: any other text on the line could be a value.
fn mistake(message: &str, written: Option<&str>) -> String {
    let known = written.filter(|name| is_known(name));
    if message.starts_with("unknown field") {
        return match (known, written.and_then(closest_setting)) {
            (Some(name), _) => format!("unknown setting `{name}`"),
            (None, Some(close)) => format!("unknown setting; did you mean `{close}`?"),
            (None, None) => "unknown setting".to_owned(),
        };
    }
    let named = |what: &str| match known {
        Some(name) => format!("`{name}`: {what}"),
        None => what.to_owned(),
    };
    if !is_about_a_value(message) {
        return named(message);
    }
    match message.rsplit_once(", expected ") {
        Some((_, expected)) => named(&format!("expected {expected}")),
        None => named("the value is not allowed here"),
    }
}

/// Keys that earlier releases wrote, which the file doesn't take any more.
const OLD_SETTINGS: [&str; 1] = ["max_diff_chars"];

fn is_known(name: &str) -> bool {
    ConfigFile::setting_names().contains(&name) || OLD_SETTINGS.contains(&name)
}

/// The setting whose name is a typo or two away from `written`, if there is one.
fn closest_setting(written: &str) -> Option<&'static str> {
    ConfigFile::setting_names()
        .into_iter()
        .map(|name| (edits(written, name), name))
        .filter(|(distance, _)| *distance <= 2)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, name)| name)
}

/// The fewest single-character changes that turn `from` into `to`.
fn edits(from: &str, to: &str) -> usize {
    let to: Vec<char> = to.chars().collect();
    let mut row: Vec<usize> = (0..=to.len()).collect();
    for (i, a) in from.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, b) in to.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(a != *b))
                .min(above + 1)
                .min(row[j] + 1);
            diagonal = above;
        }
    }
    row[to.len()]
}

/// Whether serde's `message` is about the value of a setting, which it repeats, and not about
/// the syntax, which TOML describes without quoting the file.
fn is_about_a_value(message: &str) -> bool {
    [
        "invalid type",
        "invalid value",
        "invalid length",
        "unknown variant",
        "missing field",
    ]
    .iter()
    .any(|start| message.starts_with(start))
}

/// The line and column, counted from 1, where `span` starts.
fn position(text: &str, span: &Range<usize>) -> (usize, usize) {
    let before = text.get(..span.start).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let column = before.chars().rev().take_while(|c| *c != '\n').count() + 1;
    (line, column)
}

/// The name before the `=` on the line that holds the byte at `offset`, if it looks like a
/// name. Whatever it is, it is text from the file.
fn name_on_the_line_of(text: &str, offset: usize) -> Option<String> {
    let start = text
        .get(..offset)?
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let line = text.get(start..)?.lines().next()?;
    let (name, _) = line.split_once('=')?;
    let name = name.trim();
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    plain.then(|| name.to_owned())
}

fn looks_like_an_api_key(key: &str) -> bool {
    let plain: String = key
        .chars()
        .filter(|c| !matches!(c, '_' | '-'))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    plain.contains("apikey") || plain == "key" || plain == "token"
}

#[cfg(test)]
mod tests {
    use super::edits;
    use crate::config::ConfigFile;

    fn error_of(text: &str) -> String {
        format!("{:#}", ConfigFile::parse(text).unwrap_err())
    }

    fn secret() -> String {
        format!("sk-{}", "Z9".repeat(12))
    }

    #[test]
    fn an_api_key_in_the_file_is_refused_without_being_shown() {
        for name in [
            "api_key", "apikey", "api-key", "key", "token", "API_KEY", "ApiKey",
        ] {
            let text = format!("timeout_secs = 5\n{name} = \"{}\"\n", secret());
            let error = error_of(&text);
            assert!(!error.contains(&secret()), "{error}");
            assert!(!error.contains("Z9Z9"), "{error}");
            assert!(error.contains("line 2, column 1"), "{error}");
            assert!(
                error.contains("API keys don't go in the config file"),
                "{error}"
            );
            assert!(error.contains("git agent config"), "{error}");
            assert!(error.contains("GIT_AGENT_API_KEY"), "{error}");
        }
    }

    #[test]
    fn an_unquoted_api_key_is_refused_without_being_shown() {
        let error = error_of(&format!("api_key = {}\n", secret()));
        assert!(!error.contains(&secret()), "{error}");
        assert!(!error.contains("Z9Z9"), "{error}");
        assert!(
            error.contains("API keys don't go in the config file"),
            "{error}"
        );
    }

    #[test]
    fn a_misspelt_setting_gets_a_suggestion_and_its_value_is_not_shown() {
        let error = error_of(&format!("modle = \"{}\"\n", secret()));
        assert!(error.contains("did you mean `model`?"), "{error}");
        assert!(error.contains("line 1, column 1"), "{error}");
        assert!(!error.contains(&secret()), "{error}");
    }

    #[test]
    fn a_bad_url_with_credentials_is_not_shown() {
        let password = format!("pw{}", "Q7".repeat(8));
        let url = format!("https://user:{password}@example.com/v1");
        for text in [
            format!("base_url = {url}\n"),
            format!("base_url = \"{url}\" trailing\n"),
            format!("base_url = [\"{url}\"]\n"),
            format!("timeout_secs = \"{url}\"\n"),
            format!("on_secrets = \"{url}\"\n"),
            format!("max_output_tokens = {{ x = \"{url}\" }}\n"),
            format!("exclude = \"{url}\"\n"),
        ] {
            let error = error_of(&text);
            assert!(!error.contains(&password), "{text}\n{error}");
            assert!(!error.contains("user:"), "{text}\n{error}");
            assert!(error.contains("line 1"), "{text}\n{error}");
        }
    }

    #[test]
    fn a_wrong_type_names_the_setting_and_what_was_expected() {
        let error = error_of("timeout_secs = \"soon\"\n");
        assert!(error.contains("`timeout_secs`"), "{error}");
        assert!(error.contains("expected"), "{error}");
        assert!(!error.contains("soon"), "{error}");
    }

    #[test]
    fn a_syntax_error_on_a_later_line_is_placed_there() {
        let error = error_of("timeout_secs = 5\n\nmodel = = 3\n");
        assert!(error.contains("line 3"), "{error}");
    }

    #[test]
    fn the_old_keys_of_v0_2_0_are_named() {
        let error = error_of("max_diff_chars = 12000\n");
        assert!(
            error.contains("unknown setting `max_diff_chars`"),
            "{error}"
        );
    }

    fn marker() -> String {
        format!("mk{}", "X7q".repeat(5))
    }

    #[test]
    fn an_unknown_bare_name_is_not_repeated() {
        let error = error_of(&format!("{} = 1\n", marker()));
        assert!(error.contains("unknown setting"), "{error}");
        assert!(!error.contains(&marker()), "{error}");
        assert!(!error.contains("did you mean"), "{error}");
    }

    #[test]
    fn a_name_close_to_a_setting_gets_that_setting_as_a_suggestion() {
        let error = error_of("modle = \"x\"\n");
        assert!(error.contains("did you mean `model`?"), "{error}");
        assert!(!error.contains("modle"), "{error}");
        let error = error_of("timeout_sec = 3\n");
        assert!(error.contains("did you mean `timeout_secs`?"), "{error}");
    }

    #[test]
    fn a_padded_token_alone_on_a_line_is_not_repeated() {
        let token = format!("{}==", marker());
        for text in [format!("{token}\n"), format!("timeout_secs = 5\n{token}\n")] {
            let error = error_of(&text);
            assert!(!error.contains(&marker()), "{error}");
        }
    }

    #[test]
    fn text_with_an_equals_sign_inside_an_array_or_a_string_is_not_repeated() {
        let marker = marker();
        for text in [
            format!("exclude = [\n\"a\",\n{marker}=x\n]\n"),
            format!("exclude = [\n  {marker} = 1,\n]\n"),
            format!("system_prompt = \"\"\"\nline {marker}=x\n"),
            format!("system_prompt = \"\"\"\n{marker}=x\n\"\"\" trailing\n"),
            format!("system_prompt = \"{marker}=x\" trailing\n"),
        ] {
            let error = error_of(&text);
            assert!(!error.contains(&marker), "{text}\n{error}");
        }
    }

    #[test]
    fn a_wrong_choice_names_the_choices_and_not_the_value() {
        let error = error_of(&format!("on_secrets = \"{}\"\n", marker()));
        assert!(error.contains("`on_secrets`"), "{error}");
        assert!(error.contains("reject"), "{error}");
        assert!(error.contains("warn"), "{error}");
        assert!(!error.contains(&marker()), "{error}");
        let tricky = format!("on_secrets = \"{}, expected `ok`\"\n", marker());
        let error = error_of(&tricky);
        assert!(!error.contains(&marker()), "{error}");
        assert!(!error.contains("`ok`"), "{error}");
    }

    #[test]
    fn counts_the_edits_between_names() {
        assert_eq!(edits("model", "model"), 0);
        assert_eq!(edits("modle", "model"), 2);
        assert_eq!(edits("mode", "model"), 1);
        assert_eq!(edits("", "abc"), 3);
        assert_eq!(edits("abc", ""), 3);
    }
}
