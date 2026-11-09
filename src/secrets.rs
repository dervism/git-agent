//! Secrets in what goes out: finding them and putting `[REDACTED]` in their place before text
//! reaches a model, and refusing or warning when a commit or a push would add them.
//!
//! The checks read what git publishes, every file of every commit, rather than what the model
//! reads, so that no setting, such as `exclude`, can hide a secret from them.

pub mod redact;

use crate::config::OnSecrets;
use crate::git::{CommitPatch, Repo, Scope};
use crate::ui;
use anyhow::{Result, bail};
use redact::{Change, Finding};

/// How to make a check pass when its findings aren't secrets.
const ADVICE: &str = "If they aren't real secrets, add --on-secrets warn, or choose Warn under \
                      Secrets in `git agent config`.";

/// The commits a push of HEAD sends: those no remote-tracking branch has.
const OUTGOING: [&str; 3] = ["HEAD", "--not", "--remotes"];

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
/// flow lists itself, which a warning leaves out.
pub fn check_changes(
    repo: &Repo,
    scope: Scope,
    on_secrets: OnSecrets,
    reported: &[Finding],
    wording: &Wording,
) -> Result<()> {
    settle(changes_verdict(repo, scope, on_secrets, reported, wording)?)
}

/// Checks the messages and every file of the commits `range` (`git log` arguments) publishes.
/// `reported` holds the findings the flow lists itself, which a warning leaves out.
pub fn check_commits(
    repo: &Repo,
    range: &[&str],
    on_secrets: OnSecrets,
    reported: &[Finding],
    wording: &Wording,
) -> Result<()> {
    settle(commits_verdict(repo, range, on_secrets, reported, wording)?)
}

/// Warns, whatever the setting, about the secrets the commits `range` publishes add, apart
/// from those in `reported`, which the flow lists itself.
pub fn warn_about_commits(
    repo: &Repo,
    range: &[&str],
    reported: &[Finding],
    warned: &str,
) -> Result<()> {
    settle(commits_warning(repo, range, reported, warned)?)
}

/// Checks the commits a push of HEAD would send.
pub fn check_outgoing(repo: &Repo, on_secrets: OnSecrets) -> Result<()> {
    check_commits(repo, &OUTGOING, on_secrets, &[], &PUSH)
}

/// What [`check_changes`] decides.
fn changes_verdict(
    repo: &Repo,
    scope: Scope,
    on_secrets: OnSecrets,
    reported: &[Finding],
    wording: &Wording,
) -> Result<Verdict> {
    let findings = redact::diff_findings(&repo.changes_to_publish(scope)?);
    Ok(verdict(&findings, on_secrets, reported, wording))
}

/// What [`check_commits`] decides.
fn commits_verdict(
    repo: &Repo,
    range: &[&str],
    on_secrets: OnSecrets,
    reported: &[Finding],
    wording: &Wording,
) -> Result<Verdict> {
    let findings = scan_commits(&repo.published_commits(range)?);
    Ok(verdict(&findings, on_secrets, reported, wording))
}

/// What [`warn_about_commits`] decides.
fn commits_warning(
    repo: &Repo,
    range: &[&str],
    reported: &[Finding],
    warned: &str,
) -> Result<Verdict> {
    let findings = scan_commits(&repo.published_commits(range)?);
    Ok(warning(&findings, reported, warned).map_or(Verdict::Clean, Verdict::Warn))
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
    let new: Vec<&Finding> = added(findings)
        .into_iter()
        .filter(|finding| !covered(finding, reported))
        .collect();
    (!new.is_empty()).then(|| format!("{warned}:\n{}", list_findings(&new)))
}

/// Whether `reported` holds the same kind of secret, changed in the same way, in the same
/// place. A finding in a commit is also held by one in its file, as a diff of several commits
/// names it.
fn covered(finding: &Finding, reported: &[Finding]) -> bool {
    let file = finding
        .place
        .strip_prefix("commit ")
        .and_then(|rest| rest.split_once(' '))
        .map(|(_, file)| file);
    reported.iter().any(|shown| {
        shown.kind == finding.kind
            && shown.change == finding.change
            && (shown.place == finding.place || Some(shown.place.as_str()) == file)
    })
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

/// The secrets in the messages and patches of `commits`.
fn scan_commits(commits: &[CommitPatch]) -> Vec<Finding> {
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
    use crate::config::DEFAULT_EXCLUDE;
    use crate::git::test_support::{isolated_repo, move_pointer, run, with_submodule};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    const WORDING: Wording = Wording {
        refused: "stopped",
        fix: "Fix it.",
        warned: "going on",
    };

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
        run(dir, &["rev-parse", "--short", "HEAD"])
            .trim()
            .to_owned()
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
    fn refusal_of(checked: Result<()>) -> String {
        checked.unwrap_err().to_string()
    }

    /// The refusal that lists `shown`, one finding per line.
    fn refused(shown: &[&str]) -> String {
        format!("stopped:\n{}\nFix it. {ADVICE}", shown.join("\n"))
    }

    fn check_staged(repo: &Repo, scope: Scope) -> Result<()> {
        check_changes(repo, scope, OnSecrets::Reject, &[], &WORDING)
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
    fn refuses_every_secret_a_change_adds_and_nothing_else() {
        let left_alone = [
            finding("a.rs", Some(1), Change::Unchanged),
            finding("a.rs", Some(2), Change::Removed),
        ];
        assert!(added(&left_alone).is_empty());
        let verdict_on = |findings: &[Finding], reported: &[Finding]| {
            verdict(findings, OnSecrets::Reject, reported, &WORDING)
        };
        assert_eq!(verdict_on(&left_alone, &[]), Verdict::Clean);
        assert_eq!(verdict_on(&[], &[]), Verdict::Clean);

        let findings = [
            finding("a.rs", Some(1), Change::Unchanged),
            finding(".env", Some(2), Change::Added),
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
            finding("a.rs", Some(1), Change::Removed),
            finding("Cargo.lock", Some(4), Change::Added),
            finding("a.rs", Some(3), Change::Added),
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

    #[test]
    fn takes_a_finding_in_a_commit_for_the_same_one_in_its_file() {
        let added = |place: &str| finding(place, Some(2), Change::Added);
        let in_commit = added("commit 1a2b3c4 src/my file.rs");
        let other_kind = Finding {
            kind: "AWS access key".to_owned(),
            ..added("src/my file.rs")
        };
        let cases = [
            (added("src/my file.rs"), true),
            (added("commit 1a2b3c4 src/my file.rs"), true),
            (added("commit 5d6e7f8 src/my file.rs"), false),
            (added("my file.rs"), false),
            (finding("src/my file.rs", Some(2), Change::Unchanged), false),
            (other_kind, false),
        ];
        for (shown, held) in cases {
            let reported = std::slice::from_ref(&shown);
            assert_eq!(covered(&in_commit, reported), held, "{shown:?}");
        }
        assert!(!covered(&in_commit, &[]));
    }

    #[test]
    fn takes_any_other_finding_for_one_in_the_same_place_on_any_line() {
        let message = finding("commit 1a2b3c4 message", None, Change::Added);
        assert!(covered(&message, std::slice::from_ref(&message)));
        let in_file = finding("a.rs", Some(1), Change::Added);
        assert!(covered(
            &in_file,
            &[finding("a.rs", Some(9), Change::Added)]
        ));
        assert!(!covered(
            &in_file,
            &[finding("b.rs", Some(1), Change::Added)]
        ));
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
            stage(dir.path(), path, &format!("aws {}\n", key()));
            let (_, for_model) = repo.changes(exclude).unwrap().unwrap();
            assert!(!for_model.diff.contains(&key()), "{}", for_model.diff);

            let refusal = refusal_of(check_staged(&repo, Scope::Staged));

            assert_eq!(refusal, refused(&[&format!("   {path}:1  AWS access key")]));
            let warned = changes_verdict(&repo, Scope::Staged, OnSecrets::Warn, &[], &WORDING);
            let listed = format!("going on:\n   {path}:1  AWS access key");
            assert_eq!(warned.unwrap(), Verdict::Warn(listed));
        }
    }

    /// `git commit --all` commits what is staged too, so the Tracked scope reads from HEAD; what
    /// HEAD holds already, and an untracked file, aren't added by the commit.
    #[test]
    fn reads_the_staged_and_unstaged_changes_a_commit_of_all_changes_holds() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        commit_file(dir, &format!("aws {}\n", key()), "add the deploy settings");
        stage(dir, "lib.rs", &format!("// aws {}\n", key()));
        fs::write(dir.join("draft.env"), format!("aws {}\n", key())).unwrap();
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
        let hash = commit_file(dir, &format!("aws {}\n", key()), "add the deploy settings");
        stage(dir, "lib.rs", &format!("// aws {}\n", key()));
        let repo = Repo::open(dir.join("notes")).unwrap();
        let range = ["HEAD~1..HEAD"];

        let staged = refusal_of(check_staged(&repo, Scope::Staged));
        let pushed = refusal_of(check_commits(
            &repo,
            &range,
            OnSecrets::Reject,
            &[],
            &WORDING,
        ));

        assert_eq!(staged, refused(&["   lib.rs:1  AWS access key"]));
        let shown = format!("   commit {hash} deploy.env:1  AWS access key");
        assert_eq!(pushed, refused(&[&shown]));
    }

    #[test]
    fn reads_the_changes_of_a_first_commit() {
        let (dir, repo) = unborn_repo();
        stage(dir.path(), "deploy.env", &format!("aws {}\n", key()));
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
        stage(dir, "notes/ø.env", &format!("a\n\nb\naws {}\n", key()));

        let refusal = refusal_of(check_staged(&repo, Scope::Staged));

        assert_eq!(refusal, refused(&["   notes/ø.env:4  AWS access key"]));
    }

    #[test]
    fn reads_the_commits_whatever_the_users_diff_settings() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        commit_at(dir, "notes/ø.env", "a\n\nb\n", "add notes");
        hide_from_plain_reads(dir);
        let text = format!("a\n\nb\naws {}\n", key());
        let hash = commit_at(dir, "notes/ø.env", &text, "more notes");
        let check = check_commits(&repo, &["HEAD~1..HEAD"], OnSecrets::Reject, &[], &WORDING);

        let refusal = refusal_of(check);

        let shown = format!("   commit {hash} notes/ø.env:4  AWS access key");
        assert_eq!(refusal, refused(&[&shown]));
    }

    /// `git replace` can make HEAD look as if it held a key the changes add.
    #[test]
    fn reads_the_changes_against_head_as_it_is() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let line = format!("aws {}\n", key());
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
            &format!("aws {}\n", key()),
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
            &format!("aws {}\n", key()),
            "add the deploy settings",
        );

        let error = check_outgoing(&repo, OnSecrets::Reject).unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "commits the push would send add what look like secrets:\n   commit {hash} \
                 deploy.env:1  AWS access key\nRemove them from those commits, for example with \
                 `git rebase -i`. {ADVICE}"
            )
        );
    }

    /// A commit message can hold git's field separator; what follows it is still scanned as part
    /// of the message.
    #[test]
    fn refuses_a_key_in_a_message_after_a_separator_byte() {
        let (dir, repo) = isolated_repo();
        let message = format!("tidy up\n\n\u{1f}aws {}", key());
        let hash = commit_file(dir.path(), "plain\n", &message);

        let error = check_outgoing(&repo, OnSecrets::Reject).unwrap_err();

        let shown = format!("commit {hash} message  AWS access key");
        assert!(error.to_string().contains(&shown), "{error}");
    }

    /// A commit that changes only files the model doesn't read is still published, message and
    /// all.
    #[test]
    fn refuses_a_key_in_a_commit_of_lockfiles_only() {
        let (dir, repo) = isolated_repo();
        let in_message = commit_at(dir.path(), "Cargo.lock", "v1\n", &format!("aws {}", key()));
        let in_file = commit_at(
            dir.path(),
            "Cargo.lock",
            &format!("aws {}\n", key()),
            "bump",
        );
        let refusal = refusal_of(check_commits(
            &repo,
            &OUTGOING,
            OnSecrets::Reject,
            &[],
            &WORDING,
        ));
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
        let merge = commit_file(dir, &format!("aws {}\n", key()), "merge the side");

        let error = check_outgoing(&repo, OnSecrets::Reject).unwrap_err();

        let shown = format!("commit {merge} deploy.env:1  AWS access key");
        assert!(error.to_string().contains(&shown), "{error}");
    }

    /// `log.showRoot=false` hides a first commit's diff, and `git replace` can show a commit
    /// other than the one a push sends.
    #[test]
    fn reads_the_commits_a_push_sends_whatever_the_users_settings() {
        let (dir, repo) = unborn_repo();
        let dir = dir.path();
        let first = commit_file(dir, &format!("aws {}\n", key()), "add the deploy settings");
        run(dir, &["config", "log.showRoot", "false"]);
        let shown = format!("commit {first} deploy.env:1  AWS access key");
        let error = check_outgoing(&repo, OnSecrets::Reject).unwrap_err();
        assert!(error.to_string().contains(&shown), "{error}");

        let second = commit_file(dir, &format!("aws {}\nmore\n", key()), "more");
        let empty = repo.empty_tree().unwrap();
        let stand_in = run(dir, &["commit-tree", "-m", "plain", &empty]);
        run(dir, &["replace", "-f", &first, stand_in.trim()]);
        let replaced = run(dir, &["log", "-p", "--root", "--format=%h %s"]);
        assert!(replaced.contains(&format!("{second} more\n")), "{replaced}");
        assert!(replaced.contains("+aws"), "{replaced}");
        assert!(!replaced.contains("add the deploy settings"), "{replaced}");
        let error = check_outgoing(&repo, OnSecrets::Reject).unwrap_err();
        assert!(error.to_string().contains(&shown), "{error}");
        assert!(!error.to_string().contains(&second), "{error}");
    }

    #[test]
    fn checks_only_the_commits_of_the_range() {
        let (dir, repo) = isolated_repo();
        commit_file(
            dir.path(),
            &format!("aws {}\n", key()),
            "add the deploy settings",
        );
        run(dir.path(), &["checkout", "-q", "-b", "feature"]);
        commit_file(dir.path(), "aws none\n", "take the key out");
        let check = |range: &str| check_commits(&repo, &[range], OnSecrets::Reject, &[], &WORDING);

        assert!(check("main..HEAD").is_ok());
        assert!(check("HEAD").is_err());
    }

    #[test]
    fn only_warns_about_commits_whatever_the_setting() {
        let (dir, repo) = isolated_repo();
        let hash = commit_file(dir.path(), &format!("aws {}\n", key()), "add the settings");
        let shown = format!("going on:\n   commit {hash} deploy.env:1  AWS access key");

        let warned = commits_warning(&repo, &["HEAD"], &[], "going on").unwrap();

        assert_eq!(warned, Verdict::Warn(shown));
        let in_file = finding("deploy.env", Some(1), Change::Added);
        let reported = [Finding {
            kind: "AWS access key".to_owned(),
            ..in_file
        }];
        let warned = commits_warning(&repo, &["HEAD"], &reported, "going on").unwrap();
        assert_eq!(warned, Verdict::Clean);
        assert!(warn_about_commits(&repo, &["HEAD"], &[], "going on").is_ok());
    }

    #[test]
    fn lets_the_push_through_when_set_to_warn() {
        let (dir, repo) = isolated_repo();
        let hash = commit_file(dir.path(), &format!("aws {}\n", key()), "add the settings");
        let shown = format!(
            "pushing commits that add what look like secrets:\n   commit {hash} deploy.env:1  \
             AWS access key"
        );

        let warned = commits_verdict(&repo, &OUTGOING, OnSecrets::Warn, &[], &PUSH).unwrap();

        assert_eq!(warned, Verdict::Warn(shown));
        assert!(check_outgoing(&repo, OnSecrets::Warn).is_ok());
    }

    /// A merge of commits that were checked or published adds nothing itself.
    #[test]
    fn lets_a_merge_of_published_commits_through() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        run(dir, &["checkout", "-q", "-b", "side"]);
        commit_file(dir, &format!("aws {}\n", key()), "add the deploy settings");
        run(dir, &["update-ref", "refs/remotes/origin/side", "HEAD"]);
        run(dir, &["checkout", "-q", "main"]);
        commit_at(dir, "main.txt", "main\n", "work on main");
        run(
            dir,
            &["merge", "-q", "--no-ff", "-m", "merge the side", "side"],
        );

        assert!(check_outgoing(&repo, OnSecrets::Reject).is_ok());
    }

    /// A submodule's own files aren't part of a commit or push of this repository, whatever
    /// `diff.submodule` says.
    #[test]
    fn reads_a_submodule_as_the_commit_it_points_to() {
        let (dir, repo, library, _) = with_submodule();
        commit_at(
            library.path(),
            "k.env",
            &format!("aws {}\n", key()),
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
        assert!(check_commits(&repo, &range, OnSecrets::Reject, &[], &WORDING).is_ok());
    }

    /// A graft can make a commit look published, or change what a commit adds, but a push sends
    /// the parents a commit has.
    #[test]
    fn reads_the_commits_a_push_sends_whatever_a_graft_says() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let first = run(dir, &["rev-parse", "HEAD"]);
        let hash = commit_file(dir, &format!("aws {}\n", key()), "add the deploy settings");
        let second = run(dir, &["rev-parse", "HEAD"]);
        run(
            dir,
            &["update-ref", "refs/remotes/origin/main", first.trim()],
        );
        let grafts = format!("{} {}\n", first.trim(), second.trim());
        fs::write(dir.join(".git/info/grafts"), grafts).unwrap();
        let grafted = run(dir, &["log", "--format=%h", "HEAD", "--not", "--remotes"]);
        assert!(grafted.is_empty(), "{grafted}");

        let error = check_outgoing(&repo, OnSecrets::Reject).unwrap_err();

        let shown = format!("commit {hash} deploy.env:1  AWS access key");
        assert!(error.to_string().contains(&shown), "{error}");
    }

    #[test]
    fn lets_through_a_push_of_commits_a_remote_has_or_that_only_remove_a_secret() {
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
        assert!(check_outgoing(&repo, OnSecrets::Reject).is_ok());

        commit_file(dir.path(), "aws none\n", "take the key out");

        assert!(check_outgoing(&repo, OnSecrets::Reject).is_ok());
    }
}
