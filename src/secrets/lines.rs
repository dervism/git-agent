//! Text read a line at a time in bounded memory. A line up to [`WINDOW`] bytes comes whole, and a
//! longer one, such as a minified bundle's, is scanned in windows that overlap, or skipped, so
//! that no more than a window or two of it is held at once.

use super::fingerprint::{Fingerprint, Key};
use super::redact::{self, Matched, Secret, Windows};
use std::collections::HashMap;
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
            let piece = self.piece(WINDOW - OVERLAP)?;
            if piece.bytes.is_empty() {
                break tally.scan(&window);
            }
            feed(&piece.bytes);
            let whole = whole_characters(&window);
            tally.scan(&window[..whole]);
            let next = character_start(&window, window.len().saturating_sub(OVERLAP));
            tally.overlap(matched_in(&window[next..whole]));
            window = [&window[next..], &piece.bytes].concat();
            if piece.last {
                break tally.scan(&window);
            }
        };
        Ok(Windows {
            secrets: tally.secrets(),
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

/// What the rules match in `bytes`, scanned on their own.
fn matched_in(bytes: &[u8]) -> Vec<Matched> {
    redact::matched_in(&String::from_utf8_lossy(bytes))
}

/// How many bytes of `bytes` hold whole characters: all of them, unless their last character
/// is cut off, which then goes whole to the next window rather than read as a stand-in here.
fn whole_characters(bytes: &[u8]) -> usize {
    let last = (bytes.len().saturating_sub(4)..bytes.len())
        .rev()
        .find(|&at| !is_continuation(bytes[at]));
    match last {
        Some(at) if bytes.len() - at < utf8_length(bytes[at]) => at,
        _ => bytes.len(),
    }
}

/// Where the character that `at` falls in starts, or the next one if `at` cuts one, so that a
/// window starts with a whole character.
fn character_start(bytes: &[u8], at: usize) -> usize {
    let ahead = &bytes[at.min(bytes.len())..];
    match ahead
        .iter()
        .take(4)
        .position(|&byte| !is_continuation(byte))
    {
        Some(skipped) => at + skipped,
        None if ahead.len() < 4 => bytes.len(),
        None => at,
    }
}

fn is_continuation(byte: u8) -> bool {
    byte & 0xc0 == 0x80
}

/// How many bytes the UTF-8 character that `first` starts takes.
fn utf8_length(first: u8) -> usize {
    match first {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// What a long line's windows find, less what their overlaps find on their own, secret by secret,
/// by its kind and its print. A secret in an overlap is found by both its windows and by the
/// overlap, so it counts once, as long as what's around it differs on one side only. A scan can
/// still count differently at an edge, for example when a window has used up its searches again
/// inside implausible matches, so no secret ever comes out below the most that one scan, of a
/// window or an overlap, found: the subtraction may lower a count, but never takes away a secret
/// a scan found.
///
/// A match that runs to a window's end may be cut there, so the window leaves it to the next one,
/// which starts [`OVERLAP`] bytes earlier and holds it whole when it starts there. When the next
/// window has no match of its kind that begins with its text, it counts once, with a print of only
/// the part the window saw. At the line's end it isn't cut, and counts as found.
#[derive(Default)]
struct Tally {
    /// Each secret found, in the order found, with how often.
    found: Vec<(Secret, Count)>,
    /// Where each secret is in `found`, by its kind and print.
    index: HashMap<(String, Fingerprint), usize>,
    /// The matches that ran to the end of the last window scanned.
    to_end: Vec<Matched>,
}

/// How often the scans of a long line found one secret.
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
        self.window(scanned.matched);
        scanned.ends_with_begin
    }

    /// Counts what the rules `matched` in a window, after the window before.
    fn window(&mut self, matched: Vec<Matched>) {
        self.settle_cut(&matched);
        let (to_end, inside): (Vec<_>, Vec<_>) = matched.into_iter().partition(|m| m.to_end);
        self.count(inside.into_iter().map(|matched| matched.secret), |count| {
            &mut count.windows
        });
        self.to_end = to_end;
    }

    /// Counts what an overlap, scanned on its own, holds inside it. What runs to its end runs to
    /// its window's end too, where the window leaves it to the next one.
    fn overlap(&mut self, matched: Vec<Matched>) {
        let inside = matched.into_iter().filter(|matched| !matched.to_end);
        self.count(inside.map(|matched| matched.secret), |count| {
            &mut count.overlaps
        });
    }

    /// Settles the matches that ran to the end of the window before, now that `next`, the next
    /// window's, are known: one that a match of its kind there begins with is counted there; any
    /// other counts as found, by a print of the part the window saw.
    fn settle_cut(&mut self, next: &[Matched]) {
        let cut = std::mem::take(&mut self.to_end).into_iter().filter(|cut| {
            !next.iter().any(|matched| {
                matched.secret.kind == cut.secret.kind && matched.text.starts_with(&cut.text)
            })
        });
        let partial = cut.map(|cut| {
            let mut print = Key::for_run().start();
            print.feed(cut.text.as_bytes());
            Secret {
                kind: cut.secret.kind,
                print: print.finish_partial(),
            }
        });
        let partial: Vec<Secret> = partial.collect();
        self.count(partial.into_iter(), |count| &mut count.windows);
    }

    /// Counts `secrets`, which one scan found, with `field` the count to add to.
    fn count(
        &mut self,
        secrets: impl Iterator<Item = Secret>,
        field: fn(&mut Count) -> &mut usize,
    ) {
        let mut times: HashMap<usize, usize> = HashMap::new();
        let mut order = Vec::new();
        for secret in secrets {
            let at = self.place_of(secret);
            let seen = times.entry(at).or_default();
            if *seen == 0 {
                order.push(at);
            }
            *seen += 1;
        }
        for at in order {
            let count = &mut self.found[at].1;
            let n = times[&at];
            *field(count) += n;
            count.most = count.most.max(n);
        }
    }

    /// Where `secret` is in `found`, which it joins if it's new.
    fn place_of(&mut self, secret: Secret) -> usize {
        let found = &mut self.found;
        *self
            .index
            .entry((secret.kind.clone(), secret.print))
            .or_insert_with(|| {
                found.push((secret, Count::default()));
                found.len() - 1
            })
    }

    /// The secrets found, kind by kind, each as often as it counts. What ran to the end of the last
    /// window ran to the line's end, and counts as found.
    fn secrets(mut self) -> Vec<Secret> {
        let last = std::mem::take(&mut self.to_end);
        self.count(last.into_iter().map(|matched| matched.secret), |count| {
            &mut count.windows
        });
        let mut found = self.found;
        found.sort_by(|(one, _), (other, _)| one.kind.cmp(&other.kind));
        found
            .into_iter()
            .flat_map(|(secret, count)| std::iter::repeat_n(secret, count.counted()))
            .collect()
    }
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

        let whole = secrets_in(&line);
        assert_eq!(whole.len(), 4);
        let (long, fed) = read[0].1.as_ref().unwrap();
        assert_eq!((&long.secrets, *fed), (&whole, length));
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

    /// What a scan of all of `line` at once finds.
    fn secrets_in(line: &[u8]) -> Vec<Secret> {
        redact::matched_in(&String::from_utf8_lossy(line))
            .into_iter()
            .map(|matched| matched.secret)
            .collect()
    }

    /// A match of the secret `text`, of the kind "token", that runs to its window's end or not.
    fn token(text: &str, to_end: bool) -> Matched {
        Matched {
            secret: Secret {
                kind: "token".to_owned(),
                print: Key::for_run().of(text.as_bytes()),
            },
            text: text.to_owned(),
            to_end,
        }
    }

    /// What the windows of `line` find, read as one long line.
    fn found_in_windows(line: &[u8]) -> Vec<Secret> {
        let read = read(&[line, b"\n"].concat());
        read[0].1.as_ref().unwrap().0.secrets.clone()
    }

    /// `length` bytes of padding with `text` put at `at`.
    fn line_with(length: usize, at: &[(usize, String)]) -> Vec<u8> {
        let mut line = vec![b'x'; length];
        for (at, text) in at {
            line[*at..*at + text.len()].copy_from_slice(text.as_bytes());
        }
        line
    }

    /// Tokens at an overlap's first byte and just before it, and two different tokens of one kind
    /// inside one overlap, are each found once, as a scan of the whole line finds them.
    #[test]
    fn finds_what_lies_at_and_in_an_overlap_once() {
        let noise = crate::secrets::test_support::noise;
        let overlap = WINDOW - OVERLAP;
        let token = |tag: usize| format!(" ghp_{} ", &noise(40 + tag)[tag..]);
        for at in [overlap - 1, overlap, overlap - 5] {
            let line = line_with(WINDOW + 5000, &[(at, token(0))]);
            assert_eq!(found_in_windows(&line), secrets_in(&line), "at {at}");
        }
        let two = line_with(
            WINDOW + 5000,
            &[(overlap + 100, token(1)), (overlap + 300, token(2))],
        );
        assert_eq!(found_in_windows(&two), secrets_in(&two));
        assert_eq!(secrets_in(&two).len(), 2);
    }

    /// A token of a window's length or more that starts inside an overlap is found once, by a
    /// partial print, since no window holds it whole.
    #[test]
    fn finds_a_token_longer_than_a_window_once() {
        let noise = crate::secrets::test_support::noise;
        let at = 2 * (WINDOW - OVERLAP) + 100;
        let line = line_with(4 * WINDOW, &[(at, format!(" ghp_{} ", noise(WINDOW)))]);
        let (found, whole) = (found_in_windows(&line), secrets_in(&line));
        assert_eq!((found.len(), whole.len()), (1, 1));
        assert_eq!(found[0].kind, whole[0].kind);
        assert_ne!(found[0].print, whole[0].print);
    }

    /// A character that a window's end splits goes whole to the next window, so a secret it's in
    /// or next to is found once, as a scan of the whole line finds it, at every byte it can be
    /// split at.
    #[test]
    fn finds_a_secret_with_a_character_split_by_a_window_once() {
        let noise = crate::secrets::test_support::noise;
        let around = [
            (format!(" db_{}=Ab12cd34", "password"), "ef56gh ".to_owned()),
            (format!(" ghp_{}", noise(40)), format!("{} ", noise(20))),
        ];
        let cases = ['é', '\u{200b}', '\u{3000}']
            .into_iter()
            .flat_map(|character| {
                let around = around.clone();
                (1..character.len_utf8()).flat_map(move |cut| {
                    around
                        .clone()
                        .into_iter()
                        .map(move |(before, after)| (character, cut, before, after))
                })
            });
        for (character, cut, before, after) in cases {
            let text = format!("{before}{character}{after}");
            let line = line_with(WINDOW + 5000, &[(WINDOW - cut - before.len(), text)]);
            let case = format!("{character:?} cut after {cut} byte(s) in {before:?}");
            assert_eq!(found_in_windows(&line), secrets_in(&line), "{case}");
        }
    }

    /// A window ends before a character it would cut, and the next one starts at a whole
    /// character, whatever their lengths in bytes.
    #[test]
    fn cuts_windows_between_characters() {
        for character in ['a', 'é', '\u{200b}', '\u{1f600}'] {
            let bytes = format!("xy{character}").into_bytes();
            assert_eq!(whole_characters(&bytes), bytes.len(), "{character:?}");
            for cut in 1..character.len_utf8() {
                assert_eq!(
                    whole_characters(&bytes[..2 + cut]),
                    2,
                    "{character:?} at {cut}"
                );
                assert_eq!(
                    character_start(&bytes, 2 + cut),
                    bytes.len(),
                    "{character:?}"
                );
            }
            assert_eq!(character_start(&bytes, 2), 2);
        }
        assert_eq!(whole_characters(b""), 0);
        assert_eq!(character_start(b"", 0), 0);
    }

    /// Many different secrets on one long line are counted in time that grows with how many there
    /// are, not with its square: a few seconds here in a debug build, where counting them two by
    /// two took minutes.
    #[test]
    fn counts_many_secrets_on_one_long_line_quickly() {
        let line: String = (0..100_000).map(|n| format!(" AKIA{n:0>16} ")).collect();
        let started = std::time::Instant::now();
        let found = found_in_windows(line.as_bytes());
        assert_eq!(found.len(), secrets_in(line.as_bytes()).len());
        assert!(started.elapsed().as_secs() < 30, "{:?}", started.elapsed());
    }

    /// What the scans of `windows`, between which each of `overlaps` lies, find, as a [`Tally`]
    /// counts it; each scan finds a secret of each kind named, the same for the same name.
    fn tally(windows: &[&[&str]], overlaps: &[&[&str]]) -> Vec<String> {
        let found = |kinds: &[&str]| -> Vec<Matched> {
            kinds
                .iter()
                .map(|kind| Matched {
                    secret: Secret {
                        kind: (*kind).to_owned(),
                        ..token(kind, false).secret
                    },
                    ..token(kind, false)
                })
                .collect()
        };
        let mut tally = Tally::default();
        tally.window(found(windows[0]));
        for (overlap, window) in overlaps.iter().zip(&windows[1..]) {
            tally.overlap(found(overlap));
            tally.window(found(window));
        }
        tally
            .secrets()
            .into_iter()
            .map(|secret| secret.kind)
            .collect()
    }

    /// A match cut at a window's end that the next window finds whole counts once, by the whole
    /// one's print; one the next window doesn't find counts once, by a partial print; at the
    /// line's end nothing is cut.
    #[test]
    fn counts_a_match_cut_at_a_window_end_once() {
        let whole = token("ghp_abcdef", false);
        let mut tally = Tally::default();
        tally.window(vec![token("ghp_abc", true)]);
        tally.window(vec![whole.clone()]);
        assert_eq!(tally.secrets(), std::slice::from_ref(&whole.secret));

        let mut unseen = Tally::default();
        unseen.window(vec![token("ghp_abc", true)]);
        unseen.window(vec![token("other", false)]);
        let found = unseen.secrets();
        assert_eq!(found.len(), 2);
        assert_ne!(found[1].print, token("ghp_abc", false).secret.print);
        assert_ne!(found[1].print, whole.secret.print);

        let mut last = Tally::default();
        last.window(vec![token("ghp_abc", true)]);
        assert_eq!(last.secrets(), [token("ghp_abc", false).secret]);
    }

    /// Two different secrets of one kind in one overlap count as two; one in an overlap, once.
    #[test]
    fn tells_secrets_of_one_kind_apart_by_their_prints() {
        let (one, two) = (token("ghp_one", false), token("ghp_two", false));
        let mut tally = Tally::default();
        tally.window(vec![one.clone(), two.clone()]);
        tally.overlap(vec![one.clone(), two.clone()]);
        tally.window(vec![one.clone(), two.clone()]);
        assert_eq!(tally.secrets(), [one.secret, two.secret]);
    }

    /// Tokens that a window's end cuts are found once each, as a scan of the whole line finds
    /// them: by the whole token's print when the next window holds its start, one across an edge
    /// and one longer than half an overlap; and by a partial print when no window holds it whole,
    /// one longer than an overlap.
    #[test]
    fn finds_a_token_a_window_cuts_once() {
        let noise = crate::secrets::test_support::noise;
        let mut line = vec![b'x'; 3 * WINDOW + 100];
        let mut put = |at: usize, length: usize| {
            let token = format!(" ghp_{} ", noise(length));
            line[at..at + token.len()].copy_from_slice(token.as_bytes());
        };
        put(WINDOW - 20, 40);
        put(2 * WINDOW - OVERLAP - 100, OVERLAP / 2);
        put(3 * WINDOW - 3 * OVERLAP - 200, OVERLAP + 1000);

        let read = read(&[&line[..], b"\n"].concat());

        let whole = secrets_in(&line);
        assert_eq!(whole.len(), 3, "{whole:?}");
        let found = &read[0].1.as_ref().unwrap().0.secrets;
        let kinds = |secrets: &[Secret]| -> Vec<String> {
            secrets.iter().map(|secret| secret.kind.clone()).collect()
        };
        assert_eq!(kinds(found), kinds(&whole));
        assert_eq!(found[..2], whole[..2]);
        assert_ne!(found[2], whole[2]);
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
