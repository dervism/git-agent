//! Secrets in the whole content of the files a diff shows only as binary: those git finds a NUL
//! byte in, and those `.gitattributes` marks `-diff` or `binary`. A diff shows no lines of them,
//! so their blobs are read instead.

use super::redact::{self, Change, Finding, SecretLine};
use crate::git::blob::BlobBatch;
use crate::git::{Repo, names};
use anyhow::{Context, Result, bail};
use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, BufRead, BufReader, Cursor, Read};
use std::rc::Rc;

/// The most of a binary file that is read. A larger one isn't checked.
pub const MOST_BINARY_BYTES: u64 = 16 * 1024 * 1024;

/// How much of a file git looks at to tell text from binary: text has no NUL byte there.
const TEXT_TEST_BYTES: u64 = 8000;

/// The shortest run of printable characters in a binary file that is scanned. No secret the
/// rules know is shorter.
const SHORTEST_RUN: usize = 8;

/// A file a diff shows as binary, with the blobs its `--full-index` lines name.
#[derive(Debug, Clone, PartialEq)]
pub struct BinaryFile {
    /// Where it is, as a finding names it.
    pub place: String,
    /// The blob of its new content.
    pub result: String,
    /// The blobs of what it was in each parent that had it.
    pub parents: Vec<String>,
}

/// A binary file over [`MOST_BINARY_BYTES`], which isn't checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Unchecked {
    pub place: String,
    pub size: u64,
}

/// What a check read: the secrets it found, and the files it couldn't check.
#[derive(Debug, Default, PartialEq)]
pub struct Scanned {
    pub findings: Vec<Finding>,
    pub unchecked: Vec<Unchecked>,
}

/// What a blob holds, as far as secrets go.
enum Content {
    /// The lines that hold secrets: a text file's lines, or a binary file's printable runs.
    Read { lines: Vec<Secrets>, text: bool },
    /// A binary blob over the cap, of this size.
    TooLarge(u64),
}

/// A line that holds secrets, kept by its fingerprint rather than its text, which can be as long
/// as the file, so that a scan of many blobs holds little of each.
struct Secrets {
    number: usize,
    fingerprint: Fingerprint,
    kinds: Vec<String>,
}

/// What tells one line from another: 128 bits, keyed afresh for each scan, so that no text can
/// be made to share another's, which would turn an added secret into an unchanged one.
type Fingerprint = u128;

/// Gives lines their fingerprints, with a key of their own.
struct Fingerprints(RandomState);

impl Fingerprints {
    fn of(&self, text: &str) -> Fingerprint {
        let half = |part: u8| {
            let mut hasher = self.0.build_hasher();
            hasher.write_u8(part);
            hasher.write(text.as_bytes());
            hasher.finish()
        };
        (u128::from(half(0)) << 64) | u128::from(half(1))
    }

    /// `lines`, each kept by its fingerprint.
    fn of_lines(&self, lines: Vec<SecretLine>) -> Vec<Secrets> {
        let secrets = |line: SecretLine| Secrets {
            number: line.number,
            fingerprint: self.of(&line.text),
            kinds: line.kinds,
        };
        lines.into_iter().map(secrets).collect()
    }
}

/// The files whose new content `diff` names without showing it: those with an `index` line and
/// no hunk, as git shows a binary file, whatever made it binary. A deleted file adds nothing, so
/// it isn't one of them. Fails on a binary file without the blobs to read.
pub fn binary_files(diff: &str) -> Result<Vec<BinaryFile>> {
    let mut files = Vec::new();
    let mut section = Section::default();
    for line in diff.lines() {
        if ["diff --git ", "diff --cc ", "diff --combined "]
            .iter()
            .any(|start| line.starts_with(start))
        {
            files.extend(section.unshown()?);
            section = Section {
                place: names::of_diff(line),
                ..Section::default()
            };
        } else if let Some(index) = line.strip_prefix("index ") {
            section.blobs = index_blobs(index);
        } else if line.starts_with("@@") {
            section.hunks = true;
        } else if line.starts_with("Binary files ") {
            section.binary = true;
        }
    }
    files.extend(section.unshown()?);
    Ok(files)
}

/// What a diff says of one file, as far as reading it whole goes.
#[derive(Default)]
struct Section {
    place: String,
    blobs: Option<(Vec<String>, String)>,
    hunks: bool,
    binary: bool,
}

impl Section {
    /// The file, if the diff names its new content without showing it.
    fn unshown(self) -> Result<Option<BinaryFile>> {
        match self.blobs {
            Some((parents, result)) if !self.hunks && !is_missing(&result) => {
                Ok(Some(BinaryFile {
                    place: self.place,
                    result,
                    parents: parents
                        .into_iter()
                        .filter(|hash| !is_missing(hash))
                        .collect(),
                }))
            }
            None if self.binary => bail!(
                "git showed {} as binary without naming its content, so git-agent couldn't \
                 check it",
                self.place
            ),
            _ => Ok(None),
        }
    }
}

/// The blobs an `index` line names: one before `..` for each parent, and the result after it,
/// as in `1a2b..3c4d 100644`, or `1a2b,5e6f..3c4d` for a merge.
fn index_blobs(index: &str) -> Option<(Vec<String>, String)> {
    let (parents, result) = index.split_whitespace().next()?.split_once("..")?;
    let parents = parents.split(',').map(str::to_owned).collect();
    Some((parents, result.to_owned()))
}

/// Whether `hash` is git's all-zero stand-in for a file that isn't there.
fn is_missing(hash: &str) -> bool {
    hash.bytes().all(|byte| byte == b'0')
}

/// The secrets in the whole content of `files`, each finding added unless a parent's blob holds
/// the same line. A binary file over `cap` bytes isn't read, and is listed as unchecked. Every
/// size is read at once and each blob once, however many files and commits have it. The parents'
/// sizes are read up front too, so a parent git can't read fails the check even where no finding
/// needs it.
pub fn scan(repo: &Repo, files: &[BinaryFile], cap: u64) -> Result<Scanned> {
    scan_from(&mut GitBlobs { repo, batch: None }, files, cap)
}

/// [`scan`], reading the blobs from `blobs`.
fn scan_from(blobs: &mut impl Blobs, files: &[BinaryFile], cap: u64) -> Result<Scanned> {
    if files.is_empty() {
        return Ok(Scanned::default());
    }
    let mut contents = Contents::new(blobs, files, cap)?;
    files
        .iter()
        .try_fold(Scanned::default(), |mut scanned, file| {
            match &*contents.of(&file.result)? {
                Content::Read { lines, text } => {
                    scanned
                        .findings
                        .extend(findings_in(&mut contents, file, lines, *text)?)
                }
                Content::TooLarge(size) => scanned.unchecked.push(Unchecked {
                    place: file.place.clone(),
                    size: *size,
                }),
            }
            contents.done_with(file);
            Ok(scanned)
        })
}

/// The blobs `file` names: its new content's, then each parent's.
fn blobs_of(file: &BinaryFile) -> impl Iterator<Item = &String> {
    std::iter::once(&file.result).chain(&file.parents)
}

/// Where a scan reads blobs: the sizes of many at once, one up to the cap whole, and a larger one
/// as a stream, so that a binary one can be left after its first bytes.
trait Blobs {
    fn sizes(&mut self, hashes: &[String]) -> Result<HashMap<String, u64>>;
    fn read(&mut self, hash: &str, size: u64) -> Result<Vec<u8>>;
    fn stream(&mut self, hash: &str) -> Result<Box<dyn Read>>;
}

/// The repository's blobs: every size from one `git cat-file`, every blob up to the cap from
/// one more, started when the first is read, and each larger one from one of its own.
struct GitBlobs<'r> {
    repo: &'r Repo,
    batch: Option<BlobBatch>,
}

impl Blobs for GitBlobs<'_> {
    fn sizes(&mut self, hashes: &[String]) -> Result<HashMap<String, u64>> {
        self.repo.blob_sizes(hashes)
    }

    fn read(&mut self, hash: &str, size: u64) -> Result<Vec<u8>> {
        let batch = match self.batch.take() {
            Some(batch) => batch,
            None => self.repo.blob_batch()?,
        };
        self.batch.insert(batch).read(hash, size)
    }

    fn stream(&mut self, hash: &str) -> Result<Box<dyn Read>> {
        Ok(Box::new(self.repo.open_blob(hash)?))
    }
}

/// What each blob holds, read once however many files have it, and held only until the last file
/// that names it.
struct Contents<'b, B> {
    blobs: &'b mut B,
    sizes: HashMap<String, u64>,
    read: HashMap<String, Rc<Content>>,
    /// How many of the files still to scan name each blob, as their content or a parent's.
    uses: BTreeMap<String, usize>,
    cap: u64,
    fingerprints: Fingerprints,
}

impl<'b, B: Blobs> Contents<'b, B> {
    /// What the blobs of `files` hold, read from `blobs`, with every size read at once.
    fn new(blobs: &'b mut B, files: &[BinaryFile], cap: u64) -> Result<Self> {
        let uses = files
            .iter()
            .flat_map(blobs_of)
            .fold(BTreeMap::new(), |mut uses, hash| {
                *uses.entry(hash.clone()).or_insert(0) += 1;
                uses
            });
        let hashes: Vec<String> = uses.keys().cloned().collect();
        Ok(Self {
            sizes: blobs.sizes(&hashes)?,
            blobs,
            read: HashMap::new(),
            uses,
            cap,
            fingerprints: Fingerprints(RandomState::new()),
        })
    }

    /// Forgets what the blobs of `file`, now scanned, hold, unless a file still to scan names
    /// them, so that a scan holds only what it will need again.
    fn done_with(&mut self, file: &BinaryFile) {
        for hash in blobs_of(file) {
            if self.last_use(hash) {
                self.read.remove(hash);
            }
        }
    }

    /// Counts one use of the blob `hash` as done, and says whether it was the last.
    fn last_use(&mut self, hash: &str) -> bool {
        self.uses.get_mut(hash).is_some_and(|left| {
            *left = left.saturating_sub(1);
            *left == 0
        })
    }

    /// What the blob `hash` holds: its secret lines, or that it's a binary over the cap.
    fn of(&mut self, hash: &str) -> Result<Rc<Content>> {
        if let Some(content) = self.read.get(hash) {
            return Ok(Rc::clone(content));
        }
        let size = *self
            .sizes
            .get(hash)
            .with_context(|| format!("git gave no size for the blob {hash}"))?;
        let content = Rc::new(if size <= self.cap {
            content_of(&self.blobs.read(hash, size)?, &self.fingerprints)?
        } else {
            streamed_content(self.blobs.stream(hash)?, size, &self.fingerprints)?
        });
        self.read.insert(hash.to_owned(), Rc::clone(&content));
        Ok(content)
    }
}

/// The findings of `lines`, the secret lines of `file`'s new content.
fn findings_in(
    contents: &mut Contents<'_, impl Blobs>,
    file: &BinaryFile,
    lines: &[Secrets],
    text: bool,
) -> Result<Vec<Finding>> {
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let held = held_by_parents(contents, &file.parents)?;
    let findings = lines.iter().flat_map(|line| {
        let change = match &held {
            Some(held) if held.contains(&line.fingerprint) => Change::Unchanged,
            _ => Change::Added,
        };
        let number = text.then_some(line.number);
        line.kinds.iter().map(move |kind| Finding {
            place: file.place.clone(),
            line: number,
            change,
            kind: kind.clone(),
        })
    });
    Ok(findings.collect())
}

/// The lines with secrets that the blobs of `parents` hold, by their fingerprints, or `None` when
/// one is a binary over the cap, so that every finding counts as added.
fn held_by_parents(
    contents: &mut Contents<'_, impl Blobs>,
    parents: &[String],
) -> Result<Option<HashSet<Fingerprint>>> {
    parents
        .iter()
        .try_fold(Some(HashSet::new()), |held, parent| {
            Ok(match (held, &*contents.of(parent)?) {
                (Some(mut held), Content::Read { lines, .. }) => {
                    held.extend(lines.iter().map(|line| line.fingerprint));
                    Some(held)
                }
                _ => None,
            })
        })
}

/// What a blob read whole holds: every line of a text file, or the printable runs of a binary
/// one. Text is told from binary as git does.
fn content_of(bytes: &[u8], fingerprints: &Fingerprints) -> io::Result<Content> {
    let head = &bytes[..bytes.len().min(TEXT_TEST_BYTES as usize)];
    let text = !head.contains(&0);
    let lines = if text {
        secret_lines_of(bytes)?
    } else {
        redact::secret_lines(printable_runs(bytes))
    };
    Ok(Content::Read {
        lines: fingerprints.of_lines(lines),
        text,
    })
}

/// What a blob over the cap holds, read from `content`: every line of a text file, however
/// large; a binary one, of `size` bytes, is left unread after its first bytes.
fn streamed_content(
    mut content: impl Read,
    size: u64,
    fingerprints: &Fingerprints,
) -> io::Result<Content> {
    let mut head = Vec::new();
    (&mut content)
        .take(TEXT_TEST_BYTES)
        .read_to_end(&mut head)?;
    if head.contains(&0) {
        return Ok(Content::TooLarge(size));
    }
    let lines = secret_lines_of(BufReader::new(Cursor::new(head).chain(content)))?;
    Ok(Content::Read {
        lines: fingerprints.of_lines(lines),
        text: true,
    })
}

/// The secret lines of text read from `reader`, a line at a time.
fn secret_lines_of(reader: impl BufRead) -> io::Result<Vec<SecretLine>> {
    let mut failure = None;
    let lines = reader.split(b'\n').map_while(|line| match line {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(error) => {
            failure = Some(error);
            None
        }
    });
    let found = redact::secret_lines(lines);
    failure.map_or(Ok(found), Err)
}

/// The runs of printable ASCII characters in `bytes` that are long enough to hold a secret, as
/// `strings` finds them: one byte to a character, and in UTF-16 as Windows and .NET write it,
/// little-endian from either byte, and big-endian after its byte order mark.
fn printable_runs(bytes: &[u8]) -> Vec<String> {
    let mut runs = runs_of(bytes.iter().map(|byte| Some(*byte)));
    for start in [0, 1] {
        let (pairs, _) = bytes.get(start..).unwrap_or_default().as_chunks::<2>();
        runs.extend(runs_of(
            pairs.iter().map(|[low, high]| (*high == 0).then_some(*low)),
        ));
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        let (pairs, _) = bytes.as_chunks::<2>();
        runs.extend(runs_of(
            pairs.iter().map(|[high, low]| (*high == 0).then_some(*low)),
        ));
    }
    runs
}

/// The runs of printable characters in `characters` long enough to hold a secret. `None`, like a
/// byte that isn't printable, ends a run.
fn runs_of(characters: impl Iterator<Item = Option<u8>>) -> Vec<String> {
    let mut runs = Vec::new();
    let mut run = Vec::new();
    for character in characters.chain(std::iter::once(None)) {
        match character.filter(|byte| *byte == b'\t' || (0x20..0x7f).contains(byte)) {
            Some(byte) => run.push(byte),
            None => {
                if run.len() >= SHORTEST_RUN {
                    runs.push(String::from_utf8_lossy(&run).into_owned());
                }
                run.clear();
            }
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support::{isolated_repo, run};
    use std::fs;
    use std::path::Path;

    fn key() -> String {
        format!("AKIA{}", "Q".repeat(16))
    }

    /// Writes `bytes` to `name` in `dir` and stores them as a blob, whose hash it returns.
    fn blob(dir: &Path, name: &str, bytes: &[u8]) -> String {
        fs::write(dir.join(name), bytes).unwrap();
        run(dir, &["hash-object", "-w", "--", name])
            .trim()
            .to_owned()
    }

    fn file(place: &str, result: &str, parents: &[&str]) -> BinaryFile {
        BinaryFile {
            place: place.to_owned(),
            result: result.to_owned(),
            parents: parents.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn finds_the_files_a_diff_shows_only_as_binary() {
        let (a, b, c, zero) = (
            "a".repeat(40),
            "b".repeat(40),
            "c".repeat(40),
            "0".repeat(40),
        );
        let diff = format!(
            "diff --git a/img.png b/img.png\nindex {a}..{b} 100644\nBinary files a/img.png and \
             b/img.png differ\ndiff --git a/new.bin b/new.bin\nnew file mode 100644\nindex \
             {zero}..{c}\nBinary files /dev/null and b/new.bin differ\ndiff --git a/gone.bin \
             b/gone.bin\ndeleted file mode 100644\nindex {a}..{zero}\nBinary files a/gone.bin \
             and /dev/null differ\ndiff --git a/notes.md b/notes.md\nindex {a}..{b} 100644\n\
             @@ -1 +1,2 @@\n x\n+index {a}..{c}\n+Binary files x and y differ\n\
             diff --cc merged.bin\nindex {a},{b}..{c}\nBinary files differ\ndiff --cc added.bin\n\
             index {zero},{a}..{c}\nBinary files differ\n"
        );

        assert_eq!(
            binary_files(&diff).unwrap(),
            [
                file("img.png", &b, &[&a]),
                file("new.bin", &c, &[]),
                file("merged.bin", &c, &[&a, &b]),
                file("added.bin", &c, &[&a]),
            ]
        );
        assert!(binary_files("").unwrap().is_empty());
    }

    /// Whatever git writes in place of the lines, a file whose new content is named and not
    /// shown is read; one shown as binary without its blobs fails rather than pass unread.
    #[test]
    fn reads_any_file_whose_content_the_diff_names_without_showing_it() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let unshown = format!("diff --git a/x.dat b/x.dat\nindex {a}..{b} 100644\nsomething new\n");
        assert_eq!(binary_files(&unshown).unwrap(), [file("x.dat", &b, &[&a])]);
        let moved = "diff --git a/x b/y\nsimilarity index 100%\nrename from x\nrename to y\n";
        let mode = "diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n";
        assert!(binary_files(&format!("{moved}{mode}")).unwrap().is_empty());
        let blind = "diff --git a/x.bin b/x.bin\nBinary files a/x.bin and b/x.bin differ\n";
        let error = binary_files(blind).unwrap_err();
        assert!(error.to_string().contains("x.bin"), "{error}");
    }

    #[test]
    fn takes_long_enough_runs_of_printable_characters_from_binary_content() {
        let bytes = b"\x00\x01short\x00a\ttabbed line\x00\xffcaf\xc3\xa9 au lait\x00\x7fsixchr";
        assert_eq!(printable_runs(bytes), ["a\ttabbed line", " au lait"]);
        assert!(printable_runs(b"").is_empty());
    }

    /// `text` in UTF-16, little-endian or big-endian.
    fn utf16(text: &str, little: bool) -> Vec<u8> {
        let bytes = |unit: u16| {
            if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            }
        };
        text.encode_utf16().flat_map(bytes).collect()
    }

    /// Windows and .NET write text as UTF-16, with a NUL byte beside every ASCII character.
    #[test]
    fn takes_runs_of_utf_16_text_from_binary_content() {
        let line = format!("AWS={}", key());
        let little = [&[0xff, 0xfe][..], &utf16(&line, true)].concat();
        assert_eq!(printable_runs(&little), std::slice::from_ref(&line));
        let odd = [&b"\0\x01\x02"[..], &utf16(&line, true), b"\0\0"].concat();
        assert_eq!(printable_runs(&odd), std::slice::from_ref(&line));
        let big = [&[0xfe, 0xff][..], &utf16(&line, false)].concat();
        assert!(printable_runs(&big).contains(&line));
        // Without its byte order mark, big-endian text isn't taken for text of its own.
        assert!(!printable_runs(&utf16(&line, false)).contains(&line));
    }

    #[test]
    fn reads_a_text_file_line_by_line_and_a_binary_one_by_its_printable_runs() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let text = blob(dir, "app.env", format!("A=1\nAWS={}\n", key()).as_bytes());
        let binary = blob(dir, "data.bin", format!("\0\0aws {}\0", key()).as_bytes());
        let files = [file("app.env", &text, &[]), file("data.bin", &binary, &[])];

        let scanned = scan(&repo, &files, MOST_BINARY_BYTES).unwrap();

        let found: Vec<(&str, Option<usize>, Change)> = scanned
            .findings
            .iter()
            .map(|finding| (finding.place.as_str(), finding.line, finding.change))
            .collect();
        assert_eq!(
            found,
            [
                ("app.env", Some(2), Change::Added),
                ("data.bin", None, Change::Added)
            ]
        );
        assert!(
            scanned
                .findings
                .iter()
                .all(|finding| finding.kind == "AWS access key")
        );
        assert!(scanned.unchecked.is_empty());
    }

    /// A finding is added only when no parent's blob holds the same line; a parent too large
    /// to read holds nothing that can be known.
    #[test]
    fn counts_a_secret_as_added_only_when_no_parent_holds_it() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let line = format!("aws {}", key());
        let old = blob(dir, "old.bin", format!("\0{line}\0").as_bytes());
        let other = blob(dir, "other.bin", b"\0nothing here\0");
        let new = blob(dir, "new.bin", format!("\0more\0{line}\0").as_bytes());
        let change_with = |parents: &[&str], cap: u64| {
            let scanned = scan(&repo, &[file("new.bin", &new, parents)], cap).unwrap();
            scanned.findings[0].change
        };

        assert_eq!(change_with(&[&old], MOST_BINARY_BYTES), Change::Unchanged);
        assert_eq!(
            change_with(&[&other, &old], MOST_BINARY_BYTES),
            Change::Unchanged
        );
        assert_eq!(change_with(&[&other], MOST_BINARY_BYTES), Change::Added);
        assert_eq!(change_with(&[], MOST_BINARY_BYTES), Change::Added);
        let big_old = blob(
            dir,
            "big.bin",
            format!("\0{line}\0{}", "x".repeat(200)).as_bytes(),
        );
        assert_eq!(change_with(&[&big_old], 100), Change::Added);
    }

    /// A binary file over the cap is listed rather than read; a text file is read whatever its
    /// size.
    #[test]
    fn lists_a_binary_file_over_the_cap_and_reads_any_text_file() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let binary = blob(
            dir,
            "big.bin",
            format!("\0aws {}\0{}", key(), "x".repeat(200)).as_bytes(),
        );
        let text = blob(
            dir,
            "big.env",
            format!("{}\naws {}\n", "x".repeat(200), key()).as_bytes(),
        );
        let files = [file("big.bin", &binary, &[]), file("big.env", &text, &[])];

        let scanned = scan(&repo, &files, 100).unwrap();

        let size = repo.blob_sizes(std::slice::from_ref(&binary)).unwrap()[&binary];
        assert_eq!(
            scanned.unchecked,
            [Unchecked {
                place: "big.bin".to_owned(),
                size
            }]
        );
        assert_eq!(scanned.findings.len(), 1);
        assert_eq!(
            (scanned.findings[0].place.as_str(), scanned.findings[0].line),
            ("big.env", Some(2))
        );
    }

    /// A private key is told from another by all of its data, since keys of one type begin
    /// with the same line: one put in place of another, or beside it, is added.
    #[test]
    fn counts_a_key_put_in_place_of_another_or_beside_it_as_added() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let key = |rest: &str| {
            format!(
                "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9u\n\
                 {rest}\n-----END OPENSSH PRIVATE KEY-----\n"
            )
        };
        let (old_key, new_key) = (key(&"A".repeat(64)), key(&"B".repeat(64)));
        let old = blob(dir, "old.key", format!("\0{old_key}").as_bytes());
        let replaced = blob(dir, "new.key", format!("\0{new_key}").as_bytes());
        let both = blob(
            dir,
            "both.key",
            format!("\0{old_key}\0{new_key}").as_bytes(),
        );
        let changes = |result: &str| -> Vec<Change> {
            let scanned = scan(
                &repo,
                &[file("deploy.key", result, &[&old])],
                MOST_BINARY_BYTES,
            );
            scanned
                .unwrap()
                .findings
                .iter()
                .map(|finding| finding.change)
                .collect()
        };

        assert_eq!(changes(&old), [Change::Unchanged]);
        assert_eq!(changes(&replaced), [Change::Added]);
        assert_eq!(changes(&both), [Change::Unchanged, Change::Added]);
    }

    /// A blob git can't read fails the scan, rather than pass for one without secrets.
    #[test]
    fn fails_on_a_blob_git_cannot_read() {
        let (_dir, repo) = isolated_repo();
        let missing = "1".repeat(40);
        assert!(scan(&repo, &[file("gone.bin", &missing, &[])], MOST_BINARY_BYTES).is_err());
        assert!(scan(&repo, &[file("odd.bin", "--batch", &[])], MOST_BINARY_BYTES).is_err());
    }

    /// A loose object whose end is damaged has a readable size, so the stream fails at its end.
    #[test]
    fn fails_on_a_blob_whose_content_git_cannot_read_to_the_end() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let content: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
        let hash = blob(dir, "data.bin", &content);
        let object = dir.join(".git/objects").join(&hash[..2]).join(&hash[2..]);
        let mut stored = fs::read(&object).unwrap();
        let end = stored.len();
        stored[end - 64..].fill(0x55);
        fs::remove_file(&object).unwrap();
        fs::write(&object, stored).unwrap();
        assert_eq!(
            repo.blob_sizes(std::slice::from_ref(&hash)).unwrap()[&hash],
            200_000
        );

        assert!(scan(&repo, &[file("data.bin", &hash, &[])], MOST_BINARY_BYTES).is_err());
        let text: Vec<u8> = content.iter().map(|byte| b'a' + byte % 26).collect();
        let streamed = blob(dir, "data.txt", &text);
        let object = dir
            .join(".git/objects")
            .join(&streamed[..2])
            .join(&streamed[2..]);
        let mut stored = fs::read(&object).unwrap();
        let end = stored.len();
        stored[end - 64..].fill(0x55);
        fs::remove_file(&object).unwrap();
        fs::write(&object, stored).unwrap();
        assert!(scan(&repo, &[file("data.txt", &streamed, &[])], 1000).is_err());
    }

    /// Blobs kept in memory, which count how a scan reads them.
    #[derive(Default)]
    struct Counted {
        blobs: HashMap<String, Vec<u8>>,
        sized: Vec<Vec<String>>,
        read: Vec<String>,
        streamed: Vec<String>,
    }

    impl Counted {
        fn with(blobs: &[(&str, &[u8])]) -> Self {
            let blobs = blobs
                .iter()
                .map(|(hash, bytes)| ((*hash).to_owned(), bytes.to_vec()))
                .collect();
            Self {
                blobs,
                ..Self::default()
            }
        }
    }

    impl Blobs for Counted {
        fn sizes(&mut self, hashes: &[String]) -> Result<HashMap<String, u64>> {
            self.sized.push(hashes.to_vec());
            let size = |hash: &String| self.blobs[hash].len() as u64;
            Ok(hashes
                .iter()
                .map(|hash| (hash.clone(), size(hash)))
                .collect())
        }

        fn read(&mut self, hash: &str, size: u64) -> Result<Vec<u8>> {
            self.read.push(hash.to_owned());
            assert_eq!(self.blobs[hash].len() as u64, size);
            Ok(self.blobs[hash].clone())
        }

        fn stream(&mut self, hash: &str) -> Result<Box<dyn Read>> {
            self.streamed.push(hash.to_owned());
            Ok(Box::new(Cursor::new(self.blobs[hash].clone())))
        }
    }

    /// A binary reverted and changed again across commits is read once, and every size is
    /// asked for at once, so that many files cost two git processes rather than two each.
    #[test]
    fn reads_every_size_at_once_and_each_blob_once() {
        let (old, new, plain) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        let (line, other) = (
            format!("aws {}", key()),
            format!("aws AKIA{}", "W".repeat(16)),
        );
        let (old_bytes, new_bytes) = (format!("\0{line}\0"), format!("\0{line}\0{other}\0"));
        let mut blobs = Counted::with(&[
            (&old, old_bytes.as_bytes()),
            (&new, new_bytes.as_bytes()),
            (&plain, b"\0nothing\0"),
        ]);
        let files = [
            file("commit 1 img.bin", &new, &[&old]),
            file("commit 2 img.bin", &old, &[&new]),
            file("commit 3 img.bin", &new, &[&old]),
            file("commit 3 other.bin", &plain, &[&old]),
        ];

        let scanned = scan_from(&mut blobs, &files, MOST_BINARY_BYTES).unwrap();

        assert_eq!(blobs.sized, [vec![old.clone(), new.clone(), plain.clone()]]);
        blobs.read.sort();
        assert_eq!(blobs.read, [old, new, plain]);
        assert!(blobs.streamed.is_empty());
        let changes: Vec<(&str, Change)> = scanned
            .findings
            .iter()
            .map(|finding| (finding.place.as_str(), finding.change))
            .collect();
        assert_eq!(
            changes,
            [
                ("commit 1 img.bin", Change::Unchanged),
                ("commit 1 img.bin", Change::Added),
                ("commit 2 img.bin", Change::Unchanged),
                ("commit 3 img.bin", Change::Unchanged),
                ("commit 3 img.bin", Change::Added),
            ]
        );
    }

    /// Only a blob over the cap is read as a stream: a binary one is left after its first
    /// bytes and listed, and a text one is read whole.
    #[test]
    fn streams_only_the_blobs_over_the_cap() {
        let (small, big_binary, big_text) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        let small_bytes = format!("\0aws {}\0", key());
        let binary_bytes = format!("\0aws {}\0{}", key(), "x".repeat(200));
        let text_bytes = format!("{}\naws {}\n", "x".repeat(200), key());
        let mut blobs = Counted::with(&[
            (&small, small_bytes.as_bytes()),
            (&big_binary, binary_bytes.as_bytes()),
            (&big_text, text_bytes.as_bytes()),
        ]);
        let files = [
            file("small.bin", &small, &[]),
            file("big.bin", &big_binary, &[]),
            file("big.env", &big_text, &[]),
        ];

        let scanned = scan_from(&mut blobs, &files, 100).unwrap();

        assert_eq!(blobs.read, [small]);
        assert_eq!(blobs.streamed, [big_binary, big_text]);
        let size = binary_bytes.len() as u64;
        let listed = Unchecked {
            place: "big.bin".to_owned(),
            size,
        };
        assert_eq!(scanned.unchecked, [listed]);
        let found: Vec<(&str, Option<usize>)> = scanned
            .findings
            .iter()
            .map(|finding| (finding.place.as_str(), finding.line))
            .collect();
        assert_eq!(found, [("small.bin", None), ("big.env", Some(2))]);
    }

    #[test]
    fn asks_for_nothing_without_files() {
        let mut blobs = Counted::default();
        assert_eq!(scan_from(&mut blobs, &[], 100).unwrap(), Scanned::default());
        assert!(blobs.sized.is_empty());
    }

    /// A parent's size is read up front with the others, so one git can't read fails the check
    /// even when the file's content holds no secret that would need it.
    #[test]
    fn fails_on_a_parent_git_cannot_read_even_when_no_finding_needs_it() {
        let (dir, repo) = isolated_repo();
        let plain = blob(dir.path(), "plain.bin", b"\0nothing here\0");
        let files = [file("plain.bin", &plain, &["1".repeat(40).as_str()])];
        assert!(scan(&repo, &files, MOST_BINARY_BYTES).is_err());
    }

    /// The blob is read as it is: a replacement of the same size, which `git replace` would
    /// show instead, hides nothing.
    #[test]
    fn reads_a_blob_and_not_its_replacement() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let keyed = format!("\0aws {}\0", key());
        let hidden = blob(dir, "keyed.bin", keyed.as_bytes());
        let plain = format!("\0{}\0", "x".repeat(keyed.len() - 2));
        let stand_in = blob(dir, "plain.bin", plain.as_bytes());
        run(dir, &["replace", &hidden, &stand_in]);

        let scanned = scan(&repo, &[file("keyed.bin", &hidden, &[])], MOST_BINARY_BYTES).unwrap();

        assert_eq!(scanned.findings.len(), 1);
    }

    /// Every blob up to the cap comes from one git, however many are read.
    #[test]
    fn reads_every_blob_up_to_the_cap_through_one_git() {
        let (dir, repo) = isolated_repo();
        let one = blob(dir.path(), "1.bin", b"\0one\0");
        let two = blob(dir.path(), "2.bin", b"\0two\0");
        let mut blobs = GitBlobs {
            repo: &repo,
            batch: None,
        };

        blobs.read(&one, 5).unwrap();
        let first = blobs.batch.as_ref().map(BlobBatch::id);
        blobs.read(&two, 5).unwrap();

        assert!(first.is_some());
        assert_eq!(blobs.batch.as_ref().map(BlobBatch::id), first);
    }

    /// A blob is held only until the last file that names it, as content or parent, is scanned.
    #[test]
    fn forgets_each_blob_after_the_last_file_that_names_it() {
        let (one, two) = ("a".repeat(40), "b".repeat(40));
        let line = format!("\0aws {}\0", key());
        let mut blobs = Counted::with(&[(&one, line.as_bytes()), (&two, line.as_bytes())]);
        let files = [file("1.bin", &one, &[]), file("2.bin", &two, &[&one])];
        let mut contents = Contents::new(&mut blobs, &files, MOST_BINARY_BYTES).unwrap();

        contents.of(&one).unwrap();
        contents.done_with(&files[0]);
        assert!(contents.read.contains_key(&one));
        contents.of(&two).unwrap();
        contents.of(&one).unwrap();
        contents.done_with(&files[1]);

        assert!(contents.read.is_empty());
        assert_eq!(blobs.read, [one, two]);
    }

    /// The size is read as the blob is too: a stand-in over the cap, which `git replace` would
    /// show instead, can't make the blob be listed as unchecked rather than read.
    #[test]
    fn sizes_a_blob_and_not_its_replacement() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let hidden = blob(dir, "keyed.bin", format!("\0aws {}\0", key()).as_bytes());
        let large = blob(
            dir,
            "large.bin",
            format!("\0{}\0", "x".repeat(200)).as_bytes(),
        );
        run(dir, &["replace", &hidden, &large]);

        let scanned = scan(&repo, &[file("keyed.bin", &hidden, &[])], 100).unwrap();

        assert!(scanned.unchecked.is_empty(), "{:?}", scanned.unchecked);
        assert_eq!(scanned.findings.len(), 1);
    }

    /// Each scan keys its fingerprints afresh, so that no content can be made to share one with
    /// another line.
    #[test]
    fn keys_the_fingerprints_of_each_scan_afresh() {
        let one = Fingerprints(RandomState::new());
        let two = Fingerprints(RandomState::new());
        let line = format!("aws {}", key());

        assert_eq!(one.of(&line), one.of(&line));
        assert_ne!(one.of(&line), one.of("aws other"));
        assert_ne!(one.of(&line), two.of(&line));
    }
}
