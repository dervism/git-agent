//! A commit as git names it.

use std::fmt;
use std::hash::{Hash, Hasher};

/// A commit, the same as another by its full hash, and shown by the abbreviation git gives it.
#[derive(Debug, Clone)]
pub struct CommitId {
    full: String,
    short: String,
}

/// How long a full hash is: SHA-1's and SHA-256's, in hex digits.
const FULL_LENGTHS: [usize; 2] = [40, 64];

/// The shortest abbreviation git writes.
const SHORTEST: usize = 4;

impl CommitId {
    /// The commit whose full hash is `full`, which git abbreviates `short`: `full` is a SHA-1 or
    /// SHA-256 hash in lowercase hex, and `short` its first 4 digits or more. `None` otherwise.
    pub fn new(full: &str, short: &str) -> Option<Self> {
        let hex = full
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        let abbreviates = short.len() >= SHORTEST && full.starts_with(short);
        (FULL_LENGTHS.contains(&full.len()) && hex && abbreviates).then(|| Self {
            full: full.to_owned(),
            short: short.to_owned(),
        })
    }

    /// A commit for tests, whose full hash is `short` followed by zeros.
    #[cfg(test)]
    pub fn of(short: &str) -> Self {
        let full = format!("{short:0<40}");
        Self::new(&full, short).expect("a test commit's hash")
    }

    /// The abbreviation git gives the commit.
    pub fn short(&self) -> &str {
        &self.short
    }
}

impl PartialEq for CommitId {
    fn eq(&self, other: &Self) -> bool {
        self.full == other.full
    }
}

impl Eq for CommitId {}

impl Hash for CommitId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.full.hash(state);
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.short)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commits_are_the_same_by_their_full_hash_whatever_their_abbreviation() {
        let full = format!("1a2b3c4d{}", "e".repeat(32));
        let seven = CommitId::new(&full, "1a2b3c4").unwrap();
        let nine = CommitId::new(&full, "1a2b3c4de").unwrap();
        assert_eq!(seven, nine);
        assert_eq!(nine.to_string(), "1a2b3c4de");
        let other = CommitId::new(&format!("1a2b3c4f{}", "e".repeat(32)), "1a2b3c4").unwrap();
        assert_ne!(seven, other);
    }

    #[test]
    fn a_commit_needs_an_abbreviation_of_its_hash() {
        let full = "1a2b3c4d".repeat(5);
        assert!(CommitId::new(&full, "").is_none());
        assert!(CommitId::new("", "").is_none());
        assert!(CommitId::new(&full, "9f9f9f9").is_none());
        assert!(CommitId::new(&full, "1a2").is_none());
        assert!(CommitId::new(&full, "1a2b").is_some());
        assert!(CommitId::new(&full, &full).is_some());
        assert!(CommitId::new(&full, "1a2b3c4").is_some());
    }

    #[test]
    fn a_full_hash_is_a_sha1_or_sha256_hash_in_lowercase_hex() {
        let sha256 = "1a2b3c4d".repeat(8);
        assert!(CommitId::new(&sha256, "1a2b3c4").is_some());
        for full in [
            "1a2b3c4".to_owned(),
            "1a2b3c4d".repeat(5) + "e",
            "1A2B3C4D".repeat(5),
            format!("1a2b3c4{}", "g".repeat(33)),
            format!("1a2b3c4{}", " ".repeat(33)),
        ] {
            assert!(CommitId::new(&full, "1a2b3c4").is_none(), "{full:?}");
        }
        assert!(CommitId::new("Good \"git\" signature", "Good").is_none());
    }

    #[test]
    fn abbreviations_of_one_commit_hash_alike() {
        let full = format!("1a2b3c4d{}", "e".repeat(32));
        let ids: std::collections::HashSet<CommitId> = ["1a2b", "1a2b3c4", "1a2b3c4de"]
            .into_iter()
            .map(|short| CommitId::new(&full, short).unwrap())
            .collect();
        assert_eq!(ids.len(), 1);
    }
}
