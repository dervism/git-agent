//! What a scan finds, and where: the words the scans, the checks and the flows share.

use super::fingerprint::Fingerprint;
use crate::git::CommitId;
use std::fmt;

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
    /// The finding as one in `commit`, whose patch holds the file it names.
    pub fn in_commit(self, commit: &CommitId) -> Self {
        Self {
            place: self.place.in_commit(commit),
            ..self
        }
    }
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
            other => other,
        }
    }

    /// Whether a finding here, listed before, holds one at `later`: the same place, a file of
    /// the changes holds that file in a commit, and the message of the commit being made holds
    /// a commit's message.
    pub fn holds(&self, later: &Place) -> bool {
        match (self, later) {
            (Self::File(before), Self::File(file)) => before.holds(file),
            (Self::Message(None), Self::Message(_)) => true,
            _ => self == later,
        }
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

    /// The same file in `commit`'s patch.
    pub fn in_commit(self, commit: &CommitId) -> Self {
        Self {
            commit: Some(commit.clone()),
            ..self
        }
    }

    /// Whether this file, listed before, holds `later`: the same path, in the same commit or,
    /// for a file of the changes, in any.
    pub fn holds(&self, later: &FileRef) -> bool {
        self.path == later.path && (self.commit.is_none() || self.commit == later.commit)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn in_commit(short: &str, path: &str) -> Place {
        Place::file(path).in_commit(&CommitId::of(short))
    }

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

    #[test]
    fn messages_hold_their_own_commit_and_the_one_being_made_holds_any() {
        let first = Place::Message(Some(CommitId::of("1a2b3c4")));
        let second = Place::Message(Some(CommitId::of("5d6e7f8")));
        assert!(first.holds(&first.clone()));
        assert!(!first.holds(&second));
        assert!(!first.holds(&Place::Message(None)));
        assert!(Place::Message(None).holds(&first));
        assert!(Place::Arguments.holds(&Place::Arguments));
        assert!(!Place::Arguments.holds(&Place::Message(None)));
        assert!(!Place::text("message").holds(&Place::Message(None)));
    }
}
