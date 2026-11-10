//! Finds secrets in text before it is sent to a model, and replaces them with
//! [`REDACTED`].
//!
//! The rules match formats that are almost always secrets: tokens with a known
//! prefix, private key blocks, passwords in URLs, bearer tokens, and values
//! assigned to names such as `password` or `API_KEY`. Placeholders such as
//! `your-api-key` and references such as `${TOKEN}` are left alone, and so are
//! secrets in formats none of the rules know.

use crate::git::diff::is_file_header;
use crate::git::names;
use crate::safe_text::without_hidden;
use regex_lite::Regex;
use std::ops::Range;
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

static KEY_BEGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN[A-Z ]*PRIVATE KEY(?: BLOCK)?-----").expect("valid key pattern")
});

/// Replaces the secrets in a diff. Findings name the file and the line.
pub fn diff(diff: &str) -> Redacted {
    let mut text = String::with_capacity(diff.len());
    let findings = scan_diff(diff, |part| text.push_str(part));
    Redacted { text, findings }
}

/// The secrets in a diff, as [`diff`] finds them, without a copy of the diff to send.
pub fn diff_findings(diff: &str) -> Vec<Finding> {
    scan_diff(diff, |_| {})
}

/// Finds the secrets in a diff, and gives `keep` the diff in order with them replaced.
fn scan_diff(diff: &str, mut keep: impl FnMut(&str)) -> Vec<Finding> {
    let mut scan = DiffScan::default();
    for line in diff.split_inclusive('\n') {
        match scan.line(line) {
            Kept::Outside(line) => keep(line),
            Kept::Scanned { markers, content } => {
                keep(markers);
                keep(&content);
            }
            Kept::Dropped => {}
        }
    }
    scan.findings()
}

/// A line of a diff as scanning it leaves it.
pub enum Kept<'l> {
    /// A line outside the hunks, as it was.
    Outside(&'l str),
    /// A hunk's line: its markers, and its content with its secrets replaced.
    Scanned { markers: &'l str, content: String },
    /// A line of a private key's data that the key's one [`REDACTED`] line already stands for.
    Dropped,
}

/// A hunk's line too long to scan whole, from its start until its windows have been scanned:
/// how many bytes its markers take, where it is, and what it is to a private key's block.
pub struct LongText {
    pub markers: usize,
    change: Change,
    number: usize,
    line: LongLineScan,
}

impl LongText {
    /// Takes the next piece of the line, after its markers.
    pub fn feed(&mut self, piece: &[u8]) {
        self.line.feed(piece);
    }
}

/// Scans a diff's lines one after another, keeping track of the file, the hunk and a private
/// key's block they are in, and of what they hold.
#[derive(Default)]
pub struct DiffScan {
    scanner: Scanner,
    path: String,
    hunk: Option<Hunk>,
    findings: Vec<Finding>,
}

impl DiffScan {
    /// Scans `line`, the next line of the diff, with its line end.
    pub fn line<'l>(&mut self, line: &'l str) -> Kept<'l> {
        if is_file_header(line) {
            self.path = names::of_diff(line);
            self.hunk = None;
            self.scanner = Scanner::default();
        } else if line.starts_with("@@") {
            self.hunk = Some(Hunk::starting(line));
            self.scanner = Scanner::default();
        } else if let Some((markers, change, number)) = self.take(line) {
            let mut kinds = Vec::new();
            let content = self.scanner.scan(&line[markers.len()..], &mut kinds);
            self.found(change, number, kinds);
            return content.map_or(Kept::Dropped, |content| Kept::Scanned { markers, content });
        }
        Kept::Outside(line)
    }

    /// Takes the next line, too long to scan whole, which starts with `start`. A hunk's line
    /// comes back to be scanned in windows from after its markers, fed to its [`LongText`], and
    /// given to [`DiffScan::long_text`]; any other line needs nothing more than its start.
    pub fn long_line(&mut self, start: &str) -> Option<LongText> {
        let Some((markers, change, number)) = self.take(start) else {
            self.line(start);
            return None;
        };
        Some(LongText {
            markers: markers.len(),
            change,
            number,
            line: self.scanner.start_long_line(&start[markers.len()..]),
        })
    }

    /// Takes what the windows of `text` hold: the secrets that look like `kinds`, and the line's
    /// end, `tail`, after which a private key's block may begin.
    pub fn long_text(&mut self, text: LongText, kinds: Vec<String>, tail: &str) {
        if let Some(kinds) = self.scanner.end_long_line(text.line, kinds, tail) {
            self.found(text.change, text.number, kinds);
        }
    }

    /// The findings so far.
    pub fn findings(self) -> Vec<Finding> {
        self.findings
    }

    /// The markers of `line`, if it's a line of the current hunk, with what the line does and
    /// its number. Moves the hunk past it.
    fn take<'l>(&mut self, line: &'l str) -> Option<(&'l str, Change, usize)> {
        let hunk = self.hunk.as_mut()?;
        let markers = hunk.markers(line)?;
        let (change, number) = hunk.take(markers.as_bytes());
        Some((markers, change, number))
    }

    fn found(&mut self, change: Change, number: usize, kinds: Vec<String>) {
        self.findings.extend(kinds.into_iter().map(|kind| Finding {
            place: self.path.clone(),
            line: Some(number),
            change,
            kind,
        }));
    }
}

/// Where a hunk has got to: the number of the next line in each parent and in the result. A
/// plain diff has one parent, and a merge's combined diff one per parent, each with a column of
/// markers: `-` for a line that parent has and the result doesn't, `+` for a line the result has
/// and that parent doesn't, and a space for one both have, or neither in a removed line.
struct Hunk {
    parents: Vec<usize>,
    result: usize,
}

impl Hunk {
    /// The hunk a header such as `@@ -10,4 +10,5 @@`, or `@@@ -1,3 -1,3 +1,4 @@@` for a merge
    /// of two, starts.
    fn starting(header: &str) -> Self {
        let signs = header.bytes().take_while(|byte| *byte == b'@').count();
        let parents = signs.saturating_sub(1).max(1);
        let mut words = header.split_whitespace().skip(1);
        let start = |word: &str| -> usize {
            word[1..]
                .split(',')
                .next()
                .and_then(|number| number.parse().ok())
                .unwrap_or(1)
        };
        let mut starts: Vec<usize> = words
            .by_ref()
            .take_while(|word| word.starts_with('-'))
            .map(start)
            .collect();
        starts.resize(parents, 1);
        let result = header
            .split_whitespace()
            .find(|word| word.starts_with('+'))
            .map_or(1, start);
        Self {
            parents: starts,
            result,
        }
    }

    /// The marker columns that start `line`, if it's a line of this hunk.
    fn markers<'a>(&self, line: &'a str) -> Option<&'a str> {
        let markers = line.get(..self.parents.len())?;
        markers
            .bytes()
            .all(|marker| matches!(marker, b'+' | b'-' | b' '))
            .then_some(markers)
    }

    /// What the line with these `markers` does, and its number: in the result, or for a removed
    /// line in the first parent that has it. Moves past the line.
    fn take(&mut self, markers: &[u8]) -> (Change, usize) {
        let in_result = !markers.contains(&b'-');
        let (change, number) = if !in_result {
            let parent = markers.iter().position(|marker| *marker == b'-');
            (
                Change::Removed,
                parent.map_or(self.result, |parent| self.parents[parent]),
            )
        } else if markers.iter().all(|marker| *marker == b'+') {
            (Change::Added, self.result)
        } else {
            (Change::Unchanged, self.result)
        };
        if in_result {
            self.result = self.result.saturating_add(1);
        }
        for (marker, line) in markers.iter().zip(&mut self.parents) {
            if *marker == b'-' || (*marker == b' ' && in_result) {
                *line = line.saturating_add(1);
            }
        }
        (change, number)
    }
}

/// The secrets in a commit's message and patch. Findings name the commit.
pub fn commit(hash: &str, message: &str, patch: &str) -> Vec<Finding> {
    let mut findings = text(message, &format!("commit {hash} message")).findings;
    findings.extend(diff_findings(patch).into_iter().map(|finding| Finding {
        place: format!("commit {hash} {}", finding.place),
        ..finding
    }));
    findings
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
pub struct LineScan(Scanner);

impl LineScan {
    /// What `line`, the next line, holds.
    pub fn line(&mut self, line: &str) -> Held {
        let mut kinds = Vec::new();
        match self.0.scan(line, &mut kinds) {
            None => Held::MoreOfAKey,
            Some(_) if kinds.is_empty() => Held::Nothing,
            Some(_) => Held::Secrets(kinds),
        }
    }

    /// Starts on the next line, too long to scan whole, which starts with `start`: it's to be
    /// scanned in windows, its pieces fed to the [`LongLineScan`], and given to
    /// [`LineScan::end_long_line`].
    pub fn start_long_line(&self, start: &str) -> LongLineScan {
        self.0.start_long_line(start)
    }

    /// What the long line `line` holds, now that its windows found secrets that look like
    /// `kinds`, and it ended with `tail`.
    pub fn end_long_line(&mut self, line: LongLineScan, kinds: Vec<String>, tail: &str) -> Held {
        match self.0.end_long_line(line, kinds, tail) {
            None => Held::MoreOfAKey,
            Some(kinds) if kinds.is_empty() => Held::Nothing,
            Some(kinds) => Held::Secrets(kinds),
        }
    }
}

/// A line too long to scan whole, read a piece at a time: whether the line before left a private
/// key's block open, whether this one is a header of the key's, and whether all of it is the
/// key's data, which only the whole line can tell.
pub struct LongLineScan {
    in_key: bool,
    header: bool,
    data: KeyData,
}

impl LongLineScan {
    /// Takes the next piece of the line.
    pub fn feed(&mut self, piece: &[u8]) {
        if self.in_key {
            self.data
                .feed(&without_hidden(&String::from_utf8_lossy(piece)).text);
        }
    }
}

/// Whether text fed a piece at a time is a private key's data, as [`is_key_data`] tells a whole
/// line: base64, with what [`strip_code`] takes off its ends around it, and nothing else.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
enum KeyData {
    /// Before the data, among what code puts in front of it.
    #[default]
    Before,
    /// Before the data, after a backslash, which only `n` can follow there.
    BeforeBackslash,
    Data,
    /// After the data, among what code puts after it.
    After,
    /// After the data, after a backslash.
    AfterBackslash,
    /// Not a key's data.
    Not,
}

impl KeyData {
    fn feed(&mut self, text: &str) {
        *self = text.chars().fold(*self, KeyData::next);
    }

    fn next(self, character: char) -> Self {
        let code = character.is_whitespace() || "\"'`,;+".contains(character);
        let data = character.is_ascii_alphanumeric() || "+/=".contains(character);
        match (self, character) {
            (KeyData::Before, '\\') => KeyData::BeforeBackslash,
            (KeyData::BeforeBackslash, 'n') => KeyData::Before,
            (KeyData::Before, _) if code => KeyData::Before,
            (KeyData::Before | KeyData::Data, _) if data => KeyData::Data,
            (KeyData::Data | KeyData::After | KeyData::AfterBackslash, '\\') => {
                KeyData::AfterBackslash
            }
            (KeyData::AfterBackslash, 'n') => KeyData::After,
            (KeyData::Data | KeyData::After | KeyData::AfterBackslash, _) if code => KeyData::After,
            _ => KeyData::Not,
        }
    }

    fn is_key_data(self) -> bool {
        matches!(
            self,
            KeyData::Data | KeyData::After | KeyData::AfterBackslash
        )
    }
}

/// What the secrets in `text`, scanned as a line on its own, look like.
pub fn kinds_in(text: &str) -> Vec<String> {
    let mut kinds = Vec::new();
    Scanner::default().scan(text, &mut kinds);
    kinds
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

/// Remembers whether the lines being scanned are inside a private key block.
#[derive(Default)]
struct Scanner {
    /// After a line that ends with a BEGIN marker, until the key's data ends.
    in_key: bool,
    /// Whether the key's data has been replaced with one [`REDACTED`] line.
    key_replaced: bool,
}

impl Scanner {
    /// `line` with its secrets replaced, adding what they look like to `kinds`, or `None` if
    /// it's part of a key already replaced: a private key's lines become one [`REDACTED`] line.
    /// The patterns see the line without hidden characters, since a secret with an invisible
    /// character inside matches none. A line that held a secret goes on as that visible text;
    /// any other line goes on as it was, so the model sees its hidden characters escaped.
    fn scan(&mut self, line: &str, kinds: &mut Vec<String>) -> Option<String> {
        let visible = without_hidden(line);
        let found = kinds.len();
        let scanned = self.scan_visible(&visible.text, kinds)?;
        Some(if kinds.len() > found {
            scanned
        } else {
            line.to_owned()
        })
    }

    /// Starts on the next line, too long to scan whole, which starts with `start`. Only the
    /// start is needed to tell a key's header line, whose name comes first.
    fn start_long_line(&self, start: &str) -> LongLineScan {
        LongLineScan {
            in_key: self.in_key,
            header: self.in_key && is_key_header(&without_hidden(start).text),
            data: KeyData::default(),
        }
    }

    /// What the long line `line` holds, now that its windows found secrets that look like
    /// `kinds`, and it ended with `tail`, as [`Scanner::scan_visible`] takes a line: inside a
    /// key's block, a line all of the key's data is the key, where its data starts, and is left
    /// out (`None`) after that, whatever the windows found in it; any other line holds what the
    /// windows found, ends the block unless it's a header of the key's, and starts one if
    /// `tail` ends with a key's BEGIN line.
    fn end_long_line(
        &mut self,
        line: LongLineScan,
        kinds: Vec<String>,
        tail: &str,
    ) -> Option<Vec<String>> {
        if line.in_key && line.data.is_key_data() {
            if self.key_replaced {
                return None;
            }
            self.key_replaced = true;
            return Some(vec!["private key".to_owned()]);
        }
        self.in_key = line.header;
        if starts_key_block(&without_hidden(tail).text) {
            self.in_key = true;
            self.key_replaced = false;
        }
        Some(kinds)
    }

    fn scan_visible(&mut self, line: &str, kinds: &mut Vec<String>) -> Option<String> {
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
    use crate::git::test_support::{isolated_repo, run};
    use std::fs;

    #[test]
    fn finds_a_secret_split_by_an_invisible_character() {
        let key = format!("AKIA{}", "Q".repeat(16));
        let hidden = format!("key = \"{}\u{200b}{}\"\n", &key[..8], &key[8..]);
        let redacted = text(&hidden, "message");
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
        assert_eq!(text(&line[1..], "message").text, &line[1..]);
    }

    /// A random-looking string of `len` letters and digits, so no real-looking
    /// secret is written out in this file.
    fn noise(len: usize) -> String {
        "q7Rk2LmZ9vXa4TbN8cWp3HdY6sJf5GeU"
            .chars()
            .cycle()
            .take(len)
            .collect()
    }

    /// OpenSSH's names for two of its security key algorithms, and usage text with a password
    /// one may leave out. Each is put together from two parts, so that a git-agent older than
    /// 0.12.0, whose check takes them for secrets, still commits this file.
    const SK_ECDSA_CERT: &str = concat!("sk", "-ecdsa-sha2-nistp256-cert-v01@openssh.com");
    const SK_ED25519_CERT: &str = concat!("sk", "-ssh-ed25519-cert-v01@openssh.com");
    const AFP_USAGE: &str = concat!("afp:", "//user[:pass]@host");

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
        assert_eq!(diff_findings(&diff), result.findings);
    }

    /// The places, lines and changes of what `diff` finds.
    fn found_in(diff: &str) -> Vec<(String, Option<usize>, Change)> {
        super::diff(diff)
            .findings
            .into_iter()
            .map(|finding| (finding.place, finding.line, finding.change))
            .collect()
    }

    #[test]
    fn reads_what_a_merge_adds_in_a_combined_diff() {
        let diff = format!(
            "diff --cc settings.env\nindex 1a2b3c4,5d6e7f8..9a0b1c2\n--- a/settings.env\n\
             +++ b/settings.env\n@@@ -1,4 -1,3 +1,4 @@@\n  shared = 1\n\
             - ours_password = {ours}\n -their_password = {theirs}\n- old_password = {old}\n\
             ++db_password = {added}\n + kept_password = {kept}\n  last = 2\n",
            ours = noise(16),
            theirs = noise(17),
            added = noise(18),
            kept = noise(19),
            old = noise(20),
        );
        let result = super::diff(&diff);
        assert!(!result.text.contains(&noise(18)), "{}", result.text);
        assert!(
            result.text.contains("++db_password = [REDACTED]\n"),
            "{}",
            result.text
        );
        let place = "settings.env".to_owned();
        assert_eq!(
            found_in(&diff),
            [
                (place.clone(), Some(2), Change::Removed),
                (place.clone(), Some(2), Change::Removed),
                (place.clone(), Some(3), Change::Removed),
                (place.clone(), Some(2), Change::Added),
                (place, Some(3), Change::Unchanged),
            ]
        );
        assert_eq!(diff_findings(&diff), result.findings);
    }

    #[test]
    fn reads_an_octopus_merge_with_a_column_per_parent() {
        let diff = format!(
            "diff --cc a.env\n@@@@ -1 -1 -1 +1,2 @@@@\n  +shared = 1\n+++db_password = {}\n",
            noise(18)
        );
        assert_eq!(
            found_in(&diff),
            [("a.env".to_owned(), Some(2), Change::Added)]
        );
    }

    #[test]
    fn finds_a_secret_a_merge_adds_in_gits_own_output() {
        let (dir, _repo) = isolated_repo();
        let write = |text: &str| fs::write(dir.path().join("app.env"), text).unwrap();
        write("mode = base\n");
        run(dir.path(), &["add", "app.env"]);
        run(dir.path(), &["commit", "-q", "-m", "base"]);
        run(dir.path(), &["switch", "-q", "-c", "side"]);
        write("mode = side\n");
        run(dir.path(), &["commit", "-q", "-am", "side"]);
        run(dir.path(), &["switch", "-q", "main"]);
        write("mode = main\n");
        run(dir.path(), &["commit", "-q", "-am", "main"]);
        let _ = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["merge", "-q", "side"])
            .output();
        write(&format!("mode = both\ndb_password = {}\n", noise(18)));
        run(dir.path(), &["commit", "-q", "-am", "merge side"]);

        let patch = run(dir.path(), &["log", "-1", "--patch", "--cc", "--format="]);

        assert_eq!(
            found_in(&patch),
            [("app.env".to_owned(), Some(2), Change::Added)]
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
        assert_eq!(diff_findings(&diff), result.findings);

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
        let start = |header: &str| {
            let hunk = Hunk::starting(header);
            (hunk.parents, hunk.result)
        };
        assert_eq!(start("@@ -10,3 +12,4 @@ fn main\n"), (vec![10], 12));
        assert_eq!(start("@@ -0,0 +1 @@\n"), (vec![0], 1));
        assert_eq!(start("@@@ -4,2 -7 +9,3 @@@ fn main\n"), (vec![4, 7], 9));
        assert_eq!(start("@@@@ -1 -2 -3 +5 @@@@\n"), (vec![1, 2, 3], 5));
        assert_eq!(start("@@\n"), (vec![1], 1));
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

    /// The key-data check fed a piece at a time agrees with [`is_key_data`] on the whole line, on
    /// short lines of every character that matters to it, cut anywhere.
    #[test]
    fn tells_key_data_a_piece_at_a_time_as_a_whole_line_does() {
        let alphabet: Vec<char> = "An+/=\\ \"',;`x\t".chars().collect();
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut pick = |below: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            usize::try_from(state % u64::try_from(below).unwrap()).unwrap()
        };
        for _ in 0..200_000 {
            let length = pick(9);
            let line: String = (0..length)
                .map(|_| alphabet[pick(alphabet.len())])
                .collect();
            let cut = line
                .char_indices()
                .nth(pick(length + 1))
                .map_or(line.len(), |(at, _)| at);
            let mut data = KeyData::default();
            data.feed(&line[..cut]);
            data.feed(&line[cut..]);

            assert_eq!(data.is_key_data(), is_key_data(&line), "{line:?}");
        }
    }

    /// Takes a long line, of `pieces`, whose windows found `kinds`, the way the line readers do.
    fn long(scan: &mut LineScan, pieces: &[&str], kinds: &[&str]) -> Held {
        let mut line = scan.start_long_line(pieces[0]);
        pieces.iter().for_each(|piece| line.feed(piece.as_bytes()));
        let kinds = kinds.iter().map(ToString::to_string).collect();
        scan.end_long_line(line, kinds, pieces[pieces.len() - 1])
    }

    /// In a private key's block, a long line all of key data is the key where its data starts,
    /// and holds nothing more after that, whatever its windows found; one that isn't holds what
    /// its windows found and ends the block, unless it's a header of the key's; and a block
    /// begins after a long line whose end is a BEGIN line.
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
        scan.line(begin);
        assert_eq!(long(&mut scan, &["Comment: xx", "x x"], &[]), Held::Nothing);
        assert_eq!(
            scan.line("AAAA"),
            found("private key"),
            "a header keeps the block"
        );

        let mut scan = LineScan::default();
        assert_eq!(long(&mut scan, &["AAAA", "BBBB"], &[]), Held::Nothing);
        assert_eq!(long(&mut scan, &["x", begin], &[]), Held::Nothing);
        assert_eq!(scan.line("AAAA"), found("private key"), "a block begins");
    }

    /// A diff's long line outside the hunks needs no more than its start, which still counts as a
    /// file's header; a hunk's comes back with its markers, and what its windows find lands on its
    /// number, the lines after it keeping theirs.
    #[test]
    fn takes_a_long_line_of_a_diff_by_where_it_is() {
        let mut scan = DiffScan::default();

        assert!(scan.long_line("diff --git a/x.env b/x.env").is_none());
        scan.line("@@ -1,2 +1,3 @@\n");
        scan.line(" same\n");
        let text = scan.long_line("+added").expect("a hunk's line");
        assert_eq!(text.markers, 1);
        scan.long_text(text, vec!["AWS access key".to_owned()], "end");
        scan.line(&format!("+{}\n", crate::secrets::test_support::aws_key()));

        let found: Vec<(String, Option<usize>, Change)> = scan
            .findings()
            .into_iter()
            .map(|finding| (finding.place, finding.line, finding.change))
            .collect();
        let at = |line| ("x.env".to_owned(), Some(line), Change::Added);
        assert_eq!(found, [at(2), at(3)]);
    }
}
