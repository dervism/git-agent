//! Scans a diff as git prints it, a line at a time: the file and hunk each line is in, and each
//! side of a hunk, the result and every parent, followed as a text of its own.

use super::key_block::{KeyBlock, KeyBlockFacts, LongLineFacts, LongLineScan, Taken};
use super::{Redacted, ScannedLine, Secret, Windows};
use crate::git::diff::is_file_header;
use crate::git::names;
use crate::secrets::finding::{Change, Finding, Place};
use crate::secrets::fingerprint::Fingerprint;

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
    /// A line of a private key's data that the key's one [`REDACTED`](super::REDACTED) line
    /// already stands for.
    Dropped,
}

/// A hunk's line too long to scan whole, from its start until its windows have been scanned:
/// its markers, where it is, and what it is to a private key's block.
pub struct LongText {
    columns: String,
    change: Change,
    number: usize,
    line: LongLineScan,
}

impl LongText {
    /// How many bytes the line's markers take.
    pub fn markers(&self) -> usize {
        self.columns.len()
    }

    /// Takes the next piece of the line, after its markers.
    pub fn feed(&mut self, piece: &[u8]) {
        self.line.feed(piece);
    }
}

/// Scans a diff's lines one after another, keeping track of the file, the hunk and a private
/// key's block they are in, and of what they hold.
#[derive(Default)]
pub struct DiffScan {
    sides: Sides,
    path: String,
    hunk: Option<Hunk>,
    findings: Vec<Finding>,
}

impl DiffScan {
    /// Scans `line`, the next line of the diff, with its line end.
    pub fn line<'l>(&mut self, line: &'l str) -> Kept<'l> {
        if is_file_header(line) {
            self.restart_sides(Sides::default());
            self.path = names::of_diff(line);
            self.hunk = None;
        } else if line.starts_with("@@") {
            let hunk = Hunk::starting(line);
            self.restart_sides(Sides::of(hunk.parents.len()));
            self.hunk = Some(hunk);
        } else if let Some((markers, change, number)) = self.take(line) {
            let slot = self.findings.len();
            let scanned = self
                .sides
                .scan(markers, &line[markers.len()..], change, slot);
            self.found(change, number, scanned.secrets);
            let markers = markers.as_str();
            let content = scanned.text;
            return content.map_or(Kept::Dropped, |content| Kept::Scanned { markers, content });
        }
        Kept::Outside(line)
    }

    /// Takes the next line, too long to scan whole, which starts with `start`. A hunk's line
    /// comes back to be scanned in windows from after its markers, fed to its [`LongText`], and
    /// given to [`DiffScan::end_long_line`]; any other line needs nothing more than its start.
    pub fn start_long_line(&mut self, start: &str) -> Option<LongText> {
        let Some((markers, change, number)) = self.take(start) else {
            self.line(start);
            return None;
        };
        Some(LongText {
            columns: markers.as_str().to_owned(),
            change,
            number,
            line: self.sides.start_long_line(markers, change),
        })
    }

    /// Takes what the windows of `text` found.
    pub fn end_long_line(&mut self, text: LongText, windows: Windows) {
        let (change, number) = (text.change, text.number);
        let slot = self.findings.len();
        let secrets = self.sides.end_long_line(text, windows, slot);
        self.found(change, number, secrets);
    }

    /// The findings of the whole diff: a private key still open is cut off.
    pub fn findings(mut self) -> Vec<Finding> {
        self.restart_sides(Sides::default());
        self.findings
    }

    /// Ends the hunk's sides, which cuts off a private key still open, gives the findings of the
    /// keys their prints, and goes on with `next`.
    fn restart_sides(&mut self, next: Sides) {
        let mut ended = std::mem::replace(&mut self.sides, next);
        for (slot, print) in ended.end() {
            self.findings[slot].print = print;
        }
    }

    /// The markers of `line`, if it's a line of the current hunk, with what the line does and
    /// its number. Moves the hunk past it.
    fn take<'l>(&mut self, line: &'l str) -> Option<(Markers<'l>, Change, usize)> {
        let hunk = self.hunk.as_mut()?;
        let markers = hunk.markers(line)?;
        let (change, number) = hunk.take(markers);
        Some((markers, change, number))
    }

    /// Takes the `secrets` of the line `number`, which makes `change`.
    fn found(&mut self, change: Change, number: usize, secrets: Vec<Secret>) {
        let path = &self.path;
        self.findings
            .extend(secrets.into_iter().map(|secret| Finding {
                place: Place::file(path.clone()),
                line: Some(number),
                change,
                print: secret.print,
                kind: secret.kind,
            }));
    }
}

/// A hunk's line as its sides took it: what goes on to the model, if anything, and its secrets.
struct ScannedLineOfHunk {
    text: Option<String>,
    secrets: Vec<Secret>,
}

/// A hunk's sides, each scanned as a file of its own: the result, which holds its kept and added
/// lines, and each parent, which holds its kept and removed lines. A line one side lacks, such
/// as an old END line or a placeholder removed, then can't end a private key's block that the
/// other side has open.
#[derive(Default)]
struct Sides {
    result: KeyBlock,
    parents: Vec<KeyBlock>,
}

impl Sides {
    /// The sides of a hunk with `parents` parents, before its first line.
    fn of(parents: usize) -> Self {
        Self {
            result: KeyBlock::default(),
            parents: (0..parents).map(|_| KeyBlock::default()).collect(),
        }
    }

    /// The sides that hold a line with these `markers`, which makes `change`, each with what
    /// the line does there, the side it's reported on first: the result for a line it holds,
    /// else the first parent that removes it. A parent holds a line it removes, and one it keeps.
    fn holding<'s>(
        &'s mut self,
        markers: Markers<'s>,
        change: Change,
    ) -> impl Iterator<Item = (&'s mut KeyBlock, Change)> {
        debug_assert_eq!(
            markers.len(),
            self.parents.len(),
            "a marker for each parent"
        );
        let in_parents = if markers.in_result() {
            Change::Unchanged
        } else {
            Change::Removed
        };
        let parents = self
            .parents
            .iter_mut()
            .enumerate()
            .filter(move |(parent, _)| markers.held_by(*parent))
            .map(move |(_, side)| (side, in_parents));
        markers
            .in_result()
            .then_some((&mut self.result, change))
            .into_iter()
            .chain(parents)
    }

    /// Ends every side, which cuts off a private key still open, and gives the keys' prints, by
    /// the slots of their findings.
    fn end(&mut self) -> Vec<(usize, Fingerprint)> {
        std::iter::once(&mut self.result)
            .chain(&mut self.parents)
            .flat_map(|side| {
                side.end();
                side.take_prints()
            })
            .collect()
    }

    /// `content`, a line with these `markers` that makes `change`, with its secrets replaced,
    /// read once and taken by each side that holds it, with what the side it's reported on finds,
    /// a key there taking `slot`. It's left out, or becomes its key's
    /// [`REDACTED`](super::REDACTED) line, where any side that holds it takes it for a private
    /// key's data; and a line shaped like key data shows only as that line while any side is in a
    /// key's block, whether it holds the line or not, since the old key's BEGIN line can be all
    /// that's left of it.
    fn scan(
        &mut self,
        markers: Markers,
        content: &str,
        change: Change,
        slot: usize,
    ) -> ScannedLineOfHunk {
        let line = ScannedLine::of(content);
        let data_in_an_open_block = self.any_in_key() && line.is_data();
        let mut reported: Option<(&mut KeyBlock, Taken)> = None;
        let mut least = Taken::Text;
        for (side, change) in self.holding(markers, change) {
            let taken = side.take(&line, change);
            least = least.min(taken);
            if reported.is_none() {
                reported = Some((side, taken));
            }
        }
        if data_in_an_open_block {
            least = least.min(Taken::Key);
        }
        let taken = reported.map_or(Taken::Text, |(side, taken)| side.wait_if_key(taken, slot));
        ScannedLineOfHunk {
            text: line.text(least),
            secrets: line.secrets(taken),
        }
    }

    /// Whether any side is in a private key's block.
    fn any_in_key(&self) -> bool {
        self.result.in_key() || self.parents.iter().any(KeyBlock::in_key)
    }

    /// Starts on a line with these `markers`, which makes `change`, too long to scan whole.
    fn start_long_line(&mut self, markers: Markers, change: Change) -> LongLineScan {
        let sides: Vec<_> = self
            .holding(markers, change)
            .map(|(side, _)| (side.in_key(), side.data_print()))
            .collect();
        let in_key = sides.iter().any(|(in_key, _)| *in_key);
        LongLineScan::starting(in_key, sides.into_iter().map(|(_, print)| print).collect())
    }

    /// What the long line `text` holds on the side it's reported on, a key there taking `slot`,
    /// now that its `windows` have been scanned. Every side that holds it takes it, with the facts
    /// started for it, so that each side's block goes on from it.
    fn end_long_line(&mut self, text: LongText, windows: Windows, slot: usize) -> Vec<Secret> {
        let facts = LongLineFacts::of(text.line, windows.ends_with_begin);
        let markers = Markers::of(&text.columns);
        let holding: Vec<_> = self.holding(markers, text.change).collect();
        debug_assert_eq!(
            holding.len(),
            facts.len(),
            "the sides the line was started for"
        );
        let mut reported: Option<(&mut KeyBlock, Taken)> = None;
        for ((side, change), facts) in holding.into_iter().zip(&facts) {
            let taken = side.take(facts, change);
            if reported.is_none() {
                reported = Some((side, taken));
            }
        }
        match reported.map(|(side, taken)| side.wait_if_key(taken, slot)) {
            Some(Taken::Key) => vec![Secret::key()],
            Some(Taken::Text) => windows.secrets,
            Some(Taken::MoreOfAKey) | None => Vec::new(),
        }
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

    /// The marker columns that start `line`, one for each of the hunk's parents, if it's a line
    /// of this hunk.
    fn markers<'a>(&self, line: &'a str) -> Option<Markers<'a>> {
        let markers = line.get(..self.parents.len())?;
        markers
            .bytes()
            .all(|marker| matches!(marker, b'+' | b'-' | b' '))
            .then_some(Markers::of(markers))
    }

    /// What the line with these `markers` does, and its number: in the result, or for a removed
    /// line in the first parent that removes it. Moves past the line on every side that holds it.
    fn take(&mut self, markers: Markers) -> (Change, usize) {
        let number = markers
            .first_removing()
            .map_or(self.result, |parent| self.parents[parent]);
        if markers.in_result() {
            self.result = self.result.saturating_add(1);
        }
        for (parent, line) in self.parents.iter_mut().enumerate() {
            if markers.held_by(parent) {
                *line = line.saturating_add(1);
            }
        }
        (markers.change(), number)
    }
}

/// The marker columns that start a hunk's line, one for each parent: `-` where that parent has
/// the line and the result doesn't, `+` where the result has it and that parent doesn't, and a
/// space where both have it, or, on a removed line, neither.
/// Whether the result holds the line is read once, when the markers are, so that a line costs
/// time in proportion to its markers, however many parents a merge has.
#[derive(Clone, Copy)]
struct Markers<'l> {
    columns: &'l str,
    in_result: bool,
}

impl<'l> Markers<'l> {
    /// The markers `columns`, read.
    fn of(columns: &'l str) -> Self {
        Self {
            columns,
            in_result: !columns.contains('-'),
        }
    }

    /// The markers as they start the line.
    fn as_str(self) -> &'l str {
        self.columns
    }

    /// How many bytes the markers take: one for each of the hunk's parents.
    fn len(self) -> usize {
        self.columns.len()
    }

    /// Whether the result holds the line: no parent removes it.
    fn in_result(self) -> bool {
        self.in_result
    }

    /// Whether `parent`, one of the hunk's parents, holds the line: it removes the line, or keeps
    /// it. [`Hunk::markers`] takes as many markers as the hunk has parents, and [`Sides`] has a
    /// side for each.
    fn held_by(self, parent: usize) -> bool {
        let marker = self.columns.as_bytes()[parent];
        marker == b'-' || (marker == b' ' && self.in_result)
    }

    /// The first parent that removes the line, which a removed line is numbered by.
    fn first_removing(self) -> Option<usize> {
        self.columns.find('-')
    }

    /// What the line does: removed where a parent removes it, added where every parent lacks it,
    /// and kept otherwise.
    fn change(self) -> Change {
        if !self.in_result() {
            Change::Removed
        } else if self.columns.bytes().all(|marker| marker == b'+') {
            Change::Added
        } else {
            Change::Unchanged
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::REDACTED;
    use super::*;
    use crate::git::test_support::{isolated_repo, run};
    use crate::secrets::test_support::{KEY_TAGS, key_data, key_edits, noise};
    use std::fs;
    use std::time::{Duration, Instant};

    #[test]
    fn names_a_file_with_norwegian_letters_in_a_finding() {
        let secret = format!("ghp_{}", noise(36));
        for header in [
            "diff --git a/Kjøre.txt b/Kjøre.txt\n",
            "diff --git \"a/Kj\\303\\270re.txt\" \"b/Kj\\303\\270re.txt\"\n",
        ] {
            let found = diff(&format!("{header}@@ -1 +1 @@\n+token = {secret}\n"));
            assert_eq!(found.findings[0].place, Place::file("Kjøre.txt"));
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
            .map(|finding| (finding.place.to_string(), finding.line, finding.change))
            .collect();
        assert_eq!(
            found,
            [
                ("src/a.rs".to_owned(), Some(11), Change::Removed),
                ("src/a.rs".to_owned(), Some(11), Change::Added),
                ("src/a.rs".to_owned(), Some(12), Change::Unchanged),
                (".env".to_owned(), Some(2), Change::Added)
            ]
        );
        assert_eq!(diff_findings(&diff), result.findings);
    }

    /// The places, lines and changes of what `diff` finds.
    fn found_in(diff: &str) -> Vec<(String, Option<usize>, Change)> {
        super::diff(diff)
            .findings
            .into_iter()
            .map(|finding| (finding.place.to_string(), finding.line, finding.change))
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
    }

    /// A key that replaces another in place is reported as added, though git shows the old
    /// key's lines first, and the model sees one line for each change the block makes.
    #[test]
    fn reports_a_key_that_replaces_another_once_for_each_change() {
        let data = |marker: char| -> String {
            (0..3).map(|_| format!("{marker}{}\n", noise(64))).collect()
        };
        let (begin, end) = (
            "-----BEGIN OPENSSH PRIVATE KEY",
            "-----END OPENSSH PRIVATE KEY",
        );
        let header = "diff --git a/deploy_key b/deploy_key\n@@ -1,6 +1,6 @@\n";
        let diff = format!(
            "{header} {begin}-----\n {}\n{}{} {end}-----\n",
            noise(64),
            data('-'),
            data('+')
        );

        let result = super::diff(&diff);

        assert_eq!(
            result.text,
            format!("{header} {begin}-----\n {REDACTED}\n-{REDACTED}\n+{REDACTED}\n {end}-----\n")
        );
        let found: Vec<(Option<usize>, Change)> = result
            .findings
            .iter()
            .map(|finding| (finding.line, finding.change))
            .collect();
        let expected = [
            (Some(2), Change::Unchanged),
            (Some(3), Change::Removed),
            (Some(3), Change::Added),
        ];
        assert_eq!(found, expected);
        assert_eq!(diff_findings(&diff), result.findings);
    }

    /// Every way of changing a private key gives its findings, the new key's data as added
    /// wherever git puts the old key's lines, and the model sees no line of either key's data.
    #[test]
    fn finds_every_way_of_changing_a_key_and_shows_none_of_its_data() {
        let data = KEY_TAGS.map(key_data);
        for change in key_edits() {
            let result = super::diff(&change.diff);

            let found: Vec<(usize, Change)> = result
                .findings
                .iter()
                .map(|finding| (finding.line.unwrap_or_default(), finding.change))
                .collect();
            assert_eq!(found, change.found, "{}", change.name);
            assert_eq!(
                diff_findings(&change.diff),
                result.findings,
                "{}",
                change.name
            );
            let redacted = result.text.matches(REDACTED).count();
            assert_eq!(
                redacted, change.redacted,
                "{}: {}",
                change.name, result.text
            );
            let shown = data.iter().find(|line| result.text.contains(line.as_str()));
            assert_eq!(shown, None, "{}: {}", change.name, result.text);
        }
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

    /// A diff's long line outside the hunks needs no more than its start, which still counts as a
    /// file's header; a hunk's comes back with its markers, and what its windows find lands on its
    /// number, the lines after it keeping theirs.
    #[test]
    fn takes_a_long_line_of_a_diff_by_where_it_is() {
        let mut scan = DiffScan::default();

        assert!(scan.start_long_line("diff --git a/x.env b/x.env").is_none());
        scan.line("@@ -1,2 +1,3 @@\n");
        scan.line(" same\n");
        let text = scan.start_long_line("+added").expect("a hunk's line");
        assert_eq!(text.markers(), 1);
        let windows = Windows {
            secrets: vec![Secret {
                kind: "AWS access key".to_owned(),
                print: crate::secrets::fingerprint::Key::for_run().of(b"key"),
            }],
            ends_with_begin: false,
        };
        scan.end_long_line(text, windows);
        scan.line(&format!("+{}\n", crate::secrets::test_support::aws_key()));

        let found: Vec<(String, Option<usize>, Change)> = scan
            .findings()
            .into_iter()
            .map(|finding| (finding.place.to_string(), finding.line, finding.change))
            .collect();
        let at = |line| ("x.env".to_owned(), Some(line), Change::Added);
        assert_eq!(found, [at(2), at(3)]);
    }

    /// A line costs time in proportion to its markers, not to their square, so a merge's combined
    /// diff with thousands of parents is read about as fast as git writes it: about a second here
    /// in a debug build, against 15 when each parent reads all the markers again. The bound sits
    /// between the two.
    #[test]
    fn reads_a_line_in_time_in_proportion_to_the_merges_parents() {
        let parents = 16_384;
        let signs = "@".repeat(parents + 1);
        let ranges = "-1,1000 ".repeat(parents);
        let line = format!("{}kept\n", " ".repeat(parents));
        let diff = format!(
            "diff --cc f\n--- a/f\n+++ b/f\n{signs} {ranges}+1,1000 {signs}\n{}",
            line.repeat(1000)
        );

        let start = Instant::now();
        let found = diff_findings(&diff);

        assert!(found.is_empty());
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{:?}",
            start.elapsed()
        );
    }

    /// A private key's block that every parent of a merge holds costs each side one hash of each
    /// data line, in one piece: a few seconds here in a debug build for 512 parents and 800 KiB of
    /// key data, which a byte at a time would take well over the limit to hash.
    #[test]
    fn reads_a_key_every_parent_holds_in_time() {
        use crate::secrets::test_support::{key_begin, key_end};
        let parents = 512;
        let signs = "@".repeat(parents + 1);
        let ranges = "-1,52 ".repeat(parents);
        let kept = |text: &str| format!("{}{text}\n", " ".repeat(parents));
        let data: String = (0..50).map(|_| kept(&"A".repeat(16 * 1024))).collect();
        let diff = format!(
            "diff --cc k\n--- a/k\n+++ b/k\n{signs} {ranges}+1,52 {signs}\n{}{data}{}",
            kept(&key_begin("OPENSSH")),
            kept(&key_end("OPENSSH")),
        );

        let start = Instant::now();
        let found = diff_findings(&diff);

        assert_eq!(found.len(), 1);
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{:?}",
            start.elapsed()
        );
    }
}
