//! git's settings, read as git reads them: the value of one, every value of one, or the values of
//! a few in the order git reads them, and a value read as git reads a boolean.

use super::Repo;
use anyhow::{Context, Result, bail};

impl Repo {
    /// The value of the setting `key`, or `None` when it isn't set. A setting git can't read
    /// fails, rather than pass for one not set.
    pub(super) fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .config_values(&["--get", key])?
            .map(|values| values.into_iter().next().unwrap_or_default()))
    }

    /// Every value of the setting `key`, which can be set more than once.
    pub(super) fn settings(&self, key: &str) -> Result<Vec<String>> {
        Ok(self.config_values(&["--get-all", key])?.unwrap_or_default())
    }

    /// Each value of the settings whose names match `pattern`, in the order git reads them, as
    /// the name in lower case and the value, `None` for a name set without one.
    pub(super) fn settings_matching(&self, pattern: &str) -> Result<Vec<(String, Option<String>)>> {
        let entries = self.config_values(&["--get-regexp", pattern])?;
        Ok(entries
            .unwrap_or_default()
            .into_iter()
            .map(|entry| match entry.split_once('\n') {
                Some((name, value)) => (name.to_owned(), Some(value.to_owned())),
                None => (entry, None),
            })
            .collect())
    }

    /// The values `git config -z` with `args` prints, or `None` when what it asks for isn't set:
    /// git exits with 1 then.
    fn config_values(&self, args: &[&str]) -> Result<Option<Vec<String>>> {
        let args = [&["config", "-z"][..], args].concat();
        let output = self
            .command(&args)
            .output()
            .context("failed to run git; is it installed?")?;
        match output.status.code() {
            Some(0) => Ok(Some(
                String::from_utf8_lossy(&output.stdout)
                    .split_terminator('\0')
                    .map(str::to_owned)
                    .collect(),
            )),
            Some(1) => Ok(None),
            _ => bail!(
                "`git {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
    }
}

/// `value` as git reads a boolean setting, or `None` where git takes it for neither: a name set
/// without a value is true, an empty value false, and a number true unless it's zero.
pub(super) fn git_bool(value: Option<&str>) -> Option<bool> {
    let Some(value) = value else {
        return Some(true);
    };
    let said = |words: [&str; 3]| words.iter().any(|word| value.eq_ignore_ascii_case(word));
    if value.is_empty() || said(["false", "no", "off"]) {
        Some(false)
    } else if said(["true", "yes", "on"]) {
        Some(true)
    } else {
        git_int(value).map(|number| number != 0)
    }
}

/// `value` as git 2.50 reads a whole number in a setting, as C's `strtoimax` reads one, so that
/// `0x` starts a hexadecimal number and `0` an octal one, then `k`, `m` or `g` for 1024 to the
/// power of 1, 2 or 3, within an `int`. Older gits refuse the lowest `int`, and git 2.26 reads a
/// unit alone as 0.
fn git_int(value: &str) -> Option<i32> {
    let value = value.trim_start_matches([' ', '\t', '\n', '\u{b}', '\u{c}', '\r']);
    let (sign, unsigned) = match value.strip_prefix('-') {
        Some(unsigned) => (-1, unsigned),
        None => (1, value.strip_prefix('+').unwrap_or(value)),
    };
    let hexadecimal = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))
        .filter(|digits| digits.starts_with(|digit: char| digit.is_ascii_hexdigit()));
    let (radix, digits) = match hexadecimal {
        Some(digits) => (16, digits),
        None if unsigned.starts_with('0') => (8, unsigned),
        None => (10, unsigned),
    };
    let end = digits
        .find(|digit: char| !digit.is_digit(radix))
        .unwrap_or(digits.len());
    let factor: i64 = match &digits[end..] {
        "" => 1,
        "k" | "K" => 1 << 10,
        "m" | "M" => 1 << 20,
        "g" | "G" => 1 << 30,
        _ => return None,
    };
    let number = i64::from_str_radix(&digits[..end], radix).ok()?;
    i32::try_from(sign * number.checked_mul(factor)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support::{isolated_repo, run};
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    /// How git reads `value` as a boolean, or `None` where it refuses it: `value` `None` is a name
    /// set without a value.
    fn read_by_git(value: Option<&str>) -> Option<bool> {
        let setting = value.map_or("probe.value".to_owned(), |value| {
            format!("probe.value={value}")
        });
        let outside = TempDir::new().unwrap();
        let output = Command::new("git")
            .current_dir(outside.path())
            .args([
                "-c",
                &setting,
                "config",
                "--type=bool",
                "--get",
                "probe.value",
            ])
            .output()
            .unwrap();
        let read = String::from_utf8_lossy(&output.stdout);
        output.status.success().then(|| read.trim() == "true")
    }

    /// Ways of writing a boolean, a number or neither.
    const BOOLEANS: [&str; 36] = [
        "",
        "true",
        "TRUE",
        "Yes",
        "on",
        "false",
        "No",
        "OFF",
        "0",
        "1",
        "00",
        "010",
        "08",
        "0x0",
        "0x1f",
        "0X0",
        "0x",
        "0k",
        "1k",
        "2M",
        "1g",
        "3g",
        " +0",
        "\t0",
        "-0",
        "-1",
        "+-1",
        "0 ",
        "1.0",
        "2147483647",
        "2147483648",
        "-2147483649",
        "maybe",
        "check",
        "truee",
        "é",
    ];

    /// Whole numbers are read as git 2.50 reads them, where older gits differ too: the lowest
    /// `int`, which they refuse, and a unit alone, which git 2.26 reads as 0.
    #[test]
    fn reads_whole_numbers_as_current_git_does() {
        let cases = [
            ("-2147483648", Some(i32::MIN)),
            ("-2g", Some(i32::MIN)),
            ("-2147483649", None),
            ("2147483647", Some(i32::MAX)),
            ("2147483648", None),
            ("k", None),
            ("010", Some(8)),
            ("0x1f", Some(31)),
            ("1k", Some(1024)),
            (" +0", Some(0)),
            ("1 ", None),
        ];

        for (value, expected) in cases {
            assert_eq!(git_int(value), expected, "{value:?}");
        }
    }

    /// Every way of writing a boolean, a number or neither that the gits git-agent runs with
    /// agree on is read as the installed git reads it.
    #[test]
    fn reads_a_boolean_as_git_does() {
        assert_eq!(git_bool(None), read_by_git(None));
        for value in BOOLEANS {
            assert_eq!(git_bool(Some(value)), read_by_git(Some(value)), "{value:?}");
        }
    }

    /// A setting git can't read fails, rather than pass for one not set, and one that isn't set
    /// is `None`.
    #[test]
    fn a_setting_git_cannot_read_fails_rather_than_pass_for_one_not_set() {
        let (dir, repo) = isolated_repo();
        assert_eq!(repo.setting("push.default").unwrap(), None);
        assert!(repo.settings("remote.origin.fetch").unwrap().is_empty());

        let config = dir.path().join(".git/config");
        let mut text = fs::read_to_string(&config).unwrap();
        text.push_str("[push\n");
        fs::write(&config, text).unwrap();

        assert!(repo.setting("push.default").is_err());
        assert!(repo.settings("remote.origin.fetch").is_err());
        assert!(repo.settings_matching("^push\\.").is_err());
    }

    /// Values are read whole: with the spaces and newlines a quoted value can hold, each value of
    /// a setting set more than once, and a name set without a value apart from an empty one.
    #[test]
    fn reads_each_value_whole_and_in_order() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        run(dir, &["config", "probe.spaced", " two words \nand a line "]);
        run(dir, &["config", "--add", "probe.many", "one"]);
        run(dir, &["config", "--add", "probe.many", "two\nlines"]);
        let config = dir.join(".git/config");
        let mut text = fs::read_to_string(&config).unwrap();
        text.push_str("[probe]\n\tbare\n\tempty =\n");
        fs::write(&config, text).unwrap();

        let spaced = repo.setting("probe.spaced").unwrap();
        assert_eq!(spaced.as_deref(), Some(" two words \nand a line "));
        assert_eq!(repo.settings("probe.many").unwrap(), ["one", "two\nlines"]);
        let entries = repo
            .settings_matching("^probe\\.(bare|empty|many)$")
            .unwrap();
        let expected = [
            ("probe.many", Some("one")),
            ("probe.many", Some("two\nlines")),
            ("probe.bare", None),
            ("probe.empty", Some("")),
        ];
        let entries: Vec<(&str, Option<&str>)> = entries
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_deref()))
            .collect();
        assert_eq!(entries, expected);
    }
}
