//! What a commit or a push publishes, read for secrets as git streams it: the secrets in each line
//! of its diffs and commit messages, and the files its diffs show only as binary, which are read
//! whole afterwards (`contents`). No diff or history is held whole, and a line too long to hold is
//! scanned in windows (`lines`).

use super::contents::{BinaryFile, BinaryFiles};
use super::lines::{Line, Lines};
use super::redact::{Change, DiffScan, Finding, Held, LineScan};
use crate::git::{LogLine, LogMarks, UNREADABLE_LOG};
use anyhow::{Result, bail};
use std::io::BufRead;

/// What a read of a publication found: the secrets in its lines, and the binary files to read
/// whole.
#[derive(Debug, Default)]
pub struct Found {
    pub findings: Vec<Finding>,
    pub binary: Vec<BinaryFile>,
}

/// What `output`, a diff as git writes it, publishes.
pub fn read_diff(output: &mut dyn BufRead) -> Result<Found> {
    let mut lines = Lines::new(output);
    let mut diff = DiffRead::default();
    while let Some(line) = lines.next_line()? {
        diff.line(line, &mut lines)?;
    }
    diff.finish()
}

/// What `output`, a log of commits with their patches in the format of `marks`, publishes:
/// findings and files name their commit, as in "commit 1a2b3c4 message".
pub fn read_log(output: &mut dyn BufRead, marks: &LogMarks) -> Result<Found> {
    let mut lines = Lines::new(output);
    let mut log = LogRead::default();
    while let Some(line) = lines.next_line()? {
        let (Line::Whole(bytes) | Line::Long(bytes)) = &line;
        match marks.line(bytes)? {
            LogLine::Commit(hash) => log.start_commit(hash)?,
            LogLine::MessageEnd => log.message_end(),
            LogLine::Text => log.text(line, &mut lines)?,
        }
    }
    log.finish()
}

/// A diff read line by line: the secrets in its hunks, and its binary files.
#[derive(Default)]
struct DiffRead {
    scan: DiffScan,
    binary: BinaryFiles,
}

impl DiffRead {
    /// Takes `line`, the next line of the diff, and reads the rest of it from `lines` when it's
    /// long.
    fn line<R: BufRead>(&mut self, line: Line, lines: &mut Lines<R>) -> Result<()> {
        match line {
            Line::Whole(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                self.binary.line(without_end(&text))?;
                self.scan.line(&text);
                Ok(())
            }
            Line::Long(start) => self.long_line(start, lines),
        }
    }

    /// Takes a line too long to read whole, whose first bytes are `start` and whose rest
    /// `lines` reads next: a hunk's line is scanned in windows from after its markers, and any
    /// other is left for `lines` to skip.
    fn long_line<R: BufRead>(&mut self, mut start: Vec<u8>, lines: &mut Lines<R>) -> Result<()> {
        let head = String::from_utf8_lossy(&start);
        self.binary.line(&head)?;
        let Some(mut text) = self.scan.start_long_line(&head) else {
            return Ok(());
        };
        start.drain(..text.markers);
        let windows = lines.scan_rest(start, |piece| text.feed(piece))?;
        self.scan.end_long_line(text, windows);
        Ok(())
    }

    fn finish(self) -> Result<Found> {
        Ok(Found {
            findings: self.scan.findings(),
            binary: self.binary.finish()?,
        })
    }
}

/// `line` without its line end, as [`str::lines`] gives it.
fn without_end(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// A log read line by line, a commit at a time.
#[derive(Default)]
struct LogRead {
    found: Found,
    commit: Option<CommitRead>,
}

impl LogRead {
    /// Starts the commit `hash`, after the one before it.
    fn start_commit(&mut self, hash: String) -> Result<()> {
        self.finish_commit()?;
        self.commit = Some(CommitRead::new(hash));
        Ok(())
    }

    fn message_end(&mut self) {
        if let Some(commit) = &mut self.commit {
            commit.message_end();
        }
    }

    /// Takes `line`, a line of the commit's message or patch. Before the first commit, only
    /// blank lines can come.
    fn text<R: BufRead>(&mut self, line: Line, lines: &mut Lines<R>) -> Result<()> {
        match &mut self.commit {
            Some(commit) => commit.text(line, lines),
            None if matches!(&line, Line::Whole(bytes) if bytes.trim_ascii().is_empty()) => Ok(()),
            None => bail!(UNREADABLE_LOG),
        }
    }

    fn finish(mut self) -> Result<Found> {
        self.finish_commit()?;
        Ok(self.found)
    }

    fn finish_commit(&mut self) -> Result<()> {
        if let Some(commit) = self.commit.take() {
            let found = commit.finish()?;
            self.found.findings.extend(found.findings);
            self.found.binary.extend(found.binary);
        }
        Ok(())
    }
}

/// The commit being read: its message until it ends, then its patch.
struct CommitRead {
    hash: String,
    message: Option<MessageRead>,
    in_message: Vec<Finding>,
    patch: DiffRead,
}

impl CommitRead {
    fn new(hash: String) -> Self {
        Self {
            hash,
            message: Some(MessageRead::default()),
            in_message: Vec::new(),
            patch: DiffRead::default(),
        }
    }

    fn message_end(&mut self) {
        if let Some(message) = self.message.take() {
            self.in_message = message.findings(&format!("commit {} message", self.hash));
        }
    }

    fn text<R: BufRead>(&mut self, line: Line, lines: &mut Lines<R>) -> Result<()> {
        match &mut self.message {
            Some(message) => message.line(line, lines),
            None => self.patch.line(line, lines),
        }
    }

    /// What the commit holds: the secrets in its message, then those in its patch, and its
    /// binary files, each named after the commit.
    fn finish(mut self) -> Result<Found> {
        self.message_end();
        let Self {
            hash,
            in_message,
            patch,
            ..
        } = self;
        let patch = patch.finish()?;
        let place = |place: &str| format!("commit {hash} {place}");
        let in_patch = patch.findings.into_iter().map(|finding| Finding {
            place: place(&finding.place),
            ..finding
        });
        let binary = patch.binary.into_iter().map(|file| BinaryFile {
            place: place(&file.place),
            ..file
        });
        Ok(Found {
            findings: in_message.into_iter().chain(in_patch).collect(),
            binary: binary.collect(),
        })
    }
}

/// A commit's message read line by line. Blank lines and the space a line starts with hold no
/// secret and start no key's block, so the message is scanned as git writes it, untrimmed.
#[derive(Default)]
struct MessageRead {
    scan: LineScan,
    kinds: Vec<String>,
}

impl MessageRead {
    fn line<R: BufRead>(&mut self, line: Line, lines: &mut Lines<R>) -> Result<()> {
        match line {
            Line::Whole(bytes) => {
                let held = self.scan.line(&String::from_utf8_lossy(&bytes));
                self.take(held);
                Ok(())
            }
            Line::Long(start) => self.long_line(start, lines),
        }
    }

    /// Takes a line too long to read whole, whose first bytes are `start` and whose rest
    /// `lines` reads next, scanned in windows.
    fn long_line<R: BufRead>(&mut self, start: Vec<u8>, lines: &mut Lines<R>) -> Result<()> {
        let mut line = self.scan.start_long_line();
        let windows = lines.scan_rest(start, |piece| line.feed(piece))?;
        let held = self.scan.end_long_line(line, windows);
        self.take(held);
        Ok(())
    }

    /// Keeps the secrets a line holds.
    fn take(&mut self, held: Held) {
        if let Held::Secrets(kinds) = held {
            self.kinds.extend(kinds);
        }
    }

    /// The secrets found, as findings in `place`.
    fn findings(self, place: &str) -> Vec<Finding> {
        self.kinds
            .into_iter()
            .map(|kind| Finding {
                place: place.to_owned(),
                line: None,
                change: Change::Added,
                kind,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::lines::WINDOW;
    use super::super::lines::test_support::line_with_keys;
    use super::super::redact;
    use super::super::test_support::{aws_key, github_token};
    use super::*;
    use crate::git::test_support::{isolated_repo, run};
    use std::fs;
    use std::io::Cursor;

    fn diff_of(text: &str) -> Found {
        read_diff(&mut Cursor::new(text.as_bytes())).unwrap()
    }

    /// What a scan of all of `diff` at once finds: its findings and its binary files.
    fn whole(diff: &str) -> (Vec<Finding>, Vec<BinaryFile>) {
        let mut files = BinaryFiles::default();
        for line in diff.lines() {
            files.line(line).unwrap();
        }
        (redact::diff_findings(diff), files.finish().unwrap())
    }

    /// A diff read as git streams it gives what a scan of all of it gives: secrets added,
    /// kept and removed, in a merge's combined diff too, a private key's block across lines,
    /// lines that end in CRLF, and binary files.
    #[test]
    fn reads_a_diff_as_a_scan_of_all_of_it() {
        let key = aws_key();
        let (a, b) = ("1".repeat(40), "2".repeat(40));
        let diffs = [
            format!(
                "diff --git a/x.env b/x.env\nindex {a}..{b} 100644\n--- a/x.env\n+++ b/x.env\n\
                 @@ -1,3 +1,3 @@\n keep {key}\n-old {key}\n+new {key}\r\n tail\n"
            ),
            format!(
                "diff --cc m.env\nindex {a},{b}..{a}\n--- a/m.env\n+++ b/m.env\n\
                 @@@ -1,1 -1,1 +1,2 @@@\n  same\n++merged {key}\n"
            ),
            format!(
                "diff --git a/k.pem b/k.pem\nindex {a}..{b} 100644\n--- a/k.pem\n+++ b/k.pem\n\
                 @@ -0,0 +1,4 @@\n+-----BEGIN OPENSSH PRIVATE KEY-----\n+{}\n+{}\n\
                 +-----END OPENSSH PRIVATE KEY-----\n",
                "A".repeat(64),
                "B".repeat(64)
            ),
            format!(
                "diff --git a/x.dat b/x.dat\nindex {a}..{b} 100644\nBinary files a/x.dat and \
                 b/x.dat differ\ndiff --git a/y.env b/y.env\nnew file mode 100644\nindex \
                 {}..{b}\n--- /dev/null\n+++ b/y.env\n@@ -0,0 +1 @@\n+{key}",
                "0".repeat(40)
            ),
            String::new(),
        ];

        for diff in diffs {
            let read = diff_of(&diff);
            assert_eq!((read.findings, read.binary), whole(&diff), "{diff:?}");
        }
    }

    /// A hunk's line longer than a window is scanned in windows from after its markers, with
    /// its number, and the lines after it keep theirs: the same findings as a scan of the whole
    /// diff, which holds the line whole. The removed line ends inside its second key, which
    /// counts for nothing.
    #[test]
    fn scans_a_long_line_of_a_hunk_in_windows() {
        let long = line_with_keys(2 * WINDOW + 100, &[50, WINDOW - 8, 2 * WINDOW]);
        let diff = format!(
            "diff --git a/b.js b/b.js\n--- a/b.js\n+++ b/b.js\n@@ -1,2 +1,3 @@\n same\n-{}\n+{}\n+{}\n",
            String::from_utf8_lossy(&long[..WINDOW + 10]),
            String::from_utf8_lossy(&long),
            aws_key()
        );

        let read = diff_of(&diff);

        let (findings, _) = whole(&diff);
        assert_eq!(read.findings, findings);
        let lines: Vec<(Option<usize>, Change)> = read
            .findings
            .iter()
            .map(|finding| (finding.line, finding.change))
            .collect();
        assert_eq!(
            lines,
            [
                (Some(2), Change::Removed),
                (Some(2), Change::Added),
                (Some(2), Change::Added),
                (Some(2), Change::Added),
                (Some(3), Change::Added)
            ]
        );
    }

    /// A private key whose data is one line longer than a window is found where its data
    /// starts, as a scan of the whole diff finds it, and a line outside the hunks that long is
    /// passed over.
    #[test]
    fn takes_a_long_line_of_key_data_as_the_key() {
        let diff = format!(
            "diff --git a/k.pem b/k.pem\n--- a/k.pem\n+++ b/k.pem\n@@ -0,0 +1,3 @@\n\
             +-----BEGIN OPENSSH PRIVATE KEY-----\n+{}\n+-----END OPENSSH PRIVATE KEY-----\n\
             {}\n@@ -9 +9 @@\n+{}\n",
            "A".repeat(2 * WINDOW),
            "x".repeat(2 * WINDOW),
            aws_key()
        );

        let read = diff_of(&diff);

        let (findings, _) = whole(&diff);
        assert_eq!(read.findings, findings);
        let kinds: Vec<(Option<usize>, &str)> = read
            .findings
            .iter()
            .map(|finding| (finding.line, finding.kind.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [(Some(2), "private key"), (Some(9), "AWS access key")]
        );
    }

    /// The data line of a private key, after its BEGIN line.
    const KEY_DATA: &str = "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9u";

    /// A diff that adds `text` as the new file `path`.
    fn added(path: &str, text: &str) -> String {
        let lines: String = text.lines().map(|line| format!("+{line}\n")).collect();
        let count = text.lines().count();
        format!(
            "diff --git a/{path} b/{path}\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{count} @@\n{lines}"
        )
    }

    /// A key whose block begins at the end of a long line is found on the line after it, in a
    /// diff and in a message, as a scan of all of it finds it.
    #[test]
    fn finds_a_key_whose_block_begins_at_the_end_of_a_long_line() {
        let block = format!(
            "{}-----BEGIN OPENSSH PRIVATE KEY-----\n{KEY_DATA}\n-----END OPENSSH PRIVATE KEY-----",
            "x".repeat(2 * WINDOW)
        );
        let diff = added("k.pem", &block);

        let read = diff_of(&diff);
        let message = log_of(&log(&[("1a2b3c4", &block, "")])).unwrap();

        assert_eq!(read.findings, whole(&diff).0);
        assert_eq!(read.findings.len(), 1);
        let place = "commit 1a2b3c4 message";
        assert_eq!(message.findings, redact::text(&block, place).findings);
        assert_eq!(message.findings.len(), 1);
    }

    /// Inside a key's block, a long line whose first window is key data but whose rest holds a
    /// token holds the token, even when the key's data began on a line the diff only shows; and
    /// a long header line of the key's keeps its block open. As a scan of all of it finds.
    #[test]
    fn reads_a_long_line_in_a_key_block_by_all_of_it() {
        let begin = "-----BEGIN OPENSSH PRIVATE KEY-----";
        let token = format!("{} {}", "A".repeat(WINDOW + 100), github_token());
        let context = format!(
            "diff --git a/k.pem b/k.pem\n--- a/k.pem\n+++ b/k.pem\n@@ -1,2 +1,3 @@\n \
             {begin}\n {KEY_DATA}\n+{token}\n"
        );
        let comment = "x ".repeat(WINDOW);
        let header = added(
            "r.pem",
            &format!(
                "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nComment: {comment}\n\n\
                 {KEY_DATA}\n-----END RSA PRIVATE KEY-----"
            ),
        );

        assert_eq!(
            kinds_of(&context),
            [
                (Change::Unchanged, "private key".to_owned()),
                (Change::Added, "GitHub token".to_owned())
            ]
        );
        assert_eq!(
            kinds_of(&header),
            [(Change::Added, "private key".to_owned())]
        );
    }

    /// A long line whose BEGIN line is part of a value that a rule replaces starts no key's
    /// block, as in a scan of the whole diff, so the lines after it are scanned as any others
    /// and the key added among them is found.
    #[test]
    fn starts_no_key_block_after_a_begin_line_a_value_swallows() {
        let begin = "-----BEGIN RSA PRIVATE KEY-----";
        let line = format!("{} token=Ab1{begin}", "x".repeat(WINDOW));
        let diff = format!(
            "diff --git a/k.env b/k.env\n--- a/k.env\n+++ b/k.env\n@@ -1,2 +1,3 @@\n {line}\n \
             AAAA\n+AAAA/{}/AAAA\n",
            aws_key()
        );

        let kinds = kinds_of(&diff);

        assert_eq!(
            kinds.last(),
            Some(&(Change::Added, "AWS access key".to_owned()))
        );
    }

    /// What reading `diff` finds, which a scan of all of it finds too, as each finding's change
    /// and kind.
    fn kinds_of(diff: &str) -> Vec<(Change, String)> {
        let read = diff_of(diff);
        assert_eq!(read.findings, whole(diff).0);
        read.findings
            .into_iter()
            .map(|finding| (finding.change, finding.kind))
            .collect()
    }

    /// A hidden character at the edge of a long line's pieces counts as it does in the whole
    /// line: a joiner at a piece's start, after a letter, keeps the line from being key data,
    /// so the key in it is found after a key the diff only shows; and a zero-width space cut in
    /// two leaves a long line of key data a key, in a diff and in a message.
    #[test]
    fn reads_a_hidden_character_at_a_piece_s_edge_as_the_whole_line_does() {
        let begin = "-----BEGIN OPENSSH PRIVATE KEY-----";
        let end = "-----END OPENSSH PRIVATE KEY-----";
        let joined = format!(
            "{}\u{200d}{}/{}/AAAA",
            "A".repeat(WINDOW - 1),
            "A".repeat(100),
            aws_key()
        );
        let context = format!(
            "diff --git a/k.pem b/k.pem\n--- a/k.pem\n+++ b/k.pem\n@@ -1,2 +1,3 @@\n \
             {begin}\n {KEY_DATA}\n+{joined}\n"
        );
        let cut = |at: usize| format!("{}\u{200b}{}", "A".repeat(at), "A".repeat(100));
        let data = added("k.pem", &format!("{begin}\n{}\n{end}", cut(WINDOW - 2)));
        let message = format!("{begin}\n{}\n{end}", cut(WINDOW - 1));

        let read = log_of(&log(&[("1a2b3c4", &message, "")])).unwrap();

        let found = |change, kind: &str| (change, kind.to_owned());
        assert_eq!(
            kinds_of(&context),
            [
                found(Change::Unchanged, "private key"),
                found(Change::Added, "AWS access key")
            ]
        );
        assert_eq!(kinds_of(&data), [found(Change::Added, "private key")]);
        let place = "commit 1a2b3c4 message";
        assert_eq!(read.findings, redact::text(&message, place).findings);
        assert_eq!(read.findings.len(), 1);
    }

    /// A log with each commit's hash, message and patch, as git writes it in
    /// [`LogMarks::plain`]'s marks: git's separators as they are.
    fn log(commits: &[(&str, &str, &str)]) -> String {
        commits
            .iter()
            .map(|(hash, message, patch)| format!("\u{1e}{hash}\n{message}\n\u{1f}\n{patch}"))
            .collect()
    }

    fn log_of(text: &str) -> Result<Found> {
        read_log(&mut Cursor::new(text.as_bytes()), &LogMarks::plain())
    }

    /// The secrets in each commit's message, then those in its patch, each named after the
    /// commit, as a scan of each commit on its own names them; its binary files are too.
    #[test]
    fn reads_the_message_and_the_patch_of_each_commit() {
        let key = aws_key();
        let patch = format!(
            "\ndiff --git a/x.env b/x.env\n--- a/x.env\n+++ b/x.env\n@@ -0,0 +1 @@\n+{key}\n\
             diff --git a/x.dat b/x.dat\nindex {}..{} 100644\nBinary files differ\n",
            "1".repeat(40),
            "2".repeat(40)
        );
        let commits = [
            ("1a2b3c4", format!("add {key}\n"), String::new()),
            ("5d6e7f8", format!("tidy\n\nmore {key}\n"), patch),
        ];
        let parts: Vec<(&str, &str, &str)> = commits
            .iter()
            .map(|(hash, message, patch)| (*hash, message.as_str(), patch.as_str()))
            .collect();

        let read = log_of(&log(&parts)).unwrap();

        let each: Vec<Finding> = commits
            .iter()
            .flat_map(|(hash, message, patch)| redact::commit(hash, message.trim(), patch))
            .collect();
        assert_eq!(read.findings, each);
        assert_eq!(read.findings.len(), 3);
        let binary: Vec<&str> = read.binary.iter().map(|file| file.place.as_str()).collect();
        assert_eq!(binary, ["commit 5d6e7f8 x.dat"]);
        assert!(log_of("").unwrap().findings.is_empty());
    }

    /// A message read as git writes it, untrimmed, holds what a scan of it trimmed finds, a
    /// long line of it scanned in windows; an empty message holds nothing.
    #[test]
    fn finds_in_a_message_what_a_scan_of_it_trimmed_finds() {
        let long = String::from_utf8(line_with_keys(WINDOW + 50, &[WINDOW - 8])).unwrap();
        let message = format!("\n \n   {}\nnext\n{long}\n", aws_key());

        let read = log_of(&log(&[("1a2b3c4", &message, ""), ("5d6e7f8", "", "")])).unwrap();

        let trimmed = redact::text(message.trim(), "commit 1a2b3c4 message").findings;
        assert_eq!(read.findings, trimmed);
        assert_eq!(read.findings.len(), 2);
    }

    /// Only blank lines can come before the first commit: anything else means the marks didn't
    /// come out as written, and the read fails rather than miss a commit.
    #[test]
    fn fails_on_text_before_the_first_commit() {
        let commit = log(&[("1a2b3c4", "fine", "")]);

        assert!(log_of(&format!("\n\n{commit}")).is_ok());
        let error = log_of(&format!("stray\n{commit}")).unwrap_err().to_string();
        assert!(error.contains("a commit in it has no hash"), "{error}");
        let long = "x".repeat(WINDOW + 1);
        assert!(log_of(&format!("{long}\n{commit}")).is_err());
    }

    /// Commits a key in `a.env`, then one with a message that holds a key and has no line end,
    /// whose hash it returns, then one with an empty message.
    fn commits_with_odd_messages(dir: &std::path::Path) -> String {
        fs::write(dir.join("a.env"), format!("{}\n", aws_key())).unwrap();
        run(dir, &["add", "a.env"]);
        run(dir, &["commit", "-qm", "add a"]);
        let tree = run(dir, &["write-tree"]).trim().to_owned();
        let message = dir.join(".git/bare-message");
        fs::write(&message, format!("no line end {}", aws_key())).unwrap();
        let file = message.to_str().unwrap();
        let bare = run(dir, &["commit-tree", &tree, "-p", "HEAD", "-F", file]);
        run(dir, &["reset", "-q", "--hard", bare.trim()]);
        let empty = [
            "commit",
            "-q",
            "--allow-empty",
            "--allow-empty-message",
            "-m",
            "",
        ];
        run(dir, &empty);
        bare.trim().to_owned()
    }

    /// Read from git: a message without a line end, and an empty one, each come out as git
    /// wrote them, between their marks.
    #[test]
    fn reads_the_commits_git_writes() {
        let (dir, repo) = isolated_repo();
        let bare = commits_with_odd_messages(dir.path());

        let read = repo.published_commits(&["HEAD~3..HEAD"], read_log).unwrap();

        let places: Vec<(&str, Option<usize>)> = read
            .findings
            .iter()
            .map(|finding| (finding.place.as_str(), finding.line))
            .collect();
        let short = &bare[..7];
        assert_eq!(places.len(), 2, "{places:?}");
        assert!(places[0].0.starts_with("commit ") && places[0].0.ends_with(" message"));
        assert!(places[0].0.contains(short), "{places:?}");
        assert!(places[1].0.ends_with(" a.env"));
        assert_eq!(places[1].1, Some(1));
    }
}
