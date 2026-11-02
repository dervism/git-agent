//! Choosing what `git agent explain` explains: a commit, a range of commits,
//! a branch, or what the last pull brought in.

use crate::cli::ExplainArgs;
use crate::git::{Commit, Repo};
use anyhow::{Context, Result, bail};

/// What `git agent explain` explains.
#[derive(Debug, PartialEq)]
pub struct Subject {
    /// Names it in the output, e.g. "Commit a1b2c3d".
    pub heading: String,
    /// Names it for the model, e.g. "commit a1b2c3d".
    pub about: String,
    /// `git log` arguments that list its commits.
    pub log: Vec<String>,
    /// `git diff` arguments that show what its commits change together.
    pub diff: Vec<String>,
    /// What to say when it has no commits.
    pub nothing: String,
    /// Whether it is one commit.
    pub single: bool,
}

pub fn subject(repo: &Repo, args: &ExplainArgs) -> Result<Subject> {
    if args.pull {
        return pull(repo);
    }
    if args.branch {
        return branch(repo, args.target.as_deref(), args.base.as_deref());
    }
    let target = args.target.as_deref().unwrap_or("HEAD");
    match split_range(target) {
        Some((from, dots, to)) => range(repo, target, from, dots, to),
        None => commit(repo, target),
    }
}

/// The commit as the model sees it: "- a1b2c3d subject (author, date)" and
/// then the rest of the message, indented.
pub fn listed(commit: &Commit) -> String {
    let message = commit.message.strip_prefix("- ").unwrap_or(&commit.message);
    let (subject, body) = message.split_once('\n').unwrap_or((message, ""));
    let mut text = format!(
        "- {} {subject} ({}, {})",
        commit.hash, commit.author, commit.date
    );
    if !body.is_empty() {
        text.push('\n');
        text.push_str(body);
    }
    text
}

/// The ends of a range such as `a..b` or `a...b`, and its dots. A missing
/// end means HEAD, as it does for git.
fn split_range(text: &str) -> Option<(&str, &'static str, &str)> {
    fn or_head(end: &str) -> &str {
        if end.is_empty() { "HEAD" } else { end }
    }
    ["...", ".."].into_iter().find_map(|dots| {
        let (from, to) = text.split_once(dots)?;
        Some((or_head(from), dots, or_head(to)))
    })
}

fn resolve(repo: &Repo, revision: &str) -> Result<String> {
    repo.resolve(revision)?
        .with_context(|| format!("{revision} is not a commit or a branch"))
}

fn short(repo: &Repo, hash: &str) -> Result<(String, String)> {
    let summary = repo
        .summaries(&[hash])?
        .into_iter()
        .next()
        .with_context(|| format!("could not read commit {hash}"))?;
    Ok((summary.short, summary.subject))
}

/// One commit. A merge is explained through the commits it merged.
fn commit(repo: &Repo, target: &str) -> Result<Subject> {
    let hash = resolve(repo, target)?;
    let (short, subject) = short(repo, &hash)?;
    let one = |from: String, about: String| Subject {
        heading: format!("Commit {short}"),
        about,
        log: vec!["--max-count=1".to_owned(), hash.clone()],
        diff: vec![from, hash.clone()],
        nothing: format!("Commit {short} changes nothing."),
        single: true,
    };
    let subject = match repo.parents(&hash)?.as_slice() {
        [] => one(
            repo.empty_tree()?,
            format!("commit {short}, the repository's first"),
        ),
        [parent] => one(parent.clone(), format!("commit {short}")),
        [first, ..] => Subject {
            heading: format!("Merge {short}"),
            about: format!("merge commit {short} (\"{subject}\") through the commits it merged"),
            log: vec![format!("{first}..{hash}")],
            diff: vec![first.clone(), hash.clone()],
            nothing: format!("Merge {short} brought in no commits of its own."),
            single: false,
        },
    };
    Ok(subject)
}

fn range(repo: &Repo, text: &str, from: &str, dots: &str, to: &str) -> Result<Subject> {
    let (from, to) = (resolve(repo, from)?, resolve(repo, to)?);
    Ok(Subject {
        heading: text.to_owned(),
        about: format!("the commits in {text}"),
        log: vec![format!("{from}{dots}{to}")],
        // Like `git log a..b`, what b has that a doesn't, from where they part.
        diff: vec![format!("{from}...{to}")],
        nothing: format!("{text} has no commits."),
        single: false,
    })
}

/// The commits of `name` (by default the current branch) since it left `base`
/// (by default the remote's default branch).
fn branch(repo: &Repo, name: Option<&str>, base: Option<&str>) -> Result<Subject> {
    let name = match name {
        Some(name) => name.to_owned(),
        None => repo.current_branch()?,
    };
    if split_range(&name).is_some() {
        bail!("--branch takes a branch, not a range like {name}");
    }
    let tip = resolve(repo, &name)?;
    let base = match base {
        Some(base) => base.to_owned(),
        None => repo.default_base()?,
    };
    let from = resolve(repo, &base)?;
    Ok(Subject {
        heading: format!("Branch {name}"),
        about: format!("branch {name}: its commits since it left {base}"),
        log: vec![format!("{from}..{tip}")],
        diff: vec![format!("{from}...{tip}")],
        nothing: format!("{name} has no commits that {base} doesn't have."),
        single: false,
    })
}

/// The commits the last `git pull` brought into the current branch. After a
/// pull with rebase, the branch's own commits are new copies; leaving out
/// the commits that match ones from before the pull leaves only those that
/// arrived.
fn pull(repo: &Repo) -> Result<Subject> {
    let branch = repo.current_branch()?;
    let Some(pull) = repo.last_pull(&branch)? else {
        bail!(
            "git has no record of a pull that changed {branch} (its reflog has none), so there \
             is nothing to explain"
        );
    };
    Ok(Subject {
        heading: format!("Pull into {branch}, {}", pull.when),
        about: format!(
            "what the developer's last pull ({}) brought into {branch}: the commits that arrived",
            pull.when
        ),
        log: vec![
            "--cherry-pick".to_owned(),
            "--right-only".to_owned(),
            format!("{}...{}", pull.before, pull.after),
        ],
        diff: vec![pull.before.clone(), pull.after.clone()],
        nothing: format!(
            "The last pull into {branch} ({}) brought no new commits.",
            pull.when
        ),
        single: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    fn run(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn isolate(dir: &Path) {
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "no-hooks"),
        ] {
            run(dir, &["config", key, value]);
        }
    }

    /// Writes `file` and commits it with `subject`.
    fn commit(dir: &Path, file: &str, subject: &str) {
        fs::write(dir.join(file), format!("{subject}\n")).unwrap();
        run(dir, &["add", "--all"]);
        run(dir, &["commit", "-q", "-m", subject]);
    }

    /// A repository on main with one commit, "first", which adds first.txt.
    fn setup() -> (TempDir, Repo) {
        let dir = TempDir::new().unwrap();
        run(dir.path(), &["init", "-q", "--initial-branch", "main"]);
        isolate(dir.path());
        commit(dir.path(), "first.txt", "first");
        let repo = Repo::open(dir.path()).unwrap();
        (dir, repo)
    }

    fn args(target: Option<&str>) -> ExplainArgs {
        ExplainArgs {
            target: target.map(str::to_owned),
            branch: false,
            base: None,
            pull: false,
            question: None,
        }
    }

    /// The subjects of the commits `args` explains, and the diff they make.
    fn explained(repo: &Repo, args: &ExplainArgs) -> (Subject, Vec<String>, String) {
        let subject = subject(repo, args).unwrap();
        let log: Vec<&str> = subject.log.iter().map(String::as_str).collect();
        let subjects = repo
            .commits(&log)
            .unwrap()
            .into_iter()
            .map(|commit| commit.message.trim_start_matches("- ").to_owned())
            .collect();
        let diff: Vec<&str> = subject.diff.iter().map(String::as_str).collect();
        let changes = repo.changes_between(&diff, &[]).unwrap();
        (subject, subjects, changes.diff)
    }

    #[test]
    fn explains_a_commit_and_the_first_one() {
        let (dir, repo) = setup();
        commit(dir.path(), "second.txt", "second");
        let (subject, subjects, diff) = explained(&repo, &args(None));
        assert!(subject.single && subject.heading.starts_with("Commit "));
        assert_eq!(subjects, ["second"]);
        assert!(diff.contains("second.txt") && !diff.contains("first.txt"));

        let (subject, subjects, diff) = explained(&repo, &args(Some("HEAD~1")));
        assert!(subject.about.ends_with("the repository's first"));
        assert_eq!(subjects, ["first"]);
        assert!(diff.contains("+first"));

        let err = super::subject(&repo, &args(Some("nope"))).unwrap_err();
        assert_eq!(err.to_string(), "nope is not a commit or a branch");
    }

    #[test]
    fn explains_a_merge_through_the_commits_it_merged() {
        let (dir, repo) = setup();
        run(dir.path(), &["switch", "-q", "-c", "feature"]);
        commit(dir.path(), "x.txt", "add x");
        commit(dir.path(), "y.txt", "add y");
        run(dir.path(), &["switch", "-q", "main"]);
        commit(dir.path(), "z.txt", "add z");
        run(
            dir.path(),
            &["merge", "-q", "--no-ff", "-m", "Merge feature", "feature"],
        );

        let (subject, subjects, diff) = explained(&repo, &args(None));
        assert!(!subject.single && subject.heading.starts_with("Merge "));
        assert!(subject.about.contains("(\"Merge feature\")"));
        assert_eq!(subjects, ["add x", "add y"]);
        assert!(diff.contains("x.txt") && diff.contains("y.txt") && !diff.contains("z.txt"));
    }

    #[test]
    fn explains_ranges_and_branches() {
        let (dir, repo) = setup();
        run(dir.path(), &["tag", "v1"]);
        run(dir.path(), &["switch", "-q", "-c", "feature"]);
        commit(dir.path(), "x.txt", "add x");
        commit(dir.path(), "y.txt", "add y");
        run(dir.path(), &["switch", "-q", "main"]);
        commit(dir.path(), "z.txt", "add z");

        let (subject, subjects, _) = explained(&repo, &args(Some("v1..feature")));
        assert_eq!(subject.heading, "v1..feature");
        assert_eq!(subjects, ["add x", "add y"]);

        let mut branch = args(Some("feature"));
        branch.branch = true;
        let (subject, subjects, diff) = explained(&repo, &branch);
        assert_eq!(
            subject.about,
            "branch feature: its commits since it left main"
        );
        assert_eq!(subjects, ["add x", "add y"]);
        // Main's own later commit is not the branch's.
        assert!(!diff.contains("z.txt"));

        let (_, subjects, _) = explained(&repo, &args(Some("feature..")));
        assert_eq!(subjects, ["add z"]);
        branch.target = Some("main..feature".into());
        let err = super::subject(&repo, &branch).unwrap_err();
        assert!(err.to_string().contains("takes a branch, not a range"));
    }

    #[test]
    fn explains_what_the_last_pull_brought_in() {
        let (dir, repo) = setup();
        let remote = TempDir::new().unwrap();
        let url = remote.path().to_str().unwrap();
        run(
            remote.path(),
            &["init", "-q", "--bare", "--initial-branch", "main"],
        );
        run(dir.path(), &["remote", "add", "origin", url]);
        run(
            dir.path(),
            &["push", "-q", "--set-upstream", "origin", "main"],
        );
        let mut pull = args(None);
        pull.pull = true;
        let err = super::subject(&repo, &pull).unwrap_err();
        assert!(err.to_string().contains("no record of a pull"));

        let other = TempDir::new().unwrap();
        run(other.path(), &["clone", "-q", url, "."]);
        isolate(other.path());
        commit(other.path(), "remote.txt", "remote change");
        run(other.path(), &["push", "-q"]);
        // A local commit that the pull's rebase copies; it didn't arrive with the pull.
        commit(dir.path(), "local.txt", "local change");
        run(dir.path(), &["pull", "-q", "--rebase"]);

        let (subject, subjects, diff) = explained(&repo, &pull);
        assert!(subject.heading.starts_with("Pull into main, "));
        assert_eq!(subjects, ["remote change"]);
        assert!(diff.contains("remote.txt") && !diff.contains("local.txt"));
    }

    #[test]
    fn splits_ranges_like_git() {
        assert_eq!(
            split_range("v0.5.0..v0.6.0"),
            Some(("v0.5.0", "..", "v0.6.0"))
        );
        assert_eq!(
            split_range("main...feature"),
            Some(("main", "...", "feature"))
        );
        assert_eq!(split_range("main.."), Some(("main", "..", "HEAD")));
        assert_eq!(
            split_range("..origin/main"),
            Some(("HEAD", "..", "origin/main"))
        );
        assert_eq!(split_range("HEAD~3"), None);
    }

    #[test]
    fn lists_a_commit_with_its_hash_and_author() {
        let commit = Commit {
            hash: "a1b2c3d".into(),
            author: "Ann".into(),
            date: "2025-10-02".into(),
            message: "- feat: add b\n  b replaces a.".into(),
        };
        assert_eq!(
            listed(&commit),
            "- a1b2c3d feat: add b (Ann, 2025-10-02)\n  b replaces a."
        );
        let short = Commit {
            message: "- fix: c".into(),
            ..commit
        };
        assert_eq!(listed(&short), "- a1b2c3d fix: c (Ann, 2025-10-02)");
    }
}
