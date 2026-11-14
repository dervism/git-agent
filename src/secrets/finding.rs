//! What a scan finds, and where: the words the scans, the checks and the flows share, and the rule
//! that tells when what was found before holds what is found later.

use super::fingerprint::Fingerprint;
use crate::git::CommitId;
use std::fmt;
use std::iter;

/// A secret that was found, and replaced wherever the text goes to a model.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Finding {
    pub place: Place,
    /// Its line in the file's new version, or in the old version for a removed
    /// line. `None` in plain text, whose lines may not match any file's.
    pub line: Option<usize>,
    pub change: Change,
    /// What it looks like, e.g. "GitHub token".
    pub kind: String,
    /// The secret's fingerprint, which tells it from another of the same kind.
    pub print: Fingerprint,
}

impl Finding {
    /// The finding as one in `commit`: a file of the changes is that file in its patch, and the
    /// message being made and the arguments, whose trailers join it, are its message. What names
    /// a commit already stays in it.
    pub fn in_commit(self, commit: &CommitId) -> Self {
        Self {
            place: self.place.in_commit(commit),
            ..self
        }
    }

    /// What a later finding looks this one up by.
    pub fn identity(&self) -> Identity<'_> {
        self.identity_at(self.place.spot())
    }

    /// The identities of the findings listed before that hold this one: the same secret, of the
    /// same kind, changed the same way, in a place that holds it, as `Place::held_at` tells; and for
    /// a print of only part of a secret, on the same line too, which a finding without one never is.
    pub fn held_by(&self) -> impl Iterator<Item = Identity<'_>> {
        let placed = !self.print.is_partial() || self.line.is_some();
        self.place
            .held_at()
            .filter(move |_| placed)
            .map(|spot| self.identity_at(spot))
    }

    /// Its identity at `spot`. Every field is named, so a new one can't be left out of the rule
    /// unnoticed.
    fn identity_at<'a>(&'a self, spot: Spot<'a>) -> Identity<'a> {
        let Finding {
            place: _,
            line,
            change,
            kind,
            print,
        } = self;
        Identity {
            print: *print,
            kind,
            change: *change,
            spot,
            line: line.filter(|_| print.is_partial()),
        }
    }
}

/// What the rule tells findings apart by: the secret, its kind and change, where it is, and for a
/// print of only part of a secret, its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Identity<'a> {
    print: Fingerprint,
    kind: &'a str,
    change: Change,
    spot: Spot<'a>,
    line: Option<usize>,
}

/// Where something is, as the rule tells places apart: a file by its path and commit, any other
/// place whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Spot<'a> {
    File {
        path: &'a str,
        commit: Option<&'a CommitId>,
    },
    Other(&'a Place),
}

/// What a change does to the line a secret is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Change {
    /// The change adds the line, or the secret is in a commit message.
    Added,
    /// The line is shown around a change, which leaves it as it was.
    Unchanged,
    /// The change removes the line, and the secret with it.
    Removed,
}

/// Where a finding is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Place {
    File(FileRef),
    /// A commit's message, or without a commit the message of the commit being made.
    Message(Option<CommitId>),
    /// The arguments the commit being made is made with.
    Arguments,
    /// Other text the model reads, by what it is, such as "issue #12" or "git blame".
    Text(String),
}

impl Place {
    /// The file at `path` among the changes.
    pub fn file(path: impl Into<String>) -> Self {
        Self::File(FileRef::changed(path))
    }

    /// Text the model reads, named `name`.
    pub fn text(name: impl Into<String>) -> Self {
        Self::Text(name.into())
    }

    fn in_commit(self, commit: &CommitId) -> Self {
        match self {
            Self::File(file) => Self::File(file.in_commit(commit)),
            Self::Message(None) | Self::Arguments => Self::Message(Some(commit.clone())),
            other => other,
        }
    }

    /// Whether the place is in a commit: a file of its patch, or its message.
    pub fn is_in_a_commit(&self) -> bool {
        match self {
            Self::File(file) => file.commit.is_some(),
            Self::Message(commit) => commit.is_some(),
            Self::Arguments | Self::Text(_) => false,
        }
    }

    fn spot(&self) -> Spot<'_> {
        match self {
            Self::File(file) => file.spot(),
            other => Spot::Other(other),
        }
    }

    /// The spots of the places a finding here is held at: its own, and for a file in a commit, that
    /// file among the changes, which holds it in any commit.
    fn held_at(&self) -> impl Iterator<Item = Spot<'_>> {
        let among_changes = match self {
            Self::File(file) => file.among_changes(),
            _ => None,
        };
        iter::once(self.spot()).chain(among_changes)
    }
}

impl fmt::Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(file) => file.fmt(f),
            Self::Message(Some(commit)) => write!(f, "commit {commit} message"),
            Self::Message(None) => f.write_str("message"),
            Self::Arguments => f.write_str("arguments"),
            Self::Text(name) => f.write_str(name),
        }
    }
}

/// A file of the changes, or of a commit's patch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct FileRef {
    pub commit: Option<CommitId>,
    pub path: String,
}

impl FileRef {
    /// The file at `path` among the changes.
    pub fn changed(path: impl Into<String>) -> Self {
        Self {
            commit: None,
            path: path.into(),
        }
    }

    /// The same file in `commit`'s patch, unless it names a commit already.
    pub fn in_commit(self, commit: &CommitId) -> Self {
        Self {
            commit: self.commit.or_else(|| Some(commit.clone())),
            ..self
        }
    }

    fn spot(&self) -> Spot<'_> {
        Spot::File {
            path: &self.path,
            commit: self.commit.as_ref(),
        }
    }

    fn held_at(&self) -> impl Iterator<Item = Spot<'_>> {
        iter::once(self.spot()).chain(self.among_changes())
    }

    /// For a file in a commit, the same file among the changes.
    fn among_changes(&self) -> Option<Spot<'_>> {
        self.commit.as_ref().map(|_| Spot::File {
            path: &self.path,
            commit: None,
        })
    }
}

impl fmt::Display for FileRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.commit {
            Some(commit) => write!(f, "commit {commit} {}", self.path),
            None => f.write_str(&self.path),
        }
    }
}

/// A binary file over the size the checks read, which they don't check.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Unchecked {
    pub place: FileRef,
    pub size: u64,
    /// The blob of its content, by its full hash.
    pub blob: String,
}

impl Unchecked {
    /// The same file in `commit`, unless it names a commit already.
    pub fn in_commit(self, commit: &CommitId) -> Self {
        Self {
            place: self.place.in_commit(commit),
            ..self
        }
    }

    /// What a later file looks this one up by.
    pub fn identity(&self) -> FileIdentity<'_> {
        self.identity_at(self.place.spot())
    }

    /// The identities of the files listed before that hold this one: the same content in a place
    /// that holds it, as `FileRef::held_at` tells.
    pub fn held_by(&self) -> impl Iterator<Item = FileIdentity<'_>> {
        self.place.held_at().map(|spot| self.identity_at(spot))
    }

    /// Its identity at `spot`. Every field is named, so a new one can't be left out of the rule
    /// unnoticed.
    fn identity_at<'a>(&'a self, spot: Spot<'a>) -> FileIdentity<'a> {
        let Unchecked {
            place: _,
            size: _,
            blob,
        } = self;
        FileIdentity { blob, spot }
    }
}

/// What the rule tells files too large to check apart by: their content, and where they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileIdentity<'a> {
    blob: &'a str,
    spot: Spot<'a>,
}

/// What a check read: the secrets it found, and the files it couldn't check.
#[derive(Debug, Default, PartialEq)]
pub struct Scanned {
    pub findings: Vec<Finding>,
    pub unchecked: Vec<Unchecked>,
}

impl Scanned {
    /// What was read just before `commits` were made, as in each of them, as [`Finding::in_commit`]
    /// and [`Unchecked::in_commit`] place each.
    pub fn in_commits(&self, commits: &[CommitId]) -> Self {
        let findings = commits.iter().flat_map(|commit| {
            let each = self.findings.iter().cloned();
            each.map(move |finding| finding.in_commit(commit))
        });
        let unchecked = commits.iter().flat_map(|commit| {
            let each = self.unchecked.iter().cloned();
            each.map(move |file| file.in_commit(commit))
        });
        Self {
            findings: findings.collect(),
            unchecked: unchecked.collect(),
        }
    }
}

/// What the tests of the rule share.
#[cfg(test)]
pub mod test_support {
    use super::super::fingerprint::Key;
    use super::*;

    /// Whether what was found before holds what is found later, by the keys a check looks them up
    /// by.
    impl Finding {
        pub fn holds(&self, later: &Finding) -> bool {
            let own = self.identity();
            later.held_by().any(|identity| identity == own)
        }
    }

    impl Place {
        pub fn holds(&self, later: &Place) -> bool {
            let own = self.spot();
            later.held_at().any(|spot| spot == own)
        }
    }

    impl Unchecked {
        pub fn holds(&self, later: &Unchecked) -> bool {
            let own = self.identity();
            later.held_by().any(|identity| identity == own)
        }
    }

    /// The print of `secret`, whole or as only part of what it stands for.
    pub fn print_of(secret: &str, partial: bool) -> Fingerprint {
        let mut print = Key::for_run().start();
        print.feed(secret.as_bytes());
        if partial {
            print.finish_partial()
        } else {
            print.finish()
        }
    }

    /// The file at `path` in the commit abbreviated `short`.
    pub fn in_commit(short: &str, path: &str) -> Place {
        Place::file(path).in_commit(&CommitId::of(short))
    }

    /// The rule as the design states it, to check the keys against.
    pub fn rule_holds(before: &Finding, later: &Finding) -> bool {
        let Finding {
            place,
            line,
            change,
            kind,
            print,
        } = before;
        let place_holds = match (place, &later.place) {
            (Place::File(old), Place::File(new)) => {
                old.path == new.path && (old.commit.is_none() || old.commit == new.commit)
            }
            (old, new) => old == new,
        };
        let line_holds = !later.print.is_partial() || later.line.is_some() && *line == later.line;
        *kind == later.kind
            && *change == later.change
            && *print == later.print
            && place_holds
            && line_holds
    }

    /// Places that hold each other, or don't, in every way the rule tells them apart.
    fn every_kind_of_place() -> [Place; 10] {
        [
            Place::file("a.env"),
            Place::file("b.env"),
            in_commit("1a2b3c4", "a.env"),
            in_commit("5d6e7f8", "a.env"),
            in_commit("1a2b3c4", "b.env"),
            Place::Message(None),
            Place::Message(Some(CommitId::of("1a2b3c4"))),
            Place::Message(Some(CommitId::of("5d6e7f8"))),
            Place::Arguments,
            Place::text("a.env"),
        ]
    }

    /// Findings in every combination of print, kind, change, place and line.
    pub fn every_kind_of_finding() -> Vec<Finding> {
        let prints = [
            print_of("one", false),
            print_of("two", false),
            print_of("one", true),
        ];
        let places = every_kind_of_place();
        let kinds = ["GitHub token", "AWS access key"];
        let changes = [Change::Added, Change::Removed];
        let lines = [None, Some(1), Some(2)];
        let found = |print, place: &Place, kind: &str, change, line| Finding {
            place: place.clone(),
            line,
            change,
            kind: kind.to_owned(),
            print,
        };
        prints
            .iter()
            .flat_map(|&print| places.iter().map(move |place| (print, place)))
            .flat_map(|(print, place)| kinds.iter().map(move |kind| (print, place, kind)))
            .flat_map(|(print, place, kind)| {
                changes
                    .iter()
                    .map(move |&change| (print, place, kind, change))
            })
            .flat_map(|(print, place, kind, change)| {
                lines
                    .iter()
                    .map(move |&line| found(print, place, kind, change, line))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{every_kind_of_finding, in_commit, print_of, rule_holds};
    use super::*;

    #[test]
    fn places_read_as_they_always_have() {
        let commit = CommitId::of("1a2b3c4");
        let cases = [
            (Place::file("src/my file.rs"), "src/my file.rs"),
            (in_commit("1a2b3c4", "src/a.rs"), "commit 1a2b3c4 src/a.rs"),
            (Place::Message(Some(commit)), "commit 1a2b3c4 message"),
            (Place::Message(None), "message"),
            (Place::Arguments, "arguments"),
            (Place::text("issue #12"), "issue #12"),
        ];
        for (place, shown) in cases {
            assert_eq!(place.to_string(), shown);
        }
    }

    #[test]
    fn a_file_of_the_changes_holds_that_file_in_any_commit() {
        let changed = Place::file("src/my file.rs");
        let cases = [
            (Place::file("src/my file.rs"), true),
            (in_commit("1a2b3c4", "src/my file.rs"), true),
            (in_commit("5d6e7f8", "src/my file.rs"), true),
            (Place::file("my file.rs"), false),
            (in_commit("1a2b3c4", "my file.rs"), false),
        ];
        for (later, held) in cases {
            assert_eq!(changed.holds(&later), held, "{later}");
        }
    }

    #[test]
    fn a_file_in_a_commit_holds_only_that_file_in_that_commit() {
        let shown = in_commit("1a2b3c4", "src/a.rs");
        assert!(shown.holds(&in_commit("1a2b3c4", "src/a.rs")));
        assert!(!shown.holds(&in_commit("5d6e7f8", "src/a.rs")));
        assert!(!shown.holds(&Place::file("src/a.rs")));
    }

    #[test]
    fn files_named_message_or_arguments_never_hold_a_message_or_the_arguments() {
        let commit = CommitId::of("1a2b3c4");
        let message = Place::Message(Some(commit.clone()));
        let file = Place::file("message");
        let file_in_commit = in_commit("1a2b3c4", "message");
        assert!(!file.holds(&message));
        assert!(!message.holds(&file_in_commit));
        assert!(!file_in_commit.holds(&message));
        assert!(!Place::Message(None).holds(&file));
        assert!(!Place::Message(None).holds(&file_in_commit));
        for arguments in [Place::file("arguments"), in_commit("1a2b3c4", "arguments")] {
            assert!(!arguments.holds(&Place::Arguments));
            assert!(!Place::Arguments.holds(&arguments));
        }
    }

    #[test]
    fn a_path_that_starts_like_a_commit_is_matched_whole() {
        let odd = Place::file("commit 1a2b3c4 notes.txt");
        assert!(!odd.holds(&in_commit("1a2b3c4", "notes.txt")));
        assert!(!Place::file("notes.txt").holds(&odd));
        assert!(odd.holds(&in_commit("9f8e7d6", "commit 1a2b3c4 notes.txt")));
    }

    /// A message, the arguments and other text each hold only themselves: a commit's message
    /// only that commit's, and the message being made only itself.
    #[test]
    fn a_message_holds_only_itself() {
        let first = Place::Message(Some(CommitId::of("1a2b3c4")));
        let second = Place::Message(Some(CommitId::of("5d6e7f8")));
        let being_made = Place::Message(None);
        let cases = [
            (first.clone(), first.clone(), true),
            (first.clone(), second, false),
            (first.clone(), being_made.clone(), false),
            (being_made.clone(), first, false),
            (being_made.clone(), Place::Message(None), true),
            (being_made.clone(), Place::Arguments, false),
            (Place::Arguments, Place::Arguments, true),
            (Place::Arguments, being_made.clone(), false),
            (Place::text("message"), being_made, false),
            (Place::text("issue #1"), Place::text("issue #1"), true),
            (Place::text("issue #1"), Place::text("issue #2"), false),
        ];
        for (listed, later, holds) in cases {
            assert_eq!(listed.holds(&later), holds, "{listed} {later}");
        }
    }

    /// Bound to the commit made, the changes' files become its files, and the message being made
    /// and the arguments its message; what is already in a commit and other text stay as they
    /// were.
    #[test]
    fn binds_the_changes_and_the_message_being_made_to_the_commit_made() {
        let made = CommitId::of("1a2b3c4");
        let cases = [
            (Place::file("a.rs"), in_commit("1a2b3c4", "a.rs")),
            (Place::Message(None), Place::Message(Some(made.clone()))),
            (in_commit("5d6e7f8", "a.rs"), in_commit("5d6e7f8", "a.rs")),
            (
                Place::Message(Some(CommitId::of("5d6e7f8"))),
                Place::Message(Some(CommitId::of("5d6e7f8"))),
            ),
            (Place::Arguments, Place::Message(Some(made.clone()))),
            (Place::text("issue #1"), Place::text("issue #1")),
        ];
        for (before, bound) in cases {
            assert_eq!(before.clone().in_commit(&made), bound, "{before}");
        }
    }

    #[test]
    fn tells_the_places_in_a_commit() {
        let cases = [
            (in_commit("1a2b3c4", "a.rs"), true),
            (Place::Message(Some(CommitId::of("1a2b3c4"))), true),
            (Place::file("a.rs"), false),
            (Place::Message(None), false),
            (Place::Arguments, false),
            (Place::text("issue #1"), false),
        ];
        for (place, in_a_commit) in cases {
            assert_eq!(place.is_in_a_commit(), in_a_commit, "{place}");
        }
    }

    fn found(place: Place, line: Option<usize>) -> Finding {
        Finding {
            place,
            line,
            change: Change::Added,
            kind: "GitHub token".to_owned(),
            print: print_of("one token", false),
        }
    }

    /// A finding holds a later one of the same secret, kind and change, in a place it holds, on
    /// any line; any of them told apart, it doesn't.
    #[test]
    fn holds_the_same_secret_of_the_same_kind_changed_the_same_way_in_a_place_it_holds() {
        let listed = found(Place::file("src/my file.rs"), Some(2));
        let cases = [
            (found(Place::file("src/my file.rs"), Some(2)), true),
            (found(Place::file("src/my file.rs"), Some(9)), true),
            (found(Place::file("src/my file.rs"), None), true),
            (found(in_commit("1a2b3c4", "src/my file.rs"), Some(2)), true),
            (found(in_commit("5d6e7f8", "src/my file.rs"), Some(7)), true),
            (found(Place::file("my file.rs"), Some(2)), false),
            (found(Place::Message(None), None), false),
            (
                Finding {
                    print: print_of("another token", false),
                    ..found(Place::file("src/my file.rs"), Some(2))
                },
                false,
            ),
            (
                Finding {
                    kind: "AWS access key".to_owned(),
                    ..found(Place::file("src/my file.rs"), Some(2))
                },
                false,
            ),
            (
                Finding {
                    change: Change::Unchanged,
                    ..found(Place::file("src/my file.rs"), Some(2))
                },
                false,
            ),
        ];
        for (later, held) in cases {
            assert_eq!(listed.holds(&later), held, "{later:?}");
        }
    }

    #[test]
    fn a_finding_in_a_commit_holds_only_the_same_file_in_that_commit() {
        let listed = found(in_commit("1a2b3c4", "a.env"), Some(2));
        assert!(listed.holds(&found(in_commit("1a2b3c4", "a.env"), Some(5))));
        assert!(!listed.holds(&found(in_commit("5d6e7f8", "a.env"), Some(2))));
        assert!(!listed.holds(&found(Place::file("a.env"), Some(2))));
        assert!(!listed.holds(&found(in_commit("1a2b3c4", "b.env"), Some(2))));
    }

    /// A partial print stands for only part of a secret, so it holds another only on the same
    /// line, which both must have; a whole print never holds a partial one.
    #[test]
    fn a_partial_print_holds_only_on_the_same_line() {
        let cut = |line: Option<usize>| Finding {
            print: print_of("-----BEGIN", true),
            ..found(Place::file("id_key"), line)
        };
        assert!(cut(Some(3)).holds(&cut(Some(3))));
        assert!(!cut(Some(3)).holds(&cut(Some(4))));
        assert!(!cut(None).holds(&cut(None)));
        assert!(!cut(None).holds(&cut(Some(3))));
        assert!(!cut(Some(3)).holds(&cut(None)));
        let whole = Finding {
            print: print_of("-----BEGIN", false),
            ..found(Place::file("id_key"), Some(3))
        };
        assert!(!whole.holds(&cut(Some(3))));
        assert!(!cut(Some(3)).holds(&whole));
    }

    fn unchecked(place: FileRef, blob: &str) -> Unchecked {
        Unchecked {
            place,
            size: 1,
            blob: blob.repeat(40),
        }
    }

    /// A file too large to check holds a later one with the same content, in a place it holds.
    #[test]
    fn a_file_too_large_to_check_holds_the_same_content_in_a_place_it_holds() {
        let first = CommitId::of("1a2b3c4");
        let changed = unchecked(FileRef::changed("big.bin"), "a");
        let committed = unchecked(FileRef::changed("big.bin").in_commit(&first), "a");
        let cases = [
            (&changed, unchecked(FileRef::changed("big.bin"), "a"), true),
            (&changed, committed.clone(), true),
            (&changed, unchecked(FileRef::changed("big.bin"), "b"), false),
            (
                &changed,
                unchecked(FileRef::changed("other.bin"), "a"),
                false,
            ),
            (&committed, committed.clone(), true),
            (&committed, changed.clone(), false),
            (
                &committed,
                unchecked(
                    FileRef::changed("big.bin").in_commit(&CommitId::of("5d6e7f8")),
                    "a",
                ),
                false,
            ),
        ];
        for (listed, later, holds) in cases {
            assert_eq!(listed.holds(&later), holds, "{listed:?} {later:?}");
        }
    }

    /// What a check read before a commit: a file of the changes, the message being made, the
    /// arguments, and a file too large to check.
    fn read_before_a_commit() -> Scanned {
        let at = |place: Place| Finding {
            place,
            line: None,
            change: Change::Added,
            kind: "AWS access key".to_owned(),
            print: Fingerprint::unmatched(),
        };
        Scanned {
            findings: vec![
                at(Place::file("a.env")),
                at(Place::Message(None)),
                at(Place::Arguments),
            ],
            unchecked: vec![unchecked(FileRef::changed("big.bin"), "a")],
        }
    }

    /// What was read before a commit, in the commit made: each finding and file as it is placed
    /// there.
    #[test]
    fn binds_what_was_read_before_a_commit_to_the_commit_made() {
        let made = CommitId::of("1a2b3c4");
        let read = read_before_a_commit();

        let bound = read.in_commits(std::slice::from_ref(&made));

        let moved_to = |index: usize, place: Place| Finding {
            place,
            ..read.findings[index].clone()
        };
        let in_made = Place::File(FileRef::changed("a.env").in_commit(&made));
        let made_message = Place::Message(Some(made.clone()));
        let findings = [
            moved_to(0, in_made),
            moved_to(1, made_message.clone()),
            moved_to(2, made_message),
        ];
        assert_eq!(bound.findings, findings);
        let big = unchecked(FileRef::changed("big.bin").in_commit(&made), "a");
        assert_eq!(bound.unchecked, [big]);
    }

    /// Bound to several commits made, what was read is in each of them, in their order; bound to
    /// none, or with nothing read, it's nothing.
    #[test]
    fn binds_what_was_read_to_each_commit_made() {
        let (made, other) = (CommitId::of("1a2b3c4"), CommitId::of("5d6e7f8"));
        let read = read_before_a_commit();

        let in_both = read.in_commits(&[made.clone(), other.clone()]);

        assert_eq!(in_both.findings.len(), 6);
        assert_eq!(
            in_both.findings[0].place,
            Place::file("a.env").in_commit(&made)
        );
        assert_eq!(
            in_both.findings[3].place,
            Place::file("a.env").in_commit(&other)
        );
        assert_eq!(in_both.unchecked.len(), 2);
        assert_eq!(read.in_commits(&[]), Scanned::default());
        let nothing = Scanned::default().in_commits(std::slice::from_ref(&made));
        assert_eq!(nothing, Scanned::default());
    }

    /// The keys a finding is looked up by agree with the rule, for every pair of findings.
    #[test]
    fn the_keys_agree_with_the_rule() {
        let all = every_kind_of_finding();
        assert_eq!(all.len(), 360);
        for before in &all {
            for later in &all {
                assert_eq!(
                    before.holds(later),
                    rule_holds(before, later),
                    "{before:?} {later:?}"
                );
            }
        }
    }
}
