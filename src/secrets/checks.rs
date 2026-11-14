//! What a run's checks go by: the setting, and what the checks of commits found, which the push
//! doesn't list again; and what an earlier check found, for a later one to look its own up in.

use super::finding::{FileIdentity, Finding, Identity, Scanned, Unchecked};
use crate::config::OnSecrets;
use std::collections::HashSet;

/// What an earlier check found, for a later check to look up by the rule of [`Finding::held_by`]
/// and [`Unchecked::held_by`], in time that doesn't grow with how much was found.
pub struct Earlier<K>(HashSet<K>);

impl<'a> Earlier<Identity<'a>> {
    pub fn findings(findings: impl IntoIterator<Item = &'a Finding>) -> Self {
        Self(findings.into_iter().map(Finding::identity).collect())
    }

    /// Whether a finding found before holds `finding`.
    pub fn holds(&self, finding: &Finding) -> bool {
        finding.held_by().any(|identity| self.0.contains(&identity))
    }
}

impl<'a> Earlier<FileIdentity<'a>> {
    pub fn files(files: impl IntoIterator<Item = &'a Unchecked>) -> Self {
        Self(files.into_iter().map(Unchecked::identity).collect())
    }

    /// Whether a file found before holds `file`.
    pub fn holds(&self, file: &Unchecked) -> bool {
        file.held_by().any(|identity| self.0.contains(&identity))
    }
}

/// What the checks of one run go by: the setting, and what the checks that read commits found,
/// each made commit's and the pull request's commits'. The push leaves out their findings under
/// warn, and their files too large to check under either setting, since those never refuse.
pub struct Checks {
    on_secrets: OnSecrets,
    in_commits: Scanned,
}

impl Checks {
    pub fn new(on_secrets: OnSecrets) -> Self {
        Self {
            on_secrets,
            in_commits: Scanned::default(),
        }
    }

    pub fn on_secrets(&self) -> OnSecrets {
        self.on_secrets
    }

    /// Keeps what a check that read commits found in them: the findings in a commit's files or
    /// message, and the files in a commit too large to check.
    pub(super) fn record(&mut self, read: Scanned) {
        let findings = read
            .findings
            .into_iter()
            .filter(|finding| finding.place.is_in_a_commit());
        self.in_commits.findings.extend(findings);
        let files = read
            .unchecked
            .into_iter()
            .filter(|file| file.place.commit.is_some());
        self.in_commits.unchecked.extend(files);
    }

    /// The findings the run's checks of commits found.
    pub(super) fn findings(&self) -> &[Finding] {
        &self.in_commits.findings
    }

    /// The files the run's checks of commits found too large to check.
    pub(super) fn unchecked(&self) -> &[Unchecked] {
        &self.in_commits.unchecked
    }
}

#[cfg(test)]
mod tests {
    use super::super::finding::test_support::{
        every_kind_of_finding, in_commit, print_of, rule_holds,
    };
    use super::super::finding::{Change, FileRef, Place};
    use super::*;
    use crate::git::CommitId;

    fn found(place: Place, secret: &str) -> Finding {
        Finding {
            place,
            line: Some(1),
            change: Change::Added,
            kind: "GitHub token".to_owned(),
            print: print_of(secret, false),
        }
    }

    /// What was listed holds a finding as one of them holds it, whichever it is, and nothing
    /// holds a finding when nothing was listed.
    #[test]
    fn holds_a_finding_that_one_listed_before_holds() {
        let listed = [
            found(Place::file("a.env"), "one"),
            found(Place::file("b.env"), "one"),
            found(Place::file("a.env"), "two"),
        ];
        let held = Earlier::findings(&listed);
        let cases = [
            (found(in_commit("1a2b3c4", "a.env"), "one"), true),
            (found(in_commit("1a2b3c4", "b.env"), "one"), true),
            (found(in_commit("1a2b3c4", "a.env"), "two"), true),
            (found(in_commit("1a2b3c4", "b.env"), "two"), false),
            (found(Place::file("c.env"), "one"), false),
            (found(Place::file("a.env"), "three"), false),
        ];
        for (finding, holds) in cases {
            assert_eq!(held.holds(&finding), holds, "{finding:?}");
        }
        assert!(!Earlier::findings([]).holds(&listed[0]));
    }

    /// The run keeps only what is in a commit: the findings in a commit's files and message, and
    /// the files in a commit too large to check.
    #[test]
    fn keeps_what_the_checks_of_commits_found_in_commits() {
        let commit = CommitId::of("1a2b3c4");
        let in_a_commit = [
            found(in_commit("1a2b3c4", "a.env"), "one"),
            found(Place::Message(Some(commit.clone())), "two"),
        ];
        let elsewhere = [
            found(Place::file("a.env"), "one"),
            found(Place::Message(None), "two"),
            found(Place::Arguments, "three"),
            found(Place::text("issue #1"), "four"),
        ];
        let file = |place: FileRef| Unchecked {
            place,
            size: 1,
            blob: "a".repeat(40),
        };
        let big_in_commit = file(FileRef::changed("big.bin").in_commit(&commit));
        let read = Scanned {
            findings: elsewhere.iter().chain(&in_a_commit).cloned().collect(),
            unchecked: vec![file(FileRef::changed("big.bin")), big_in_commit.clone()],
        };
        let mut checks = Checks::new(OnSecrets::Warn);
        assert!(checks.findings().is_empty() && checks.unchecked().is_empty());

        checks.record(read);
        checks.record(Scanned::default());

        assert_eq!(checks.on_secrets(), OnSecrets::Warn);
        assert_eq!(checks.findings(), in_a_commit);
        assert_eq!(checks.unchecked(), [big_in_commit]);
    }

    /// A lookup in what was found before agrees with the rule, for findings of every kind, found
    /// before in groups.
    #[test]
    fn a_lookup_in_what_was_found_before_agrees_with_the_rule() {
        let all = every_kind_of_finding();
        for group in all.chunks(7) {
            let earlier = Earlier::findings(group);
            for later in &all {
                let held = group.iter().any(|before| rule_holds(before, later));
                assert_eq!(earlier.holds(later), held, "{later:?}");
            }
        }
    }

    /// A lookup of a file found before agrees with the rule: the same content, at the same path,
    /// in the same commit or, for a file of the changes, in any.
    #[test]
    fn a_lookup_of_a_file_found_before_agrees_with_the_rule() {
        let file = |commit: Option<&str>, path: &str, blob: &str| Unchecked {
            place: FileRef {
                commit: commit.map(CommitId::of),
                path: path.to_owned(),
            },
            size: 1,
            blob: blob.repeat(40),
        };
        let all: Vec<Unchecked> = [None, Some("1a2b3c4"), Some("5d6e7f8")]
            .into_iter()
            .flat_map(|commit| ["a.bin", "b.bin"].map(|path| (commit, path)))
            .flat_map(|(commit, path)| ["a", "b"].map(|blob| file(commit, path, blob)))
            .collect();
        for before in &all {
            let earlier = Earlier::files([before]);
            for later in &all {
                let held = before.blob == later.blob
                    && before.place.path == later.place.path
                    && (before.place.commit.is_none() || before.place.commit == later.place.commit);
                assert_eq!(earlier.holds(later), held, "{before:?} {later:?}");
                assert_eq!(before.holds(later), held, "{before:?} {later:?}");
            }
        }
    }
}
