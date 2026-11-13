//! The defaults that releases have written into config files.
//!
//! A file that still holds one of them hasn't chosen anything. It counts as unset, and the
//! default of a later release reaches it.

use super::{ConfigFile, DEFAULT_EXCLUDE, DEFAULT_SYSTEM_PROMPT};

/// Every system prompt that a release has written into files as the default, the current
/// one included. A release that changes the default prompt adds the old text here.
const SYSTEM_PROMPTS: &[&str] = &[DEFAULT_SYSTEM_PROMPT];

/// Every exclude list that a release has written into files as the default, the current one
/// included. A release that changes the default list adds the old one here, and only adds
/// patterns to the new default: every list here is a subset of `DEFAULT_EXCLUDE`, so a file
/// that stops holding a released list excludes at least what it did.
const EXCLUDES: &[&[&str]] = &[DEFAULT_EXCLUDE];

/// The defaults that releases have written into files, which count as unset in a file.
pub struct ReleasedDefaults<'a> {
    pub system_prompts: &'a [&'a str],
    pub excludes: &'a [&'a [&'a str]],
}

impl ReleasedDefaults<'static> {
    pub const ALL: Self = Self {
        system_prompts: SYSTEM_PROMPTS,
        excludes: EXCLUDES,
    };
}

impl ReleasedDefaults<'_> {
    /// `file` without the settings that only repeat a released default.
    pub fn forget(&self, file: ConfigFile) -> ConfigFile {
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
        ConfigFile {
            base_url,
            model,
            temperature,
            timeout_secs,
            max_output_tokens,
            context_length,
            on_secrets,
            exclude: exclude.filter(|patterns| !self.has_exclude(patterns)),
            system_prompt: system_prompt.filter(|prompt| !self.has_prompt(prompt)),
        }
    }

    /// Prompts are compared line by line after trimming, so line endings and the blank
    /// lines around a literal block don't matter.
    fn has_prompt(&self, prompt: &str) -> bool {
        self.system_prompts
            .iter()
            .any(|released| released.trim().lines().eq(prompt.trim().lines()))
    }

    fn has_exclude(&self, patterns: &[String]) -> bool {
        self.excludes.iter().any(|released| {
            patterns
                .iter()
                .map(String::as_str)
                .eq(released.iter().copied())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_with_prompt(prompt: &str) -> ConfigFile {
        ConfigFile {
            system_prompt: Some(prompt.to_owned()),
            ..ConfigFile::default()
        }
    }

    fn file_with_exclude(patterns: &[&str]) -> ConfigFile {
        ConfigFile {
            exclude: Some(
                patterns
                    .iter()
                    .map(|pattern| (*pattern).to_owned())
                    .collect(),
            ),
            ..ConfigFile::default()
        }
    }

    fn newer_release<'a>() -> ReleasedDefaults<'a> {
        ReleasedDefaults {
            system_prompts: &["an old prompt", "the prompt of a newer release"],
            excludes: &[&["old.lock"], &["new.lock"]],
        }
    }

    #[test]
    fn every_current_default_is_among_the_released_ones() {
        assert!(
            ReleasedDefaults::ALL
                .system_prompts
                .contains(&DEFAULT_SYSTEM_PROMPT)
        );
        assert!(ReleasedDefaults::ALL.excludes.contains(&DEFAULT_EXCLUDE));
    }

    #[test]
    fn every_released_exclude_list_is_a_subset_of_the_default() {
        for list in ReleasedDefaults::ALL.excludes {
            for pattern in *list {
                assert!(DEFAULT_EXCLUDE.contains(pattern), "{pattern}");
            }
        }
    }

    #[test]
    fn a_released_prompt_counts_as_unset_after_trimming() {
        for text in [
            DEFAULT_SYSTEM_PROMPT.to_owned(),
            format!("{DEFAULT_SYSTEM_PROMPT}\n"),
            format!("\n  {DEFAULT_SYSTEM_PROMPT}\n\n"),
        ] {
            let file = ReleasedDefaults::ALL.forget(file_with_prompt(&text));
            assert_eq!(file.system_prompt, None, "{text:?}");
        }
    }

    #[test]
    fn a_released_prompt_with_windows_line_ends_counts_as_unset() {
        let windows = DEFAULT_SYSTEM_PROMPT.replace('\n', "\r\n");
        assert_ne!(windows, DEFAULT_SYSTEM_PROMPT);
        for text in [
            windows.clone(),
            format!("{windows}\r\n"),
            format!("\r\n{windows}\r\n"),
        ] {
            let file = ReleasedDefaults::ALL.forget(file_with_prompt(&text));
            assert_eq!(file.system_prompt, None, "{text:?}");
        }
    }

    #[test]
    fn a_prompt_that_differs_in_one_character_is_set() {
        let inner = DEFAULT_SYSTEM_PROMPT.replacen("Format:", "Formats:", 1);
        let lone_return = DEFAULT_SYSTEM_PROMPT.replacen('\n', "\r", 1);
        let one_line = DEFAULT_SYSTEM_PROMPT.replace('\n', " ");
        for text in [inner, lone_return, one_line, "mine".to_owned()] {
            let file = ReleasedDefaults::ALL.forget(file_with_prompt(&text));
            assert_eq!(file.system_prompt.as_deref(), Some(text.as_str()));
        }
    }

    #[test]
    fn an_old_prompt_still_counts_as_unset_after_the_default_changes() {
        let release = newer_release();
        for old in [
            "an old prompt",
            "\nan old prompt\n",
            "the prompt of a newer release",
        ] {
            let file = release.forget(file_with_prompt(old));
            assert_eq!(file.system_prompt, None, "{old:?}");
        }
        let theirs = release.forget(file_with_prompt("their own prompt"));
        assert_eq!(theirs.system_prompt.as_deref(), Some("their own prompt"));
    }

    #[test]
    fn a_prompt_the_release_does_not_list_is_the_users_own() {
        let forgotten = newer_release().forget(file_with_prompt(DEFAULT_SYSTEM_PROMPT));
        assert_eq!(
            forgotten.system_prompt.as_deref(),
            Some(DEFAULT_SYSTEM_PROMPT)
        );
    }

    #[test]
    fn the_default_exclude_list_counts_as_unset() {
        let file = ReleasedDefaults::ALL.forget(file_with_exclude(DEFAULT_EXCLUDE));
        assert_eq!(file.exclude, None);
    }

    #[test]
    fn any_other_exclude_list_is_set() {
        let all = &ReleasedDefaults::ALL;
        let fewer = &DEFAULT_EXCLUDE[1..];
        let mut more: Vec<&str> = DEFAULT_EXCLUDE.to_vec();
        more.push("*.min.js");
        let reversed: Vec<&str> = DEFAULT_EXCLUDE.iter().rev().copied().collect();
        for patterns in [fewer, more.as_slice(), reversed.as_slice(), &[]] {
            let file = all.forget(file_with_exclude(patterns));
            assert_eq!(
                file.exclude.as_deref().map(<[String]>::len),
                Some(patterns.len())
            );
        }
    }

    #[test]
    fn an_old_exclude_list_still_counts_as_unset_after_the_default_changes() {
        let release = newer_release();
        for old in [&["old.lock"][..], &["new.lock"]] {
            assert_eq!(release.forget(file_with_exclude(old)).exclude, None);
        }
        let theirs = release.forget(file_with_exclude(DEFAULT_EXCLUDE));
        assert!(theirs.exclude.is_some());
    }

    #[test]
    fn forgetting_leaves_the_other_settings_alone() {
        let file = ConfigFile {
            base_url: Some("https://example.com/v1".into()),
            timeout_secs: Some(300),
            temperature: Some(0.4),
            system_prompt: Some(DEFAULT_SYSTEM_PROMPT.into()),
            exclude: Some(DEFAULT_EXCLUDE.iter().map(|s| (*s).to_owned()).collect()),
            ..ConfigFile::default()
        };
        let forgotten = ReleasedDefaults::ALL.forget(file.clone());
        assert_eq!(
            forgotten,
            ConfigFile {
                system_prompt: None,
                exclude: None,
                ..file
            }
        );
    }
}
