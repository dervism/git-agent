//! Finds secrets in text before it is sent to a model, and replaces them with
//! [`REDACTED`].
//!
//! The rules match formats that are almost always secrets: tokens with a known
//! prefix, private key blocks, passwords in URLs, bearer tokens, and values
//! assigned to names such as `password` or `API_KEY`. Placeholders such as
//! `your-api-key` and references such as `${TOKEN}` are left alone, and so are
//! secrets in formats none of the rules know.

use crate::git::names;
use regex_lite::Regex;
use std::sync::LazyLock;

pub const REDACTED: &str = "[REDACTED]";

/// A secret that was replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    /// The file it is in, or what the text is, e.g. "commit 1a2b3c4".
    pub place: String,
    /// Its line in the file's new version, or in the old version for a removed
    /// line. `None` in plain text, whose lines may not match any file's.
    pub line: Option<usize>,
    pub change: Change,
    /// What it looks like, e.g. "GitHub token".
    pub kind: String,
}

/// What a change does to the line a secret is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The change adds the line, or the secret is in a commit message.
    Added,
    /// The line is shown around a change, which leaves it as it was.
    Unchanged,
    /// The change removes the line, and the secret with it.
    Removed,
}

pub struct Redacted {
    pub text: String,
    pub findings: Vec<Finding>,
}

#[derive(Clone, Copy)]
enum Check {
    /// A format that is a secret by itself: only placeholders are skipped.
    Token,
    /// A value that may be code or a setting: it also needs letters and digits,
    /// and no characters that only code uses.
    Value,
}

struct Rule {
    kind: &'static str,
    /// Lowercase text every match contains; lines with none of it are skipped.
    needles: &'static [&'static str],
    /// The secret is the group named `secret`. A group named `name`, if any,
    /// replaces `kind` in the finding. For assignments, the groups `op` and
    /// `quote` hold the operator and the quote before the value.
    pattern: &'static str,
    check: Check,
}

/// Names of settings that hold secrets, followed by an assignment and its value.
/// The value can't start with ":", so Rust paths such as `Token::new` don't match.
const ASSIGNMENT: &str = r#"(?i)(?P<name>[A-Za-z0-9_.-]*(?:password|passwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|secret[_-]?key|auth[_-]?key|credentials?))["']?\s*(?P<op>:=|=>|[:=])\s*(?P<quote>["'`]?)(?P<secret>[^\s"'`,;:][^\s"'`,;]{7,})"#;

/// Rules with a more specific kind come first: once a secret is replaced, later
/// rules don't match it again.
const RULES: &[Rule] = &[
    Rule {
        kind: "private key",
        needles: &["private key"],
        pattern: r"-----BEGIN[A-Z ]*PRIVATE KEY(?: BLOCK)?-----(?P<secret>.{16,}?)-----END",
        check: Check::Token,
    },
    Rule {
        kind: "AWS access key",
        needles: &["akia", "asia", "abia", "acca"],
        pattern: r"\b(?P<secret>(?:AKIA|ASIA|ABIA|ACCA)[0-9A-Z]{16})\b",
        check: Check::Token,
    },
    Rule {
        kind: "GitHub token",
        needles: &["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"],
        pattern: r"\b(?P<secret>gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})",
        check: Check::Token,
    },
    Rule {
        kind: "GitLab token",
        needles: &["glpat-"],
        pattern: r"\b(?P<secret>glpat-[A-Za-z0-9_-]{20,})",
        check: Check::Token,
    },
    Rule {
        kind: "Slack token",
        needles: &["xox"],
        pattern: r"\b(?P<secret>xox[abposr]-[A-Za-z0-9-]{10,})",
        check: Check::Token,
    },
    Rule {
        kind: "Slack webhook",
        needles: &["hooks.slack.com"],
        pattern: r"hooks\.slack\.com/services/(?P<secret>[A-Za-z0-9/_-]{20,})",
        check: Check::Token,
    },
    Rule {
        kind: "Stripe key",
        needles: &["_live_", "_test_"],
        pattern: r"\b(?P<secret>(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{16,})",
        check: Check::Token,
    },
    Rule {
        kind: "Anthropic API key",
        needles: &["sk-ant-"],
        pattern: r"\b(?P<secret>sk-ant-[A-Za-z0-9_-]{20,})",
        check: Check::Token,
    },
    // OpenAI, OpenRouter, DeepSeek and others use this prefix.
    Rule {
        kind: "API key",
        needles: &["sk-"],
        pattern: r"\b(?P<secret>sk-[A-Za-z0-9_-]{20,})",
        check: Check::Token,
    },
    Rule {
        kind: "Google API key",
        needles: &["aiza"],
        pattern: r"\b(?P<secret>AIza[0-9A-Za-z_-]{35})",
        check: Check::Token,
    },
    Rule {
        kind: "Hugging Face token",
        needles: &["hf_"],
        pattern: r"\b(?P<secret>hf_[A-Za-z0-9]{34,})",
        check: Check::Token,
    },
    Rule {
        kind: "Groq API key",
        needles: &["gsk_"],
        pattern: r"\b(?P<secret>gsk_[A-Za-z0-9]{40,})",
        check: Check::Token,
    },
    Rule {
        kind: "xAI API key",
        needles: &["xai-"],
        pattern: r"\b(?P<secret>xai-[A-Za-z0-9]{40,})",
        check: Check::Token,
    },
    Rule {
        kind: "npm token",
        needles: &["npm_"],
        pattern: r"\b(?P<secret>npm_[A-Za-z0-9]{36})",
        check: Check::Token,
    },
    Rule {
        kind: "PyPI token",
        needles: &["pypi-"],
        pattern: r"\b(?P<secret>pypi-[A-Za-z0-9_-]{50,})",
        check: Check::Token,
    },
    Rule {
        kind: "JSON web token",
        needles: &["eyj"],
        pattern: r"\b(?P<secret>eyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,})",
        check: Check::Token,
    },
    Rule {
        kind: "password in a URL",
        needles: &["://"],
        // The host is part of the match, so placeholders such as example.com are skipped.
        pattern: r#"\b[A-Za-z][A-Za-z0-9+.-]*://[^\s:/@]+:(?P<secret>[^\s/@]+)@[^\s/:?#"'`]+"#,
        check: Check::Token,
    },
    Rule {
        kind: "bearer token",
        needles: &["bearer"],
        pattern: r"(?i)\bbearer\s+(?P<secret>[A-Za-z0-9._~+/-]{20,}=*)",
        check: Check::Value,
    },
    Rule {
        kind: "secret",
        needles: &["password", "passwd", "secret", "token", "key", "credential"],
        pattern: ASSIGNMENT,
        check: Check::Value,
    },
];

static COMPILED: LazyLock<Vec<(&Rule, Regex)>> = LazyLock::new(|| {
    RULES
        .iter()
        .map(|rule| {
            (
                rule,
                Regex::new(rule.pattern).expect("valid secret pattern"),
            )
        })
        .collect()
});

static KEY_BEGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN[A-Z ]*PRIVATE KEY(?: BLOCK)?-----").expect("valid key pattern")
});

/// Replaces the secrets in a diff. Findings name the file and the line.
pub fn diff(diff: &str) -> Redacted {
    let mut scanner = Scanner::default();
    let mut text = String::with_capacity(diff.len());
    let mut findings = Vec::new();
    let mut path = String::new();
    let (mut old_line, mut new_line) = (0, 0);
    let mut in_hunk = false;
    for line in diff.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            path = names::of_diff(line);
            in_hunk = false;
            scanner = Scanner::default();
        } else if line.starts_with("@@") {
            (old_line, new_line) = hunk_start(line);
            in_hunk = true;
            scanner = Scanner::default();
        } else if in_hunk && let Some(marker @ ('+' | '-' | ' ')) = line.chars().next() {
            let (change, number) = match marker {
                '+' => (Change::Added, new_line),
                '-' => (Change::Removed, old_line),
                _ => (Change::Unchanged, new_line),
            };
            if marker != '+' {
                old_line += 1;
            }
            if marker != '-' {
                new_line += 1;
            }
            let mut kinds = Vec::new();
            if let Some(content) = scanner.scan(&line[1..], &mut kinds) {
                text.push(marker);
                text.push_str(&content);
            }
            findings.extend(kinds.into_iter().map(|kind| Finding {
                place: path.clone(),
                line: Some(number),
                change,
                kind,
            }));
            continue;
        }
        text.push_str(line);
    }
    Redacted { text, findings }
}

/// The secrets in a commit's message and patch. Findings name the commit.
pub fn commit(hash: &str, message: &str, patch: &str) -> Vec<Finding> {
    let mut findings = text(message, &format!("commit {hash} message")).findings;
    findings.extend(diff(patch).findings.into_iter().map(|finding| Finding {
        place: format!("commit {hash} {}", finding.place),
        ..finding
    }));
    findings
}

/// Replaces the secrets in plain text, such as a commit message. Findings name
/// the text as `place`.
pub fn text(text: &str, place: &str) -> Redacted {
    let mut scanner = Scanner::default();
    let mut out = String::with_capacity(text.len());
    let mut kinds = Vec::new();
    for line in text.split_inclusive('\n') {
        if let Some(content) = scanner.scan(line, &mut kinds) {
            out.push_str(&content);
        }
    }
    let findings = kinds
        .into_iter()
        .map(|kind| Finding {
            place: place.to_owned(),
            line: None,
            change: Change::Added,
            kind,
        })
        .collect();
    Redacted {
        text: out,
        findings,
    }
}

/// The first old and new line numbers from a "@@ -a,b +c,d @@" hunk header.
fn hunk_start(header: &str) -> (usize, usize) {
    let number = |after: &str| -> usize {
        header
            .split_once(after)
            .map(|(_, rest)| rest)
            .and_then(|rest| {
                let digits = rest
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len());
                rest[..digits].parse().ok()
            })
            .unwrap_or(1)
    };
    (number("@@ -"), number(" +"))
}

/// Remembers whether the lines being scanned are inside a private key block.
#[derive(Default)]
struct Scanner {
    /// After a line that ends with a BEGIN marker, until the key's data ends.
    in_key: bool,
    /// Whether the key's data has been replaced with one [`REDACTED`] line.
    key_replaced: bool,
}

impl Scanner {
    /// The line with its secrets replaced, adding what they look like to
    /// `kinds`. A private key's lines become one [`REDACTED`] line, so `None`
    /// means the line is left out.
    fn scan(&mut self, line: &str, kinds: &mut Vec<String>) -> Option<String> {
        if self.in_key {
            if is_key_data(line) {
                if self.key_replaced {
                    return None;
                }
                self.key_replaced = true;
                kinds.push("private key".to_owned());
                let ending = &line[line.trim_end_matches(['\r', '\n']).len()..];
                return Some(format!("{REDACTED}{ending}"));
            }
            // Encrypted keys have headers such as "Proc-Type: 4,ENCRYPTED" before
            // their data. Anything else, such as the END line, ends the block.
            self.in_key = is_key_header(line);
        }
        let line = replace_secrets(line, kinds);
        if starts_key_block(&line) {
            self.in_key = true;
            self.key_replaced = false;
        }
        Some(line)
    }
}

/// Whether `line` ends with a private key's BEGIN marker, so the key's data
/// follows. Code that only mentions the marker, such as
/// `starts_with("-----BEGIN PRIVATE KEY-----")`, has more after it.
fn starts_key_block(line: &str) -> bool {
    KEY_BEGIN
        .find_iter(line)
        .last()
        .is_some_and(|marker| strip_code(&line[marker.end()..]).is_empty())
}

/// Whether `line` is a line of a key's base64 data, perhaps inside a string literal.
fn is_key_data(line: &str) -> bool {
    let data = strip_code(line);
    !data.is_empty()
        && data
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+/=".contains(&byte))
}

fn is_key_header(line: &str) -> bool {
    let line = strip_code(line);
    line.is_empty()
        || line.split_once(": ").is_some_and(|(name, _)| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

/// `text` without what code puts around a key written into a string literal:
/// quotes, escaped line breaks, commas and concatenation.
fn strip_code(text: &str) -> &str {
    let mut text = text.trim();
    loop {
        let stripped = text
            .trim_matches(|c: char| c.is_whitespace() || "\"'`,;+".contains(c))
            .trim_end_matches('\\')
            .trim_end_matches("\\n")
            .trim_start_matches("\\n");
        if stripped == text {
            return text;
        }
        text = stripped;
    }
}

fn replace_secrets(line: &str, kinds: &mut Vec<String>) -> String {
    let lower = line.to_ascii_lowercase();
    let mut line = line.to_owned();
    for (rule, regex) in COMPILED.iter() {
        if !rule.needles.iter().any(|needle| lower.contains(needle)) {
            continue;
        }
        let found: Vec<_> = regex
            .captures_iter(&line)
            .filter_map(|captures| {
                let secret = captures.name("secret")?;
                let whole = captures.get(0)?.as_str();
                if !plausible(secret.as_str(), whole, rule.check)
                    || is_type_annotation(&captures, &line[secret.end()..])
                {
                    return None;
                }
                let kind = captures.name("name").map_or(rule.kind.to_owned(), |name| {
                    format!("{} value", name.as_str())
                });
                Some((secret.range(), kind))
            })
            .collect();
        // From the end, so the ranges of earlier matches stay valid.
        for (range, _) in found.iter().rev() {
            line.replace_range(range.clone(), REDACTED);
        }
        kinds.extend(found.into_iter().map(|(_, kind)| kind));
    }
    line
}

/// Whether an unquoted value after ":" is code, as in `TOKEN: Kind = Kind::A`,
/// rather than a setting, as in YAML's `password: hunter2abc`: in a setting,
/// nothing but a comment follows the value.
fn is_type_annotation(captures: &regex_lite::Captures, after: &str) -> bool {
    let (Some(op), Some(quote)) = (captures.name("op"), captures.name("quote")) else {
        return false;
    };
    let after = after.trim();
    op.as_str() == ":"
        && quote.as_str().is_empty()
        && !(after.is_empty() || after.starts_with('#') || after.starts_with("//"))
}

/// Whether a match looks like a real secret rather than a placeholder or code.
/// Placeholder words count anywhere in the `whole` match, e.g. in a URL's host.
fn plausible(secret: &str, whole: &str, check: Check) -> bool {
    const PLACEHOLDER_PARTS: &[&str] = &[
        "example",
        "placeholder",
        "changeme",
        "your",
        "xxxx",
        "****",
        "redacted",
        "dummy",
        // Template fields, as in `${TOKEN}`, `{{ token }}` or `postgres://app:{password}@db`.
        "{",
        "<",
    ];
    const PLACEHOLDERS: &[&str] = &["password", "passwd", "pass", "secret", "token", "user"];
    let whole = whole.to_ascii_lowercase();
    if PLACEHOLDER_PARTS.iter().any(|part| whole.contains(part))
        || PLACEHOLDERS.contains(&secret.to_ascii_lowercase().as_str())
    {
        return false;
    }
    let mut chars: Vec<char> = secret.chars().collect();
    chars.sort_unstable();
    chars.dedup();
    if chars.len() < 4 {
        return false;
    }
    match check {
        Check::Token => true,
        Check::Value => {
            secret.bytes().any(|byte| byte.is_ascii_digit())
                && secret.bytes().any(|byte| byte.is_ascii_alphabetic())
                && !secret.contains(['(', ')', '[', ']', '{', '}', '<', '>', '$', '%', '\\'])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A random-looking string of `len` letters and digits, so no real-looking
    /// secret is written out in this file.
    fn noise(len: usize) -> String {
        "q7Rk2LmZ9vXa4TbN8cWp3HdY6sJf5GeU"
            .chars()
            .cycle()
            .take(len)
            .collect()
    }

    fn redacted(line: &str) -> (String, Vec<String>) {
        let result = text(line, "test");
        let kinds = result
            .findings
            .into_iter()
            .map(|finding| finding.kind)
            .collect();
        (result.text, kinds)
    }

    #[test]
    fn replaces_tokens_with_known_prefixes() {
        let cases = [
            (
                format!("AKIA{}", noise(16).to_uppercase()),
                "AWS access key",
            ),
            (format!("ghp_{}", noise(36)), "GitHub token"),
            (format!("github_pat_{}", noise(40)), "GitHub token"),
            (format!("glpat-{}", noise(20)), "GitLab token"),
            (format!("xoxb-{}", noise(24)), "Slack token"),
            (format!("sk_live_{}", noise(24)), "Stripe key"),
            (format!("sk-ant-api03-{}", noise(40)), "Anthropic API key"),
            (format!("sk-proj-{}", noise(40)), "API key"),
            (format!("AIza{}", noise(35)), "Google API key"),
            (format!("hf_{}", noise(34)), "Hugging Face token"),
            (format!("gsk_{}", noise(48)), "Groq API key"),
            (format!("xai-{}", noise(64)), "xAI API key"),
            (format!("npm_{}", noise(36)), "npm token"),
            (format!("pypi-{}", noise(60)), "PyPI token"),
            (
                format!("eyJ{}.eyJ{}.{}", noise(12), noise(20), noise(30)),
                "JSON web token",
            ),
        ];
        for (secret, kind) in cases {
            let (line, kinds) = redacted(&format!("client = Client::new(\"{secret}\");\n"));
            assert_eq!(
                line,
                format!("client = Client::new(\"{REDACTED}\");\n"),
                "{kind}"
            );
            assert_eq!(kinds, [kind]);
        }
    }

    #[test]
    fn replaces_passwords_in_urls_and_bearer_tokens() {
        let url =
            |password: &str| format!("DATABASE_URL=postgres://app:{password}@db.internal/app");
        let (line, kinds) = redacted(&url(&noise(12)));
        assert_eq!(line, url(REDACTED));
        assert_eq!(kinds, ["password in a URL"]);

        let (line, kinds) = redacted(&format!("Authorization: Bearer {}", noise(40)));
        assert_eq!(line, format!("Authorization: Bearer {REDACTED}"));
        assert_eq!(kinds, ["bearer token"]);
    }

    #[test]
    fn replaces_values_assigned_to_secret_names() {
        let value = noise(20);
        for (line, name) in [
            (format!("DB_PASSWORD={value}"), "DB_PASSWORD"),
            (
                format!("  \"client_secret\": \"{value}\","),
                "client_secret",
            ),
            (format!("api_key: '{value}'"), "api_key"),
            (format!("SECRET_KEY = \"{value}\""), "SECRET_KEY"),
            (format!("$token := \"{value}\""), "token"),
        ] {
            let (redacted_line, kinds) = redacted(&line);
            assert_eq!(redacted_line, line.replace(&value, REDACTED));
            assert_eq!(kinds, [format!("{name} value")]);
        }
    }

    #[test]
    fn leaves_placeholders_and_code_alone() {
        for line in [
            "api_key = \"${OPENAI_API_KEY}\"",
            "password = \"changeme123\"",
            "token = get_token(user, 42);",
            "secret: config.client_secret,",
            "max_output_tokens = 16000",
            "tokenizer = \"cl100k_base\"",
            "pub api_key: Option<String>,",
            "postgres://user:password@localhost/app",
            "url = \"https://example.com:8080/path\"",
            "dsn = f\"postgres://{user}:{password}@{host}/app\"",
            &format!("ghp_{}", "x".repeat(36)),
            "AKIAIOSFODNN7EXAMPLE",
            "Authorization: Bearer $TOKEN",
            "let key = \"<your-api-key>\";",
            "//! impls, and [`InvalidToken::to_compile_error2`].",
            "pub const STABLE_SECRET: _bindgen_ty_3 = _bindgen_ty_3::STABLE_SECRET;",
            "let mut tokens = \"tuple.0.0\".parse::<TokenStream>();",
            "let url = Url::parse(\"ftp://user1:secret1@example.com\")?;",
        ] {
            assert_eq!(redacted(line), (line.to_owned(), Vec::new()), "{line}");
        }
    }

    #[test]
    fn reads_unquoted_values_after_a_colon_as_settings_only_at_the_end() {
        let value = noise(12);
        let (line, kinds) = redacted(&format!("  password: {value}  # rotated in May"));
        assert_eq!(line, format!("  password: {REDACTED}  # rotated in May"));
        assert_eq!(kinds, ["password value"]);
        for line in [
            format!("api_token: {value} = Kind::A;"),
            format!("fn check(secret: {value}, other: u8)"),
        ] {
            assert_eq!(redacted(&line), (line.clone(), Vec::new()), "{line}");
        }
    }

    #[test]
    fn code_that_mentions_a_key_marker_is_not_a_key() {
        let code =
            "if pem.starts_with(\"-----BEGIN PRIVATE KEY-----\") {\n    return decode(pem);\n}\n";
        assert_eq!(text(code, "test").text, code);
        let message = "expected '-----BEGIN PRIVATE KEY-----'\\\n    , found something else\n";
        assert_eq!(text(message, "test").text, message);
        assert!(text(message, "test").findings.is_empty());
    }

    /// A key's BEGIN or END line, put together so this file doesn't look like
    /// it holds a key.
    fn marker(word: &str) -> String {
        format!("-----{word} RSA PRIVATE KEY-----")
    }

    #[test]
    fn replaces_keys_written_into_string_literals() {
        let (begin, end, data) = (marker("BEGIN"), marker("END"), noise(64));
        let code =
            format!("const KEY: &str = \"{begin}\\n\\\n{data}\\n\\\n{data}\\n\\\n{end}\";\n");
        let result = text(&code, "test");
        assert_eq!(
            result.text,
            format!("const KEY: &str = \"{begin}\\n\\\n{REDACTED}\n{end}\";\n")
        );
        assert_eq!(result.findings.len(), 1);
    }

    #[test]
    fn keeps_the_headers_of_encrypted_keys() {
        let key = format!(
            "{}\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,{}\n\n{}\n{}\n",
            marker("BEGIN"),
            noise(32).to_uppercase(),
            noise(64),
            marker("END")
        );
        let result = text(&key, "test");
        assert!(result.text.contains("Proc-Type: 4,ENCRYPTED\n"));
        assert!(result.text.contains(&format!("\n{REDACTED}\n-----END")));
        assert!(!result.text.contains(&noise(64)));
    }

    #[test]
    fn leaves_quoted_and_url_examples_alone() {
        for line in [
            "Proxy::http(\"https://user:pass%3Bword@proxy.example.com:8080\")",
            "irc://alex:alex@alex.example.com:6667/foo",
        ] {
            assert_eq!(redacted(line), (line.to_owned(), Vec::new()), "{line}");
        }
    }

    #[test]
    fn names_a_file_with_norwegian_letters_in_a_finding() {
        let secret = format!("ghp_{}", noise(36));
        for header in [
            "diff --git a/Kjøre.txt b/Kjøre.txt\n",
            "diff --git \"a/Kj\\303\\270re.txt\" \"b/Kj\\303\\270re.txt\"\n",
        ] {
            let found = diff(&format!("{header}@@ -1 +1 @@\n+token = {secret}\n"));
            assert_eq!(found.findings[0].place, "Kjøre.txt");
        }
    }

    #[test]
    fn finds_secrets_by_file_and_line_in_a_diff() {
        let token = format!("ghp_{}", noise(36));
        let diff = format!(
            "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n\
             @@ -10,4 +10,4 @@ fn main\n context\n-let old = \"{token}\";\n+let new = \"{token}\";\n \
             let kept = \"{token}\";\n context\ndiff --git a/.env b/.env\n@@ -0,0 +1,2 @@\n\
             +DEBUG=1\n+DB_PASSWORD={}\n",
            noise(16)
        );
        let result = super::diff(&diff);
        assert!(!result.text.contains(&token));
        assert!(result.text.contains("+++ b/src/a.rs\n"));
        let found: Vec<_> = result
            .findings
            .iter()
            .map(|finding| (finding.place.as_str(), finding.line, finding.change))
            .collect();
        assert_eq!(
            found,
            [
                ("src/a.rs", Some(11), Change::Removed),
                ("src/a.rs", Some(11), Change::Added),
                ("src/a.rs", Some(12), Change::Unchanged),
                (".env", Some(2), Change::Added)
            ]
        );
    }

    #[test]
    fn names_the_commit_in_its_findings() {
        let patch = format!(
            "diff --git a/.env b/.env\n@@ -0,0 +1 @@\n+API_TOKEN={}\n",
            noise(20)
        );
        let message = format!("ci: deploy\n\nUses sk-proj-{} for now.", noise(40));
        let found: Vec<_> = commit("1a2b3c4", &message, &patch)
            .into_iter()
            .map(|finding| (finding.place, finding.line, finding.change))
            .collect();
        assert_eq!(
            found,
            [
                ("commit 1a2b3c4 message".to_owned(), None, Change::Added),
                ("commit 1a2b3c4 .env".to_owned(), Some(1), Change::Added)
            ]
        );
    }

    #[test]
    fn replaces_private_key_blocks() {
        let body: String = (0..5).map(|_| format!("+{}\n", noise(64))).collect();
        let diff = format!(
            "diff --git a/id_rsa b/id_rsa\n@@ -0,0 +1,7 @@\n+-----BEGIN OPENSSH PRIVATE KEY-----\n\
             {body}+-----END OPENSSH PRIVATE KEY-----\n"
        );
        let result = super::diff(&diff);
        assert_eq!(
            result.text,
            format!(
                "diff --git a/id_rsa b/id_rsa\n@@ -0,0 +1,7 @@\n+-----BEGIN OPENSSH PRIVATE KEY-----\n\
                 +{REDACTED}\n+-----END OPENSSH PRIVATE KEY-----\n"
            )
        );
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].kind, "private key");
        assert_eq!(result.findings[0].line, Some(2));

        // A key on one line, as in a cloud provider's JSON credentials.
        let line = format!(
            "\"private_key\": \"-----BEGIN PRIVATE KEY-----\\n{}\\n-----END PRIVATE KEY-----\\n\"",
            noise(64)
        );
        let (line, kinds) = redacted(&line);
        assert_eq!(
            line,
            format!(
                "\"private_key\": \"-----BEGIN PRIVATE KEY-----{REDACTED}-----END PRIVATE KEY-----\\n\""
            )
        );
        assert_eq!(kinds, ["private key"]);
    }

    #[test]
    fn names_the_text_in_findings_for_plain_text() {
        let result = text(
            &format!("fix: rotate the key\n\nold key: sk-proj-{}\n", noise(40)),
            "commit 1a2b3c4",
        );
        assert_eq!(
            result.text,
            format!("fix: rotate the key\n\nold key: {REDACTED}\n")
        );
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].place, "commit 1a2b3c4");
        assert_eq!(result.findings[0].line, None);
    }

    #[test]
    fn reads_hunk_headers() {
        assert_eq!(hunk_start("@@ -10,3 +12,4 @@ fn main\n"), (10, 12));
        assert_eq!(hunk_start("@@ -0,0 +1 @@\n"), (0, 1));
    }
}
