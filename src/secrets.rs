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
