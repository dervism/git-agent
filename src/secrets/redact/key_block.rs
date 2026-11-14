//! A private key's block: what a line is to it, whole or read a piece at a time, and how a text,
//! or one side of a diff, follows a block from line to line.

use crate::safe_text::Unhidden;
use crate::secrets::finding::Change;
use crate::secrets::fingerprint::{Fingerprint, Fingerprinting, Key};
use regex_lite::Regex;
use std::mem;
use std::sync::LazyLock;

static KEY_BEGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN[A-Z ]*PRIVATE KEY(?: BLOCK)?-----").expect("valid key pattern")
});

/// How a side takes a line, the one that shows the least first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Taken {
    /// As more of a key's data than its one [`REDACTED`](super::REDACTED) line: it's left out.
    MoreOfAKey,
    /// As a key's data where it first makes its change: it's the key's
    /// [`REDACTED`](super::REDACTED) line.
    Key,
    /// As text, its secrets replaced.
    Text,
}

/// What a line is to a private key's block, which is all a side needs to take it.
pub(super) trait KeyBlockFacts {
    /// Whether the line is a line of a private key's data.
    fn is_data(&self) -> bool;
    /// Whether the line is a header of a private key's, such as `Proc-Type: 4,ENCRYPTED`.
    fn is_header(&self) -> bool;
    /// Whether the line, with its secrets replaced, ends with a private key's BEGIN line.
    fn begins_key(&self) -> bool;
    /// Feeds the line's key data to `print`, the print of the block the line is in. A long line,
    /// read before its block takes it, gives the print it fed as it came from that block's own.
    fn feed_data(&self, print: &mut Fingerprinting);
}

/// What a line too long to scan whole is to a private key's block, once all of it has been read:
/// inside a block, whether it's the key's data or a header (`in_key`), as only all of it can
/// tell, and whether its last window ends with a key's BEGIN line.
pub(super) struct LongLineFacts {
    in_key: Option<InKey>,
    begins_key: bool,
    /// The print of the side's block, fed the line's key data.
    print: Option<Fingerprinting>,
}

impl LongLineFacts {
    /// The facts of `line`, all of it read, whose last window, its secrets replaced, ends with a
    /// key's BEGIN line or not (`ends_with_begin`): one for each side it was started for, in that
    /// order, with that side's print.
    pub(super) fn of(line: LongLineScan, ends_with_begin: bool) -> Vec<Self> {
        let (in_key, prints) = line.verdict();
        prints
            .into_iter()
            .map(|print| Self {
                in_key,
                begins_key: ends_with_begin,
                print,
            })
            .collect()
    }
}

impl KeyBlockFacts for LongLineFacts {
    fn is_data(&self) -> bool {
        self.in_key.as_ref().is_some_and(|line| line.data)
    }

    fn is_header(&self) -> bool {
        self.in_key.as_ref().is_some_and(|line| line.header)
    }

    fn begins_key(&self) -> bool {
        self.begins_key
    }

    fn feed_data(&self, print: &mut Fingerprinting) {
        if let Some(fed) = &self.print {
            print.clone_from(fed);
        }
    }
}

/// A line too long to scan whole, read a piece at a time: whether the line before left a private
/// key's block open, and, which only all of the line can tell, whether it's the key's data or a
/// header of the key's.
pub struct LongLineScan {
    in_key: bool,
    text: Unhidden,
    data: KeyData,
    header: KeyHeader,
    /// The print of the block of each side that holds the line, fed its key data as it comes,
    /// which the side keeps only if all of the line turns out to be data.
    prints: Vec<Option<Fingerprinting>>,
}

/// What a long line in a private key's block is, once all of it has been read.
#[derive(Clone, Copy)]
struct InKey {
    data: bool,
    header: bool,
}

impl LongLineScan {
    /// Starts on a line, `in_key` if the line before left a private key's block open, with the
    /// `prints` of the blocks of the sides that hold it.
    pub(super) fn starting(in_key: bool, prints: Vec<Option<Fingerprinting>>) -> Self {
        Self {
            in_key,
            text: Unhidden::default(),
            data: KeyData::default(),
            header: KeyHeader::default(),
            prints,
        }
    }

    /// Takes the next piece of the line, cleaned of hidden characters as the whole line would
    /// be, however the pieces cut it.
    pub fn feed(&mut self, piece: &[u8]) {
        if self.in_key {
            let text = self.text.piece(piece);
            self.take(&text);
        }
    }

    fn take(&mut self, text: &str) {
        let mut data = Vec::new();
        self.data
            .feed_printing(text, |piece| data.extend_from_slice(piece));
        for print in self.prints.iter_mut().flatten() {
            print.feed(&data);
        }
        self.header.feed(text);
    }

    /// What the line is to the key's block it's in, or `None` outside a block, and the prints
    /// fed its data.
    fn verdict(mut self) -> (Option<InKey>, Vec<Option<Fingerprinting>>) {
        if !self.in_key {
            return (None, self.prints);
        }
        let rest = self.text.finish();
        self.take(&rest);
        let in_key = InKey {
            data: self.data.is_key_data(),
            header: self.header.is_key_header(),
        };
        (Some(in_key), self.prints)
    }
}

/// Whether text, whole or fed a piece at a time, is a header line of a private key's: nothing but
/// what code puts around a key written into a string literal (white space, quotes, commas,
/// semicolons, `+`, and backslashes and escaped line breaks), or a name of letters, digits and
/// `-`, then `: ` and a value that's more than that.
struct KeyHeader {
    /// Whether all of the text so far is what the stripping takes off a line's end.
    stripped: bool,
    after_backslash: bool,
    form: HeaderForm,
}

/// How far text has come in the form of a header: `Name: value`.
#[derive(Clone, Copy)]
enum HeaderForm {
    /// Before the name, among what code puts in front of it.
    Before,
    /// Before the name, after a backslash, which only `n` can follow there.
    BeforeBackslash,
    Name,
    Colon,
    /// After `: `, and whether all of what came after it is what the stripping takes off.
    Value {
        stripped: bool,
    },
    Not,
}

impl Default for KeyHeader {
    fn default() -> Self {
        Self {
            stripped: true,
            after_backslash: false,
            form: HeaderForm::Before,
        }
    }
}

impl KeyHeader {
    /// What all of `text` is.
    fn of(text: &str) -> Self {
        let mut header = Self::default();
        header.feed(text);
        header
    }

    fn feed(&mut self, text: &str) {
        text.chars().for_each(|character| self.next(character));
    }

    fn next(&mut self, character: char) {
        let code = character.is_whitespace() || "\"'`,;+".contains(character);
        let stripped = code || character == '\\' || (character == 'n' && self.after_backslash);
        let name = character.is_ascii_alphanumeric() || character == '-';
        self.stripped &= stripped;
        self.form = match (self.form, character) {
            (HeaderForm::Before, '\\') => HeaderForm::BeforeBackslash,
            (HeaderForm::BeforeBackslash, 'n') => HeaderForm::Before,
            (HeaderForm::Before, _) if code => HeaderForm::Before,
            (HeaderForm::Before | HeaderForm::Name, _) if name => HeaderForm::Name,
            (HeaderForm::Name, ':') => HeaderForm::Colon,
            (HeaderForm::Colon, ' ') => HeaderForm::Value { stripped: true },
            (HeaderForm::Value { stripped: before }, _) => HeaderForm::Value {
                stripped: before && stripped,
            },
            _ => HeaderForm::Not,
        };
        self.after_backslash = character == '\\';
    }

    fn is_key_header(&self) -> bool {
        self.stripped || matches!(self.form, HeaderForm::Value { stripped: false })
    }

    /// Whether the text so far is nothing but what code puts around a key in a string literal.
    fn is_only_code(&self) -> bool {
        self.stripped
    }
}

/// Whether text, whole or fed a piece at a time, is a private key's data: base64, with only what
/// code puts around a key in a string literal (as [`KeyHeader`] lists it) before and after it.
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
    /// What all of `text` is.
    fn of(text: &str) -> Self {
        let mut data = Self::default();
        data.feed(text);
        data
    }

    fn feed(&mut self, text: &str) {
        *self = text.chars().fold(*self, KeyData::next);
    }

    /// Feeds `text`, as [`KeyData::feed`] does, and gives `data` each character it takes as data.
    fn feed_printing(&mut self, text: &str, mut data: impl FnMut(&[u8])) {
        for character in text.chars() {
            *self = self.next(character);
            if *self == KeyData::Data {
                data(&[character as u8]);
            }
        }
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

/// Follows a private key's block across the lines of one text, or one side of a diff, and gives
/// the findings that wait for a key its print once the key's block closes.
#[derive(Default)]
pub(super) struct KeyBlock {
    /// After a line that ends with a BEGIN marker, until the key's data ends.
    open: Option<OpenBlock>,
    /// The prints of the blocks that closed, by the slot of each finding that waited for one.
    closed: Vec<(usize, Fingerprint)>,
}

/// A key's block that is open: the print of its data so far, what the lines of its data that have
/// been reported do, and the slots of the findings that wait for its print. The key's data becomes
/// one [`REDACTED`](super::REDACTED) line for each change, so that a key that replaces another,
/// whose old lines git shows first, is reported as added too.
struct OpenBlock {
    print: Fingerprinting,
    key_changes: Vec<Change>,
    waiting: Vec<usize>,
}

impl OpenBlock {
    fn new() -> Self {
        Self {
            print: Key::for_run().start(),
            key_changes: Vec::new(),
            waiting: Vec::new(),
        }
    }

    /// How a line of the key's data that makes `change` is taken: as the key, the first in the
    /// block to make the change, else as more of it.
    fn key_data(&mut self, change: Change) -> Taken {
        if self.key_changes.contains(&change) {
            Taken::MoreOfAKey
        } else {
            self.key_changes.push(change);
            Taken::Key
        }
    }
}

impl KeyBlock {
    /// Whether the lines so far have left a key's block open.
    pub(super) fn in_key(&self) -> bool {
        self.open.is_some()
    }

    /// Starts on the next line, too long to scan whole.
    pub(super) fn start_long_line(&self) -> LongLineScan {
        LongLineScan::starting(self.in_key(), vec![self.data_print()])
    }

    /// The print of the open block's data so far.
    pub(super) fn data_print(&self) -> Option<Fingerprinting> {
        self.open.as_ref().map(|open| open.print.clone())
    }

    /// Has the finding at `slot` wait for the open block's print, as it's the key's.
    pub(super) fn wait(&mut self, slot: usize) {
        if let Some(open) = &mut self.open {
            open.waiting.push(slot);
        }
    }

    /// Has the key a line was taken for, as `taken` tells, wait for the open block's print at
    /// `slot`.
    pub(super) fn wait_if_key(&mut self, taken: Taken, slot: usize) -> Taken {
        if taken == Taken::Key {
            self.wait(slot);
        }
        taken
    }

    /// Ends the text, or the hunk: an open block's key is cut off, and its print partial.
    pub(super) fn end(&mut self) {
        self.close(true);
    }

    /// The prints of the blocks that closed, by the slot of each finding that waited.
    pub(super) fn take_prints(&mut self) -> Vec<(usize, Fingerprint)> {
        mem::take(&mut self.closed)
    }

    /// Takes `line`, which makes `change`, whole or a long line read in pieces: inside a key's
    /// block, a line of the key's data is the key where its data first makes the change, and
    /// more of it after that. Any other line is text; it ends the block unless it's a header of
    /// the key's, and starts one if, with its secrets replaced, it ends with a key's BEGIN line.
    pub(super) fn take(&mut self, line: &impl KeyBlockFacts, change: Change) -> Taken {
        if let Some(open) = &mut self.open
            && line.is_data()
        {
            line.feed_data(&mut open.print);
            return open.key_data(change);
        }
        // Encrypted keys have headers such as "Proc-Type: 4,ENCRYPTED" before
        // their data. Anything else, such as the END line, ends the block.
        if self.in_key() && !line.is_header() {
            self.close(false);
        }
        if line.begins_key() {
            self.close(false);
            self.open = Some(OpenBlock::new());
        }
        Taken::Text
    }

    /// Closes the open block, whole or `partial`: each finding that waits gets its print.
    fn close(&mut self, partial: bool) {
        if let Some(open) = self.open.take() {
            let print = if partial {
                open.print.finish_partial()
            } else {
                open.print.finish()
            };
            self.closed
                .extend(open.waiting.into_iter().map(|slot| (slot, print)));
        }
    }
}

/// Whether `line` ends with a private key's BEGIN marker, so the key's data
/// follows. Code that only mentions the marker, such as
/// `starts_with("-----BEGIN PRIVATE KEY-----")`, has more after it.
pub(super) fn starts_key_block(line: &str) -> bool {
    KEY_BEGIN
        .find_iter(line)
        .last()
        .is_some_and(|marker| KeyHeader::of(&line[marker.end()..]).is_only_code())
}

/// Whether `line` is a line of a key's base64 data, perhaps inside a string literal, as
/// [`KeyData`] tells it.
pub(super) fn is_key_data(line: &str) -> bool {
    KeyData::of(line).is_key_data()
}

/// The key data of `line`, a line of a key's data: its base64 characters, without what code puts
/// around them.
pub(super) fn key_data_of(line: &str) -> Vec<u8> {
    let mut data = Vec::new();
    KeyData::default().feed_printing(line, |piece| data.extend_from_slice(piece));
    data
}

/// The print of a private key written on one line, as a string literal of code writes it, or as
/// git joins a message's first paragraph: the key data of each of its lines, split where white
/// space or a `\n` escape is, as the lines of a block would give it.
pub(super) fn key_print(text: &str) -> Fingerprint {
    let mut print = Key::for_run().start();
    text.split("\\n")
        .flat_map(str::split_whitespace)
        .filter(|line| is_key_data(line))
        .for_each(|line| print.feed(&key_data_of(line)));
    print.finish()
}

/// Whether `line` is a header line of a private key's, as [`KeyHeader`] tells it.
pub(super) fn is_key_header(line: &str) -> bool {
    KeyHeader::of(line).is_key_header()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safe_text::without_hidden;
    use crate::test_support::Picks;

    /// What a long line in a key's block fed a piece at a time, cut at any byte, is to the block
    /// agrees with what [`is_key_data`] and [`is_key_header`] tell from the whole line without its
    /// hidden characters: on short lines of every character that matters to them, hidden ones,
    /// ones of more than a byte, and bytes that start or end no character among them.
    #[test]
    fn tells_a_long_line_in_a_key_block_a_piece_at_a_time_as_a_whole_line_does() {
        let characters = [
            "A", "n", "-", ":", "+", "/", "=", "\\", " ", "\"", "'", ",", ";", "`", "x", "\t", "é",
            "\u{a0}", "\u{3000}", "\u{200b}", "\u{200d}", "\u{fe0f}",
        ];
        let invalid: [&[u8]; 2] = [&[0xe2], &[0x80]];
        let parts: Vec<&[u8]> = characters
            .iter()
            .map(|part| part.as_bytes())
            .chain(invalid)
            .collect();
        let mut picks = Picks::seeded();
        for _ in 0..200_000 {
            let (whole, pieces) = picks.pieces(&parts, 9);
            let in_key = KeyBlock {
                open: Some(OpenBlock::new()),
                ..KeyBlock::default()
            };
            let mut line = in_key.start_long_line();

            pieces.iter().for_each(|piece| line.feed(piece));

            let text = without_hidden(&String::from_utf8_lossy(&whole)).text;
            let verdict = line.verdict().0.expect("in a key's block");
            let by_stripping = (data_by_stripping(&text), header_by_stripping(&text));
            assert_eq!(
                (verdict.data, verdict.header),
                by_stripping,
                "{text:?} in {pieces:?}"
            );
            assert_eq!((is_key_data(&text), is_key_header(&text)), by_stripping);
            let only_code = KeyHeader::of(&text).is_only_code();
            assert_eq!(only_code, strip_code(&text).is_empty(), "{text:?}");
        }
    }

    /// `text` without what code puts around a key written into a string literal: quotes,
    /// escaped line breaks, commas and concatenation. The rules of [`KeyData`] and [`KeyHeader`],
    /// written another way, for the tests to hold them to.
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

    /// Whether `line` is a key's base64 data, once [`strip_code`] has taken off its ends.
    fn data_by_stripping(line: &str) -> bool {
        let data = strip_code(line);
        !data.is_empty()
            && data
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+/=".contains(&byte))
    }

    /// Whether `line` is a key's header, once [`strip_code`] has taken off its ends.
    fn header_by_stripping(line: &str) -> bool {
        let line = strip_code(line);
        line.is_empty()
            || line.split_once(": ").is_some_and(|(name, _)| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
    }
}
