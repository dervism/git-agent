//! Finds secrets in text before it is sent to a model, and replaces them with
//! [`REDACTED`].
//!
//! The rules match formats that are almost always secrets: tokens with a known
//! prefix, private key blocks, passwords in URLs, bearer tokens, and values
//! assigned to names such as `password` or `API_KEY`. Placeholders such as
//! `your-api-key` and references such as `${TOKEN}` are left alone, and so are
//! secrets in formats none of the rules know.

use super::finding::{Change, Finding, Place};
use crate::git::CommitId;
use crate::safe_text::without_hidden;
use key_block::{
    KeyBlock, KeyBlockFacts, LongLineFacts, Taken, is_key_data, is_key_header, starts_key_block,
};
use regex_lite::Regex;
use std::cell::OnceCell;
use std::ops::Range;
use std::sync::LazyLock;

mod diff_scan;
mod key_block;

pub use diff_scan::{DiffScan, diff, diff_findings};
pub use key_block::LongLineScan;

pub const REDACTED: &str = "[REDACTED]";

/// The secrets in `commit`'s message and patch. Findings name the commit.
pub fn commit(commit: &CommitId, message: &str, patch: &str) -> Vec<Finding> {
    let mut findings = text(message, &Place::Message(Some(commit.clone()))).findings;
    findings.extend(
        diff_findings(patch)
            .into_iter()
            .map(|finding| finding.in_commit(commit)),
    );
    findings
}

pub struct Redacted {
    pub text: String,
    pub findings: Vec<Finding>,
}

#[derive(Clone, Copy)]
enum Check {
    /// A format that is a secret by itself: only placeholders are skipped.
    Token,
    /// A token, unless it's one of the names OpenSSH gives its algorithms: those end with
    /// `@openssh.com`, and the names for security keys start with `sk-` as an API key does.
    TokenNotOpenSshName,
    /// A value that may be code or a setting: it also needs letters and digits,
    /// and no characters that only code uses.
    Value,
    /// A token as a URL's user name: a value, but not the public key of a Sentry DSN, which
    /// is meant to be public.
    UrlUser,
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

/// The kind of a private key's finding.
const PRIVATE_KEY: &str = "private key";

/// What a line of text that isn't part of a diff does, such as a message's or a file's: it's
/// published as it is, so a secret on it is added.
const PLAIN: Change = Change::Added;

/// Rules with a more specific kind come first: once a secret is replaced, later
/// rules don't match it again.
const RULES: &[Rule] = &[
    Rule {
        kind: PRIVATE_KEY,
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
        pattern: r"\b(?P<secret>sk-[A-Za-z0-9_-]{20,})(?:@openssh\.com\b)?",
        check: Check::TokenNotOpenSshName,
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
        pattern: r#"\b[A-Za-z][A-Za-z0-9+.-]*://[^\s:/@]*:(?P<secret>[^\s/@]+)@[^\s/:?#"'`,;]+"#,
        check: Check::Token,
    },
    // A token as the user name of a web address, as some package registries and GitHub write
    // it: 20 letters and digits, or 32 characters with `-` or `_`, as a UUID has. Names such as
    // `git`, `oauth2` or a team's name are too short or hold other characters.
    Rule {
        kind: "token in a URL",
        needles: &["://"],
        pattern: r#"\b(?:(?:git|registry|sparse)\+)?https?://(?P<secret>[A-Za-z0-9]{20,}|[A-Za-z0-9_-]{32,})(?::x-oauth-basic|:)?@[^\s/:?#"'`,;]+(?::[0-9]+)?(?P<path>/[^\s"'`]*)?"#,
        check: Check::UrlUser,
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

/// A line cleaned of hidden characters and read once for every side that takes it: whether it's
/// a private key's data or a header of a key's, its secrets replaced, and whether it then ends
/// with a key's BEGIN line, each worked out the first time a side needs it, so that a line costs
/// the same however many sides take it.
struct ScannedLine<'l> {
    original: &'l str,
    visible: String,
    data: OnceCell<bool>,
    header: OnceCell<bool>,
    replaced: OnceCell<(String, Vec<String>)>,
    begins_key: OnceCell<bool>,
}

impl<'l> ScannedLine<'l> {
    /// `original`, read. The patterns see it without hidden characters, since a secret with an
    /// invisible character inside matches none.
    fn of(original: &'l str) -> Self {
        Self {
            original,
            visible: without_hidden(original).text,
            data: OnceCell::new(),
            header: OnceCell::new(),
            replaced: OnceCell::new(),
            begins_key: OnceCell::new(),
        }
    }

    /// The line with its secrets replaced, and what they look like.
    fn replaced(&self) -> &(String, Vec<String>) {
        self.replaced.get_or_init(|| {
            let mut kinds = Vec::new();
            (replace_secrets(&self.visible, &mut kinds), kinds)
        })
    }

    /// What a side finds on the line when it takes it as `taken`.
    fn kinds(&self, taken: Taken) -> Vec<String> {
        match taken {
            Taken::MoreOfAKey => Vec::new(),
            Taken::Key => vec![PRIVATE_KEY.to_owned()],
            Taken::Text => self.replaced().1.clone(),
        }
    }

    /// The line as it goes on when taken as `taken`: left out, as its key's one [`REDACTED`]
    /// line, as the visible text where it held a secret, or as it was, so that the model sees its
    /// hidden characters escaped.
    fn text(&self, taken: Taken) -> Option<String> {
        match taken {
            Taken::MoreOfAKey => None,
            Taken::Key => {
                let end = self.visible.trim_end_matches(['\r', '\n']).len();
                Some(format!("{REDACTED}{}", &self.visible[end..]))
            }
            Taken::Text => {
                let (text, kinds) = self.replaced();
                Some(if kinds.is_empty() {
                    self.original.to_owned()
                } else {
                    text.clone()
                })
            }
        }
    }
}

impl KeyBlockFacts for ScannedLine<'_> {
    fn is_data(&self) -> bool {
        *self.data.get_or_init(|| is_key_data(&self.visible))
    }

    fn is_header(&self) -> bool {
        *self.header.get_or_init(|| is_key_header(&self.visible))
    }

    fn begins_key(&self) -> bool {
        *self
            .begins_key
            .get_or_init(|| starts_key_block(&self.replaced().0))
    }
}

/// A line that holds what look like secrets.
#[derive(Debug, Clone, PartialEq)]
pub struct SecretLine {
    /// Its number, counted from 1.
    pub number: usize,
    /// The line, or for a private key every line of its data, since keys of one type can
    /// begin with the same line.
    pub text: String,
    /// What each secret on it looks like.
    pub kinds: Vec<String>,
}

/// The lines of `lines` that hold secrets, scanned with the rules a diff's lines get, a private
/// key's block across lines included.
pub fn secret_lines(lines: impl IntoIterator<Item = String>) -> Vec<SecretLine> {
    let mut scan = LineScan::default();
    lines
        .into_iter()
        .enumerate()
        .fold(Vec::new(), |mut found: Vec<SecretLine>, (index, text)| {
            match scan.line(&text) {
                Held::MoreOfAKey => {
                    if let Some(key) = found.last_mut() {
                        key.text.push('\n');
                        key.text.push_str(&text);
                    }
                }
                Held::Secrets(kinds) => found.push(SecretLine {
                    number: index + 1,
                    text,
                    kinds,
                }),
                Held::Nothing => {}
            }
            found
        })
}

/// What a line holds, as far as secrets go, when lines are scanned one after another.
#[derive(Debug, PartialEq)]
pub enum Held {
    /// What the secrets on it look like.
    Secrets(Vec<String>),
    /// More of the data of a private key, which belongs to the line the key's data began on.
    MoreOfAKey,
    Nothing,
}

/// Scans lines one after another, as [`secret_lines`] does, carrying a private key's block
/// across them.
#[derive(Default)]
pub struct LineScan(KeyBlock);

impl LineScan {
    /// What `line`, the next line, holds.
    pub fn line(&mut self, line: &str) -> Held {
        let line = ScannedLine::of(line);
        match self.0.take(&line, PLAIN) {
            Taken::MoreOfAKey => Held::MoreOfAKey,
            taken => Held::of(line.kinds(taken)),
        }
    }

    /// Starts on the next line, too long to scan whole: it's to be scanned in windows, all its
    /// pieces fed to the [`LongLineScan`], and given to [`LineScan::end_long_line`].
    pub fn start_long_line(&self) -> LongLineScan {
        self.0.start_long_line()
    }

    /// What the long line `line` holds, now that its `windows` have been scanned.
    pub fn end_long_line(&mut self, line: LongLineScan, windows: Windows) -> Held {
        let facts = LongLineFacts::of(line, windows.ends_with_begin);
        let taken = self.0.take(&facts, PLAIN);
        windows.kinds_for(taken).map_or(Held::MoreOfAKey, Held::of)
    }
}

impl Held {
    /// What a line holds that isn't more of a key: the secrets of these `kinds`, or nothing.
    fn of(kinds: Vec<String>) -> Self {
        if kinds.is_empty() {
            Self::Nothing
        } else {
            Self::Secrets(kinds)
        }
    }
}

/// What the windows of a line too long to scan whole found: what its secrets look like, and
/// whether, with them replaced, the line ends with a private key's BEGIN line, as its last window
/// tells.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Windows {
    pub kinds: Vec<String>,
    pub ends_with_begin: bool,
}

impl Windows {
    /// What the long line these windows were scanned from holds on a side that takes it as
    /// `taken`: nothing more (`None`) as more of a key's data, the key, or what they found.
    fn kinds_for(self, taken: Taken) -> Option<Vec<String>> {
        match taken {
            Taken::MoreOfAKey => None,
            Taken::Key => Some(vec![PRIVATE_KEY.to_owned()]),
            Taken::Text => Some(self.kinds),
        }
    }
}

/// What `text`, a window of a long line, holds when it's scanned as a line on its own.
pub fn scan_window(text: &str) -> Windows {
    let line = ScannedLine::of(text);
    let mut block = KeyBlock::default();
    let taken = block.take(&line, PLAIN);
    Windows {
        kinds: line.kinds(taken),
        ends_with_begin: block.in_key(),
    }
}

/// What the secrets in `text`, scanned as a line on its own, look like.
pub fn kinds_in(text: &str) -> Vec<String> {
    let line = ScannedLine::of(text);
    let taken = KeyBlock::default().take(&line, PLAIN);
    line.kinds(taken)
}

/// Replaces the secrets in plain text, such as a commit message. Findings are at `place`.
pub fn text(text: &str, place: &Place) -> Redacted {
    let mut block = KeyBlock::default();
    let mut out = String::with_capacity(text.len());
    let mut kinds = Vec::new();
    for line in text.split_inclusive('\n') {
        let line = ScannedLine::of(line);
        let taken = block.take(&line, PLAIN);
        kinds.extend(line.kinds(taken));
        if let Some(content) = line.text(taken) {
            out.push_str(&content);
        }
    }
    let findings = kinds
        .into_iter()
        .map(|kind| Finding {
            place: place.clone(),
            line: None,
            change: PLAIN,
            kind,
        })
        .collect();
    Redacted {
        text: out,
        findings,
    }
}

fn replace_secrets(line: &str, kinds: &mut Vec<String>) -> String {
    let lower = line.to_ascii_lowercase();
    let mut line = line.to_owned();
    for (rule, regex) in COMPILED.iter() {
        if !rule.needles.iter().any(|needle| lower.contains(needle)) {
            continue;
        }
        let found = secrets_found(rule, regex, &line);
        // From the end, so the ranges of earlier matches stay valid.
        for (range, _) in found.iter().rev() {
            line.replace_range(range.clone(), REDACTED);
        }
        kinds.extend(found.into_iter().map(|(_, kind)| kind));
    }
    line
}

/// The secrets `rule` finds in `line`, with their kinds. A match that isn't a secret is searched
/// again from the start of its value, so that it can't hide one inside it, as a package's URL can
/// in its query; a few times per line, so that a crafted line can't cost time in the square of its
/// length.
fn secrets_found(rule: &Rule, regex: &Regex, line: &str) -> Vec<(Range<usize>, String)> {
    const MOST_RESTARTS: usize = 8;
    let mut found = Vec::new();
    let (mut start, mut restarts) = (0, 0);
    while let Some(captures) = regex.captures_at(line, start) {
        let (Some(whole), Some(secret)) = (captures.get(0), captures.name("secret")) else {
            break;
        };
        let path = captures.name("path").map_or("", |path| path.as_str());
        let judged = &line[whole.start()..whole.end() - path.len()];
        if plausible(secret.as_str(), judged, path, rule.check)
            && !is_type_annotation(&captures, &line[secret.end()..])
        {
            let kind = captures.name("name").map_or(rule.kind.to_owned(), |name| {
                format!("{} value", name.as_str())
            });
            found.push((secret.range(), kind));
            start = whole.end();
        } else if restarts < MOST_RESTARTS {
            restarts += 1;
            start = restart_in(line, whole.start(), secret.start());
        } else {
            start = whole.end();
        }
    }
    found
}

/// Where to search again inside a match at `whole` that isn't a secret: from its `secret`, or
/// from the next character if the secret starts the match.
fn restart_in(line: &str, whole: usize, secret: usize) -> usize {
    if secret > whole {
        secret
    } else {
        next_char(line, whole)
    }
}

/// Where the character after the one at `at` starts.
fn next_char(text: &str, at: usize) -> usize {
    text[at..]
        .chars()
        .next()
        .map_or(text.len(), |c| at + c.len_utf8())
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
/// Placeholder words count anywhere in the `whole` match, e.g. in a URL's host, but not in a
/// URL's `path`, which can name a repository such as `example-app`.
fn plausible(secret: &str, whole: &str, path: &str, check: Check) -> bool {
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
    // Also with the `]` that ends an optional part in usage text, as in `user[:pass]@host`.
    const PLACEHOLDERS: &[&str] = &[
        "password",
        "passwd",
        "pass",
        "secret",
        "token",
        "user",
        // GitHub's password for a token given as the user name.
        "x-oauth-basic",
    ];
    let whole = whole.to_ascii_lowercase();
    if PLACEHOLDER_PARTS.iter().any(|part| whole.contains(part))
        || PLACEHOLDERS.contains(&secret.trim_end_matches(']').to_ascii_lowercase().as_str())
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
        Check::TokenNotOpenSshName => !whole.ends_with("@openssh.com"),
        Check::Value => is_value(secret),
        Check::UrlUser => is_value(secret) && !is_public_dsn(secret, path),
    }
}

/// Whether `secret` could be a setting's value rather than code or a package's name: it has
/// letters and digits, no characters that only code uses, names no package, and isn't the rest of
/// an Objective-C selector.
fn is_value(secret: &str) -> bool {
    secret.bytes().any(|byte| byte.is_ascii_digit())
        && secret.bytes().any(|byte| byte.is_ascii_alphabetic())
        && !secret.contains(['(', ')', '[', ']', '{', '}', '<', '>', '$', '%', '\\'])
        && !is_selector_parts(secret)
        && !is_package_reference(secret)
}

/// Whether `value` is the rest of an Objective-C selector: two or more names, each ending with
/// `:`, as in `notifyIdMS:isDBRv2:reply:`. One name with a `:` after it can be a value.
fn is_selector_parts(value: &str) -> bool {
    let Some(names) = value.strip_suffix(':') else {
        return false;
    };
    let names: Vec<&str> = names.split(':').collect();
    names.len() >= 2 && names.iter().all(|name| is_identifier(name))
}

/// Whether `text` is a name as code writes one: a letter or `_`, then letters, digits and `_`.
fn is_identifier(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Whether a token in a URL is a Sentry DSN's public key: 32 hex digits, with a project number
/// at the end of the URL's `path`.
fn is_public_dsn(token: &str, path: &str) -> bool {
    let project = path.rsplit_once('/').map_or("", |(_, last)| last);
    token.len() == 32
        && token.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !project.is_empty()
        && project.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether `value` names a package, as lockfiles write one, rather than holds a secret: a
/// specifier such as `npm:^9.0.0` or `github:owner/repo`, or a version such as `4.0.0-beta.1`.
fn is_package_reference(value: &str) -> bool {
    const SPECIFIERS: &[&str] = &[
        "npm:",
        "workspace:",
        "github:",
        "git+",
        "file:",
        "link:",
        "patch:",
        "portal:",
        "jsr:",
        "gitlab:",
        "bitbucket:",
        "gist:",
    ];
    SPECIFIERS
        .iter()
        .any(|specifier| value.starts_with(specifier))
        || is_version(value)
}

/// Whether `value` is a semantic version, with any prerelease tag or build after it, as
/// `4.0.0-beta.1` or `^1.2.0-rc.1`.
fn is_version(value: &str) -> bool {
    let value = value.trim_start_matches(['^', '~', '=', 'v']);
    let core = value.split(['-', '+']).next().unwrap_or_default();
    let numbers: Vec<&str> = core.split('.').collect();
    numbers.len() == 3
        && numbers
            .iter()
            .all(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::test_support::noise;

    #[test]
    fn finds_a_secret_split_by_an_invisible_character() {
        let key = crate::secrets::test_support::aws_key();
        let hidden = format!("key = \"{}\u{200b}{}\"\n", &key[..8], &key[8..]);
        let redacted = text(&hidden, &Place::text("message"));
        assert_eq!(redacted.findings.len(), 1, "{:?}", redacted.findings);
        assert!(!redacted.text.contains("AKIA"), "{}", redacted.text);
        let patch = format!("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -0,0 +1 @@\n+{hidden}");
        assert_eq!(diff(&patch).findings.len(), 1);
    }

    #[test]
    fn leaves_hidden_characters_in_a_line_without_a_secret_for_the_model_to_see() {
        let line = "+let ok = \"admin\u{202e} \u{2066}// check\u{2069}\u{2066}\";\u{1b}[2J\r\n";
        let patch = format!("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -0,0 +1 @@\n{line}");
        let redacted = diff(&patch);
        assert!(redacted.findings.is_empty());
        assert_eq!(redacted.text, patch);
        assert_eq!(text(&line[1..], &Place::text("message")).text, &line[1..]);
    }

    /// OpenSSH's names for two of its security key algorithms, and usage text with a password
    /// one may leave out. Each is put together from two parts, so that a git-agent older than
    /// 0.12.0, whose check takes them for secrets, still commits this file.
    const SK_ECDSA_CERT: &str = concat!("sk", "-ecdsa-sha2-nistp256-cert-v01@openssh.com");
    const SK_ED25519_CERT: &str = concat!("sk", "-ssh-ed25519-cert-v01@openssh.com");
    const AFP_USAGE: &str = concat!("afp:", "//user[:pass]@host");

    fn redacted(line: &str) -> (String, Vec<String>) {
        let result = text(line, &Place::text("test"));
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

    /// Checks that each line is found once as `kind`, and its secret replaced.
    fn assert_found(lines: &[String], kind: &str) {
        for line in lines {
            let (text, kinds) = redacted(line);
            assert_eq!(kinds, [kind], "{line}");
            assert!(text.contains(REDACTED), "{text}");
        }
    }

    #[test]
    fn finds_passwords_in_lockfile_urls() {
        let password = noise(16);
        let lines = [
            format!(
                r#""resolved": "https://ci-bot:{password}@npm.corp.internal/a/-/a-1.0.0.tgz","#
            ),
            format!(
                r#"source = "git+https://deploy:{password}@git.corp.internal/x.git?branch=main#3f2a1b9""#
            ),
            format!(r#"source = "registry+https://ci:{password}@crates.corp.internal/index""#),
            format!(r#"source = "sparse+https://ci:{password}@crates.corp.internal/index/""#),
        ];
        assert_found(&lines, "password in a URL");
    }

    #[test]
    fn finds_tokens_in_lockfile_urls() {
        let token = noise(24);
        let hex = "3f9a1c7e5b2d8f4a6c0e9b1d7f3a5c8e2b4d6f80";
        let as_user = [
            format!("  remote: https://{token}@gems.corp.internal/acme/"),
            format!("  remote: https://{token}:@gems.corp.internal/acme/"),
        ];
        assert_found(&as_user, "token in a URL");
        let in_query = format!(
            r#""resolved": "https://gitlab.corp.internal/api/v4/projects/7/packages/npm/a.tgz?private_token={token}""#
        );
        assert_found(&[in_query], "private_token value");
        let in_query =
            format!(r#""resolved": "https://npm.corp.internal/a.tgz?v=1&access_token={hex}""#);
        assert_found(&[in_query], "access_token value");
    }

    const HEX_32: &str = "3f9a1c7e5b2d8f4a6c0e9b1d7f3a5c8e";
    const UUID: &str = "3f9a1c7e-5b2d-8f4a-6c0e-9b1d7f3a5c8e";

    #[test]
    fn finds_tokens_as_url_user_names() {
        let hex = format!("{HEX_32}5b2d8f40");
        let lines = [
            format!(r#"source = "git+https://{hex}@git.corp.internal/x.git""#),
            format!("https://{UUID}@registry.corp.internal/"),
            format!("https://{HEX_32}@files.corp.internal/releases"),
            format!(r#"source = "git+https://{hex}@github.com/acme/example-app.git""#),
        ];
        assert_found(&lines, "token in a URL");
        let oauth = format!("https://{hex}:x-oauth-basic@github.com/acme/x.git");
        let (text, kinds) = redacted(&oauth);
        assert_eq!(kinds, ["token in a URL"], "{oauth}");
        assert!(
            !text.contains(&hex) && text.contains("x-oauth-basic"),
            "{text}"
        );
    }

    #[test]
    fn finds_a_password_with_an_empty_user_name() {
        let line = format!("https://:{}@git.heroku.com/app.git", noise(24));
        assert_found(&[line], "password in a URL");
    }

    /// A match that isn't a secret, such as a package reference, may still hold one.
    #[test]
    fn finds_a_secret_inside_a_package_reference() {
        let line = format!(
            r#""auth-token": "git+https://gitlab.corp.internal/x.git?private_token={}#v1""#,
            noise(24)
        );
        assert_found(&[line], "private_token value");
    }

    /// Searching again inside a match that isn't a secret has a limit, so a crafted line costs
    /// time in proportion to its length.
    #[test]
    fn scans_a_long_line_of_refused_values() {
        let line = "token=abcdefgh ".repeat(14_000);
        let (_, kinds) = redacted(&line);
        assert!(kinds.is_empty(), "{}", kinds.len());
    }

    /// A secret behind a semantic version is missed: versions with prerelease tags are far more
    /// common in lockfiles than secrets shaped like them.
    #[test]
    fn takes_a_version_shaped_value_for_a_version() {
        let (_, kinds) = redacted(&format!("password: 1.2.3-{}", noise(12)));
        assert!(kinds.is_empty(), "{kinds:?}");
    }

    #[test]
    fn leaves_public_keys_and_names_in_urls_alone() {
        for line in [
            format!(r#"Sentry.init({{ dsn: "https://{HEX_32}@o450123.ingest.sentry.io/5501234" }})"#),
            format!("SENTRY_DSN=https://{HEX_32}@sentry.corp.internal/2"),
            format!("SENTRY_DSN=https://{HEX_32}@sentry.corp.internal:9000/2"),
            format!("https://{}@registry.example.com/", noise(24)),
            "git clone https://contoso-platform-team-01@dev.azure.com/contoso-platform-team-01/Web/_git/web".to_owned(),
            "git clone https://jsmith-consulting-2019@bitbucket.org/acme/repo.git".to_owned(),
            "DATABASE_URL=postgres://analytics_readonly_2023@db.internal/warehouse".to_owned(),
            "DATABASE_URL=postgres://analyticsreadonly2023@db.internal/warehouse".to_owned(),
            "url = sftp://deploy-production-01@files.corp.internal/releases".to_owned(),
            r#"    "auth-token": "jsr:^1.0.0","#.to_owned(),
            r#"    "auth-token": "gitlab:acme/auth-token#v1.0.0","#.to_owned(),
        ] {
            let (_, kinds) = redacted(&line);
            assert!(kinds.is_empty(), "{line}: {kinds:?}");
        }
    }

    #[test]
    fn leaves_lockfile_lines_without_credentials_alone() {
        for line in [
            r#"  jsonwebtoken: "npm:^9.0.0""#,
            r#"    "@octokit/auth-token": "npm:^4.0.0","#,
            r#"    "jsonwebtoken": "github:auth0/node-jsonwebtoken#v9","#,
            "  '@octokit/auth-token': 4.0.0-beta.1",
            r#"    "auth-token": "workspace:^1.2.0-rc.1","#,
            r#"    "auth-token": "file:../auth-token-2","#,
            r#"    "auth-token": "patch:auth-token@npm%3A1.0.0#./fix.patch","#,
            r#"      "integrity": "sha512-3aWv1O3QW/LPPmbUnmEtmXp1jg2SHp+8CRS7IuwRWp6r6b4WjUBQSNfpQyBg3S8tLkP6CEuFjFn5Mbhr6uT8EA==","#,
            r#"checksum = "8f2d6b7cbbd4d0f8e0b3c5e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1""#,
            "golang.org/x/net v0.23.0 h1:Ab3dEf6hIj9kLm2nOp5qRs8tUv1wXy4zAb7dEf0hIj3=",
            r#"sdist = { url = "https://files.pythonhosted.org/packages/a-1.0.tar.gz", hash = "sha256:6b5d6e8f0a1c3e5f7b9d1f3a5c7e9b1d3f5a7c9e1b3d5f7a9c1e3b5d7f9a1c3e" }"#,
            r#"      "resolved": "https://registry.npmjs.org:443/a/-/a-1.0.0.tgz","#,
            r#"source = "git+ssh://git@github.com:acme/x.git#3f2a1b9c""#,
            "  remote: https://user:pass@registry.example.com/",
            "  remote: https://rubygems.org/",
        ] {
            let (_, kinds) = redacted(line);
            assert!(kinds.is_empty(), "{line}: {kinds:?}");
        }
    }

    #[test]
    fn takes_only_long_tokens_with_letters_and_digits_as_a_user_name() {
        for user in [
            "git",
            "oauth2",
            "x-access-token",
            "__token__",
            "build-agent-for-the-team",
            "12345678901234567890",
        ] {
            let (_, kinds) = redacted(&format!("https://{user}@gems.corp.internal/acme/"));
            assert!(kinds.is_empty(), "{user}: {kinds:?}");
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
        assert_eq!(text(code, &Place::text("test")).text, code);
        let message = "expected '-----BEGIN PRIVATE KEY-----'\\\n    , found something else\n";
        assert_eq!(text(message, &Place::text("test")).text, message);
        assert!(text(message, &Place::text("test")).findings.is_empty());
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
        let result = text(&code, &Place::text("test"));
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
        let result = text(&key, &Place::text("test"));
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

    /// A key's finding holds all of its data, since keys of one type can begin alike.
    #[test]
    fn keeps_a_private_keys_whole_data_with_its_line() {
        let lines = [
            "x = 1".to_owned(),
            "-----BEGIN OPENSSH PRIVATE KEY-----".to_owned(),
            "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9u".to_owned(),
            "A".repeat(64),
            "-----END OPENSSH PRIVATE KEY-----".to_owned(),
            format!("token = ghp_{}", noise(36)),
        ];

        let found = secret_lines(lines.clone());

        let numbers: Vec<usize> = found.iter().map(|line| line.number).collect();
        assert_eq!(numbers, [3, 6]);
        assert_eq!(found[0].text, format!("{}\n{}", lines[2], lines[3]));
        assert_eq!(found[0].kinds, ["private key"]);
        assert_eq!(found[1].text, lines[5]);
        assert!(secret_lines(Vec::new()).is_empty());
    }

    #[test]
    fn names_the_text_in_findings_for_plain_text() {
        let result = text(
            &format!("fix: rotate the key\n\nold key: sk-proj-{}\n", noise(40)),
            &Place::Message(Some(CommitId::of("1a2b3c4"))),
        );
        assert_eq!(
            result.text,
            format!("fix: rotate the key\n\nold key: {REDACTED}\n")
        );
        assert_eq!(result.findings.len(), 1);
        assert_eq!(
            result.findings[0].place.to_string(),
            "commit 1a2b3c4 message"
        );
        assert_eq!(result.findings[0].line, None);
    }

    /// Text that binaries hold and that isn't a secret: OpenSSH's names for its algorithms, usage
    /// text with a password one may leave out, and the parts of an Objective-C selector.
    #[test]
    fn leaves_names_usage_and_selectors_alone() {
        for line in [
            SK_ECDSA_CERT.to_string(),
            format!("webauthn-{SK_ECDSA_CERT}"),
            format!("ssh-ed25519-cert-v01@openssh.com,{SK_ED25519_CERT}"),
            format!("usage: mount_afp {AFP_USAGE}/volume"),
            "reset:idmsTargetContext:idmsCuttlefishPassword:notifyIdMS:isDBRv2:accountIsW:reply:"
                .to_string(),
        ] {
            let (kept, kinds) = redacted(&line);
            assert!(kinds.is_empty(), "{line}: {kinds:?}");
            assert_eq!(kept, line);
        }
    }

    /// The same rules still find the secrets beside such text, and those the skips must not take
    /// for it: a key before an `@` that isn't OpenSSH's, a password with brackets in it or before
    /// an IPv6 host, and a value with one `:` after it.
    #[test]
    fn finds_keys_passwords_and_values_beside_names_and_selectors() {
        let key = format!("sk-proj-{}", noise(40));
        let password = noise(16);
        let cases = [
            (format!("OPENAI_API_KEY={key}"), "API key"),
            (format!("{SK_ED25519_CERT},{key}"), "API key"),
            (format!("{key},{SK_ECDSA_CERT}"), "API key"),
            (format!("{key}@corp.net"), "API key"),
            (format!("wss://{key}@stream.corp.net"), "API key"),
            (
                format!("afp://user:{password}@files.corp.net/share"),
                "password in a URL",
            ),
            (
                format!("smb://admin[:{password}]@files.corp.net"),
                "password in a URL",
            ),
            (
                format!("postgres://app:{password}@[::1]:5432/db"),
                "password in a URL",
            ),
            (
                format!("postgres://app:Xy7[:{password}@db.corp.net"),
                "password in a URL",
            ),
            (format!("password: {password}"), "password value"),
            (format!("token={password}: unauthorized"), "token value"),
            (format!("token={}:{}", noise(12), noise(12)), "token value"),
        ];
        for (line, kind) in cases {
            let (_, kinds) = redacted(&line);
            assert_eq!(kinds, [kind], "{line}");
        }
    }

    /// Usage text beside a real address, or a list of addresses, as a failover broker's setting
    /// writes them, leaves none of the passwords out.
    #[test]
    fn finds_every_password_in_a_line_of_addresses() {
        let password = noise(16);
        let usage = format!("{AFP_USAGE},").repeat(10);
        for line in [
            format!("{usage}afp://user:{password}@files.corp.net"),
            format!("amqp://app:{password}@mq1.corp.net;amqp://app:{password}@mq2.corp.net"),
            format!("amqp://app:{password}@mq1.corp.net,amqp://app:{password}@mq2.corp.net"),
        ] {
            let (kept, kinds) = redacted(&line);
            assert!(!kept.contains(&password), "{kept}");
            assert!(!kinds.is_empty(), "{line}");
        }
    }

    /// Takes a long line, of `pieces`, whose windows found `kinds`, the way the line readers do.
    fn long(scan: &mut LineScan, pieces: &[&str], kinds: &[&str]) -> Held {
        let mut line = scan.start_long_line();
        pieces.iter().for_each(|piece| line.feed(piece.as_bytes()));
        let windows = Windows {
            kinds: kinds.iter().map(ToString::to_string).collect(),
            ends_with_begin: scan_window(pieces[pieces.len() - 1]).ends_with_begin,
        };
        scan.end_long_line(line, windows)
    }

    /// In a private key's block, a long line all of key data is the key where its data starts,
    /// and holds nothing more after that, whatever its windows found; one that isn't holds what
    /// its windows found and ends the block; and a block begins after a long line whose end is a
    /// BEGIN line.
    #[test]
    fn takes_a_long_line_in_a_key_block_by_all_of_it() {
        let begin = "-----BEGIN OPENSSH PRIVATE KEY-----";
        let found = |kind: &str| Held::Secrets(vec![kind.to_owned()]);
        let mut scan = LineScan::default();
        scan.line(begin);
        assert_eq!(
            long(&mut scan, &["AAAA", "BBBB"], &["x"]),
            found("private key")
        );
        assert_eq!(long(&mut scan, &["CCCC", "DDDD"], &["x"]), Held::MoreOfAKey);
        assert_eq!(long(&mut scan, &["EEEE", " ghp"], &["y"]), found("y"));
        assert_eq!(scan.line("FFFF"), Held::Nothing, "the block ended");

        let mut scan = LineScan::default();
        assert_eq!(long(&mut scan, &["AAAA", "BBBB"], &[]), Held::Nothing);
        assert_eq!(long(&mut scan, &["x", begin], &[]), Held::Nothing);
        assert_eq!(scan.line("AAAA"), found("private key"), "a block begins");
    }

    /// What a key's block holds of a long line of `pieces`, after the lines `before`, and of the
    /// line `next` after it.
    fn in_block(before: &[&str], pieces: &[&str], next: &str) -> (Held, Held) {
        let mut scan = LineScan::default();
        scan.line("-----BEGIN OPENSSH PRIVATE KEY-----");
        for line in before {
            scan.line(line);
        }
        let held = long(&mut scan, pieces, &[]);
        (held, scan.line(next))
    }

    /// All of a long line in a key's block tells whether it's a header of the key's, which keeps
    /// the block open: one with a name and a value, one whose name is long, and not one of
    /// spaces with text after them.
    #[test]
    fn tells_a_long_header_line_by_all_of_it() {
        let key = || Held::Secrets(vec!["private key".to_owned()]);

        let header = in_block(&[], &["Comment: xx", "x x"], "AAAA");
        let long_name = in_block(&[], &["AAAAAA", "AA: v"], "BBBB");
        let spaces = in_block(&["AAAA"], &["      ", "  x y"], "BBBB");

        assert_eq!(header, (Held::Nothing, key()));
        assert_eq!(long_name, (Held::Nothing, key()));
        assert_eq!(spaces, (Held::Nothing, Held::Nothing));
    }

    /// A key on one line, as in a cloud provider's JSON credentials, is replaced whole.
    #[test]
    fn replaces_a_key_written_on_one_line() {
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
    fn names_the_commit_in_its_findings() {
        let patch = format!(
            "diff --git a/.env b/.env\n@@ -0,0 +1 @@\n+API_TOKEN={}\n",
            noise(20)
        );
        let message = format!("ci: deploy\n\nUses sk-proj-{} for now.", noise(40));
        let found: Vec<_> = commit(&CommitId::of("1a2b3c4"), &message, &patch)
            .into_iter()
            .map(|finding| (finding.place.to_string(), finding.line, finding.change))
            .collect();
        assert_eq!(
            found,
            [
                ("commit 1a2b3c4 message".to_owned(), None, Change::Added),
                ("commit 1a2b3c4 .env".to_owned(), Some(1), Change::Added)
            ]
        );
    }
}
