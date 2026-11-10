//! Text read a line at a time in bounded memory. A line up to [`WINDOW`] bytes comes whole, and a
//! longer one, such as a minified bundle's, is scanned in windows that overlap, or skipped, so
//! that no more than a window or two of it is held at once.

use super::redact::{self, Windows};
use std::collections::BTreeMap;
use std::io::{self, BufRead, Read};

/// The longest line read whole. A longer one is scanned in windows of this size.
pub const WINDOW: usize = 1024 * 1024;

/// How much each window of a long line overlaps the next: more than any secret the rules find on
/// one line, so that each lies whole in some window.
pub const OVERLAP: usize = 64 * 1024;

/// A line as [`Lines`] reads it.
#[derive(Debug, PartialEq)]
pub enum Line {
    /// A line of up to [`WINDOW`] bytes, with its line end if it has one.
    Whole(Vec<u8>),
    /// The first [`WINDOW`] bytes of a longer line, whose rest [`Lines::scan_rest`] reads next,
    /// or [`Lines::next_line`] skips.
    Long(Vec<u8>),
}

/// The lines of the text a reader holds.
pub struct Lines<R> {
    reader: R,
    /// Whether the last line was long, and its rest hasn't been read.
    unfinished: bool,
}

impl<R: BufRead> Lines<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            unfinished: false,
        }
    }

    /// The next line, or `None` at the end of the text. The rest of a long line that wasn't
    /// scanned is skipped first.
    pub fn next_line(&mut self) -> io::Result<Option<Line>> {
        if std::mem::take(&mut self.unfinished) {
            self.skip_rest()?;
        }
        let mut start = Vec::new();
        if (&mut self.reader)
            .take(WINDOW as u64)
            .read_until(b'\n', &mut start)?
            == 0
        {
            return Ok(None);
        }
        Ok(Some(if start.len() < WINDOW || start.ends_with(b"\n") {
            Line::Whole(start)
        } else {
            self.unfinished = true;
            Line::Long(start)
        }))
    }

    /// Scans a long line, whose first bytes are `start` and whose rest comes next, in windows
    /// that each overlap the next by [`OVERLAP`] bytes, counting through a [`Tally`]. `feed` is
    /// given the line, without its line end, a piece at a time.
    pub fn scan_rest(
        &mut self,
        start: Vec<u8>,
        mut feed: impl FnMut(&[u8]),
    ) -> io::Result<Windows> {
        self.unfinished = false;
        feed(&start);
        let mut tally = Tally::default();
        let mut window = start;
        let ends_with_begin = loop {
            let ends_with_begin = tally.scan(&window);
            let piece = self.piece(WINDOW - OVERLAP)?;
            if piece.bytes.is_empty() {
                break ends_with_begin;
            }
            feed(&piece.bytes);
            let overlap = &window[window.len().saturating_sub(OVERLAP)..];
            tally.overlap(kinds_in(overlap));
            window = [overlap, &piece.bytes].concat();
            if piece.last {
                break tally.scan(&window);
            }
        };
        Ok(Windows {
            kinds: tally.kinds(),
            ends_with_begin,
        })
    }

    /// Reads past the rest of a long line, its line end included, without holding it.
    fn skip_rest(&mut self) -> io::Result<()> {
        loop {
            let buffer = self.reader.fill_buf()?;
            if buffer.is_empty() {
                return Ok(());
            }
            let (length, ended) = buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or((buffer.len(), false), |end| (end + 1, true));
            self.reader.consume(length);
            if ended {
                return Ok(());
            }
        }
    }

    /// Up to `most` more bytes of the line being read, without its line end.
    fn piece(&mut self, most: usize) -> io::Result<Piece> {
        let mut bytes = Vec::new();
        (&mut self.reader)
            .take(most as u64)
            .read_until(b'\n', &mut bytes)?;
        let ended = bytes.ends_with(b"\n");
        bytes.truncate(bytes.len() - usize::from(ended));
        let last = ended || bytes.len() < most;
        Ok(Piece { bytes, last })
    }
}

/// A piece of a long line, and whether the line ends with it.
struct Piece {
    bytes: Vec<u8>,
    last: bool,
}

/// What the secrets in `bytes`, scanned on their own, look like.
fn kinds_in(bytes: &[u8]) -> Vec<String> {
    redact::kinds_in(&String::from_utf8_lossy(bytes))
}

/// What a long line's windows find, less what their overlaps find on their own, kind by kind. A
/// secret in an overlap is found by both its windows and by the overlap, so it counts once, as
/// long as what's around it differs on one side only. A scan can still count differently at an
/// edge, for example when a window has used up its searches again inside implausible matches, so
/// no kind ever comes out below the most that one scan, of a window or an overlap, found: the
/// subtraction may lower a count, but never takes away a secret a scan found.
#[derive(Default)]
struct Tally(BTreeMap<String, Count>);

/// How often the scans of a long line found one kind of secret.
#[derive(Default)]
struct Count {
    windows: usize,
    overlaps: usize,
    most: usize,
}

impl Count {
    fn counted(&self) -> usize {
        self.windows.saturating_sub(self.overlaps).max(self.most)
    }
}

impl Tally {
    /// Counts what `window`, scanned as a line on its own, holds, and says whether, with its
    /// secrets replaced, it ends with a private key's BEGIN line.
    fn scan(&mut self, window: &[u8]) -> bool {
        let scanned = redact::scan_window(&String::from_utf8_lossy(window));
        self.window(scanned.kinds);
        scanned.ends_with_begin
    }

    fn window(&mut self, kinds: Vec<String>) {
        for (kind, times) in times_each(kinds) {
            self.count(kind, times).windows += times;
        }
    }

    fn overlap(&mut self, kinds: Vec<String>) {
        for (kind, times) in times_each(kinds) {
            self.count(kind, times).overlaps += times;
        }
    }

    /// The count of `kind`, which one scan found `times`.
    fn count(&mut self, kind: String, times: usize) -> &mut Count {
        let count = self.0.entry(kind).or_default();
        count.most = count.most.max(times);
        count
    }

    /// What the secrets look like, each kind as often as it counts.
    fn kinds(self) -> Vec<String> {
        self.0
            .into_iter()
            .flat_map(|(kind, count)| std::iter::repeat_n(kind, count.counted()))
            .collect()
    }
}

/// How many times each kind is among `kinds`.
fn times_each(kinds: Vec<String>) -> BTreeMap<String, usize> {
    kinds.into_iter().fold(BTreeMap::new(), |mut times, kind| {
        *times.entry(kind).or_default() += 1;
        times
    })
}

/// What the tests of long lines share.
#[cfg(test)]
pub mod test_support {
    /// `length` bytes of padding with an AWS key put at each of `at`, between spaces.
    pub fn line_with_keys(length: usize, at: &[usize]) -> Vec<u8> {
        let mut line = vec![b'x'; length];
        for start in at {
            let key = format!(" {} ", crate::secrets::test_support::aws_key());
            line[*start..*start + key.len()].copy_from_slice(key.as_bytes());
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::line_with_keys;
    use super::*;
    use std::io::Cursor;

    /// Each line of `text`: a whole one as it is, and a long one as what its scan found, with
    /// all of it that `feed` was given.
    fn read(text: &[u8]) -> Vec<(Line, Option<(Windows, usize)>)> {
        let mut lines = Lines::new(Cursor::new(text));
        let mut read = Vec::new();
        while let Some(line) = lines.next_line().unwrap() {
            let scanned = match &line {
                Line::Whole(_) => None,
                Line::Long(start) => {
                    let mut fed = 0;
                    let long = lines.scan_rest(start.clone(), |piece| fed += piece.len());
                    Some((long.unwrap(), fed))
                }
            };
            read.push((line, scanned));
        }
        read
    }

    #[test]
    fn reads_lines_up_to_a_window_whole_with_their_ends() {
        let short = line_with_keys(WINDOW - 1, &[]);
        let text = [&short[..], b"\n", b"last"].concat();

        let read = read(&text);

        let ends: Vec<Option<u8>> = read
            .iter()
            .map(|(line, _)| match line {
                Line::Whole(bytes) => bytes.last().copied(),
                Line::Long(_) => None,
            })
            .collect();
        assert_eq!(ends, [Some(b'\n'), Some(b't')]);
        assert!(read.iter().all(|(_, scanned)| scanned.is_none()));
    }

    #[test]
    fn reads_no_line_from_no_text() {
        assert_eq!(Lines::new(Cursor::new(b"")).next_line().unwrap(), None);
    }

    /// A line of a window's length, line end included, comes whole; one byte more and it's long,
    /// with its first window's bytes as its start.
    #[test]
    fn reads_a_line_longer_than_a_window_as_long() {
        let at_most = [&line_with_keys(WINDOW - 1, &[])[..], b"\n"].concat();
        let over = line_with_keys(WINDOW + 1, &[]);
        let text = [&at_most[..], &over, b"\nnext\n"].concat();

        let read = read(&text);

        assert!(matches!(&read[0].0, Line::Whole(bytes) if bytes.len() == WINDOW));
        assert!(matches!(&read[1].0, Line::Long(start) if start.len() == WINDOW));
        assert_eq!(read[1].1.as_ref().map(|(_, fed)| *fed), Some(WINDOW + 1));
        assert_eq!(read[2].0, Line::Whole(b"next\n".to_vec()));
    }

    /// A long line's windows find what a scan of the whole line finds: a key inside a window,
    /// one inside an overlap, which two windows find, one across a window's edge, and one at the
    /// end, each once.
    #[test]
    fn scans_a_long_line_in_windows_as_a_scan_of_it_whole() {
        let length = 3 * WINDOW + WINDOW / 2;
        let at = [100_000, WINDOW - OVERLAP + 1000, WINDOW - 8, length - 40];
        let line = line_with_keys(length, &at);

        let read = read(&[&line[..], b"\n"].concat());

        let whole = redact::kinds_in(&String::from_utf8_lossy(&line));
        assert_eq!(whole.len(), 4);
        let (long, fed) = read[0].1.as_ref().unwrap();
        assert_eq!((&long.kinds, *fed), (&whole, length));
        assert!(!long.ends_with_begin);
        assert_eq!(read.len(), 1);
    }

    /// A long line ends with a private key's BEGIN line when it does once its secrets are
    /// replaced, as a scan of the whole line tells: not when the BEGIN line is part of a value
    /// that a rule replaces.
    #[test]
    fn tells_whether_a_long_line_ends_with_a_begin_line_with_its_secrets_replaced() {
        let begin = "-----BEGIN RSA PRIVATE KEY-----";
        let padding = "x".repeat(2 * WINDOW);
        let ends = |end: &str| {
            let line = format!("{padding}{end}\n");
            let read = read(line.as_bytes());
            read[0].1.as_ref().unwrap().0.ends_with_begin
        };

        assert!(ends(&format!(" {begin}")));
        assert!(!ends(&format!(" token=Ab1{begin}")));
        assert!(!ends(" plain"));
    }

    /// The rest of a long line that isn't scanned is skipped, however long, and the line after
    /// it comes next, whole.
    #[test]
    fn skips_the_rest_of_a_long_line_left_unscanned() {
        let line = line_with_keys(3 * WINDOW + 7, &[WINDOW]);
        let mut lines = Lines::new(Cursor::new([&line[..], b"\nnext"].concat()));

        let first = lines.next_line().unwrap();

        assert!(matches!(first, Some(Line::Long(start)) if start.len() == WINDOW));
        assert_eq!(
            lines.next_line().unwrap(),
            Some(Line::Whole(b"next".to_vec()))
        );
        assert_eq!(lines.next_line().unwrap(), None);
    }

    /// What the scans of `windows`, between which each of `overlaps` lies, find, as a [`Tally`]
    /// counts it; each scan finds the kinds named.
    fn tally(windows: &[&[&str]], overlaps: &[&[&str]]) -> Vec<String> {
        let found = |kinds: &[&str]| kinds.iter().map(ToString::to_string).collect();
        let mut tally = Tally::default();
        tally.window(found(windows[0]));
        for (overlap, window) in overlaps.iter().zip(&windows[1..]) {
            tally.overlap(found(overlap));
            tally.window(found(window));
        }
        tally.kinds()
    }

    /// A secret in an overlap counts once, though both its windows and the overlap find it;
    /// secrets apart from the overlaps count as often as their windows find them.
    #[test]
    fn counts_a_secret_in_an_overlap_once() {
        assert_eq!(tally(&[&["aws"], &["aws"]], &[&["aws"]]), ["aws"]);
        assert_eq!(
            tally(&[&["aws", "aws"], &["aws", "pass"]], &[&["aws"]]),
            ["aws", "aws", "pass"]
        );
        assert_eq!(
            tally(&[&["aws"], &["aws"], &["aws"]], &[&["aws"], &[]]),
            ["aws", "aws"]
        );
        assert!(tally(&[&[], &[]], &[&[]]).is_empty());
    }

    /// A count never comes out below what one scan found: not when a window misses what the
    /// overlap before it finds, and not when the overlap finds what both its windows miss.
    #[test]
    fn counts_no_kind_below_what_one_scan_found() {
        assert_eq!(tally(&[&["pass"], &[]], &[&["pass"]]), ["pass"]);
        assert_eq!(tally(&[&["aws", "aws"], &[]], &[&["aws"]]), ["aws", "aws"]);
        assert_eq!(tally(&[&[], &[]], &[&["pass"]]), ["pass"]);
        assert_eq!(
            tally(&[&["aws"], &[], &["aws"]], &[&["aws", "aws"], &[]]),
            ["aws", "aws"]
        );
    }
}
