//! A blob's content, read from git as it comes, so a large one needn't be held whole, and many
//! small blobs read one after another through one git.

use anyhow::{Context, Result};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout};

/// The content of a blob, read from `git cat-file` as git writes it. Reading to the end fails if
/// git failed, so a blob git couldn't read never passes for an empty one. Dropping it stops git.
pub struct Blob {
    child: Child,
    stdout: ChildStdout,
    hash: String,
}

impl Blob {
    /// The blob `hash` that `child`, a `git cat-file` with its output piped, writes.
    pub(super) fn new(mut child: Child, hash: &str) -> io::Result<Self> {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("git cat-file was started without its output"))?;
        Ok(Self {
            child,
            stdout,
            hash: hash.to_owned(),
        })
    }
}

impl Read for Blob {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.stdout.read(buffer)?;
        if read == 0 && !buffer.is_empty() && !self.child.wait()?.success() {
            return Err(io::Error::other(format!(
                "git couldn't read the blob {}",
                self.hash
            )));
        }
        Ok(read)
    }
}

impl Drop for Blob {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Whole blobs read one after another from one `git cat-file --batch`. Dropping it stops git.
pub struct BlobBatch {
    child: Child,
    batch: Batch<ChildStdin, BufReader<ChildStdout>>,
}

impl BlobBatch {
    /// The reader of `child`, a `git cat-file` whose `--batch` format ends each header with
    /// ` <mark>`, and with its input and output piped.
    pub(super) fn new(mut child: Child, mark: String) -> io::Result<Self> {
        let started = || io::Error::other("git cat-file was started without its input and output");
        let input = child.stdin.take().ok_or_else(started)?;
        let output = child.stdout.take().ok_or_else(started)?;
        Ok(Self {
            child,
            batch: Batch::new(input, BufReader::new(output), mark),
        })
    }

    /// The whole content of the blob `hash`, whose size is `size`. Fails if git can't read it,
    /// or gives another size, so that no more than `size` bytes are ever held.
    pub fn read(&mut self, hash: &str, size: u64) -> Result<Vec<u8>> {
        let hash = super::object_hash(hash)?;
        self.batch
            .read(hash, size)
            .with_context(|| format!("git couldn't read the blob {hash}"))
    }

    /// The process of git that reads the blobs.
    #[cfg(test)]
    pub fn id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for BlobBatch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The longest header a `git cat-file --batch` answer can have here: a hash of up to 64
/// digits, its type, its size and the mark, with room to spare.
const MOST_HEADER: u64 = 256;

/// The `git cat-file --batch` exchange over `input` and `output`: a hash goes in, and a header,
/// the content and a line end come back. Each header ends with `mark`, which a run picks at
/// random, so that content read out of step can't pass for a header. After a read fails, the
/// exchange is out of step, so every later read fails too.
struct Batch<W, R> {
    input: W,
    output: R,
    mark: String,
    broken: bool,
}

impl<W: Write, R: BufRead> Batch<W, R> {
    fn new(input: W, output: R, mark: String) -> Self {
        Self {
            input,
            output,
            mark,
            broken: false,
        }
    }

    fn read(&mut self, hash: &str, size: u64) -> Result<Vec<u8>> {
        anyhow::ensure!(!self.broken, "an earlier read from git failed");
        let content = self.content(hash, size);
        self.broken = content.is_err();
        content
    }

    fn content(&mut self, hash: &str, size: u64) -> Result<Vec<u8>> {
        writeln!(self.input, "{hash}")?;
        self.input.flush()?;
        let mut header = String::new();
        (&mut self.output)
            .take(MOST_HEADER)
            .read_line(&mut header)?;
        let given = header
            .strip_suffix('\n')
            .and_then(|header| header.strip_suffix(&format!(" {}", self.mark)))
            .and_then(|header| size_in(hash, header))
            .with_context(|| format!("git answered {:?}", header.trim_end()))?;
        anyhow::ensure!(given == size, "git gave its size as {given}, not {size}");
        let mut content = vec![0; usize::try_from(size)?];
        self.output.read_exact(&mut content)?;
        let mut end = [0];
        self.output.read_exact(&mut end)?;
        anyhow::ensure!(end == *b"\n", "git didn't end the blob where its size said");
        Ok(content)
    }
}

/// The size a `git cat-file --batch` header such as `<hash> blob 120` gives for the blob
/// `hash`, or `None` for any other answer, such as `<hash> missing` or another object's type.
pub(super) fn size_in(hash: &str, header: &str) -> Option<u64> {
    match header.split(' ').collect::<Vec<_>>()[..] {
        [named, "blob", size] if named == hash => size.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const MARK: &str = "0123abcd";

    fn hash(digit: char) -> String {
        digit.to_string().repeat(40)
    }

    /// A batch that reads `answers` as git's output.
    fn answering(answers: &str) -> Batch<Vec<u8>, Cursor<Vec<u8>>> {
        Batch::new(
            Vec::new(),
            Cursor::new(answers.as_bytes().to_vec()),
            MARK.to_owned(),
        )
    }

    #[test]
    fn reads_each_answer_in_step_and_sends_each_hash_on_a_line() {
        let (a, b) = (hash('a'), hash('b'));
        let mut batch = answering(&format!("{a} blob 5 {MARK}\nhello\n{b} blob 0 {MARK}\n\n"));

        assert_eq!(batch.read(&a, 5).unwrap(), b"hello");
        assert_eq!(batch.read(&b, 0).unwrap(), b"");
        assert_eq!(batch.input, format!("{a}\n{b}\n").as_bytes());
    }

    /// What can't be git's answer to the hash asked for fails: another hash, type or size, a
    /// header without the run's mark, as content read out of step would be, one that doesn't
    /// end within its length, and content without its line end.
    #[test]
    fn fails_on_anything_but_the_answer_to_its_hash() {
        let (a, b) = (hash('a'), hash('b'));
        for answer in [
            format!("{b} blob 5 {MARK}\nhello\n"),
            format!("{a} tree 5 {MARK}\nhello\n"),
            format!("{a} blob 6 {MARK}\nhello!\n"),
            format!("{a} blob 5\nhello\n"),
            format!("{a} blob 5 {MARK}x\nhello\n"),
            format!("{a} missing\n"),
            format!("{a} blob 5 {MARK}{}\nhello\n", " ".repeat(300)),
            format!("{a} blob 5 {MARK}\nhelloX"),
            format!("{a} blob 5 {MARK}\nhel"),
        ] {
            assert!(answering(&answer).read(&a, 5).is_err(), "{answer:?}");
        }
    }

    /// Once a read fails, what git writes next may be the rest of an answer, so no later read
    /// is taken from it, even one that looks right.
    #[test]
    fn reads_nothing_more_after_a_read_fails() {
        let (a, b) = (hash('a'), hash('b'));
        let answers =
            format!("{a} blob 6 {MARK}\n{b} blob 5 {MARK}\nhello\n\n{b} blob 5 {MARK}\nhello\n");
        let mut batch = answering(&answers);

        assert!(batch.read(&a, 5).is_err());
        let error = batch.read(&b, 5).unwrap_err();
        assert!(error.to_string().contains("earlier read"), "{error}");
    }

    /// A header is read only up to its longest, so output that never ends a line, as content
    /// read out of step could be, isn't held whole.
    #[test]
    fn reads_no_more_of_a_header_than_one_can_be() {
        let mut batch = answering(&"x".repeat(10_000_000));

        assert!(batch.read(&hash('a'), 5).is_err());
        assert!(batch.output.position() <= MOST_HEADER);
    }
}
