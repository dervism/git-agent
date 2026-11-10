//! Text read a line at a time in bounded memory. A line up to [`WINDOW`] bytes comes whole, and a
//! longer one, such as a minified bundle's, is read in pieces, or scanned in windows that overlap,
//! so that no more than a window or two of it is held at once.

use super::redact;
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
    /// The first [`WINDOW`] bytes of a longer line, whose rest [`Lines::scan_rest`] or
    /// [`Lines::read_rest`] reads next.
    Long(Vec<u8>),
}

/// What a line too long to hold whole holds: what its secrets look like, and its end, after
/// which a private key's block may begin.
#[derive(Debug, PartialEq)]
pub struct LongLine {
    pub kinds: Vec<String>,
    pub tail: String,
}

/// The lines of the text a reader holds.
pub struct Lines<R>(R);

impl<R: BufRead> Lines<R> {
    pub fn new(reader: R) -> Self {
        Self(reader)
    }

    /// The next line, or `None` at the end of the text.
    pub fn next_line(&mut self) -> io::Result<Option<Line>> {
        let mut start = Vec::new();
        if (&mut self.0)
            .take(WINDOW as u64)
            .read_until(b'\n', &mut start)?
            == 0
        {
            return Ok(None);
        }
        Ok(Some(if start.len() < WINDOW || start.ends_with(b"\n") {
            Line::Whole(start)
        } else {
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
    ) -> io::Result<LongLine> {
        feed(&start);
        let mut tally = Tally::default();
        let mut window = start;
        loop {
            tally.window(kinds_in(&window));
            let piece = self.piece(WINDOW - OVERLAP)?;
            if piece.bytes.is_empty() {
                break;
            }
            feed(&piece.bytes);
            let overlap = &window[window.len().saturating_sub(OVERLAP)..];
            tally.overlap(kinds_in(overlap));
            window = [overlap, &piece.bytes].concat();
            if piece.last {
                tally.window(kinds_in(&window));
                break;
            }
        }
        let tail = &window[window.len().saturating_sub(OVERLAP)..];
        Ok(LongLine {
            kinds: tally.kinds(),
            tail: String::from_utf8_lossy(tail).into_owned(),
        })
    }

    /// Reads a long line, whose first bytes are `start` and whose rest comes next, without
    /// scanning it: `feed` is given it, without its line end, a piece at a time.
    pub fn read_rest(&mut self, start: &[u8], mut feed: impl FnMut(&[u8])) -> io::Result<()> {
        feed(start);
        loop {
            let piece = self.piece(WINDOW)?;
            if piece.bytes.is_empty() {
                return Ok(());
            }
            feed(&piece.bytes);
            if piece.last {
                return Ok(());
            }
        }
    }

    /// Up to `most` more bytes of the line being read, without its line end.
    fn piece(&mut self, most: usize) -> io::Result<Piece> {
        let mut bytes = Vec::new();
        (&mut self.0)
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
            let key = format!(" AKIA{} ", "Q".repeat(16));
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
    fn read(text: &[u8]) -> Vec<(Line, Option<(LongLine, usize)>)> {
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
        assert!(read_nothing(b""));
    }

    fn read_nothing(text: &[u8]) -> bool {
        Lines::new(Cursor::new(text)).next_line().unwrap().is_none()
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
    /// end, each once; the tail is the line's last [`OVERLAP`] bytes.
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
        assert_eq!(
            long.tail.as_bytes(),
            &line[length - OVERLAP..],
            "the tail is the line's end"
        );
        assert_eq!(read.len(), 1);
    }

    /// Reading a long line without scanning it gives `feed` all of it, in pieces of a window at
    /// most, and leaves the next line to read.
    #[test]
    fn reads_the_rest_of_a_long_line_in_pieces() {
        let line = line_with_keys(3 * WINDOW + 7, &[WINDOW]);
        let mut lines = Lines::new(Cursor::new([&line[..], b"\nnext"].concat()));
        let Some(Line::Long(start)) = lines.next_line().unwrap() else {
            panic!("the line is long");
        };
        let mut pieces = Vec::new();

        lines
            .read_rest(&start, |piece| pieces.push(piece.len()))
            .unwrap();

        assert_eq!(pieces, [WINDOW, WINDOW, WINDOW, 7]);
        assert_eq!(
            lines.next_line().unwrap(),
            Some(Line::Whole(b"next".to_vec()))
        );
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
