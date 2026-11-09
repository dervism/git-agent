//! Secrets in the whole content of the files a diff shows only as binary: those git finds a NUL
//! byte in, and those `.gitattributes` marks `-diff` or `binary`. A diff shows no lines of them,
//! so their blobs are read instead.

use super::redact::{self, Change, Finding, SecretLine};
use crate::git::{Repo, names};
use anyhow::{Result, bail};
use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Cursor, Read};

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
    Read { lines: Vec<SecretLine>, text: bool },
    /// A binary blob over the cap, of this size.
    TooLarge(u64),
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
/// the same line. A binary file over `cap` bytes isn't read, and is listed as unchecked.
pub fn scan(repo: &Repo, files: &[BinaryFile], cap: u64) -> Result<Scanned> {
    files
        .iter()
        .try_fold(Scanned::default(), |mut scanned, file| {
            match secrets_in(repo, &file.result, cap)? {
                Content::Read { lines, text } => scanned
                    .findings
                    .extend(findings_in(repo, file, lines, text, cap)?),
                Content::TooLarge(size) => scanned.unchecked.push(Unchecked {
                    place: file.place.clone(),
                    size,
                }),
            }
            Ok(scanned)
        })
}

/// The findings of `lines`, the secret lines of `file`'s new content.
fn findings_in(
    repo: &Repo,
    file: &BinaryFile,
    lines: Vec<SecretLine>,
    text: bool,
    cap: u64,
) -> Result<Vec<Finding>> {
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let held = held_by_parents(repo, &file.parents, cap)?;
    let findings = lines.into_iter().flat_map(|line| {
        let change = match &held {
            Some(texts) if texts.contains(&line.text) => Change::Unchanged,
            _ => Change::Added,
        };
        let number = text.then_some(line.number);
        line.kinds.into_iter().map(move |kind| Finding {
            place: file.place.clone(),
            line: number,
            change,
            kind,
        })
    });
    Ok(findings.collect())
}

/// The lines with secrets that the blobs of `parents` hold, or `None` when one is a binary over
/// `cap`, so that every finding counts as added.
fn held_by_parents(repo: &Repo, parents: &[String], cap: u64) -> Result<Option<HashSet<String>>> {
    parents
        .iter()
        .try_fold(Some(HashSet::new()), |held, parent| {
            Ok(match (held, secrets_in(repo, parent, cap)?) {
                (Some(mut held), Content::Read { lines, .. }) => {
                    held.extend(lines.into_iter().map(|line| line.text));
                    Some(held)
                }
                _ => None,
            })
        })
}

/// The secret lines of the blob `hash`: every line of a text file, however large, and the
/// printable runs of a binary one up to `cap` bytes. Text is told from binary as git does.
fn secrets_in(repo: &Repo, hash: &str, cap: u64) -> Result<Content> {
    let size = repo.object_size(hash)?;
    let mut blob = repo.open_blob(hash)?;
    let mut head = Vec::new();
    (&mut blob).take(TEXT_TEST_BYTES).read_to_end(&mut head)?;
    let text = !head.contains(&0);
    if !text && size > cap {
        return Ok(Content::TooLarge(size));
    }
    let content = Cursor::new(head).chain(blob);
    let lines = if text {
        secret_lines_of(BufReader::new(content))?
    } else {
        secret_runs_of(content, cap)?
    };
    Ok(Content::Read { lines, text })
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

/// The secret runs of printable characters in the first `cap` bytes of `content`.
fn secret_runs_of(content: impl Read, cap: u64) -> io::Result<Vec<SecretLine>> {
    let mut bytes = Vec::new();
    content.take(cap).read_to_end(&mut bytes)?;
    Ok(redact::secret_lines(printable_runs(&bytes)))
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

        let size = repo.object_size(&binary).unwrap();
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
        assert_eq!(repo.object_size(&hash).unwrap(), 200_000);

        assert!(scan(&repo, &[file("data.bin", &hash, &[])], MOST_BINARY_BYTES).is_err());
    }
}
