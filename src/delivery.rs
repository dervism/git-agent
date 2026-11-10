//! Committing and pushing: the steps that record and publish what the user accepted.

use crate::config::OnSecrets;
use crate::git::{Repo, Scope};
use crate::secrets::contents::Scanned;
use crate::secrets::{self, AtCommit, Wording};
use crate::ui;
use anyhow::{Context, Result, anyhow, bail};

/// The `git commit` arguments a user may add, so that the commit holds what was checked and what
/// the message describes. Those that end in `=` take a value after it.
const COMMIT_ARGS: &[&str] = &[
    "-s",
    "--signoff",
    "--no-signoff",
    "-n",
    "--no-verify",
    "--verify",
    "--no-gpg-sign",
    "-q",
    "-v",
    "--allow-empty",
    "--no-post-rewrite",
    "--author=",
    "--date=",
    "--trailer=",
    "--cleanup=",
];

const BEFORE_COMMIT: Wording = Wording {
    refused: "the commit would add what look like secrets, so git-agent didn't make it",
    fix: "Take them out of the changes or the message.",
    warned: "committing what look like secrets",
};

const MADE_COMMIT: Wording = Wording {
    refused: "the commit git made adds what look like secrets that weren't there when git-agent \
              checked it, as when a hook adds them",
    fix: "If the commit stays, take them out before you push it.",
    warned: "the commit git made adds what look like secrets that weren't there when git-agent \
             checked it",
};

/// A commit to make.
pub struct NewCommit<'a> {
    pub message: &'a str,
    /// The changes it holds.
    pub scope: Scope,
    /// What the user adds to `git commit`, from [`COMMIT_ARGS`].
    pub git_args: &'a [String],
    /// Paths whose work-tree changes a commit of all tracked changes leaves out.
    pub leave_out: &'a [String],
}

/// Fails unless every one of `args` is a `git commit` argument the user may add. git takes
/// abbreviations and bundled short options, so only exact spellings from a list are safe.
pub fn refuse_unknown_commit_args(args: &[String]) -> Result<()> {
    match args.iter().find(|arg| !is_commit_arg(arg)) {
        None => Ok(()),
        Some(arg) => bail!(
            "git-agent passes only these arguments on to git commit, so that the commit holds what \
             it checked: {}, -S[<keyid>] and --gpg-sign[=<keyid>]; it got {:?}",
            COMMIT_ARGS.join(", "),
            arg
        ),
    }
}

/// Whether `arg` is one of [`COMMIT_ARGS`], with a value where it takes one, or signs the commit.
fn is_commit_arg(arg: &str) -> bool {
    let known = |allowed: &&str| {
        if allowed.ends_with('=') {
            arg.strip_prefix(allowed)
                .is_some_and(|value| !value.is_empty())
        } else {
            arg == *allowed
        }
    };
    COMMIT_ARGS.iter().any(known) || signs_the_commit(arg)
}

/// Whether `arg` signs the commit: `-S` or `--gpg-sign`, each with or without a key.
fn signs_the_commit(arg: &str) -> bool {
    arg.starts_with("-S")
        || arg == "--gpg-sign"
        || arg
            .strip_prefix("--gpg-sign=")
            .is_some_and(|key| !key.is_empty())
}

/// Commits `commit`, checking just before what it will hold, its message and its arguments,
/// and once it's made what it adds, as hooks can change it; under reject a commit that adds
/// secrets is undone where that's safe. `at` gives the setting and what the check before the
/// model read.
pub fn commit(repo: &Repo, commit: &NewCommit, at: AtCommit) -> Result<()> {
    refuse_unknown_commit_args(commit.git_args)?;
    let arguments = commit.git_args.join(" ");
    let texts = [
        ("message", commit.message),
        ("arguments", arguments.as_str()),
    ];
    let checked = secrets::check_before_commit(repo, commit.scope, &texts, at, &BEFORE_COMMIT)?;
    let before = repo.head_commit()?;
    let branch = repo.head_branch()?;
    repo.commit(
        commit.message,
        commit.scope,
        commit.git_args,
        commit.leave_out,
    )?;
    let made = Made {
        hash: repo
            .head_commit()?
            .context("git commit left HEAD without a commit")?,
        before,
        branch,
    };
    check_made_commit(repo, &made, at.on_secrets, &checked)?;
    ui::info(ui::green("✅ Committed."));
    Ok(())
}

/// A commit git made, and where HEAD was before it.
struct Made {
    hash: String,
    /// The commit HEAD was at, or `None` on a branch without commits.
    before: Option<String>,
    /// The branch HEAD was on, or `None` when it was detached.
    branch: Option<String>,
}

/// Checks what `made` adds itself, along its first parent, so a merge's incoming history isn't
/// read as its own. Under reject a failed check undoes it where that's safe; under warn a check
/// that couldn't read it only warns.
fn check_made_commit(
    repo: &Repo,
    made: &Made,
    on_secrets: OnSecrets,
    checked: &Scanned,
) -> Result<()> {
    let range = made.before.as_ref().map_or_else(
        || made.hash.clone(),
        |before| format!("{before}..{}", made.hash),
    );
    let at = AtCommit {
        on_secrets,
        before: checked,
    };
    let checking = secrets::check_made_commit(repo, &["--first-parent", &range], at, &MADE_COMMIT);
    let Err(failure) = checking else {
        return Ok(());
    };
    if on_secrets == OnSecrets::Warn {
        ui::warn(format!(
            "git-agent couldn't check the commit git made: {failure:#}"
        ));
        return Ok(());
    }
    Err(anyhow!("{failure:#}\n{}", undo(repo, made)))
}

/// Undoes `made` if that moves nothing but its own branch, and says what became of it.
fn undo(repo: &Repo, made: &Made) -> String {
    let short = &made.hash[..made.hash.len().min(7)];
    let undone = why_not_undo(repo, made).and_then(|()| {
        repo.undo_commit(&made.hash, made.before.as_deref(), made.branch.as_deref())
            .map_err(|error| format!("git couldn't move the branch back ({error:#})"))
    });
    match undone {
        Ok(()) => {
            let place = if repo.uses_own_index() {
                "in the work tree"
            } else {
                "staged"
            };
            format!(
                "git-agent undid commit {short}, and its changes are still {place}. Reuse its \
                 message with `git commit -c {short}`."
            )
        }
        Err(reason) => format!(
            "git-agent didn't undo commit {short}, since {reason}; take the secrets out of it \
             before you push it."
        ),
    }
}

/// Why undoing `made` could lose or move more than it, if it could: a merge, or HEAD on another
/// branch or commit since.
fn why_not_undo(repo: &Repo, made: &Made) -> Result<(), String> {
    let parents = repo
        .stored_parents(&made.hash)
        .map_err(|error| format!("git couldn't read its parents ({error:#})"))?;
    if parents.len() > 1 {
        return Err("it's a merge, and undoing it would lose its other parents".to_owned());
    }
    if parents.first() != made.before.as_ref() {
        return Err("it isn't the commit that was made on the one HEAD was at".to_owned());
    }
    let now = (repo.head_branch(), repo.head_commit());
    match now {
        (Ok(branch), Ok(Some(head))) if branch == made.branch && head == made.hash => Ok(()),
        _ => Err("HEAD moved after it was made".to_owned()),
    }
}

/// Pushes, first rebasing onto commits that reached the remote from elsewhere
/// (e.g. edits made on GitHub), then checking the commits it sends for secrets
/// as `on_secrets` says. With `ask`, the user confirms the rebase.
/// Returns whether it pushed: the user may decline the rebase.
pub fn push(repo: &Repo, ask: bool, on_secrets: OnSecrets) -> Result<bool> {
    const SHOWN: usize = 5;

    let incoming = repo.incoming_commits()?;
    if !incoming.is_empty() {
        ui::info(format!(
            "The remote has {} commit(s) you don't have yet:",
            incoming.len()
        ));
        for commit in incoming.iter().take(SHOWN) {
            ui::info(format!("   {commit}"));
        }
        if incoming.len() > SHOWN {
            ui::info(format!("   … and {} more", incoming.len() - SHOWN));
        }
        if ask && !ui::confirm("Rebase your commits on top of them and push?")? {
            ui::info("Not pushed. When you're ready, run `git pull --rebase` and `git push`.");
            return Ok(false);
        }
        repo.rebase_onto_upstream()?;
        ui::info("🔀 Rebased onto the remote commits.");
    }
    secrets::check_outgoing(repo, on_secrets)?;
    repo.push()?;
    ui::info(ui::green("✅ Pushed."));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::git::test_support::hook;
    use crate::git::test_support::{add_remote, commit_elsewhere, isolated_repo, run};
    use crate::secrets::test_support::aws_key;
    use std::fs;
    use std::path::Path;

    fn commit_key(dir: &Path) {
        fs::write(dir.join("deploy.env"), format!("aws {}\n", aws_key())).unwrap();
        run(dir, &["add", "deploy.env"]);
        run(dir, &["commit", "-q", "-m", "add the deploy settings"]);
    }

    fn remote_head(remote: &Path) -> String {
        run(remote, &["rev-parse", "main"])
    }

    #[test]
    fn refuses_a_push_of_a_secret_and_leaves_the_remote_alone() {
        let (dir, repo) = isolated_repo();
        let remote = add_remote(&repo);
        let before = remote_head(remote.path());
        commit_key(dir.path());

        let error = push(&repo, false, OnSecrets::Reject).unwrap_err();

        assert!(
            error.to_string().contains("deploy.env:1  AWS access key"),
            "{error}"
        );
        assert_eq!(remote_head(remote.path()), before);
    }

    /// The check reads the commits as the push sends them, after the rebase onto what reached
    /// the remote from elsewhere.
    #[test]
    fn checks_the_commits_after_rebasing_them() {
        let (dir, repo) = isolated_repo();
        let remote = add_remote(&repo);
        commit_elsewhere(&remote, "notes.txt", "notes\n", "edit on GitHub");
        let before = remote_head(remote.path());
        commit_key(dir.path());

        let error = push(&repo, false, OnSecrets::Reject).unwrap_err();

        let rebased = run(dir.path(), &["rev-parse", "--short", "HEAD"]);
        let shown = format!("commit {} deploy.env:1  AWS access key", rebased.trim());
        assert!(error.to_string().contains(&shown), "{error}");
        let subjects = run(dir.path(), &["log", "--format=%s"]);
        assert_eq!(
            subjects.lines().take(2).collect::<Vec<_>>(),
            ["add the deploy settings", "edit on GitHub"]
        );
        assert_eq!(remote_head(remote.path()), before);
    }

    #[test]
    fn pushes_a_secret_after_a_warning_when_set_to_warn() {
        let (dir, repo) = isolated_repo();
        let remote = add_remote(&repo);
        commit_key(dir.path());

        assert!(push(&repo, false, OnSecrets::Warn).unwrap());

        assert_eq!(
            remote_head(remote.path()),
            run(dir.path(), &["rev-parse", "HEAD"])
        );
    }

    /// Commits what's staged, or all tracked changes, with `message`, checked as `on_secrets` says.
    fn commit_as(repo: &Repo, scope: Scope, message: &str, on_secrets: OnSecrets) -> Result<()> {
        let new_commit = NewCommit {
            message,
            scope,
            git_args: &[],
            leave_out: &[],
        };
        let at = AtCommit {
            on_secrets,
            before: &Scanned::default(),
        };
        commit(repo, &new_commit, at)
    }

    fn head(dir: &Path) -> String {
        run(dir, &["rev-parse", "HEAD"])
    }

    /// How many times HEAD has moved, which a commit that's made and undone counts in too.
    fn moves_of_head(dir: &Path) -> usize {
        run(dir, &["reflog"]).lines().count()
    }

    /// Whether `refusal` is the check's before the commit, which no commit came before.
    fn refused_before_commit(refusal: &anyhow::Error) -> bool {
        refusal.to_string().starts_with(BEFORE_COMMIT.refused)
    }

    /// A hook script that writes a key to `deploy.env` and stages it.
    fn stage_a_key() -> String {
        format!(
            "echo 'aws {}' > deploy.env\ngit add deploy.env\n",
            aws_key()
        )
    }

    /// The files staged in `dir`.
    fn staged(dir: &Path) -> Vec<String> {
        let names = run(dir, &["diff", "--cached", "--name-only"]);
        names.lines().map(str::to_owned).collect()
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn passes_on_the_commit_arguments_it_knows() {
        let accepted = args(&[
            "-s",
            "--signoff",
            "--no-signoff",
            "-n",
            "--no-verify",
            "--verify",
            "-S",
            "-SABC123",
            "-S-a",
            "--gpg-sign",
            "--gpg-sign=ABC123",
            "--no-gpg-sign",
            "--author=A <a@b.c>",
            "--date=now",
            "--trailer=Fixes: 1",
            "--cleanup=strip",
            "-q",
            "-v",
            "--allow-empty",
            "--no-post-rewrite",
        ]);
        assert!(refuse_unknown_commit_args(&accepted).is_ok());
        assert!(refuse_unknown_commit_args(&[]).is_ok());
    }

    /// git takes abbreviations and bundled short options, so anything not on the list is
    /// refused, with the list.
    #[test]
    fn refuses_any_other_commit_argument() {
        for refused in [
            "--inc",
            "-sa",
            "--include",
            "--all",
            "-a",
            "--amend",
            "-C",
            "HEAD",
            "-p",
            "x.txt",
            "--pathspec-from-file=x",
            "--author",
            "--author=",
            "--gpg-sign=",
            "--signoffs",
            "--",
        ] {
            let error = refuse_unknown_commit_args(&args(&["-s", refused])).unwrap_err();
            let message = error.to_string();
            assert!(message.contains(&format!("{refused:?}")), "{message}");
            assert!(message.contains("--no-verify, --verify"), "{message}");
        }
    }

    #[test]
    fn refuses_a_commit_argument_it_does_not_know_before_committing() {
        let (dir, repo) = isolated_repo();
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir.path(), &["add", "lib.rs"]);
        let moves = moves_of_head(dir.path());
        let git_args = ["--amend".to_owned()];
        let new_commit = NewCommit {
            message: "fix: b",
            scope: Scope::Staged,
            git_args: &git_args,
            leave_out: &[],
        };
        let at = AtCommit {
            on_secrets: OnSecrets::Reject,
            before: &Scanned::default(),
        };

        let error = commit(&repo, &new_commit, at).unwrap_err();

        assert!(error.to_string().contains("\"--amend\""), "{error}");
        assert_eq!(moves_of_head(dir.path()), moves);
    }

    /// What's written or staged while the model and the review take their time is checked at
    /// the commit, which isn't made under reject.
    #[test]
    fn refuses_a_secret_written_or_staged_during_the_review() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let moves = moves_of_head(dir);
        fs::write(dir.join("lib.rs"), format!("// aws {}\n", aws_key())).unwrap();

        let written = commit_as(&repo, Scope::Tracked, "fix: a", OnSecrets::Reject).unwrap_err();

        assert!(
            written.to_string().contains("lib.rs:1  AWS access key"),
            "{written}"
        );
        fs::write(dir.join("lib.rs"), "fn b() {}\n").unwrap();
        fs::write(dir.join("deploy.env"), format!("aws {}\n", aws_key())).unwrap();
        run(dir, &["add", "deploy.env"]);
        let staged = commit_as(&repo, Scope::Tracked, "fix: b", OnSecrets::Reject).unwrap_err();
        assert!(
            staged.to_string().contains("deploy.env:1  AWS access key"),
            "{staged}"
        );
        assert!(refused_before_commit(&written) && refused_before_commit(&staged));
        assert_eq!(moves_of_head(dir), moves);
    }

    #[test]
    fn refuses_a_secret_typed_into_the_message() {
        let (dir, repo) = isolated_repo();
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir.path(), &["add", "lib.rs"]);
        let before = head(dir.path());
        let moves = moves_of_head(dir.path());
        let message = format!("fix: b\n\nThe key is {}.", aws_key());

        let error = commit_as(&repo, Scope::Staged, &message, OnSecrets::Reject).unwrap_err();

        assert!(
            error.to_string().contains("message  AWS access key"),
            "{error}"
        );
        assert!(refused_before_commit(&error), "{error}");
        assert_eq!(moves_of_head(dir.path()), moves);
        assert!(commit_as(&repo, Scope::Staged, "fix: b", OnSecrets::Reject).is_ok());
        assert_ne!(head(dir.path()), before);
    }

    /// A hook can stage more after the check before the commit; the commit it made is checked,
    /// and under reject undone, with its changes still staged.
    #[cfg(unix)]
    #[test]
    fn undoes_a_commit_a_hook_added_a_secret_to() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        hook(dir, "pre-commit", &stage_a_key());
        fs::write(dir.join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir, &["add", "lib.rs"]);
        let before = head(dir);

        let error = commit_as(&repo, Scope::Staged, "fix: b", OnSecrets::Reject).unwrap_err();

        let message = error.to_string();
        assert!(
            message.contains("deploy.env:1  AWS access key"),
            "{message}"
        );
        assert!(message.contains("undid commit"), "{message}");
        assert!(message.contains("still staged"), "{message}");
        assert_eq!(head(dir), before);
        assert_eq!(staged(dir), ["deploy.env", "lib.rs"]);
    }

    /// Under warn, the commit a hook added a secret to stays, and the warning names it.
    #[cfg(unix)]
    #[test]
    fn keeps_a_commit_a_hook_added_a_secret_to_when_set_to_warn() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        hook(dir, "pre-commit", &stage_a_key());
        fs::write(dir.join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir, &["add", "lib.rs"]);
        let before = head(dir);

        assert!(commit_as(&repo, Scope::Staged, "fix: b", OnSecrets::Warn).is_ok());

        assert_ne!(head(dir), before);
        let files = run(dir, &["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(files.lines().collect::<Vec<_>>(), ["deploy.env", "lib.rs"]);
    }

    /// On a branch without commits, undoing the first commit leaves it without commits again.
    #[cfg(unix)]
    #[test]
    fn undoes_a_first_commit_a_hook_added_a_secret_to() {
        let dir = tempfile::TempDir::new().unwrap();
        let dir = dir.path();
        run(dir, &["init", "-q", "--initial-branch", "main"]);
        crate::git::test_support::isolate(dir);
        let repo = Repo::open(dir).unwrap();
        hook(dir, "pre-commit", &stage_a_key());
        fs::write(dir.join("lib.rs"), "fn a() {}\n").unwrap();
        run(dir, &["add", "lib.rs"]);

        let error = commit_as(&repo, Scope::Staged, "feat: a", OnSecrets::Reject).unwrap_err();

        assert!(error.to_string().contains("undid commit"), "{error}");
        assert_eq!(repo.head_commit().unwrap(), None);
        assert_eq!(staged(dir), ["deploy.env", "lib.rs"]);
    }

    /// A commit HEAD moved on from, here by a hook that switches branches, isn't undone: undoing
    /// it would move a branch the user is no longer on.
    #[cfg(unix)]
    #[test]
    fn leaves_a_commit_alone_when_head_moved_after_it() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        hook(dir, "pre-commit", &stage_a_key());
        hook(dir, "post-commit", "git checkout -q -b other\n");
        fs::write(dir.join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir, &["add", "lib.rs"]);

        let error = commit_as(&repo, Scope::Staged, "fix: b", OnSecrets::Reject).unwrap_err();

        assert!(error.to_string().contains("didn't undo commit"), "{error}");
        assert!(error.to_string().contains("HEAD moved"), "{error}");
        let main = run(dir, &["rev-parse", "main"]);
        assert_eq!(run(dir, &["rev-parse", "other"]), main);
        assert_eq!(head(dir), main);
    }

    /// A merge adds what its parents don't have; what it brings in from the merged branch's
    /// history isn't its own, even a key that history added and took out again.
    #[test]
    fn commits_a_merge_whose_merged_history_held_a_key() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        run(dir, &["checkout", "-q", "-b", "feature"]);
        fs::write(dir.join("f.txt"), "f\n").unwrap();
        run(dir, &["add", "f.txt"]);
        run(dir, &["commit", "-q", "-m", "feature work"]);
        run(dir, &["checkout", "-q", "main"]);
        fs::write(dir.join("fixture.env"), format!("aws {}\n", aws_key())).unwrap();
        run(dir, &["add", "fixture.env"]);
        run(dir, &["commit", "-q", "-m", "add a fixture"]);
        run(dir, &["rm", "-q", "fixture.env"]);
        run(dir, &["commit", "-q", "-m", "drop the fixture"]);
        run(dir, &["checkout", "-q", "feature"]);
        run(dir, &["merge", "-q", "--no-edit", "--no-commit", "main"]);

        assert!(commit_as(&repo, Scope::Staged, "Merge main", OnSecrets::Reject).is_ok());

        let parents = repo.stored_parents(head(dir).trim()).unwrap();
        assert_eq!(parents.len(), 2);
    }

    /// Undoing a merge would lose its other parents, so it stays, and the refusal says so.
    #[cfg(unix)]
    #[test]
    fn leaves_a_merge_alone_that_a_hook_added_a_secret_to() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        run(dir, &["checkout", "-q", "-b", "feature"]);
        fs::write(dir.join("f.txt"), "f\n").unwrap();
        run(dir, &["add", "f.txt"]);
        run(dir, &["commit", "-q", "-m", "feature work"]);
        run(dir, &["checkout", "-q", "main"]);
        hook(dir, "pre-commit", &stage_a_key());
        run(
            dir,
            &[
                "merge",
                "-q",
                "--no-ff",
                "--no-edit",
                "--no-commit",
                "feature",
            ],
        );

        let error =
            commit_as(&repo, Scope::Staged, "Merge feature", OnSecrets::Reject).unwrap_err();

        assert!(error.to_string().contains("it's a merge"), "{error}");
        assert_eq!(repo.stored_parents(head(dir).trim()).unwrap().len(), 2);
    }

    /// What the user types into the arguments, such as a trailer, is checked before the commit.
    #[test]
    fn refuses_a_secret_typed_into_an_argument() {
        let (dir, repo) = isolated_repo();
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir.path(), &["add", "lib.rs"]);
        let moves = moves_of_head(dir.path());
        let git_args = [format!("--trailer=Token: {}", aws_key())];
        let new_commit = NewCommit {
            message: "fix: b",
            scope: Scope::Staged,
            git_args: &git_args,
            leave_out: &[],
        };
        let at = AtCommit {
            on_secrets: OnSecrets::Reject,
            before: &Scanned::default(),
        };

        let error = commit(&repo, &new_commit, at).unwrap_err();

        assert!(
            error.to_string().contains("arguments  AWS access key"),
            "{error}"
        );
        assert_eq!(moves_of_head(dir.path()), moves);
    }

    /// The commit HEAD is at, as `Made` records one, with `before` as the commit HEAD was at.
    fn made_at_head(repo: &Repo, before: Option<String>) -> Made {
        Made {
            hash: repo.head_commit().unwrap().unwrap(),
            before,
            branch: repo.head_branch().unwrap(),
        }
    }

    /// A commit whose parent isn't the commit HEAD was at, as when another one landed between,
    /// isn't undone: moving the branch back would drop that one too.
    #[test]
    fn leaves_a_commit_alone_whose_parent_is_not_the_old_head() {
        let (dir, repo) = isolated_repo();
        let first = head(dir.path()).trim().to_owned();
        run(
            dir.path(),
            &["commit", "-q", "--allow-empty", "-m", "landed meanwhile"],
        );
        let landed = head(dir.path()).trim().to_owned();
        run(dir.path(), &["commit", "-q", "--allow-empty", "-m", "made"]);

        let made = made_at_head(&repo, Some(first.clone()));
        let reason = why_not_undo(&repo, &made).unwrap_err();
        assert!(
            reason.contains("isn't the commit that was made on"),
            "{reason}"
        );
        assert!(why_not_undo(&repo, &made_at_head(&repo, Some(landed))).is_ok());
    }

    /// A check that can't read the commit made keeps it under warn, and under reject refuses;
    /// here the commit can't be tied to the old HEAD, so it stays and the refusal says why.
    #[test]
    fn keeps_a_commit_it_cannot_check_when_set_to_warn() {
        let (dir, repo) = isolated_repo();
        run(dir.path(), &["commit", "-q", "--allow-empty", "-m", "made"]);
        let missing = Some("1".repeat(40));
        let made = made_at_head(&repo, missing);
        let after = head(dir.path());

        assert!(check_made_commit(&repo, &made, OnSecrets::Warn, &Scanned::default()).is_ok());
        let error =
            check_made_commit(&repo, &made, OnSecrets::Reject, &Scanned::default()).unwrap_err();

        assert!(error.to_string().contains("didn't undo commit"), "{error}");
        assert_eq!(head(dir.path()), after);
    }
}
