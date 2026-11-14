//! Secrets in what goes out: finding them and putting `[REDACTED]` in their place before text
//! reaches a model, and refusing or warning when a commit or a push would add them.
//!
//! The checks read what git publishes, every file of every commit, rather than what the model
//! reads, so that no setting, such as `exclude`, can hide a secret from them. They read it as git
//! streams it (`publication`), a line at a time and a long line in windows (`lines`), so that no
//! diff or history is held whole. A file a diff shows only as binary is read whole (`contents`).

mod checks;
pub mod contents;
pub mod finding;
pub mod fingerprint;
mod lines;
mod publication;
pub mod redact;

use crate::config::OnSecrets;
use crate::git::{CommitId, CommitMade, Repo, Scope};
use crate::ui;
use anyhow::{Context, Result, bail};
pub use checks::Checks;
use checks::Earlier;
use contents::MOST_BINARY_BYTES;
pub use finding::Scanned;
use finding::{Change, Finding, Place, Unchecked};
use publication::Found;
use std::collections::HashSet;

/// How to make a check pass when its findings aren't secrets.
const ADVICE: &str = "If they aren't real secrets, add --on-secrets warn, or choose Warn under \
                      Secrets in `git agent config`.";

/// What a flow says when a check finds secrets.
pub struct Wording {
    /// What the flow didn't do because of them.
    pub refused: &'static str,
    /// How to take them out.
    pub fix: &'static str,
    /// What the flow goes on to do with them, under warn.
    pub warned: &'static str,
}

const PUSH: Wording = Wording {
    refused: "commits the push would send add what look like secrets",
    fix: "Remove them from those commits, for example with `git rebase -i`.",
    warned: "pushing commits that add what look like secrets",
};

/// What a check decided about the secrets in what's published.
#[derive(Debug, PartialEq)]
enum Verdict {
    Clean,
    /// Go on, after this warning.
    Warn(String),
    /// Stop, with this error.
    Refuse(String),
}

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

/// Checks every file of what a commit of `scope` will hold. `reported` holds the findings the
/// flow lists itself, which a warning leaves out. Returns what it read, for the checks at the
/// commit.
pub fn check_changes(
    repo: &Repo,
    scope: Scope,
    on_secrets: OnSecrets,
    reported: &[Finding],
    wording: &Wording,
) -> Result<Scanned> {
    let scanned = found_in_changes(repo, scope)?;
    settle(unchecked_verdict(&scanned.unchecked))?;
    settle(verdict(&scanned.findings, on_secrets, reported, wording))?;
    Ok(scanned)
}

/// A commit about to be made: the changes of `scope`, its message, and the arguments it's made
/// with.
pub struct NextCommit<'a> {
    pub scope: Scope,
    pub message: &'a str,
    pub arguments: &'a str,
}

/// What a commit made: the commits new since it began on HEAD and the branches, which hold the
/// commit git made and any its hooks made, wherever they left them; and the commit HEAD was at,
/// `None` on a branch without commits.
pub struct CommitsMade<'a> {
    pub on: Option<&'a str>,
    pub commits: &'a [CommitMade],
}

impl CommitsMade<'_> {
    /// The commits made on the commit HEAD was at: the commit git made, and any a hook made there
    /// too.
    pub fn made_on_parent(&self) -> impl Iterator<Item = &CommitMade> {
        self.commits.iter().filter(|commit| commit.made_on(self.on))
    }
}

/// What the checks say when they couldn't read what a commit made.
pub const UNCHECKED_COMMIT: &str = "git-agent couldn't check the commit git made";

/// Checks what the `next` commit will hold just before it's made: what changed or was staged since
/// the check before the model, which read `before`, and the text the review settled on. Returns
/// what it read, for the check of the commit once it's made.
pub fn check_before_commit(
    repo: &Repo,
    next: NextCommit,
    before: &Scanned,
    checks: &Checks,
    wording: &Wording,
) -> Result<Scanned> {
    let changes = found_in_changes(repo, next.scope)?;
    let message = redact::text(next.message, &Place::Message(None)).findings;
    let arguments = redact::text(next.arguments, &Place::Arguments).findings;
    let read = Scanned {
        findings: changes
            .findings
            .into_iter()
            .chain(message)
            .chain(arguments)
            .collect(),
        unchecked: changes.unchecked,
    };
    decide_at_commit(&read, before, checks.on_secrets(), wording)?;
    Ok(read)
}

/// Checks the commits `made` holds, since hooks can change what's committed after the check before
/// it. It compares them with what that check read, `checked`, as in each commit made on
/// `made.on`. Fails under reject on a secret they add, and on a read that fails, as
/// [`UNCHECKED_COMMIT`] says; otherwise the run's `checks` keep what it read.
pub fn check_made_commit(
    repo: &Repo,
    made: CommitsMade,
    checked: &Scanned,
    checks: &mut Checks,
    wording: &Wording,
) -> Result<()> {
    if made.commits.is_empty() {
        return Ok(());
    }
    let commits: Vec<CommitId> = made
        .commits
        .iter()
        .map(|commit| commit.id.clone())
        .collect();
    let read = repo
        .read_commits(&commits, publication::read_log)
        .and_then(|found| with_binary_files(repo, found))
        .context(UNCHECKED_COMMIT)?;
    let made_on_parent: Vec<CommitId> = made
        .made_on_parent()
        .map(|commit| commit.id.clone())
        .collect();
    let bound = checked.in_commits(&made_on_parent);
    decide_at_commit(&read, &bound, checks.on_secrets(), wording)?;
    checks.record(read);
    Ok(())
}

/// Lists the binary files too large to check, and warns about or refuses the secrets, that
/// `read` holds and `before`, what the earlier check read, doesn't.
fn decide_at_commit(
    read: &Scanned,
    before: &Scanned,
    on_secrets: OnSecrets,
    wording: &Wording,
) -> Result<()> {
    settle(unchecked_verdict(&new_unchecked(
        &read.unchecked,
        &before.unchecked,
    )))?;
    let fresh = new_findings(&read.findings, &before.findings);
    settle(verdict(&fresh, on_secrets, &[], wording))
}

/// Checks the messages and every file of the commits `range` (`git log` arguments) publishes, as
/// the run's `checks` say, and has them keep what it read. `reported` holds the findings the flow
/// lists itself, which a warning leaves out.
pub fn check_commits(
    repo: &Repo,
    range: &[&str],
    reported: &[Finding],
    checks: &mut Checks,
    wording: &Wording,
) -> Result<()> {
    let scanned = found_in_commits(repo, range)?;
    settle(unchecked_verdict(&scanned.unchecked))?;
    settle(verdict(
        &scanned.findings,
        checks.on_secrets(),
        reported,
        wording,
    ))?;
    checks.record(scanned);
    Ok(())
}

/// Warns, whatever the setting, about the secrets the commits `range` publishes add, apart
/// from those in `reported`, which the flow lists itself.
pub fn warn_about_commits(
    repo: &Repo,
    range: &[&str],
    reported: &[Finding],
    warned: &str,
) -> Result<()> {
    let scanned = found_in_commits(repo, range)?;
    settle(unchecked_verdict(&scanned.unchecked))?;
    settle(warning(&scanned.findings, reported, warned).map_or(Verdict::Clean, Verdict::Warn))
}

/// Checks the commits a push of `commit` would send to each place it goes to: those that none of
/// the commits `published` for that place holds, the branches it has. One verdict covers all of
/// them. Its warning leaves out what the run's `checks` of commits found in the same commits, and
/// lists every other secret they add. Without any place, it checks as for one that has nothing
/// yet, so that no commit goes unchecked.
pub fn check_outgoing(
    repo: &Repo,
    commit: &str,
    published: &[Vec<String>],
    checks: &Checks,
) -> Result<()> {
    let nowhere = [Vec::new()];
    let places = if published.is_empty() {
        &nowhere[..]
    } else {
        published
    };
    let scanned = places
        .iter()
        .map(|tips| {
            let found = repo.outgoing_commits(commit, tips, publication::read_log)?;
            with_binary_files(repo, found)
        })
        .collect::<Result<Vec<Scanned>>>()?;
    let scanned = each_once(scanned);
    settle(unchecked_verdict(&new_unchecked(
        &scanned.unchecked,
        checks.unchecked(),
    )))?;
    settle(verdict(
        &scanned.findings,
        checks.on_secrets(),
        checks.findings(),
        &PUSH,
    ))
}

/// The secrets in every file of what a commit of `scope` will hold.
fn found_in_changes(repo: &Repo, scope: Scope) -> Result<Scanned> {
    let found = repo.changes_to_publish(scope, publication::read_diff)?;
    with_binary_files(repo, found)
}

/// The secrets in the messages and every file of the commits `range` publishes.
fn found_in_commits(repo: &Repo, range: &[&str]) -> Result<Scanned> {
    let found = repo.published_commits(range, publication::read_log)?;
    with_binary_files(repo, found)
}

/// What `scans` found together, each finding and each file not checked once, in the order found.
fn each_once(scans: Vec<Scanned>) -> Scanned {
    let (mut findings, mut files) = (HashSet::new(), HashSet::new());
    scans
        .into_iter()
        .fold(Scanned::default(), |mut all, scanned| {
            let new = scanned.findings.into_iter();
            all.findings
                .extend(new.filter(|finding| findings.insert(finding.clone())));
            let new = scanned.unchecked.into_iter();
            all.unchecked
                .extend(new.filter(|file| files.insert(file.clone())));
            all
        })
}

/// What a read of a publication `found`: the secrets in its lines, then those in the whole
/// content of its binary files.
fn with_binary_files(repo: &Repo, found: Found) -> Result<Scanned> {
    let whole = contents::scan(repo, &found.binary, MOST_BINARY_BYTES)?;
    let findings = found.findings.into_iter().chain(whole.findings);
    Ok(Scanned {
        findings: findings.collect(),
        unchecked: whole.unchecked,
    })
}

/// The warning that lists the binary files too large to check, if there are any. They don't
/// stop anything, whatever the setting.
fn unchecked_verdict(unchecked: &[Unchecked]) -> Verdict {
    const SHOWN: usize = 5;
    const MIB: f64 = 1024.0 * 1024.0;
    if unchecked.is_empty() {
        return Verdict::Clean;
    }
    let mut lines: Vec<String> = unchecked
        .iter()
        .take(SHOWN)
        .map(|file| format!("   {}  ({:.1} MiB)", file.place, file.size as f64 / MIB))
        .collect();
    if unchecked.len() > SHOWN {
        lines.push(format!("   … and {} more", unchecked.len() - SHOWN));
    }
    Verdict::Warn(format!(
        "git-agent didn't check these binary files for secrets, since each is over {} MiB:\n{}",
        MOST_BINARY_BYTES / (1024 * 1024),
        lines.join("\n")
    ))
}

/// What to do about `findings`: a refusal lists every secret they add, and a warning those of
/// them `reported` doesn't hold.
fn verdict(
    findings: &[Finding],
    on_secrets: OnSecrets,
    reported: &[Finding],
    wording: &Wording,
) -> Verdict {
    match on_secrets {
        OnSecrets::Reject => refusal(findings, wording).map_or(Verdict::Clean, Verdict::Refuse),
        OnSecrets::Warn => {
            warning(findings, reported, wording.warned).map_or(Verdict::Clean, Verdict::Warn)
        }
    }
}

/// The error that lists the secrets `findings` add, if they add any.
fn refusal(findings: &[Finding], wording: &Wording) -> Option<String> {
    let added = added(findings);
    (!added.is_empty()).then(|| {
        format!(
            "{}:\n{}\n{} {ADVICE}",
            wording.refused,
            list_findings(&added),
            wording.fix
        )
    })
}

/// The warning that lists the secrets `findings` add and `reported` doesn't hold, if there are
/// any.
fn warning(findings: &[Finding], reported: &[Finding], warned: &str) -> Option<String> {
    let earlier = Earlier::findings(reported);
    let new: Vec<&Finding> = added(findings)
        .into_iter()
        .filter(|finding| !earlier.holds(finding))
        .collect();
    (!new.is_empty()).then(|| format!("{warned}:\n{}", list_findings(&new)))
}

/// Those of `findings` that `before` doesn't hold, so that a check at a commit lists only what's
/// new since the check before it.
fn new_findings(findings: &[Finding], before: &[Finding]) -> Vec<Finding> {
    let earlier = Earlier::findings(before);
    findings
        .iter()
        .filter(|finding| !earlier.holds(finding))
        .cloned()
        .collect()
}

/// Those of `unchecked` that `before` doesn't hold: the same content in a place that holds it.
fn new_unchecked(unchecked: &[Unchecked], before: &[Unchecked]) -> Vec<Unchecked> {
    let earlier = Earlier::files(before);
    unchecked
        .iter()
        .filter(|file| !earlier.holds(file))
        .cloned()
        .collect()
}

/// Prints a warning, or fails with a refusal.
fn settle(verdict: Verdict) -> Result<()> {
    match verdict {
        Verdict::Clean => Ok(()),
        Verdict::Warn(text) => {
            ui::warn(text);
            Ok(())
        }
        Verdict::Refuse(text) => bail!(text),
    }
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
                None => finding.place.to_string(),
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

/// What the tests of the secret checks share.
#[cfg(test)]
pub mod test_support {
    use super::finding::Change;
    use super::{Checks, PUSH, check_outgoing};
    use crate::git::Repo;
    use crate::ui;

    /// What a push of HEAD to a place that has nothing yet warns about, in a run whose `checks`
    /// came before.
    pub fn push_warnings(repo: &Repo, checks: &Checks) -> Vec<String> {
        let head = repo.head_commit().unwrap().unwrap();
        let said = ui::test_support::listen();
        check_outgoing(repo, &head, &[], checks).unwrap();
        said.warnings()
    }

    /// The warning of a push that lists `shown`, one finding per line.
    pub fn push_listing(shown: &[String]) -> String {
        format!("{}:\n{}", PUSH.warned, shown.join("\n"))
    }

    /// An AWS access key's shape, built at run time so that the source holds no key.
    pub fn aws_key() -> String {
        format!("AKIA{}", "Q".repeat(16))
    }

    /// Another AWS access key's shape than [`aws_key`], built at run time.
    pub fn other_aws_key() -> String {
        format!("AKIA{}", "R".repeat(16))
    }

    /// A random-looking string of `len` letters and digits, so no real-looking secret is
    /// written out in the tests.
    pub fn noise(len: usize) -> String {
        "q7Rk2LmZ9vXa4TbN8cWp3HdY6sJf5GeU"
            .chars()
            .cycle()
            .take(len)
            .collect()
    }

    /// A GitHub token's shape, built at run time.
    pub fn github_token() -> String {
        format!("ghp_{}", noise(36))
    }

    /// A private key's BEGIN line, of `kind` such as `OPENSSH`, built at run time.
    pub fn key_begin(kind: &str) -> String {
        format!("-----BEGIN {kind} {}-----", "PRIVATE KEY")
    }

    /// A private key's END line, of `kind`.
    pub fn key_end(kind: &str) -> String {
        format!("-----END {kind} {}-----", "PRIVATE KEY")
    }

    /// A line of a private key's data, 63 characters of `tag` over and over, so that keys made
    /// with different tags differ.
    pub fn key_data(tag: &str) -> String {
        tag.repeat(63 / tag.len())
    }

    /// A diff of a private key's file, by the way it changes the key: its name, the diff, and
    /// the findings a scan of it gives, as line numbers and changes. A key's data is reported
    /// once for each change it makes in its block, and the new key's data, wherever git puts the
    /// old key's lines, as added. Data the result keeps without a BEGIN line isn't a key there,
    /// and gives no finding, though the model still doesn't see it: the model sees `redacted`
    /// lines in its place, one for each change a block makes.
    pub struct KeyEdit {
        pub name: &'static str,
        pub diff: String,
        pub found: &'static [(usize, Change)],
        pub redacted: usize,
    }

    /// A way of changing a key, as [`Shape::build`] makes a [`KeyEdit`] of it: its name, how
    /// many parents its hunk has, more than one for a merge's combined diff, how many
    /// [`REDACTED`](super::redact::REDACTED) lines the model sees, and its lines, in which
    /// `{BEGIN}`, `{END}`, `{RSA BEGIN}` and `{RSA END}` stand for a key's marker lines, and a
    /// tag in braces, such as `{Ab1}`, for a line of data.
    struct Shape {
        name: &'static str,
        parents: usize,
        redacted: usize,
        lines: &'static [&'static str],
        found: &'static [(usize, Change)],
    }

    const REMOVED: Change = Change::Removed;
    const ADDED: Change = Change::Added;
    const KEPT: Change = Change::Unchanged;
    const MISSING: &str = "\\ No newline at end of file";

    const SHAPES: [Shape; 19] = [
        Shape {
            name: "replaced in place",
            parents: 1,
            redacted: 2,
            lines: &[" {BEGIN}", "-{Ab1}", "-{Ab2}", "+{Cd1}", "+{Cd2}", " {END}"],
            found: &[(2, REMOVED), (2, ADDED)],
        },
        Shape {
            name: "the old file without a final newline",
            parents: 1,
            redacted: 2,
            lines: &[
                " {BEGIN}", "-{Ab1}", "-{Ab2}", "-{END}", MISSING, "+{Cd1}", "+{Cd2}", "+{END}",
            ],
            found: &[(2, REMOVED), (2, ADDED)],
        },
        Shape {
            name: "the new file without a final newline",
            parents: 1,
            redacted: 2,
            lines: &[
                " {BEGIN}", "-{Ab1}", "-{END}", "+{Cd1}", "+{Cd2}", "+{END}", MISSING,
            ],
            found: &[(2, REMOVED), (2, ADDED)],
        },
        Shape {
            name: "a placeholder filled in",
            parents: 1,
            redacted: 1,
            lines: &[
                " {BEGIN}",
                "-<your private key here>",
                "+{Cd1}",
                "+{Cd2}",
                " {END}",
            ],
            found: &[(2, ADDED)],
        },
        Shape {
            name: "a removed comment before the new data",
            parents: 1,
            redacted: 2,
            lines: &[" {BEGIN}", " {Ab1}", "-# rotate me", "+{Cd1}", " {END}"],
            found: &[(2, KEPT), (3, ADDED)],
        },
        Shape {
            name: "lines that end in CRLF",
            parents: 1,
            redacted: 2,
            lines: &[
                " {BEGIN}\r",
                "-{Ab1}\r",
                "-{Ab2}\r",
                "+{Cd1}\r",
                "+{Cd2}\r",
                " {END}\r",
            ],
            found: &[(2, REMOVED), (2, ADDED)],
        },
        Shape {
            name: "an encrypted key, its headers kept",
            parents: 1,
            redacted: 2,
            lines: &[
                " {RSA BEGIN}",
                " Proc-Type: 4,ENCRYPTED",
                " DEK-Info: AES-128-CBC,0123456789ABCDEF",
                " ",
                "-{Ab1}",
                "+{Cd1}",
                " {RSA END}",
            ],
            found: &[(5, REMOVED), (5, ADDED)],
        },
        Shape {
            name: "runs of changes between kept lines",
            parents: 1,
            redacted: 3,
            lines: &[
                " {BEGIN}", "-{Ab1}", "+{Cd1}", " {Ef1}", "-{Ab2}", "+{Cd2}", " {Ef2}", "-{Ab3}",
                "+{Cd3}", " {END}",
            ],
            found: &[(2, REMOVED), (2, ADDED), (3, KEPT)],
        },
        Shape {
            name: "two keys in one hunk",
            parents: 1,
            redacted: 4,
            lines: &[
                " {BEGIN}", "-{Ab1}", "+{Cd1}", " {END}", " {BEGIN}", "-{Ab2}", "+{Cd2}", " {END}",
            ],
            found: &[(2, REMOVED), (2, ADDED), (5, REMOVED), (5, ADDED)],
        },
        Shape {
            name: "a line of the data removed",
            parents: 1,
            redacted: 2,
            lines: &[" {BEGIN}", "-{Ab1}", " {Ab2}", " {END}"],
            found: &[(2, REMOVED), (2, KEPT)],
        },
        Shape {
            name: "the BEGIN line removed and the data kept",
            parents: 1,
            redacted: 1,
            lines: &["-{BEGIN}", " {Ab1}", " {Ab2}", " {END}"],
            found: &[],
        },
        Shape {
            name: "a merge that resolves two keys with a third",
            parents: 2,
            redacted: 3,
            lines: &["  {BEGIN}", "- {Ab1}", " -{Ab2}", "++{Cd1}", "  {END}"],
            found: &[(2, REMOVED), (2, REMOVED), (2, ADDED)],
        },
        Shape {
            name: "a merge that keeps one side's key",
            parents: 2,
            redacted: 2,
            lines: &["  {BEGIN}", "- {Ab1}", "+ {Cd1}", "  {END}"],
            found: &[(2, REMOVED), (2, KEPT)],
        },
        Shape {
            name: "an octopus merge that resolves three keys with a fourth",
            parents: 3,
            redacted: 4,
            lines: &[
                "   {BEGIN}",
                "-  {Ab1}",
                " - {Ab2}",
                "  -{Ab3}",
                "+++{Cd1}",
                "   {END}",
            ],
            found: &[(2, REMOVED), (2, REMOVED), (2, REMOVED), (2, ADDED)],
        },
        Shape {
            name: "an octopus merge that keeps a third parent's key and adds to it",
            parents: 3,
            redacted: 3,
            lines: &["   {BEGIN}", "-- {Ab1}", "++ {Ef1}", "+++{Cd1}", "   {END}"],
            found: &[(2, REMOVED), (2, KEPT), (3, ADDED)],
        },
        Shape {
            name: "the old BEGIN line and data replaced by data alone",
            parents: 1,
            redacted: 2,
            lines: &["-{BEGIN}", "-{Ab1}", "+{Cd1}", " {END}"],
            found: &[(2, REMOVED)],
        },
        Shape {
            name: "a BEGIN line added over kept data",
            parents: 1,
            redacted: 1,
            lines: &["+{BEGIN}", " {Ab1}", " {END}"],
            found: &[(2, KEPT)],
        },
        Shape {
            name: "a kept line that ends with a BEGIN line while the old block is open",
            parents: 1,
            redacted: 1,
            lines: &["-{BEGIN}", " {NOTE} {BEGIN}", "+{Cd1}", " {END}"],
            found: &[(2, ADDED)],
        },
        Shape {
            name: "a key replaced after a kept line that ends with a BEGIN line",
            parents: 1,
            redacted: 2,
            lines: &[" {NOTE} {BEGIN}", "-{Ab1}", "+{Cd1}", " {END}"],
            found: &[(2, REMOVED), (2, ADDED)],
        },
    ];

    /// The tags of the lines of key data the shapes use.
    pub const KEY_TAGS: [&str; 8] = ["Ab1", "Ab2", "Ab3", "Cd1", "Cd2", "Cd3", "Ef1", "Ef2"];

    /// Text that isn't a key's data, which `{NOTE}` stands for in a shape's lines.
    pub const NOTE: &str = "the next key follows";

    /// Every way of changing a key that the tests take it through.
    pub fn key_edits() -> Vec<KeyEdit> {
        SHAPES.iter().map(Shape::build).collect()
    }

    impl Shape {
        /// The shape as a diff of `k.pem`, whose hunk header counts the lines each side holds.
        fn build(&self) -> KeyEdit {
            let lines: Vec<String> = self.lines.iter().map(|line| filled(line)).collect();
            let header = match self.parents {
                1 => "diff --git a/k.pem b/k.pem",
                _ => "diff --cc k.pem",
            };
            let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
            KeyEdit {
                name: self.name,
                diff: format!(
                    "{header}\n--- a/k.pem\n+++ b/k.pem\n{}\n{body}",
                    self.hunk(&lines)
                ),
                found: self.found,
                redacted: self.redacted,
            }
        }

        /// The hunk header for `lines`: each parent's lines and the result's, counted by their
        /// markers, as git counts them.
        fn hunk(&self, lines: &[String]) -> String {
            let marked: Vec<&[u8]> = lines
                .iter()
                .filter(|line| !line.starts_with('\\'))
                .map(|line| &line.as_bytes()[..self.parents])
                .collect();
            let held_by = |parent: usize| {
                let held = marked
                    .iter()
                    .filter(|markers| parent_holds(markers, parent));
                held.count()
            };
            let signs = "@".repeat(self.parents + 1);
            let parents: String = (0..self.parents)
                .map(|parent| format!("-1,{} ", held_by(parent)))
                .collect();
            let result = marked
                .iter()
                .filter(|markers| result_holds(markers))
                .count();
            format!("{signs} {parents}+1,{result} {signs}")
        }
    }

    /// Whether the result holds a line with these markers: no parent removes it.
    fn result_holds(markers: &[u8]) -> bool {
        !markers.contains(&b'-')
    }

    /// Whether `parent` holds a line with these markers: it removes the line, or keeps it.
    fn parent_holds(markers: &[u8], parent: usize) -> bool {
        markers[parent] == b'-' || (markers[parent] == b' ' && result_holds(markers))
    }

    /// `template` with its markers and data filled in.
    fn filled(template: &str) -> String {
        let markers = [
            ("{BEGIN}", key_begin("OPENSSH")),
            ("{END}", key_end("OPENSSH")),
            ("{RSA BEGIN}", key_begin("RSA")),
            ("{RSA END}", key_end("RSA")),
            ("{NOTE}", NOTE.to_owned()),
        ];
        let data = KEY_TAGS.map(|tag| (format!("{{{tag}}}"), key_data(tag)));
        let line = markers
            .iter()
            .fold(template.to_owned(), |line, (name, text)| {
                line.replace(name, text)
            });
        let line = data
            .iter()
            .fold(line, |line, (name, text)| line.replace(name, text));
        assert!(!line.contains('{'), "a token left in {template:?}");
        line
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{
        aws_key, key_begin, key_data, key_end, other_aws_key, push_listing, push_warnings,
    };
    use super::*;
    use crate::config::DEFAULT_EXCLUDE;
    use crate::git::CommitId;
    use crate::git::test_support::{
        isolated_repo, move_pointer, run, short_head, tips_of, with_submodule,
    };
    use finding::FileRef;
    use finding::test_support::{in_commit, print_of};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    /// The commits no remote-tracking branch has: what a push of HEAD sent before it named its
    /// destination, still a range for the other checks.
    const OUTGOING: [&str; 3] = ["HEAD", "--not", "--remotes"];

    /// The check of a push of HEAD, with every remote's branches counted as published.
    fn check_push(repo: &Repo, on_secrets: OnSecrets) -> Result<()> {
        check_push_after(repo, &Checks::new(on_secrets))
    }

    /// The check of a push of HEAD, as [`check_push`], in a run whose `checks` came before.
    fn check_push_after(repo: &Repo, checks: &Checks) -> Result<()> {
        let head = repo.head_commit().unwrap().unwrap();
        let tips = run(
            repo.workdir(),
            &["for-each-ref", "--format=%(objectname)", "refs/remotes/"],
        );
        let published: Vec<String> = tips.lines().map(str::to_owned).collect();
        check_outgoing(repo, &head, &[published], checks)
    }

    const WORDING: Wording = Wording {
        refused: "stopped",
        fix: "Fix it.",
        warned: "going on",
    };

    /// The file at `path` among the changes.
    fn changed(path: &str) -> Place {
        Place::file(path)
    }

    /// The message of the commit abbreviated `hash`.
    fn message_of(hash: &str) -> Place {
        Place::Message(Some(CommitId::of(hash)))
    }

    fn finding(place: Place, line: Option<usize>, change: Change) -> Finding {
        Finding {
            print: print_of("a GitHub token", false),
            place,
            line,
            change,
            kind: "GitHub token".to_owned(),
        }
    }

    /// Writes `text` to `path` in `dir` and stages it.
    fn stage(dir: &Path, path: &str, text: &str) {
        let file = dir.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
        run(dir, &["add", "--", path]);
    }

    /// Commits `text` as `path`, and returns the commit's short hash.
    fn commit_at(dir: &Path, path: &str, text: &str, message: &str) -> String {
        stage(dir, path, text);
        run(dir, &["commit", "-q", "-m", message]);
        short_head(dir)
    }

    fn commit_file(dir: &Path, text: &str, message: &str) -> String {
        commit_at(dir, "deploy.env", text, message)
    }

    /// A repository without commits, without the user's global settings for hooks.
    fn unborn_repo() -> (TempDir, Repo) {
        let dir = TempDir::new().unwrap();
        run(dir.path(), &["init", "-q", "--initial-branch", "main"]);
        run(dir.path(), &["config", "user.name", "Test"]);
        run(dir.path(), &["config", "user.email", "test@example.com"]);
        run(dir.path(), &["config", "commit.gpgsign", "false"]);
        run(dir.path(), &["config", "core.hooksPath", "no-hooks"]);
        let repo = Repo::open(dir.path()).unwrap();
        (dir, repo)
    }

    /// The refusal of a check, or a panic if it passed.
    fn refusal_of<T: std::fmt::Debug>(checked: Result<T>) -> String {
        checked.unwrap_err().to_string()
    }

    /// The refusal that lists `shown`, one finding per line.
    fn refused(shown: &[&str]) -> String {
        format!("stopped:\n{}\nFix it. {ADVICE}", shown.join("\n"))
    }

    /// The check of the commits `range` under reject, with nothing the flow lists itself.
    fn reject_commits(repo: &Repo, range: &[&str]) -> Result<()> {
        let mut checks = Checks::new(OnSecrets::Reject);
        check_commits(repo, range, &[], &mut checks, &WORDING)
    }

    fn check_staged(repo: &Repo, scope: Scope) -> Result<Scanned> {
        check_changes(repo, scope, OnSecrets::Reject, &[], &WORDING)
    }

    /// What the scans of a push's places found together holds each finding once, in time that
    /// grows with how many there are: a second or so here in a debug build, where a search of all
    /// those kept for each took minutes.
    #[test]
    fn keeps_each_finding_of_many_once_quickly() {
        let many = |from: usize| -> Vec<Finding> {
            (from..from + 100_000)
                .map(|n| Finding {
                    print: fingerprint::Key::for_run().of(&n.to_le_bytes()),
                    ..finding(changed("one.min.js"), Some(1), Change::Added)
                })
                .collect()
        };
        let scans = [many(0), many(50_000)].map(|findings| Scanned {
            findings,
            unchecked: Vec::new(),
        });
        let started = std::time::Instant::now();
        let all = each_once(scans.into());
        assert_eq!(all.findings.len(), 150_000);
        assert!(started.elapsed().as_secs() < 30, "{:?}", started.elapsed());
    }

    /// One secret in 20,000 files, or on 20,000 lines of one, in an older commit and in a commit the
    /// run read: each is looked up in time that doesn't grow with how many share its print. Under a
    /// second here in a debug build, well inside the bound.
    #[test]
    fn looks_up_many_copies_of_one_secret_quickly() {
        let in_files = |commit: &str| -> Vec<Finding> {
            (1..=20_000)
                .map(|n| {
                    finding(
                        in_commit(commit, &format!("{n}.env")),
                        Some(1),
                        Change::Added,
                    )
                })
                .collect()
        };
        let on_lines = |commit: &str| -> Vec<Finding> {
            (1..=20_000)
                .map(|line| finding(in_commit(commit, "a.env"), Some(line), Change::Added))
                .collect()
        };
        let started = std::time::Instant::now();

        for copies in [in_files, on_lines] {
            let (older, read) = (copies("1a2b3c4"), copies("5d6e7f8"));
            let warned = warning(&older, &read, "going on").unwrap();
            assert_eq!(new_findings(&older, &read).len(), 20_000);
            assert!(warned.ends_with("… and 19995 more"), "{warned}");
            assert!(new_findings(&read, &read).is_empty());
        }
        assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
    }

    /// 20,000 files of one blob too large to check in an older commit, and as many in a commit the
    /// run read: each is looked up as quickly.
    #[test]
    fn looks_up_many_files_of_one_blob_quickly() {
        let files = |commit: &str| -> Vec<Unchecked> {
            let commit = CommitId::of(commit);
            (0..20_000)
                .map(|n| Unchecked {
                    place: FileRef::changed(format!("data/{n}.bin")).in_commit(&commit),
                    size: MOST_BINARY_BYTES + 1,
                    blob: "a".repeat(40),
                })
                .collect()
        };
        let (older, read) = (files("1a2b3c4"), files("5d6e7f8"));
        let started = std::time::Instant::now();

        assert_eq!(new_unchecked(&older, &read).len(), 20_000);
        assert!(new_unchecked(&read, &read).is_empty());
        assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
    }

    #[test]
    fn lists_each_finding_with_its_place_and_what_the_change_does_to_it() {
        let findings = [
            finding(changed("src/a.rs"), Some(3), Change::Added),
            finding(message_of("1a2b3c4"), None, Change::Unchanged),
            finding(changed("b.env"), Some(9), Change::Removed),
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
            .map(|line| finding(changed("a.rs"), Some(line), Change::Added))
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
    fn refuses_every_secret_a_change_adds_and_nothing_else() {
        let left_alone = [
            finding(changed("a.rs"), Some(1), Change::Unchanged),
            finding(changed("a.rs"), Some(2), Change::Removed),
        ];
        assert!(added(&left_alone).is_empty());
        let verdict_on = |findings: &[Finding], reported: &[Finding]| {
            verdict(findings, OnSecrets::Reject, reported, &WORDING)
        };
        assert_eq!(verdict_on(&left_alone, &[]), Verdict::Clean);
        assert_eq!(verdict_on(&[], &[]), Verdict::Clean);

        let findings = [
            finding(changed("a.rs"), Some(1), Change::Unchanged),
            finding(changed(".env"), Some(2), Change::Added),
        ];
        assert_eq!(added(&findings), [&findings[1]]);
        let refusal = Verdict::Refuse(refused(&["   .env:2  GitHub token"]));
        assert_eq!(verdict_on(&findings, &[]), refusal);
        // A refusal is the last thing a run says, so it lists what was shown before too.
        assert_eq!(verdict_on(&findings, &findings), refusal);
    }

    #[test]
    fn warns_about_the_secrets_a_change_adds_that_the_flow_does_not_list() {
        let findings = [
            finding(changed("a.rs"), Some(1), Change::Removed),
            finding(changed("Cargo.lock"), Some(4), Change::Added),
            finding(changed("a.rs"), Some(3), Change::Added),
        ];
        let verdict_on =
            |reported: &[Finding]| verdict(&findings, OnSecrets::Warn, reported, &WORDING);
        assert_eq!(
            verdict_on(&findings[2..]),
            Verdict::Warn("going on:\n   Cargo.lock:4  GitHub token".to_owned())
        );
        assert_eq!(
            verdict_on(&[]),
            Verdict::Warn(
                "going on:\n   Cargo.lock:4  GitHub token\n   a.rs:3  GitHub token".to_owned()
            )
        );
        assert_eq!(verdict_on(&findings), Verdict::Clean);
        assert_eq!(
            verdict(&findings[..1], OnSecrets::Warn, &[], &WORDING),
            Verdict::Clean
        );
    }

    /// A file named like a message or the arguments is a file: it never holds them, nor they it,
    /// in a warning or at a commit.
    #[test]
    fn a_file_named_message_or_arguments_is_only_a_file() {
        let at = |place: Place| finding(place, None, Change::Added);
        let pairs = [
            (changed("message"), message_of("1a2b3c4")),
            (in_commit("1a2b3c4", "message"), message_of("1a2b3c4")),
            (changed("message"), Place::Message(None)),
            (changed("arguments"), Place::Arguments),
            (in_commit("1a2b3c4", "arguments"), Place::Arguments),
        ];
        for (file, text) in pairs {
            for (shown, later) in [(at(file.clone()), at(text.clone())), (at(text), at(file))] {
                let before = std::slice::from_ref(&shown);
                let listed = format!("w:\n{}", list_findings(&[&later]));
                assert_eq!(
                    warning(std::slice::from_ref(&later), before, "w"),
                    Some(listed)
                );
                assert_eq!(new_findings(std::slice::from_ref(&later), before), [later]);
            }
        }
    }

    /// The warning of a pull request lists a key that a later commit replaced: the flow's own
    /// list, from the branch's overall diff, holds only the key that replaced it.
    #[test]
    fn lists_a_key_that_a_later_commit_of_a_pull_request_replaced() {
        let (dir, repo) = isolated_repo();
        let base = run(dir.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        let first = commit_file(dir.path(), &format!("aws {}\n", aws_key()), "add a key");
        commit_file(
            dir.path(),
            &format!("aws {}\n", other_aws_key()),
            "replace it",
        );
        let overall = run(dir.path(), &["diff", &base, "HEAD"]);
        let reported = redact::diff(&overall).findings;
        let range = format!("{base}..HEAD");

        let mut checks = Checks::new(OnSecrets::Warn);

        let said = ui::test_support::listen();
        check_commits(&repo, &[&range], &reported, &mut checks, &WORDING).unwrap();

        let shown = format!("going on:\n   commit {first} deploy.env:1  AWS access key");
        assert_eq!(said.warnings(), [shown]);
        drop(said);
        assert_eq!(push_warnings(&repo, &checks), Vec::<String>::new());
    }

    /// A check at a commit lists what the check before it didn't find: another secret, or the same
    /// one changed another way, in another place, or of another kind. The same secret on another
    /// line isn't listed again, and the message of the commit being made is the made commit's
    /// only once it's bound to it.
    #[test]
    fn lists_at_a_commit_only_what_the_check_before_it_did_not_find() {
        let before = Scanned {
            findings: vec![
                finding(changed("a.env"), Some(2), Change::Added),
                finding(Place::Message(None), None, Change::Added),
            ],
            unchecked: Vec::new(),
        };
        let other_secret = Finding {
            print: print_of("another token", false),
            ..finding(changed("a.env"), Some(2), Change::Added)
        };
        let found = [
            finding(changed("a.env"), Some(2), Change::Added),
            finding(changed("a.env"), Some(3), Change::Added),
            finding(in_commit("1a2b3c4", "a.env"), Some(9), Change::Added),
            finding(Place::Message(None), None, Change::Added),
            finding(message_of("1a2b3c4"), None, Change::Added),
            finding(changed("b.env"), Some(2), Change::Added),
            finding(changed("a.env"), Some(2), Change::Unchanged),
            Finding {
                kind: "AWS access key".to_owned(),
                ..finding(changed("a.env"), Some(2), Change::Added)
            },
            other_secret,
        ];

        let new = new_findings(&found, &before.findings);

        let kept = [&found[4], &found[5], &found[6], &found[7], &found[8]];
        assert_eq!(new, kept.map(Clone::clone));
        assert_eq!(new_findings(&found, &[]), found);
        let made = before.in_commits(&[CommitId::of("1a2b3c4")]);
        let in_the_commit = [found[2].clone(), found[4].clone(), found[0].clone()];
        assert_eq!(
            new_findings(&in_the_commit, &made.findings),
            [found[0].clone()]
        );
    }

    /// A binary file too large to check is listed at a commit unless the check before it listed
    /// the same content, in the same place or as the commit's file.
    #[test]
    fn lists_at_a_commit_only_the_binary_files_not_listed_before() {
        let file = |place: FileRef, blob: &str| Unchecked {
            place,
            size: MOST_BINARY_BYTES + 1,
            blob: blob.repeat(40),
        };
        let commit = CommitId::of("1a2b3c4");
        let before = [file(FileRef::changed("big.bin"), "a")];
        let found = [
            file(FileRef::changed("big.bin"), "a"),
            file(FileRef::changed("big.bin").in_commit(&commit), "a"),
            file(FileRef::changed("big.bin"), "b"),
            file(FileRef::changed("big.bin").in_commit(&commit), "b"),
            file(FileRef::changed("other.bin"), "a"),
        ];

        assert_eq!(new_unchecked(&found, &before), found[2..]);
        assert_eq!(new_unchecked(&found, &[]), found);
        let in_another = file(
            FileRef::changed("big.bin").in_commit(&CommitId::of("5d6e7f8")),
            "a",
        );
        assert_eq!(
            new_unchecked(std::slice::from_ref(&in_another), &found[1..2]),
            [in_another]
        );
    }

    /// What the commits made since `base` hold, read as the check of a commit made reads them.
    fn read_of_commits_made_since(repo: &Repo, base: &str) -> Scanned {
        let base = repo.resolve(base).unwrap().unwrap();
        let now = repo.head_and_branch_tips().unwrap();
        let made = repo.commits_new_since(&tips_of(&[&base]), &now).unwrap();
        let commits: Vec<CommitId> = made.iter().map(|commit| commit.id.clone()).collect();
        let found = repo.read_commits(&commits, publication::read_log).unwrap();
        with_binary_files(repo, found).unwrap()
    }

    /// What the check of the commits made on `base` up to HEAD refuses, after the check just
    /// before them read `before`; `None` when it lets them through.
    fn made_commit_refusal(repo: &Repo, base: &str, before: &Scanned) -> Option<String> {
        let now = repo.head_and_branch_tips().unwrap();
        let new = repo.commits_new_since(&tips_of(&[base]), &now).unwrap();
        let made = CommitsMade {
            on: Some(base),
            commits: &new,
        };
        let mut checks = Checks::new(OnSecrets::Reject);
        let checked = check_made_commit(repo, made, before, &mut checks, &WORDING);
        checked.err().map(|error| error.to_string())
    }

    /// A commit the check can't read fails the check under either setting, saying it couldn't
    /// check the commit; with no commits, there is nothing to read.
    #[test]
    fn says_it_couldnt_check_a_commit_it_couldnt_read() {
        let (_dir, repo) = isolated_repo();
        let missing = CommitMade {
            id: CommitId::of("1111111"),
            parents: Vec::new(),
        };
        for on_secrets in [OnSecrets::Reject, OnSecrets::Warn] {
            let mut checks = Checks::new(on_secrets);
            let unread = CommitsMade {
                on: None,
                commits: std::slice::from_ref(&missing),
            };
            let failed =
                check_made_commit(&repo, unread, &Scanned::default(), &mut checks, &WORDING);
            let failure = format!("{:#}", failed.unwrap_err());
            assert!(failure.starts_with(UNCHECKED_COMMIT), "{failure}");
            let nothing = CommitsMade {
                on: None,
                commits: &[],
            };
            assert!(
                check_made_commit(&repo, nothing, &Scanned::default(), &mut checks, &WORDING)
                    .is_ok()
            );
        }
    }

    /// The message of the commit made holds what the check just before it read in the message
    /// being made, and no other secret of that kind.
    #[test]
    fn lists_at_the_made_commit_a_second_secret_in_its_message() {
        let (dir, repo) = isolated_repo();
        let base = run(dir.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        let typed = format!("fix: a\n\naws {}", aws_key());
        let before = Scanned {
            findings: redact::text(&typed, &Place::Message(None)).findings,
            unchecked: Vec::new(),
        };
        commit_at(dir.path(), "a.txt", "a\n", &typed);
        assert_eq!(made_commit_refusal(&repo, &base, &before), None);

        run(dir.path(), &["reset", "-q", "--hard", &base]);
        let hash = commit_at(
            dir.path(),
            "a.txt",
            "a\n",
            &format!("{typed} aws {}", other_aws_key()),
        );

        let shown = format!("   commit {hash} message  AWS access key");
        assert_eq!(
            made_commit_refusal(&repo, &base, &before),
            Some(refused(&[&shown]))
        );
    }

    /// When a hook makes another commit after the one made, only the one made holds what the
    /// check before it read.
    #[test]
    fn binds_what_was_read_before_a_commit_only_to_the_commit_made() {
        let (dir, repo) = isolated_repo();
        let base = run(dir.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        let typed = format!("fix: a\n\naws {}", aws_key());
        let before = Scanned {
            findings: redact::text(&typed, &Place::Message(None)).findings,
            unchecked: Vec::new(),
        };
        commit_at(dir.path(), "a.txt", "a\n", &typed);
        let second = commit_at(dir.path(), "b.txt", "b\n", &typed);

        let shown = format!("   commit {second} message  AWS access key");
        assert_eq!(
            made_commit_refusal(&repo, &base, &before),
            Some(refused(&[&shown]))
        );
    }

    /// In a file, the commit made lists another secret of a kind, on a line already flagged and in
    /// a binary file, and not the same secret moved to another line.
    #[test]
    fn lists_at_the_made_commit_another_secret_of_a_kind_in_a_file() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let base = run(dir, &["rev-parse", "HEAD"]).trim().to_owned();
        stage(dir, "deploy.env", &format!("aws {}\n", aws_key()));
        stage(dir, "data.bin", &format!("\0aws {}\0", aws_key()));
        let before = found_in_changes(&repo, Scope::Staged).unwrap();
        stage(dir, "deploy.env", &format!("x\naws {}\n", aws_key()));
        stage(dir, "data.bin", &format!("\0aws {}\0", aws_key()));
        run(dir, &["commit", "-q", "-m", "add the settings"]);
        assert_eq!(made_commit_refusal(&repo, &base, &before), None);

        run(dir, &["reset", "-q", "--hard", &base]);
        let both = format!("aws {} aws {}", aws_key(), other_aws_key());
        stage(dir, "deploy.env", &format!("x\n{both}\n"));
        stage(dir, "data.bin", &format!("\0{both}\0"));
        let hash = commit_file(dir, &format!("x\n{both}\n"), "add the settings");

        let refusal = made_commit_refusal(&repo, &base, &before).unwrap();
        let in_data = format!("   commit {hash} data.bin  AWS access key");
        let in_deploy = format!("   commit {hash} deploy.env:2  AWS access key");
        assert_eq!(refusal, refused(&[&in_deploy, &in_data]));
    }

    /// The model doesn't read lockfiles, or what a user's own `exclude` names, but a commit
    /// holds them, so the check reads them.
    #[test]
    fn refuses_a_key_in_a_file_the_model_does_not_read() {
        let lockfiles: Vec<String> = DEFAULT_EXCLUDE.iter().map(|s| s.to_string()).collect();
        let own = vec!["vendor/**".to_owned()];
        for (exclude, path) in [
            (&lockfiles, "Cargo.lock"),
            (&lockfiles, "web/package-lock.json"),
            (&own, "vendor/keys.json"),
        ] {
            let (dir, repo) = isolated_repo();
            stage(dir.path(), path, &format!("aws {}\n", aws_key()));
            let (_, for_model) = repo.changes(exclude).unwrap().unwrap();
            assert!(!for_model.diff.contains(&aws_key()), "{}", for_model.diff);

            let refusal = refusal_of(check_staged(&repo, Scope::Staged));

            assert_eq!(refusal, refused(&[&format!("   {path}:1  AWS access key")]));
            let found = found_in_changes(&repo, Scope::Staged).unwrap().findings;
            let listed = format!("going on:\n   {path}:1  AWS access key");
            let warned = verdict(&found, OnSecrets::Warn, &[], &WORDING);
            assert_eq!(warned, Verdict::Warn(listed));
        }
    }

    /// `git commit --all` commits what is staged too, so the Tracked scope reads from HEAD; what
    /// HEAD holds already, and an untracked file, aren't added by the commit.
    #[test]
    fn reads_the_staged_and_unstaged_changes_a_commit_of_all_changes_holds() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        commit_file(
            dir,
            &format!("aws {}\n", aws_key()),
            "add the deploy settings",
        );
        stage(dir, "lib.rs", &format!("// aws {}\n", aws_key()));
        fs::write(dir.join("draft.env"), format!("aws {}\n", aws_key())).unwrap();
        let expected = refused(&["   lib.rs:1  AWS access key"]);
        assert_eq!(refusal_of(check_staged(&repo, Scope::Tracked)), expected);
        assert_eq!(refusal_of(check_staged(&repo, Scope::Staged)), expected);

        run(dir, &["reset", "-q"]);
        assert_eq!(refusal_of(check_staged(&repo, Scope::Tracked)), expected);
        assert!(check_staged(&repo, Scope::Staged).is_ok());
    }

    /// git-agent runs from wherever the user is, and `diff.relative` would hide the rest of the
    /// repository.
    #[test]
    fn reads_the_whole_repository_from_a_subfolder() {
        let (dir, _) = isolated_repo();
        let dir = dir.path();
        commit_at(dir, "notes/plan.txt", "plan\n", "add a plan");
        run(dir, &["config", "diff.relative", "true"]);
        let hash = commit_file(
            dir,
            &format!("aws {}\n", aws_key()),
            "add the deploy settings",
        );
        stage(dir, "lib.rs", &format!("// aws {}\n", aws_key()));
        let repo = Repo::open(dir.join("notes")).unwrap();
        let range = ["HEAD~1..HEAD"];

        let staged = refusal_of(check_staged(&repo, Scope::Staged));
        let pushed = refusal_of(reject_commits(&repo, &range));

        assert_eq!(staged, refused(&["   lib.rs:1  AWS access key"]));
        let shown = format!("   commit {hash} deploy.env:1  AWS access key");
        assert_eq!(pushed, refused(&[&shown]));
    }

    #[test]
    fn reads_the_changes_of_a_first_commit() {
        let (dir, repo) = unborn_repo();
        stage(dir.path(), "deploy.env", &format!("aws {}\n", aws_key()));
        let expected = refused(&["   deploy.env:1  AWS access key"]);
        assert_eq!(refusal_of(check_staged(&repo, Scope::Staged)), expected);
        assert_eq!(refusal_of(check_staged(&repo, Scope::Tracked)), expected);
    }

    /// Sets what a user can set to hide lines from a plain `git diff` or `git log --patch`,
    /// name files otherwise, number lines otherwise, or write the log in another encoding.
    fn hide_from_plain_reads(dir: &Path) {
        fs::write(dir.join(".gitattributes"), "*.env diff=blank\n").unwrap();
        for [name, value] in [
            ["diff.blank.textconv", "true"],
            ["diff.external", "true"],
            ["diff.noprefix", "true"],
            ["diff.suppressBlankEmpty", "true"],
            ["diff.context", "0"],
            ["color.ui", "always"],
            ["core.quotePath", "true"],
            ["core.bigFileThreshold", "1"],
            ["i18n.logOutputEncoding", "UTF-16"],
        ] {
            run(dir, &["config", name, value]);
        }
    }

    #[test]
    fn reads_the_changes_whatever_the_users_diff_settings() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        commit_at(dir, "notes/ø.env", "a\n\nb\n", "add notes");
        hide_from_plain_reads(dir);
        stage(dir, "notes/ø.env", &format!("a\n\nb\naws {}\n", aws_key()));

        let refusal = refusal_of(check_staged(&repo, Scope::Staged));

        assert_eq!(refusal, refused(&["   notes/ø.env:4  AWS access key"]));
    }

    #[test]
    fn reads_the_commits_whatever_the_users_diff_settings() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        commit_at(dir, "notes/ø.env", "a\n\nb\n", "add notes");
        hide_from_plain_reads(dir);
        let text = format!("a\n\nb\naws {}\n", aws_key());
        let hash = commit_at(dir, "notes/ø.env", &text, "more notes");
        let check = reject_commits(&repo, &["HEAD~1..HEAD"]);

        let refusal = refusal_of(check);

        let shown = format!("   commit {hash} notes/ø.env:4  AWS access key");
        assert_eq!(refusal, refused(&[&shown]));
    }

    /// `git replace` can make HEAD look as if it held a key the changes add.
    #[test]
    fn reads_the_changes_against_head_as_it_is() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let line = format!("aws {}\n", aws_key());
        let stand_in = commit_file(dir, &line, "a commit HEAD is replaced with");
        run(dir, &["reset", "-q", "--hard", "HEAD~1"]);
        run(dir, &["replace", "-f", "HEAD", &stand_in]);
        stage(dir, "deploy.env", &line);
        assert!(run(dir, &["diff", "--cached", "--stat"]).is_empty());

        let expected = refused(&["   deploy.env:1  AWS access key"]);
        assert_eq!(refusal_of(check_staged(&repo, Scope::Staged)), expected);
        assert_eq!(refusal_of(check_staged(&repo, Scope::Tracked)), expected);
    }

    /// A file renamed with a key it already had adds no key, even when the user turns off
    /// rename detection.
    #[test]
    fn lets_a_rename_through_that_only_moves_a_key() {
        let (dir, repo) = isolated_repo();
        commit_file(
            dir.path(),
            &format!("aws {}\n", aws_key()),
            "add the deploy settings",
        );
        run(dir.path(), &["config", "diff.renames", "false"]);
        run(dir.path(), &["mv", "deploy.env", "production.env"]);

        assert!(check_staged(&repo, Scope::Staged).is_ok());
    }

    #[test]
    fn refuses_a_push_that_adds_a_secret_when_set_to_reject() {
        let (dir, repo) = isolated_repo();
        let hash = commit_file(
            dir.path(),
            &format!("aws {}\n", aws_key()),
            "add the deploy settings",
        );

        let error = check_push(&repo, OnSecrets::Reject).unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "commits the push would send add what look like secrets:\n   commit {hash} \
                 deploy.env:1  AWS access key\nRemove them from those commits, for example with \
                 `git rebase -i`. {ADVICE}"
            )
        );
    }

    /// A private key that replaces another in place is refused, though git shows the old key's
    /// lines first.
    #[test]
    fn refuses_a_push_of_a_key_that_replaces_another() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        commit_file(dir, &key_file("Ab1", true), "add the deploy key");
        run(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let hash = commit_file(dir, &key_file("Cd2", true), "rotate the deploy key");

        let error = check_push(&repo, OnSecrets::Reject).unwrap_err();

        let shown = format!("commit {hash} deploy.env:2  private key");
        assert!(error.to_string().contains(&shown), "{error}");
    }

    /// A private key file's text, with `tag`'s data, ending with a newline or not.
    fn key_file(tag: &str, newline: bool) -> String {
        let (begin, end) = (key_begin("OPENSSH"), key_end("OPENSSH"));
        let data = key_data(tag);
        let ending = if newline { "\n" } else { "" };
        format!("{begin}\n{data}\n{data}\n{end}{ending}")
    }

    /// A new key replacing a key in place is refused before the commit, though the old file
    /// ends with a newline and the new one doesn't, which changes the END line too.
    #[test]
    fn refuses_to_commit_a_key_that_replaces_another_and_its_end_line() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        commit_at(dir, "k.pem", &key_file("Ab1", true), "add the key");
        stage(dir, "k.pem", &key_file("Cd1", false));

        let checked = check_changes(&repo, Scope::Staged, OnSecrets::Reject, &[], &WORDING);

        let Err(error) = checked else {
            panic!("the new key went through");
        };
        let error = error.to_string();
        assert!(error.contains("k.pem:2  private key"), "{error}");
    }

    /// A pull request's commits that replace a key are refused, wherever git puts the old key's
    /// lines.
    #[test]
    fn refuses_a_pull_request_of_a_key_that_replaces_another() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        commit_at(dir, "k.pem", &key_file("Ab1", false), "add the key");
        run(dir, &["checkout", "-q", "-b", "feature"]);
        let hash = commit_at(dir, "k.pem", &key_file("Cd1", true), "rotate");

        let check = reject_commits(&repo, &["main..HEAD"]);

        let error = check.unwrap_err().to_string();
        assert!(
            error.contains(&format!("commit {hash} k.pem:2  private key")),
            "{error}"
        );
    }

    /// A merge that resolves a conflict between two keys with a third is refused, though both
    /// sides are published.
    #[test]
    fn refuses_a_push_of_a_merge_that_resolves_two_keys_with_a_third() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        commit_at(dir, "k.pem", &key_file("Ab1", true), "add the key");
        run(dir, &["checkout", "-q", "-b", "side"]);
        commit_at(dir, "k.pem", &key_file("Cd1", true), "one key");
        run(dir, &["checkout", "-q", "main"]);
        commit_at(dir, "k.pem", &key_file("Ef1", true), "another key");
        run(dir, &["update-ref", "refs/remotes/origin/main", "main"]);
        run(dir, &["update-ref", "refs/remotes/origin/side", "side"]);
        let merged = std::process::Command::new("git")
            .current_dir(dir)
            .args(["merge", "-q", "--no-ff", "side"])
            .output()
            .unwrap();
        assert!(!merged.status.success(), "the keys conflict");
        let merge = commit_at(dir, "k.pem", &key_file("Gh1", true), "merge the side");

        let error = check_push(&repo, OnSecrets::Reject).unwrap_err();

        let shown = format!("commit {merge} k.pem:2  private key");
        assert!(error.to_string().contains(&shown), "{error}");
    }

    /// An octopus merge that resolves three keys with a fourth is refused, though its three
    /// parents are published.
    #[test]
    fn refuses_a_push_of_an_octopus_merge_that_resolves_three_keys_with_a_fourth() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        commit_at(dir, "k.pem", &key_file("Ab1", true), "add the key");
        let parents: Vec<String> = ["Cd1", "Ef1", "Gh1"]
            .iter()
            .map(|tag| {
                run(dir, &["checkout", "-q", "-B", tag, "main"]);
                commit_at(dir, "k.pem", &key_file(tag, true), "a key");
                let tip = run(dir, &["rev-parse", "HEAD"]).trim().to_owned();
                run(
                    dir,
                    &["update-ref", &format!("refs/remotes/origin/{tag}"), &tip],
                );
                tip
            })
            .collect();
        run(dir, &["checkout", "-q", "main"]);
        stage(dir, "k.pem", &key_file("Ij1", true));
        let tree = run(dir, &["write-tree"]).trim().to_owned();
        let mut args = vec!["commit-tree", tree.as_str(), "-m", "merge the keys"];
        parents
            .iter()
            .for_each(|parent| args.extend(["-p", parent.as_str()]));
        let merge = run(dir, &args).trim().to_owned();
        run(dir, &["reset", "-q", "--hard", &merge]);

        let error = check_push(&repo, OnSecrets::Reject).unwrap_err();

        let shown = format!("commit {} k.pem:2  private key", &merge[..7]);
        assert!(error.to_string().contains(&shown), "{error}");
    }

    /// A push said to go nowhere still has every commit checked, as one to a place that has
    /// nothing yet.
    #[test]
    fn checks_every_commit_of_a_push_without_a_place_to_go() {
        let (dir, repo) = isolated_repo();
        let hash = commit_file(
            dir.path(),
            &format!("aws {}\n", aws_key()),
            "add the deploy settings",
        );
        let head = repo.head_commit().unwrap().unwrap();

        let checks = Checks::new(OnSecrets::Reject);
        let error = check_outgoing(&repo, &head, &[], &checks).unwrap_err();

        let shown = format!("commit {hash} deploy.env:1  AWS access key");
        assert!(error.to_string().contains(&shown), "{error}");
    }

    /// A commit message can hold git's field separator; what follows it is still scanned as part
    /// of the message.
    #[test]
    fn refuses_a_key_in_a_message_after_a_separator_byte() {
        let (dir, repo) = isolated_repo();
        let message = format!("tidy up\n\n\u{1f}aws {}", aws_key());
        let hash = commit_file(dir.path(), "plain\n", &message);

        let error = check_push(&repo, OnSecrets::Reject).unwrap_err();

        let shown = format!("commit {hash} message  AWS access key");
        assert!(error.to_string().contains(&shown), "{error}");
    }

    /// A commit that changes only files the model doesn't read is still published, message and
    /// all.
    #[test]
    fn refuses_a_key_in_a_commit_of_lockfiles_only() {
        let (dir, repo) = isolated_repo();
        let in_message = commit_at(
            dir.path(),
            "Cargo.lock",
            "v1\n",
            &format!("aws {}", aws_key()),
        );
        let in_file = commit_at(
            dir.path(),
            "Cargo.lock",
            &format!("aws {}\n", aws_key()),
            "bump",
        );
        let refusal = refusal_of(reject_commits(&repo, &OUTGOING));
        assert_eq!(
            refusal,
            refused(&[
                &format!("   commit {in_file} Cargo.lock:1  AWS access key"),
                &format!("   commit {in_message} message  AWS access key"),
            ])
        );
    }

    #[test]
    fn refuses_a_key_a_merge_adds_itself() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        run(dir, &["checkout", "-q", "-b", "side"]);
        commit_at(dir, "side.txt", "side\n", "work on the side");
        run(dir, &["checkout", "-q", "main"]);
        commit_at(dir, "main.txt", "main\n", "work on main");
        run(dir, &["merge", "-q", "--no-ff", "--no-commit", "side"]);
        let merge = commit_file(dir, &format!("aws {}\n", aws_key()), "merge the side");

        let error = check_push(&repo, OnSecrets::Reject).unwrap_err();

        let shown = format!("commit {merge} deploy.env:1  AWS access key");
        assert!(error.to_string().contains(&shown), "{error}");
    }

    /// `log.showRoot=false` hides a first commit's diff, and `git replace` can show a commit
    /// other than the one a push sends.
    #[test]
    fn reads_the_commits_a_push_sends_whatever_the_users_settings() {
        let (dir, repo) = unborn_repo();
        let dir = dir.path();
        let first = commit_file(
            dir,
            &format!("aws {}\n", aws_key()),
            "add the deploy settings",
        );
        run(dir, &["config", "log.showRoot", "false"]);
        let shown = format!("commit {first} deploy.env:1  AWS access key");
        let error = check_push(&repo, OnSecrets::Reject).unwrap_err();
        assert!(error.to_string().contains(&shown), "{error}");

        let second = commit_file(dir, &format!("aws {}\nmore\n", aws_key()), "more");
        let empty = repo.empty_tree().unwrap();
        let stand_in = run(dir, &["commit-tree", "-m", "plain", &empty]);
        run(dir, &["replace", "-f", &first, stand_in.trim()]);
        let replaced = run(dir, &["log", "-p", "--root", "--format=%h %s"]);
        assert!(replaced.contains(&format!("{second} more\n")), "{replaced}");
        assert!(replaced.contains("+aws"), "{replaced}");
        assert!(!replaced.contains("add the deploy settings"), "{replaced}");
        let error = check_push(&repo, OnSecrets::Reject).unwrap_err();
        assert!(error.to_string().contains(&shown), "{error}");
        assert!(!error.to_string().contains(&second), "{error}");
    }

    #[test]
    fn checks_only_the_commits_of_the_range() {
        let (dir, repo) = isolated_repo();
        commit_file(
            dir.path(),
            &format!("aws {}\n", aws_key()),
            "add the deploy settings",
        );
        run(dir.path(), &["checkout", "-q", "-b", "feature"]);
        commit_file(dir.path(), "aws none\n", "take the key out");
        let check = |range: &str| reject_commits(&repo, &[range]);

        assert!(check("main..HEAD").is_ok());
        assert!(check("HEAD").is_err());
    }

    #[test]
    fn only_warns_about_commits_whatever_the_setting() {
        let (dir, repo) = isolated_repo();
        let hash = commit_file(
            dir.path(),
            &format!("aws {}\n", aws_key()),
            "add the settings",
        );
        let shown = format!("going on:\n   commit {hash} deploy.env:1  AWS access key");

        let found = found_in_commits(&repo, &["HEAD"]).unwrap().findings;

        assert_eq!(warning(&found, &[], "going on"), Some(shown));
        let reported = [Finding {
            place: changed("deploy.env"),
            ..found[0].clone()
        }];
        assert_eq!(warning(&found, &reported, "going on"), None);
        assert!(warn_about_commits(&repo, &["HEAD"], &[], "going on").is_ok());
    }

    #[test]
    fn lets_the_push_through_when_set_to_warn() {
        let (dir, repo) = isolated_repo();
        let hash = commit_file(
            dir.path(),
            &format!("aws {}\n", aws_key()),
            "add the settings",
        );
        let shown = format!(
            "pushing commits that add what look like secrets:\n   commit {hash} deploy.env:1  \
             AWS access key"
        );

        let found = found_in_commits(&repo, &OUTGOING).unwrap().findings;

        assert_eq!(
            verdict(&found, OnSecrets::Warn, &[], &PUSH),
            Verdict::Warn(shown)
        );
        assert!(check_push(&repo, OnSecrets::Warn).is_ok());
    }

    /// Under warn, the push leaves out what the run's checks found in the commits they read, and
    /// lists what an older commit adds, of the same kind in the same file too. Under reject it
    /// refuses on all of them.
    #[test]
    fn leaves_out_at_the_push_what_the_runs_checks_of_commits_found() {
        let (dir, repo) = isolated_repo();
        let older = commit_file(dir.path(), &format!("aws {}\n", aws_key()), "add a key");
        let both = format!("aws {}\naws {}\n", aws_key(), other_aws_key());
        let newer = commit_file(dir.path(), &both, "add another key");
        let mut checks = Checks::new(OnSecrets::Warn);
        checks.record(read_of_commits_made_since(&repo, &older));
        let in_older = format!("   commit {older} deploy.env:1  AWS access key");
        let in_newer = format!("   commit {newer} deploy.env:2  AWS access key");

        assert_eq!(
            push_warnings(&repo, &checks),
            [push_listing(std::slice::from_ref(&in_older))]
        );
        let none = Checks::new(OnSecrets::Warn);
        let both_listed = push_listing(&[in_newer.clone(), in_older.clone()]);
        assert_eq!(push_warnings(&repo, &none), [both_listed]);
        let mut rejecting = Checks::new(OnSecrets::Reject);
        rejecting.record(read_of_commits_made_since(&repo, &older));
        let refusal = refusal_of(check_push_after(&repo, &rejecting));
        assert!(
            refusal.contains(&format!("{in_newer}\n{in_older}\n")),
            "{refusal}"
        );
    }

    /// Two secrets of one kind on one line are two findings at the push, under warn and reject.
    #[test]
    fn lists_two_secrets_of_one_kind_on_one_line_at_the_push() {
        let (dir, repo) = isolated_repo();
        let line = format!("aws {} aws {}\n", aws_key(), other_aws_key());
        let hash = commit_file(dir.path(), &line, "add two keys");
        let shown = format!("   commit {hash} deploy.env:1  AWS access key");

        let warned = push_warnings(&repo, &Checks::new(OnSecrets::Warn));
        let refusal = refusal_of(check_push(&repo, OnSecrets::Reject));

        assert_eq!(warned, [push_listing(&[shown.clone(), shown.clone()])]);
        assert!(
            refusal.contains(&format!("{shown}\n{shown}\n")),
            "{refusal}"
        );
    }

    /// A rebase makes new commits, so the push lists again what the run found in the old ones.
    #[test]
    fn lists_a_messages_secret_again_after_a_rebase() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let base = run(dir, &["rev-parse", "HEAD"]).trim().to_owned();
        commit_at(dir, "a.txt", "a\n", &format!("add a\n\naws {}", aws_key()));
        let mut checks = Checks::new(OnSecrets::Warn);
        checks.record(read_of_commits_made_since(&repo, &base));
        assert_eq!(push_warnings(&repo, &checks), Vec::<String>::new());
        run(dir, &["checkout", "-q", "-b", "upstream", &base]);
        commit_at(dir, "b.txt", "b\n", "add b");
        run(dir, &["checkout", "-q", "main"]);
        run(dir, &["rebase", "-q", "upstream"]);
        let rebased = short_head(dir);

        let shown = format!("   commit {rebased} message  AWS access key");
        assert_eq!(push_warnings(&repo, &checks), [push_listing(&[shown])]);
    }

    /// A binary file too large to check is listed once in a run: the push leaves out the one the
    /// run's checks of commits listed, and lists an older commit's other content at that path.
    #[test]
    fn lists_a_binary_file_too_large_to_check_once_in_a_run() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let big = |mark: &str| {
            let mut bytes = format!("\0{mark}\0").into_bytes();
            bytes.resize(usize::try_from(MOST_BINARY_BYTES).unwrap() + 1, 0);
            fs::write(dir.join("big.bin"), bytes).unwrap();
            run(dir, &["add", "big.bin"]);
        };
        big("older");
        run(dir, &["commit", "-q", "-m", "add the data"]);
        let older = short_head(dir);
        big("newer");
        run(dir, &["commit", "-q", "-m", "change the data"]);
        let mut checks = Checks::new(OnSecrets::Warn);
        checks.record(read_of_commits_made_since(&repo, &older));
        assert_eq!(checks.unchecked().len(), 1);

        let said = ui::test_support::listen();
        check_push_after(&repo, &checks).unwrap();

        let Verdict::Warn(listed) = unchecked_verdict(&[Unchecked {
            place: FileRef::changed("big.bin").in_commit(&CommitId::of(&older)),
            size: MOST_BINARY_BYTES + 1,
            blob: String::new(),
        }]) else {
            panic!("no warning");
        };
        assert_eq!(said.warnings(), [listed]);
    }

    #[test]
    fn lets_a_merge_of_published_commits_through() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        run(dir, &["checkout", "-q", "-b", "side"]);
        commit_file(
            dir,
            &format!("aws {}\n", aws_key()),
            "add the deploy settings",
        );
        run(dir, &["update-ref", "refs/remotes/origin/side", "HEAD"]);
        run(dir, &["checkout", "-q", "main"]);
        commit_at(dir, "main.txt", "main\n", "work on main");
        run(
            dir,
            &["merge", "-q", "--no-ff", "-m", "merge the side", "side"],
        );

        assert!(check_push(&repo, OnSecrets::Reject).is_ok());
    }

    /// A submodule's own files aren't part of a commit or push of this repository, whatever
    /// `diff.submodule` says.
    #[test]
    fn reads_a_submodule_as_the_commit_it_points_to() {
        let (dir, repo, library, _) = with_submodule();
        commit_at(
            library.path(),
            "k.env",
            &format!("aws {}\n", aws_key()),
            "add a key",
        );
        let moved = run(library.path(), &["rev-parse", "HEAD"]);
        move_pointer(dir.path(), moved.trim());
        run(dir.path(), &["config", "diff.submodule", "diff"]);
        run(dir.path(), &["add", "library"]);
        assert!(run(dir.path(), &["diff", "--cached"]).contains("+aws"));

        assert!(check_staged(&repo, Scope::Staged).is_ok());
        run(dir.path(), &["commit", "-q", "-m", "move the library"]);
        let range = ["HEAD~1..HEAD"];
        assert!(reject_commits(&repo, &range).is_ok());
    }

    /// A graft can make a commit look published, or change what a commit adds, but a push sends
    /// the parents a commit has.
    #[test]
    fn reads_the_commits_a_push_sends_whatever_a_graft_says() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let first = run(dir, &["rev-parse", "HEAD"]);
        let hash = commit_file(
            dir,
            &format!("aws {}\n", aws_key()),
            "add the deploy settings",
        );
        let second = run(dir, &["rev-parse", "HEAD"]);
        run(
            dir,
            &["update-ref", "refs/remotes/origin/main", first.trim()],
        );
        let grafts = format!("{} {}\n", first.trim(), second.trim());
        fs::write(dir.join(".git/info/grafts"), grafts).unwrap();
        let grafted = run(dir, &["log", "--format=%h", "HEAD", "--not", "--remotes"]);
        assert!(grafted.is_empty(), "{grafted}");

        let error = check_push(&repo, OnSecrets::Reject).unwrap_err();

        let shown = format!("commit {hash} deploy.env:1  AWS access key");
        assert!(error.to_string().contains(&shown), "{error}");
    }

    #[test]
    fn lets_through_a_push_of_commits_a_remote_has_or_that_only_remove_a_secret() {
        let (dir, repo) = isolated_repo();
        commit_file(
            dir.path(),
            &format!("aws {}\n", aws_key()),
            "add the deploy settings",
        );
        run(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        assert!(check_push(&repo, OnSecrets::Reject).is_ok());

        commit_file(dir.path(), "aws none\n", "take the key out");

        assert!(check_push(&repo, OnSecrets::Reject).is_ok());
    }

    /// A file `.gitattributes` marks `-diff` shows only as binary, so it's read whole, with the
    /// rules a text file's lines get.
    #[test]
    fn refuses_a_key_in_a_file_marked_not_to_be_diffed() {
        let (dir, repo) = isolated_repo();
        stage(dir.path(), ".gitattributes", "*.env -diff\n");
        stage(
            dir.path(),
            "app.env",
            &format!("DEBUG=1\naws {}\n", aws_key()),
        );
        assert!(run(dir.path(), &["diff", "--cached"]).contains("Binary files"));

        let refusal = refusal_of(check_staged(&repo, Scope::Staged));

        assert_eq!(refusal, refused(&["   app.env:2  AWS access key"]));
    }

    /// The Tracked scope stages the work tree in a private index, as `git commit --all` would,
    /// and leaves the real one alone.
    #[test]
    fn refuses_a_key_in_a_binary_file_staged_or_in_the_work_tree() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        commit_at(dir, "data.bin", "\0old\0", "add the data");
        fs::write(dir.join("data.bin"), format!("\0aws {}\0", aws_key())).unwrap();
        let expected = refused(&["   data.bin  AWS access key"]);

        assert!(check_staged(&repo, Scope::Staged).is_ok());
        assert_eq!(refusal_of(check_staged(&repo, Scope::Tracked)), expected);
        assert!(run(dir, &["diff", "--cached", "--name-only"]).is_empty());
        run(dir, &["add", "data.bin"]);
        assert_eq!(refusal_of(check_staged(&repo, Scope::Staged)), expected);
    }

    /// A symlink is committed as the path it holds, so what it points to isn't read.
    #[cfg(unix)]
    #[test]
    fn reads_a_symlink_as_the_path_it_holds() {
        let (dir, repo) = isolated_repo();
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("keys.bin");
        fs::write(&target, format!("\0aws {}\0", aws_key())).unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("link")).unwrap();
        run(dir.path(), &["add", "link"]);

        assert!(check_staged(&repo, Scope::Staged).is_ok());
        run(dir.path(), &["commit", "-q", "-m", "add the link"]);
        fs::remove_file(dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert!(check_staged(&repo, Scope::Tracked).is_ok());
    }

    /// A binary file over the cap is listed as not checked, and lets the commit through.
    #[test]
    fn lists_a_binary_file_over_sixteen_mib_and_lets_it_through() {
        let (dir, repo) = isolated_repo();
        let mut bytes = format!("\0aws {}\0", aws_key()).into_bytes();
        bytes.resize(usize::try_from(MOST_BINARY_BYTES).unwrap() + 1, 0);
        fs::write(dir.path().join("big.bin"), &bytes).unwrap();
        run(dir.path(), &["add", "big.bin"]);

        let scanned = found_in_changes(&repo, Scope::Staged).unwrap();

        assert!(scanned.findings.is_empty());
        let big = Unchecked {
            place: FileRef::changed("big.bin"),
            size: MOST_BINARY_BYTES + 1,
            blob: run(dir.path(), &["rev-parse", ":big.bin"])
                .trim()
                .to_owned(),
        };
        assert_eq!(scanned.unchecked, [big]);
        assert!(check_staged(&repo, Scope::Staged).is_ok());
    }

    #[test]
    fn lists_the_binary_files_too_large_to_check() {
        let file = |n: u64| Unchecked {
            place: FileRef::changed(format!("pkg{n}/data.bin")),
            size: MOST_BINARY_BYTES + n * 1024 * 1024,
            blob: n.to_string().repeat(40),
        };
        assert_eq!(unchecked_verdict(&[]), Verdict::Clean);
        assert_eq!(
            unchecked_verdict(&[file(1)]),
            Verdict::Warn(
                "git-agent didn't check these binary files for secrets, since each is over 16 \
                 MiB:\n   pkg1/data.bin  (17.0 MiB)"
                    .to_owned()
            )
        );
        let six: Vec<Unchecked> = (1..=6).map(file).collect();
        let Verdict::Warn(text) = unchecked_verdict(&six) else {
            panic!("no warning");
        };
        assert!(
            text.ends_with("pkg5/data.bin  (21.0 MiB)\n   … and 1 more"),
            "{text}"
        );
    }

    /// A merge that writes a key into a binary file adds it itself.
    #[test]
    fn refuses_a_key_a_merge_writes_into_a_binary_file() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        commit_at(dir, "data.bin", "\0base\0", "add the data");
        run(dir, &["checkout", "-q", "-b", "side"]);
        commit_at(dir, "data.bin", "\0side\0", "change it on the side");
        run(dir, &["checkout", "-q", "main"]);
        commit_at(dir, "data.bin", "\0main\0", "change it on main");
        run(dir, &["merge", "-q", "--no-commit", "-X", "ours", "side"]);
        let merge = commit_at(dir, "data.bin", &format!("\0aws {}\0", aws_key()), "merge");

        let error = check_push(&repo, OnSecrets::Reject).unwrap_err();

        let shown = format!("commit {merge} data.bin  AWS access key");
        assert!(error.to_string().contains(&shown), "{error}");
    }

    /// A binary file that keeps a key it had when it was published adds no key.
    #[test]
    fn lets_through_a_binary_file_that_keeps_a_published_key() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let line = format!("aws {}", aws_key());
        commit_at(dir, "data.bin", &format!("\0{line}\0"), "add the data");
        run(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let hash = commit_at(dir, "data.bin", &format!("\0more\0{line}\0"), "add more");

        let found = found_in_commits(&repo, &OUTGOING).unwrap().findings;

        let shown: Vec<_> = found
            .iter()
            .map(|finding| (finding.place.to_string(), finding.line, finding.change))
            .collect();
        assert_eq!(
            shown,
            [(format!("commit {hash} data.bin"), None, Change::Unchanged)]
        );
        assert_eq!(found[0].kind, "AWS access key");
        assert!(check_push(&repo, OnSecrets::Reject).is_ok());
    }

    /// Whatever makes git show a file as binary, the `binary` macro or a diff driver set to
    /// binary, its content is read.
    #[test]
    fn refuses_a_key_in_a_file_any_attribute_marks_binary() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        stage(dir, ".gitattributes", "*.cfg binary\n*.ini diff=opaque\n");
        run(dir, &["config", "diff.opaque.binary", "true"]);
        stage(dir, "app.cfg", &format!("aws {}\n", aws_key()));
        stage(dir, "app.ini", &format!("x = 1\naws {}\n", aws_key()));
        assert_eq!(
            run(dir, &["diff", "--cached"])
                .matches("Binary files")
                .count(),
            2
        );

        let refusal = refusal_of(check_staged(&repo, Scope::Staged));

        let shown = [
            "   app.cfg:1  AWS access key",
            "   app.ini:2  AWS access key",
        ];
        assert_eq!(refusal, refused(&shown));
    }

    /// A binary file renamed with a change is compared with what it was under its old name.
    #[test]
    fn compares_a_renamed_binary_file_with_its_old_content() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let line = format!("aws {}", aws_key());
        let padding = "\0padding that keeps the files alike".repeat(20);
        commit_at(
            dir,
            "data.bin",
            &format!("\0{line}{padding}"),
            "add the data",
        );
        run(dir, &["mv", "data.bin", "moved.bin"]);
        stage(dir, "moved.bin", &format!("\0{line}{padding}\0more"));
        assert!(run(dir, &["diff", "--cached", "-M"]).contains("rename to moved.bin"));
        assert!(check_staged(&repo, Scope::Staged).is_ok());

        let other = format!("AKIA{}", "R".repeat(16));
        stage(dir, "moved.bin", &format!("\0{line}{padding}\0aws {other}"));
        let refusal = refusal_of(check_staged(&repo, Scope::Staged));
        assert_eq!(refusal, refused(&["   moved.bin  AWS access key"]));
    }
}
