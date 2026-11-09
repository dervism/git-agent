//! Secrets in what goes out: finding them and putting `[REDACTED]` in their place before text
//! reaches a model, and refusing or warning when a commit or a push would add them.

pub mod redact;

use crate::config::{Config, OnSecrets};
use crate::git::{CommitPatch, Repo};
use crate::ui;
use anyhow::{Result, bail};
use redact::{Change, Finding};

/// Lists the secrets that were replaced before sending, then gives `advice`.
pub fn report_secrets(findings: &[Finding], advice: &str) {
    if findings.is_empty() {
        return;
    }
    ui::warn(format!(
        "found {} possible secret(s); the model gets {} in their place:",
        findings.len(),
        redact::REDACTED
    ));
    ui::info(list_findings(&findings.iter().collect::<Vec<_>>()));
    ui::info(ui::dim(&format!("   {advice}")));
}

/// Fails if the change adds any of the secrets in `findings`, listing them
/// after `what` happened, then saying how to `fix` it.
pub fn reject_added(findings: &[Finding], what: &str, fix: &str) -> Result<()> {
    let added = added(findings);
    if added.is_empty() {
        return Ok(());
    }
    bail!(
        "{what}:\n{}\n{fix} If they aren't real secrets, add --on-secrets warn, or choose Warn \
         under Secrets in `git agent config`.",
        list_findings(&added)
    );
}

/// Checks the commits a push would send. If one adds what looks like a
/// secret, fails when `on_secrets` is reject and warns otherwise.
pub fn check_outgoing(repo: &Repo, config: &Config) -> Result<()> {
    let findings = scan_commits(&repo.outgoing_patches(&config.exclude)?);
    match config.on_secrets {
        OnSecrets::Reject => reject_added(
            &findings,
            "commits the push would send add what look like secrets",
            "Remove them from those commits, for example with `git rebase -i`.",
        ),
        OnSecrets::Warn => {
            let added = added(&findings);
            if !added.is_empty() {
                ui::warn(format!(
                    "pushing commits that add what look like secrets:\n{}",
                    list_findings(&added)
                ));
            }
            Ok(())
        }
    }
}

pub fn scan_commits(commits: &[CommitPatch]) -> Vec<Finding> {
    commits
        .iter()
        .flat_map(|commit| redact::commit(&commit.hash, &commit.message, &commit.patch))
        .collect()
}

/// The findings the change adds, rather than leaves alone or removes.
pub fn added(findings: &[Finding]) -> Vec<&Finding> {
    findings
        .iter()
        .filter(|finding| finding.change == Change::Added)
        .collect()
}

/// The first few findings, one per line.
pub fn list_findings(findings: &[&Finding]) -> String {
    const SHOWN: usize = 5;
    let mut lines: Vec<String> = findings
        .iter()
        .take(SHOWN)
        .map(|finding| {
            let place = match finding.line {
                Some(line) => format!("{}:{line}", finding.place),
                None => finding.place.clone(),
            };
            let change = match finding.change {
                Change::Added => "",
                Change::Unchanged => ", on an unchanged line",
                Change::Removed => ", on a removed line",
            };
            format!("   {place}  {}{change}", finding.kind)
        })
        .collect();
    if findings.len() > SHOWN {
        lines.push(format!("   … and {} more", findings.len() - SHOWN));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support::{isolated_repo, run};
    use std::fs;
    use std::path::Path;

    const ADVICE: &str = "If they aren't real secrets, add --on-secrets warn, or choose Warn \
                          under Secrets in `git agent config`.";

    fn finding(place: &str, line: Option<usize>, change: Change) -> Finding {
        Finding {
            place: place.to_owned(),
            line,
            change,
            kind: "GitHub token".to_owned(),
        }
    }

    fn key() -> String {
        format!("AKIA{}", "Q".repeat(16))
    }

    fn commit_file(dir: &Path, text: &str, message: &str) -> String {
        fs::write(dir.join("deploy.env"), text).unwrap();
        run(dir, &["add", "deploy.env"]);
        run(dir, &["commit", "-q", "-m", message]);
        run(dir, &["rev-parse", "--short", "HEAD"])
            .trim()
            .to_owned()
    }

    fn config(on_secrets: OnSecrets) -> Config {
        Config {
            on_secrets,
            ..Config::default()
        }
    }

    #[test]
    fn lists_each_finding_with_its_place_and_what_the_change_does_to_it() {
        let findings = [
            finding("src/a.rs", Some(3), Change::Added),
            finding("commit 1a2b3c4 message", None, Change::Unchanged),
            finding("b.env", Some(9), Change::Removed),
        ];
        assert_eq!(
            list_findings(&findings.iter().collect::<Vec<_>>()),
            "   src/a.rs:3  GitHub token\n   commit 1a2b3c4 message  GitHub token, on an \
             unchanged line\n   b.env:9  GitHub token, on a removed line"
        );
        assert_eq!(list_findings(&[]), "");
    }

    #[test]
    fn lists_the_first_five_and_counts_the_rest() {
        let findings: Vec<Finding> = (1..=6)
            .map(|line| finding("a.rs", Some(line), Change::Added))
            .collect();
        let all: Vec<&Finding> = findings.iter().collect();
        let five = list_findings(&all[..5]);
        assert_eq!(five.lines().count(), 5);
        assert!(five.ends_with("a.rs:5  GitHub token"), "{five}");
        let six = list_findings(&all);
        assert_eq!(six.lines().count(), 6);
        assert!(
            six.ends_with("a.rs:5  GitHub token\n   … and 1 more"),
            "{six}"
        );
    }

    #[test]
    fn rejects_only_the_secrets_a_change_adds() {
        let left_alone = [
            finding("a.rs", Some(1), Change::Unchanged),
            finding("a.rs", Some(2), Change::Removed),
        ];
        assert!(added(&left_alone).is_empty());
        assert!(reject_added(&left_alone, "stopped", "Fix it.").is_ok());

        let findings = [
            finding("a.rs", Some(1), Change::Unchanged),
            finding(".env", Some(2), Change::Added),
        ];
        assert_eq!(added(&findings), [&findings[1]]);
        let error = reject_added(&findings, "stopped", "Fix it.").unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("stopped:\n   .env:2  GitHub token\nFix it. {ADVICE}")
        );
    }

    #[test]
    fn scans_the_message_and_the_patch_of_each_commit() {
        let commits = [
            CommitPatch {
                hash: "1a2b3c4".to_owned(),
                message: format!("add {}", key()),
                patch: String::new(),
            },
            CommitPatch {
                hash: "5d6e7f8".to_owned(),
                message: "tidy".to_owned(),
                patch: format!(
                    "diff --git a/x.env b/x.env\n--- a/x.env\n+++ b/x.env\n@@ -0,0 +1 @@\n+{}\n",
                    key()
                ),
            },
        ];
        let places: Vec<(String, Option<usize>)> = scan_commits(&commits)
            .into_iter()
            .map(|finding| (finding.place, finding.line))
            .collect();
        assert_eq!(
            places,
            [
                ("commit 1a2b3c4 message".to_owned(), None),
                ("commit 5d6e7f8 x.env".to_owned(), Some(1)),
            ]
        );
        assert!(scan_commits(&[]).is_empty());
    }

    #[test]
    fn refuses_a_push_that_adds_a_secret_when_set_to_reject() {
        let (dir, repo) = isolated_repo();
        let hash = commit_file(
            dir.path(),
            &format!("aws {}\n", key()),
            "add the deploy settings",
        );

        let error = check_outgoing(&repo, &config(OnSecrets::Reject)).unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "commits the push would send add what look like secrets:\n   commit {hash} \
                 deploy.env:1  AWS access key\nRemove them from those commits, for example with \
                 `git rebase -i`. {ADVICE}"
            )
        );
    }

    #[test]
    fn lets_the_push_through_when_set_to_warn() {
        let (dir, repo) = isolated_repo();
        commit_file(
            dir.path(),
            &format!("aws {}\n", key()),
            "add the deploy settings",
        );

        assert!(check_outgoing(&repo, &config(OnSecrets::Warn)).is_ok());
    }

    #[test]
    fn lets_through_a_push_that_only_removes_a_secret() {
        let (dir, repo) = isolated_repo();
        commit_file(
            dir.path(),
            &format!("aws {}\n", key()),
            "add the deploy settings",
        );
        run(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        commit_file(dir.path(), "aws none\n", "take the key out");

        assert!(check_outgoing(&repo, &config(OnSecrets::Reject)).is_ok());
    }
}
