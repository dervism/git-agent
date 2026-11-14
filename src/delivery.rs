//! Committing and pushing: the steps that record and publish what the user accepted.

use crate::config::OnSecrets;
use crate::git::{CommitId, CommitMade, Destination, MovedRef, Repo, Scope, Tips};
use crate::secrets::{self, Checks, CommitsMade, NextCommit, Scanned, Wording};
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

/// What the check of the commit made found that the check before it didn't.
const MADE_SECRETS: &str = "the commit git made, or commits its hooks made, add what look like \
                            secrets that weren't there when git-agent checked the commit";

const MADE_COMMIT: Wording = Wording {
    refused: MADE_SECRETS,
    fix: "If they stay, take them out before you push them.",
    warned: MADE_SECRETS,
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
/// secrets is undone where that's safe. `before_model` is what the check before the model read.
/// The run's `checks` give the setting, and keep what the check of the commit made found.
pub fn commit(
    repo: &Repo,
    commit: &NewCommit,
    before_model: &Scanned,
    checks: &mut Checks,
) -> Result<()> {
    refuse_unknown_commit_args(commit.git_args)?;
    let arguments = commit.git_args.join(" ");
    let next = NextCommit {
        scope: commit.scope,
        message: commit.message,
        arguments: &arguments,
    };
    let checked = secrets::check_before_commit(repo, next, before_model, checks, &BEFORE_COMMIT)?;
    let before = Before {
        parent: repo.head_commit()?,
        branch: repo.head_branch()?,
        known: repo.every_tip()?,
    };
    repo.commit(
        commit.message,
        commit.scope,
        commit.git_args,
        commit.leave_out,
    )?;
    check_made_commit(repo, &before, &checked, checks)?;
    ui::info(ui::green("✅ Committed."));
    Ok(())
}

/// Where HEAD was, and what every ref pointed at, before a commit.
struct Before {
    /// The commit HEAD was at, or `None` on a branch without commits.
    parent: Option<String>,
    /// The branch HEAD was on, or `None` when it was detached.
    branch: Option<String>,
    /// Where HEAD and every ref pointed.
    known: Tips,
}

/// Checks what the commit git made adds, with any commits its hooks made: every commit HEAD and
/// the branches lead to now that nothing did `before`, after the check just before it read
/// `checked`, as the run's `checks` say. Under reject a check that fails undoes the commit where
/// that's safe.
fn check_made_commit(
    repo: &Repo,
    before: &Before,
    checked: &Scanned,
    checks: &mut Checks,
) -> Result<()> {
    let on_secrets = checks.on_secrets();
    let listed = repo
        .head_and_branch_tips()
        .and_then(|now| repo.commits_new_since(&before.known, &now));
    let made = match listed {
        Ok(made) => made,
        Err(failure) => return could_not_list_what_was_made(on_secrets, failure),
    };
    let at = CommitsMade {
        on: before.parent.as_deref(),
        commits: &made,
    };
    if at.made_on_parent().next().is_none() {
        ui::info(ui::dim(
            "   git-agent found the commit git made on no branch and not at HEAD, so it had \
             nothing of it to check.",
        ));
    }
    let Err(failure) = secrets::check_made_commit(repo, at, checked, checks, &MADE_COMMIT) else {
        return Ok(());
    };
    after_a_failed_check(on_secrets, failure)
        .map_err(|failure| anyhow!("{failure:#}\n{}", undo(repo, before, &made)))
}

/// What a check of the commit git made that failed comes to: under warn only a warning, since a
/// warning never fails the check; under reject `failure`.
fn after_a_failed_check(on_secrets: OnSecrets, failure: anyhow::Error) -> Result<()> {
    match on_secrets {
        OnSecrets::Warn => {
            ui::warn(format!("{failure:#}"));
            Ok(())
        }
        OnSecrets::Reject => Err(failure),
    }
}

/// What it comes to when git can't list what the commit made: as [`after_a_failed_check`] says,
/// with what to do under reject.
fn could_not_list_what_was_made(on_secrets: OnSecrets, failure: anyhow::Error) -> Result<()> {
    let failure = failure.context(secrets::UNCHECKED_COMMIT);
    after_a_failed_check(on_secrets, failure)
        .map_err(|failure| anyhow!("{failure:#}\nCheck it for secrets before you push it."))
}

/// Undoes the commit git made, if undoing it moves nothing else, as [`undo_target`] tells, and says
/// what became of it.
fn undo(repo: &Repo, before: &Before, made: &[CommitMade]) -> String {
    let commit = match undo_target(repo, before, made) {
        Ok(commit) => commit,
        Err(reason) => {
            return format!(
                "git-agent didn't undo the commit, since {reason}; take any secrets out of the \
                 commits it made before you push them."
            );
        }
    };
    let short = commit.short();
    match repo.undo_commit(
        commit.full(),
        before.parent.as_deref(),
        before.branch.as_deref(),
    ) {
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
        Err(error) => format!(
            "git-agent didn't undo commit {short}, since git couldn't move the branch back \
             ({error:#}); take the secrets out of it before you push it."
        ),
    }
}

/// The commit to undo: the one commit `made`, when it isn't a merge, was made on the commit HEAD
/// was at, is where HEAD is, on the branch HEAD was on, and nothing else holds it, as
/// [`ensure_nothing_else_holds`] tells. Why not, otherwise.
fn undo_target<'a>(
    repo: &Repo,
    before: &Before,
    made: &'a [CommitMade],
) -> Result<&'a CommitId, String> {
    let [commit] = made else {
        return Err("git and its hooks made more than one commit".to_owned());
    };
    if commit.parents.len() > 1 {
        return Err("it's a merge, and undoing it would lose its other parents".to_owned());
    }
    if !commit.made_on(before.parent.as_deref()) {
        return Err("it isn't made on the commit HEAD was at".to_owned());
    }
    let head = repo
        .head_commit()
        .map_err(|error| format!("git couldn't read HEAD ({error:#})"))?
        .ok_or("HEAD has no commit now")?;
    if head != commit.id.full() {
        return Err("HEAD isn't at it".to_owned());
    }
    let branch = repo
        .head_branch()
        .map_err(|error| format!("git couldn't read the branch HEAD is on ({error:#})"))?;
    if branch != before.branch {
        return Err("HEAD isn't on the branch it was on".to_owned());
    }
    ensure_nothing_else_holds(repo, before, commit.id.full())?;
    Ok(&commit.id)
}

/// Fails, saying why, when undoing `commit` could leave it in another ref, or let a merge take it
/// back: a merge started after it (`MERGE_HEAD`), or one of the refs that moved since `before`,
/// other than HEAD's branch, reaches it. A ref that didn't move can't, since it reaches only what it
/// did before the commit. With more moved refs than are worth asking about, any may; and it fails
/// when git can't read the refs or tell whether one holds the commit.
fn ensure_nothing_else_holds(repo: &Repo, before: &Before, commit: &str) -> Result<(), String> {
    const MOST_ASKED: usize = 20;
    let now = repo
        .every_tip()
        .map_err(|error| format!("git-agent couldn't read where the refs are ({error:#})"))?;
    if now.is_merging() {
        return Err(
            "a merge was started after it, and committing it may take it back in".to_owned(),
        );
    }
    let moved = now.refs_moved_since(&before.known, before.branch.as_deref());
    if moved.len() > MOST_ASKED {
        let names = names_of(&moved);
        return Err(format!("other refs moved too, and may hold it: {names}"));
    }
    let holders = moved
        .into_iter()
        .filter_map(|other| match repo.reaches(other.hash, commit) {
            Ok(true) => Some(Ok(other)),
            Ok(false) => None,
            Err(error) => Some(Err(format!(
                "git couldn't tell whether {} holds it ({error:#})",
                other.name
            ))),
        })
        .collect::<Result<Vec<_>, String>>()?;
    match holders.as_slice() {
        [] => Ok(()),
        holders => Err(format!("another ref holds it too: {}", names_of(holders))),
    }
}

/// The names of the first few of `refs`, and how many more there are.
fn names_of(refs: &[MovedRef]) -> String {
    const SHOWN: usize = 5;
    let shown: Vec<&str> = refs
        .iter()
        .take(SHOWN)
        .map(|moved| moved.name.as_str())
        .collect();
    match refs.len().saturating_sub(SHOWN) {
        0 => shown.join(", "),
        more => format!("{}, and {more} more", shown.join(", ")),
    }
}

/// Pushes the current branch where a bare `git push` would, and nothing else. The destination is
/// fetched first, so its branches are what it has now, and the branch is rebased onto commits
/// that reached its upstream from elsewhere (e.g. edits made on GitHub). Then the commits the push
/// would send to each place it goes to are checked for secrets as the run's `checks` say, and the
/// commit checked is the one pushed. With `ask`, the user confirms the rebase. Returns whether it
/// pushed: the user may decline the rebase.
pub fn push(repo: &Repo, ask: bool, checks: &Checks) -> Result<bool> {
    const SHOWN: usize = 5;

    let destination = repo.push_destination()?;
    repo.fetch_for_push(&destination)?;
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
    let commit = repo
        .head_commit()?
        .context("the branch has no commits to push")?;
    let published = repo.published_tips(&destination)?;
    secrets::check_outgoing(repo, &commit, &published, checks)?;
    repo.push_commit(&commit, &destination)?;
    if destination.sets_upstream() {
        track(repo, &destination);
    }
    ui::info(ui::green("✅ Pushed."));
    Ok(true)
}

/// Makes the branch just pushed to `destination` for the first time track it. The push has gone
/// by then, so a failure is only a warning.
fn track(repo: &Repo, destination: &Destination) {
    let tracked = format!("{}/{}", destination.remote, destination.branch);
    match repo.set_upstream(destination) {
        Ok(()) => ui::info(format!(
            "Branch {} set up to track {tracked}.",
            destination.local()
        )),
        Err(error) => ui::warn(format!(
            "pushed, but git-agent couldn't make {} track {tracked}: {error:#}",
            destination.local()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::git::test_support::hook;
    use crate::git::test_support::{
        add_remote, commit_elsewhere, isolate, isolated_repo, run, short_head, tips_of,
    };
    use crate::secrets::contents::MOST_BINARY_BYTES;
    use crate::secrets::test_support::{aws_key, other_aws_key, push_listing};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    /// An empty bare repository, to push to.
    fn bare() -> TempDir {
        let remote = TempDir::new().unwrap();
        run(
            remote.path(),
            &["init", "--bare", "-q", "--initial-branch", "main"],
        );
        remote
    }

    fn branches(remote: &Path) -> String {
        run(remote, &["branch", "--list"])
    }

    fn refused_for_the_key(error: &anyhow::Error) {
        let shown = error.to_string();
        assert!(shown.contains("deploy.env:1  AWS access key"), "{shown}");
    }

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

        let error = push(&repo, false, &Checks::new(OnSecrets::Reject)).unwrap_err();

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

        let error = push(&repo, false, &Checks::new(OnSecrets::Reject)).unwrap_err();

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

    /// A commit another remote has is checked before a push to this one: having reached an
    /// internal remote doesn't make it public on the one pushed to.
    #[test]
    fn checks_commits_another_remote_has_before_a_push_to_this_one() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let public = add_remote(&repo);
        let before = remote_head(public.path());
        let internal = bare();
        run(
            dir,
            &[
                "remote",
                "add",
                "internal",
                internal.path().to_str().unwrap(),
            ],
        );
        commit_key(dir);
        run(dir, &["push", "-q", "internal", "HEAD:refs/heads/main"]);

        let error = push(&repo, false, &Checks::new(OnSecrets::Reject)).unwrap_err();

        refused_for_the_key(&error);
        assert_eq!(remote_head(public.path()), before);
    }

    /// A branch deleted on the remote no longer counts as published, though its remote-tracking
    /// ref is still here: a new branch built on it would send its commits, so they're checked.
    #[test]
    fn checks_the_commits_of_a_branch_deleted_on_the_remote() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let remote = add_remote(&repo);
        run(dir, &["switch", "-q", "-c", "old"]);
        commit_key(dir);
        run(dir, &["push", "-q", "origin", "old"]);
        run(remote.path(), &["branch", "-q", "-D", "old"]);
        run(dir, &["switch", "-q", "-c", "new"]);

        let error = push(&repo, false, &Checks::new(OnSecrets::Reject)).unwrap_err();

        refused_for_the_key(&error);
        assert!(!branches(remote.path()).contains("new"));
    }

    /// A first push makes the branch track where it went, as `git push --set-upstream` does.
    #[test]
    fn a_first_push_makes_the_branch_track_where_it_went() {
        let (dir, repo) = isolated_repo();
        let remote = bare();
        let at = remote.path().to_str().unwrap();
        run(dir.path(), &["remote", "add", "origin", at]);

        assert!(push(&repo, false, &Checks::new(OnSecrets::Reject)).unwrap());

        assert!(branches(remote.path()).contains("main"));
        let upstream = run(dir.path(), &["rev-parse", "--abbrev-ref", "@{upstream}"]);
        assert_eq!(upstream.trim(), "origin/main");
    }

    /// The push has gone by the time the branch is made to track it, so a failure there is only
    /// a warning, and the push counts.
    #[test]
    fn a_first_push_counts_though_the_branch_cannot_be_made_to_track_it() {
        let (dir, repo) = isolated_repo();
        let remote = bare();
        let at = remote.path().to_str().unwrap();
        run(dir.path(), &["remote", "add", "origin", at]);
        fs::write(dir.path().join(".git/config.lock"), "").unwrap();

        assert!(push(&repo, false, &Checks::new(OnSecrets::Reject)).unwrap());

        assert!(branches(remote.path()).contains("main"));
        assert!(repo.push_destination().unwrap().sets_upstream());
    }

    /// A first push to a new remote checks the whole history, the root commit's diff included,
    /// whatever `log.showRoot` says.
    #[test]
    fn checks_the_whole_history_on_a_first_push_to_a_new_remote() {
        let dir = TempDir::new().unwrap();
        let dir = dir.path();
        run(dir, &["init", "-q", "--initial-branch", "main"]);
        isolate(dir);
        commit_key(dir);
        fs::write(dir.join("lib.rs"), "fn a() {}\n").unwrap();
        run(dir, &["add", "lib.rs"]);
        run(dir, &["commit", "-q", "-m", "add lib.rs"]);
        run(dir, &["config", "log.showRoot", "false"]);
        let remote = bare();
        run(
            dir,
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        let repo = Repo::open(dir).unwrap();

        let error = push(&repo, false, &Checks::new(OnSecrets::Reject)).unwrap_err();

        refused_for_the_key(&error);
        assert!(branches(remote.path()).is_empty());
    }

    /// A key on the fetched remote's branch is published there, not at a push URL the fetch
    /// doesn't come from, which is asked what it has: the key is checked before it goes there.
    #[test]
    fn checks_what_goes_to_a_push_url_the_fetch_doesnt_come_from() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let _internal = add_remote(&repo);
        commit_key(dir);
        run(dir, &["push", "-q", "origin", "main"]);
        let public = bare();
        let public_path = public.path().to_str().unwrap();
        run(dir, &["config", "remote.origin.pushurl", public_path]);

        let error = push(&repo, false, &Checks::new(OnSecrets::Reject)).unwrap_err();

        refused_for_the_key(&error);
        assert!(branches(public.path()).is_empty());
    }

    /// A remote with a second URL pushes to it too, so what goes there is checked against what it
    /// has, not against the first URL's branches.
    #[test]
    fn checks_what_goes_to_each_url_of_a_remote() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let _first = add_remote(&repo);
        commit_key(dir);
        run(dir, &["push", "-q", "origin", "main"]);
        let second = bare();
        let second_path = second.path().to_str().unwrap();
        run(dir, &["config", "--add", "remote.origin.url", second_path]);

        let error = push(&repo, false, &Checks::new(OnSecrets::Reject)).unwrap_err();

        refused_for_the_key(&error);
        assert!(branches(second.path()).is_empty());
    }

    /// A narrowed fetch refspec keeps the ref of a branch deleted on the remote, which a pruned
    /// fetch can't drop, so the remote is asked what it has, and a new branch built on that one
    /// has its commits checked.
    #[test]
    fn checks_a_deleted_branch_whose_ref_a_narrowed_fetch_keeps() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let remote = add_remote(&repo);
        run(dir, &["switch", "-q", "-c", "old"]);
        commit_key(dir);
        run(dir, &["push", "-q", "origin", "old"]);
        run(remote.path(), &["branch", "-q", "-D", "old"]);
        let narrow = "+refs/heads/main:refs/remotes/origin/main";
        run(dir, &["config", "remote.origin.fetch", narrow]);
        run(dir, &["switch", "-q", "-c", "new"]);

        let error = push(&repo, false, &Checks::new(OnSecrets::Reject)).unwrap_err();

        refused_for_the_key(&error);
        assert!(!branches(remote.path()).contains("new"));
    }

    /// A fork's branch is rebased onto what came in on its upstream, which is fetched from the
    /// upstream's remote, and goes to the fork, leaving the upstream alone.
    #[test]
    fn rebases_a_fork_s_branch_onto_its_upstream_and_pushes_it_to_the_fork() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let origin = add_remote(&repo);
        let fork = bare();
        run(
            dir,
            &["remote", "add", "fork", fork.path().to_str().unwrap()],
        );
        run(dir, &["config", "branch.main.pushRemote", "fork"]);
        commit_elsewhere(&origin, "notes.txt", "notes\n", "edit on GitHub");
        fs::write(dir.join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir, &["commit", "-q", "-am", "feat: b"]);

        assert!(push(&repo, false, &Checks::new(OnSecrets::Reject)).unwrap());

        let on_fork = run(fork.path(), &["log", "--format=%s", "-2", "main"]);
        assert_eq!(on_fork, "feat: b\nedit on GitHub\n");
        let on_origin = run(origin.path(), &["log", "--format=%s", "-1", "main"]);
        assert_eq!(on_origin, "edit on GitHub\n");
    }

    /// The pruned fetch before a push leaves local tags alone, whatever `fetch.pruneTags` says.
    #[test]
    fn keeps_local_tags_whatever_fetch_prune_tags_says() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let _remote = add_remote(&repo);
        run(dir, &["config", "fetch.pruneTags", "true"]);
        run(dir, &["tag", "local-only"]);

        assert!(push(&repo, false, &Checks::new(OnSecrets::Reject)).unwrap());

        assert_eq!(run(dir, &["tag"]), "local-only\n");
    }

    #[test]
    fn pushes_a_secret_after_a_warning_when_set_to_warn() {
        let (dir, repo) = isolated_repo();
        let remote = add_remote(&repo);
        commit_key(dir.path());

        assert!(push(&repo, false, &Checks::new(OnSecrets::Warn)).unwrap());

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
        commit(
            repo,
            &new_commit,
            &Scanned::default(),
            &mut Checks::new(on_secrets),
        )
    }

    /// How many parents the commit HEAD is at has.
    fn parents_of_head(dir: &Path) -> usize {
        run(dir, &["rev-list", "--parents", "-n", "1", "HEAD"])
            .split_whitespace()
            .count()
            - 1
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
        let mut checks = Checks::new(OnSecrets::Reject);

        let error = commit(&repo, &new_commit, &Scanned::default(), &mut checks).unwrap_err();

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

        assert!(
            error.to_string().contains("didn't undo the commit"),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("HEAD isn't on the branch it was on"),
            "{error}"
        );
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

        assert_eq!(parents_of_head(dir), 2);
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
        assert_eq!(parents_of_head(dir), 2);
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
        let mut checks = Checks::new(OnSecrets::Reject);

        let error = commit(&repo, &new_commit, &Scanned::default(), &mut checks).unwrap_err();

        assert!(
            error.to_string().contains("arguments  AWS access key"),
            "{error}"
        );
        assert_eq!(moves_of_head(dir.path()), moves);
    }

    /// What the check before the model reads of the staged changes, under warn.
    fn read_before_the_model(repo: &Repo) -> Scanned {
        secrets::check_changes(repo, Scope::Staged, OnSecrets::Warn, &[], &BEFORE_COMMIT).unwrap()
    }

    /// Commits what's staged with `message` under warn, after the check before the model read
    /// `before`, and returns the run's checks.
    fn commit_after(repo: &Repo, message: &str, before: &Scanned) -> Checks {
        let new_commit = NewCommit {
            message,
            scope: Scope::Staged,
            git_args: &[],
            leave_out: &[],
        };
        let mut checks = Checks::new(OnSecrets::Warn);
        commit(repo, &new_commit, before, &mut checks).unwrap();
        checks
    }

    /// What a push to the remote warns about, in a run whose `checks` came before.
    fn warnings_of_a_push(repo: &Repo, checks: &Checks) -> Vec<String> {
        let said = ui::test_support::listen();
        assert!(push(repo, false, checks).unwrap());
        said.warnings()
    }

    /// The warning of a push that lists the key in `place`.
    fn push_lists(place: &str) -> String {
        push_listing(&[format!("   {place}  AWS access key")])
    }

    /// The push leaves out what the commit made added, and lists the key an older commit added
    /// to the same file.
    #[test]
    fn leaves_out_at_the_push_what_the_commit_made_held() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let _remote = add_remote(&repo);
        let older_key = format!("aws {}\n", other_aws_key());
        fs::write(dir.join("deploy.env"), &older_key).unwrap();
        run(dir, &["add", "deploy.env"]);
        run(dir, &["commit", "-q", "-m", "add a key"]);
        let older = short_head(dir);
        let both = format!("{older_key}aws {}\n", aws_key());
        fs::write(dir.join("deploy.env"), both).unwrap();
        run(dir, &["add", "deploy.env"]);
        let before = read_before_the_model(&repo);

        let checks = commit_after(&repo, "feat: deploy", &before);

        let shown = push_lists(&format!("commit {older} deploy.env:1"));
        assert_eq!(warnings_of_a_push(&repo, &checks), [shown]);
    }

    /// A key the user took out during the review isn't in the commit made, so the push lists an
    /// older commit's copy of it.
    #[test]
    fn lists_at_the_push_an_older_commits_key_the_user_took_out_before_the_commit() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let _remote = add_remote(&repo);
        let key = format!("aws {}\n", aws_key());
        fs::write(dir.join("a.env"), &key).unwrap();
        run(dir, &["add", "a.env"]);
        run(dir, &["commit", "-q", "-m", "add a key"]);
        let older = short_head(dir);
        fs::write(dir.join("a.env"), "plain\n").unwrap();
        run(dir, &["commit", "-q", "-am", "take it out"]);
        fs::write(dir.join("a.env"), &key).unwrap();
        run(dir, &["add", "a.env"]);
        let before = read_before_the_model(&repo);
        fs::write(dir.join("a.env"), "plain\n").unwrap();
        fs::write(dir.join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir, &["add", "a.env", "lib.rs"]);

        let checks = commit_after(&repo, "feat: b", &before);

        let shown = push_lists(&format!("commit {older} a.env:1"));
        assert_eq!(warnings_of_a_push(&repo, &checks), [shown]);
    }

    /// A hook that makes a commit after the one made doesn't get what was checked before the
    /// commit: the check of the commits made lists the secret in its message.
    #[cfg(unix)]
    #[test]
    fn lists_what_a_commit_a_hook_makes_after_the_one_made_adds() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let again =
            "[ -e .git/hooked ] || { touch .git/hooked; git commit -q --allow-empty -C HEAD; }";
        hook(dir, "post-commit", again);
        fs::write(dir.join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir, &["add", "lib.rs"]);
        let typed = format!("fix: b\n\naws {}", aws_key());

        let said = ui::test_support::listen();
        commit_after(&repo, &typed, &Scanned::default());

        let second = short_head(dir);
        let in_message = format!("{}:\n   message  AWS access key", BEFORE_COMMIT.warned);
        let in_second = format!(
            "{}:\n   commit {second} message  AWS access key",
            MADE_COMMIT.warned
        );
        assert_eq!(said.warnings(), [in_message, in_second]);
    }

    /// A hook that resets the branch after the commit, or leaves HEAD and every branch without a
    /// commit, leaves nothing made to check: the commit goes through clean, under either setting,
    /// and git-agent says it found nothing of it to check.
    #[cfg(unix)]
    #[test]
    fn lets_a_commit_through_that_hooks_leave_nothing_to_check() {
        let hooks = [
            "git reset -q --soft HEAD~1",
            "git checkout -q --orphan gone && git branch -q -D main",
        ];
        for (post_commit, on_secrets) in hooks
            .into_iter()
            .flat_map(|hook| [OnSecrets::Reject, OnSecrets::Warn].map(|setting| (hook, setting)))
        {
            let (dir, repo) = isolated_repo();
            let dir = dir.path();
            hook(dir, "post-commit", post_commit);
            fs::write(dir.join("lib.rs"), "fn b() {}\n").unwrap();
            run(dir, &["add", "lib.rs"]);

            let said = ui::test_support::listen();
            let committed = commit_as(&repo, Scope::Staged, "fix: b", on_secrets);

            assert!(
                committed.is_ok(),
                "{post_commit} {on_secrets:?}: {committed:?}"
            );
            let warned = said.warnings();
            assert_eq!(warned, Vec::<String>::new(), "{post_commit} {on_secrets:?}");
            let nowhere = "on no branch and not at HEAD";
            let told = said.lines().iter().any(|line| line.contains(nowhere));
            assert!(told, "{post_commit} {on_secrets:?}: {:?}", said.lines());
        }
    }

    /// A hook that moves HEAD to another branch after the commit doesn't make that branch's commit
    /// one the commit made, since the branch held it before: the check reads only what's new, and
    /// the push of the other branch lists the other commit's key.
    #[cfg(unix)]
    #[test]
    fn checks_the_commit_made_on_its_branch_when_a_hook_moves_head() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let _remote = add_remote(&repo);
        let key = format!("aws {}\n", aws_key());
        run(dir, &["switch", "-q", "-c", "other"]);
        fs::write(dir.join("a.env"), &key).unwrap();
        run(dir, &["add", "a.env"]);
        run(dir, &["commit", "-q", "-m", "add a key elsewhere"]);
        let elsewhere = short_head(dir);
        run(dir, &["switch", "-q", "main"]);
        hook(dir, "post-commit", "git checkout -q other");
        fs::write(dir.join("a.env"), &key).unwrap();
        run(dir, &["add", "a.env"]);
        let before = read_before_the_model(&repo);

        let checks = commit_after(&repo, "feat: add the key", &before);

        assert_eq!(run(dir, &["branch", "--show-current"]).trim(), "other");
        let shown = push_lists(&format!("commit {elsewhere} a.env:1"));
        assert_eq!(warnings_of_a_push(&repo, &checks), [shown]);
    }

    /// What a commit came to, with hooks.
    #[cfg(unix)]
    struct Hooked {
        result: Result<()>,
        warnings: Vec<String>,
        /// The short hash of the commit whose message is `holder`.
        holder: String,
    }

    /// Commits `fix: b` under `on_secrets` with a pre-commit and a post-commit hook, either of
    /// which can be empty. Before it, the branch `other` is at HEAD, and `older` has a commit of
    /// its own made on HEAD.
    #[cfg(unix)]
    fn commit_with_hooks(
        pre_commit: &str,
        post_commit: &str,
        holder: &str,
        on_secrets: OnSecrets,
    ) -> Hooked {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        run(dir, &["branch", "other"]);
        run(dir, &["switch", "-q", "-c", "older"]);
        run(dir, &["commit", "-q", "--allow-empty", "-m", "on older"]);
        run(dir, &["switch", "-q", "main"]);
        for (name, script) in [("pre-commit", pre_commit), ("post-commit", post_commit)] {
            if !script.is_empty() {
                hook(dir, name, script);
            }
        }
        fs::write(dir.join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir, &["add", "lib.rs"]);
        let said = ui::test_support::listen();
        let result = commit_as(&repo, Scope::Staged, "fix: b", on_secrets);
        let holder = run(
            dir,
            &["log", "--all", "--format=%h", "-F", "--grep", holder],
        );
        Hooked {
            result,
            warnings: said.warnings(),
            holder: holder.trim().to_owned(),
        }
    }

    /// A hook script that runs once, though the commits it makes run hooks too.
    #[cfg(unix)]
    const ONCE: &str = "[ -e .git/hooked ] && exit 0\ntouch .git/hooked\n";

    /// Hooks that move HEAD before the commit: to another branch or a detached HEAD, after a commit
    /// of their own too, and back after the commit; as pre-commit and post-commit scripts, with the
    /// message of the commit that holds the key, which the pre-commit hook stages.
    #[cfg(unix)]
    fn hooks_that_move_head_before_the_commit() -> Vec<(String, String, &'static str)> {
        let key = stage_a_key();
        let first = |move_head: &str, post_commit: &str| {
            (
                format!("{move_head}\n{key}"),
                post_commit.to_owned(),
                "fix: b",
            )
        };
        vec![
            first("git checkout -q other", ""),
            first("git checkout -q --detach", ""),
            first(
                &format!(
                    "{ONCE}git commit -q --no-verify --allow-empty -m first\n\
                     git checkout -q -b side HEAD~1"
                ),
                "",
            ),
            first("git checkout -q other", "git checkout -q main"),
            first(
                "git checkout -q -b x",
                &format!(
                    "{ONCE}git checkout -q main\ngit commit -q --no-verify --allow-empty -m e"
                ),
            ),
        ]
    }

    /// Hooks that rename, delete or copy the branch after the commit, switch to a branch with a
    /// commit of its own, detach HEAD or leave it on a branch without commits, or make commits and
    /// merges of their own, as [`hooks_that_move_head_before_the_commit`] gives them.
    #[cfg(unix)]
    fn hooks_that_move_head_or_the_branches_after() -> Vec<(String, String, &'static str)> {
        let key = stage_a_key();
        let after = |script: &str| (key.clone(), script.to_owned(), "fix: b");
        let of_their_own =
            |script: &str, holder| (String::new(), format!("{ONCE}{script}"), holder);
        vec![
            after("git branch -m renamed"),
            after("git checkout -q -b elsewhere && git branch -q -D main"),
            after("git checkout -q older"),
            after("git branch -q backup"),
            after("git checkout -q --detach"),
            after("git checkout -q --orphan elsewhere"),
            of_their_own(
                &format!("git checkout -q -b hooks\n{key}git commit -q --no-verify -m hooks"),
                "hooks",
            ),
            of_their_own(
                &format!(
                    "git checkout -q -b hooks\n{key}git commit -q --no-verify -m hooks\n\
                     git checkout -q main"
                ),
                "hooks",
            ),
            of_their_own(
                &format!(
                    "git checkout -q -b side HEAD~1\n{key}git commit -q --no-verify -m side\n\
                     git checkout -q main\ngit merge -q --no-ff --no-verify -m merged side\n\
                     git branch -q -D side"
                ),
                "side",
            ),
        ]
    }

    /// Wherever hooks leave HEAD and the branches, the commits they lead to are checked:
    /// under reject the key a hook added is refused, and the commit stays; under warn it's listed
    /// in the commit that holds it.
    #[cfg(unix)]
    #[test]
    fn checks_the_commits_made_where_hooks_leave_head_and_the_branches() {
        let listed = |holder: &str| format!("commit {holder} deploy.env:1  AWS access key");
        let mut hooks = hooks_that_move_head_before_the_commit();
        hooks.extend(hooks_that_move_head_or_the_branches_after());
        for (pre, post, holder) in &hooks {
            let refused = commit_with_hooks(pre, post, holder, OnSecrets::Reject);
            let refusal = refused.result.unwrap_err().to_string();
            assert!(
                refusal.contains(&listed(&refused.holder)),
                "{pre} {post}: {refusal}"
            );
            assert!(refusal.contains("didn't undo"), "{pre} {post}: {refusal}");
            assert!(
                refused.warnings.is_empty(),
                "{pre} {post}: {:?}",
                refused.warnings
            );

            let warned = commit_with_hooks(pre, post, holder, OnSecrets::Warn);
            assert!(warned.result.is_ok(), "{pre} {post}: {:?}", warned.result);
            let shown = format!("{}:\n   {}", MADE_COMMIT.warned, listed(&warned.holder));
            assert_eq!(warned.warnings, [shown], "{pre} {post}");
        }
    }

    /// History that a ref other than a branch held before the commit, such as a remote-tracking
    /// branch, isn't counted as made when a hook puts it on a branch: the clean commit goes
    /// through, under either setting.
    #[cfg(unix)]
    #[test]
    fn doesnt_count_history_another_ref_held_as_made() {
        for on_secrets in [OnSecrets::Reject, OnSecrets::Warn] {
            let (dir, repo) = isolated_repo();
            let dir = dir.path();
            run(dir, &["switch", "-q", "-c", "fetched"]);
            fs::write(dir.join("remote.env"), format!("aws {}\n", aws_key())).unwrap();
            run(dir, &["add", "remote.env"]);
            run(dir, &["commit", "-q", "-m", "upstream work"]);
            run(dir, &["update-ref", "refs/remotes/upstream/main", "HEAD"]);
            run(dir, &["switch", "-q", "main"]);
            run(dir, &["branch", "-q", "-D", "fetched"]);
            hook(dir, "post-commit", "git branch -q copy upstream/main");
            fs::write(dir.join("lib.rs"), "fn b() {}\n").unwrap();
            run(dir, &["add", "lib.rs"]);

            let said = ui::test_support::listen();
            let committed = commit_as(&repo, Scope::Staged, "fix: b", on_secrets);

            assert!(committed.is_ok(), "{on_secrets:?}: {committed:?}");
            assert_eq!(said.warnings(), Vec::<String>::new(), "{on_secrets:?}");
        }
    }

    /// A merge takes in what `git commit` was asked to merge, though no ref holds it, as after a
    /// fetch of a URL: that history isn't the commit's, so a key it added and took out again is
    /// neither refused nor listed, under either setting.
    #[test]
    fn doesnt_count_what_a_merge_takes_in_as_made() {
        for on_secrets in [OnSecrets::Reject, OnSecrets::Warn] {
            let (dir, repo) = isolated_repo();
            let dir = dir.path();
            run(dir, &["switch", "-q", "-c", "fetched"]);
            fs::write(dir.join("fixture.env"), format!("aws {}\n", aws_key())).unwrap();
            run(dir, &["add", "fixture.env"]);
            run(dir, &["commit", "-q", "-m", "add a fixture"]);
            run(dir, &["rm", "-q", "fixture.env"]);
            run(dir, &["commit", "-q", "-m", "take it out"]);
            let fetched = run(dir, &["rev-parse", "HEAD"]).trim().to_owned();
            run(dir, &["switch", "-q", "main"]);
            run(dir, &["branch", "-q", "-D", "fetched"]);
            fs::write(dir.join("lib.rs"), "fn b() {}\n").unwrap();
            run(dir, &["commit", "-q", "-am", "work on main"]);
            run(dir, &["merge", "-q", "--no-ff", "--no-commit", &fetched]);

            let said = ui::test_support::listen();
            let committed = commit_as(&repo, Scope::Staged, "Merge the fetched work", on_secrets);

            assert!(committed.is_ok(), "{on_secrets:?}: {committed:?}");
            assert_eq!(said.warnings(), Vec::<String>::new(), "{on_secrets:?}");
            assert_eq!(parents_of_head(dir), 2);
        }
    }

    /// Post-commit hooks that move refs, with what undo comes to under reject: a ref that can't
    /// hold the commit doesn't stop it, such as a note about it or tags of older commits; one git
    /// can't follow to a commit does, such as a tag of a file or a tree, which may hold what the
    /// commit added, and so do more moved refs than are worth asking about, and a merge started
    /// after the commit, which may take it back in.
    #[cfg(unix)]
    fn hooks_and_what_undo_comes_to() -> Vec<(String, &'static str)> {
        let blob = "$(echo data | git hash-object -w --stdin)";
        let tags = |count: u32| format!("for n in $(seq {count}); do git tag t$n HEAD~1; done");
        vec![
            ("git notes add -m checked".to_owned(), "undid commit"),
            (tags(20), "undid commit"),
            (
                tags(21),
                "other refs moved too, and may hold it: refs/tags/t1, refs/tags/t10, refs/tags/t11, \
                 refs/tags/t12, refs/tags/t13, and 16 more",
            ),
            (
                format!("git tag blob {blob}"),
                "git couldn't tell whether refs/tags/blob holds it (error: object",
            ),
            (
                "git tag snap 'HEAD^{tree}'".to_owned(),
                "git couldn't tell whether refs/tags/snap holds it",
            ),
            (
                format!("git tag -a -m data blobtag {blob}"),
                "git couldn't tell whether refs/tags/blobtag holds it",
            ),
            (
                "git rev-parse HEAD > .git/MERGE_HEAD".to_owned(),
                "a merge was started after it, and committing it may take it back in",
            ),
        ]
    }

    /// Under reject, a commit a hook added a key to is undone unless something else may hold it.
    #[cfg(unix)]
    #[test]
    fn undoes_a_commit_unless_something_else_may_hold_it() {
        for (post_commit, wanted) in hooks_and_what_undo_comes_to() {
            let (dir, repo) = isolated_repo();
            let dir = dir.path();
            hook(dir, "pre-commit", &stage_a_key());
            hook(dir, "post-commit", &post_commit);
            fs::write(dir.join("lib.rs"), "fn b() {}\n").unwrap();
            run(dir, &["add", "lib.rs"]);

            let refused = commit_as(&repo, Scope::Staged, "fix: b", OnSecrets::Reject);

            let refusal = refused.unwrap_err().to_string();
            assert!(refusal.contains(wanted), "{post_commit}: {refusal}");
        }
    }

    /// A key the user took out during the review, which a hook stages again, is listed by the
    /// check of the commit made: the check just before the commit didn't read it.
    #[cfg(unix)]
    #[test]
    fn lists_a_key_a_hook_stages_again_after_the_user_took_it_out() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let key = format!("aws {}\n", aws_key());
        fs::write(dir.join("a.env"), &key).unwrap();
        run(dir, &["add", "a.env"]);
        let before = read_before_the_model(&repo);
        fs::write(dir.join("a.env"), "plain\n").unwrap();
        run(dir, &["add", "a.env"]);
        let again = format!("printf '%s' '{key}' > a.env && git add a.env");
        hook(dir, "pre-commit", &again);

        let said = ui::test_support::listen();
        commit_after(&repo, "feat: settings", &before);

        let made = short_head(dir);
        let listed = format!(
            "{}:\n   commit {made} a.env:1  AWS access key",
            MADE_COMMIT.warned
        );
        assert_eq!(said.warnings(), [listed]);
    }

    /// A key in a trailer is listed once, at the arguments: the commit's message holds it because
    /// the trailer put it there.
    #[test]
    fn lists_a_key_in_a_trailer_once() {
        let (dir, repo) = isolated_repo();
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir.path(), &["add", "lib.rs"]);
        let git_args = [format!("--trailer=Token: {}", aws_key())];
        let new_commit = NewCommit {
            message: "fix: b",
            scope: Scope::Staged,
            git_args: &git_args,
            leave_out: &[],
        };
        let mut checks = Checks::new(OnSecrets::Warn);

        let said = ui::test_support::listen();
        commit(&repo, &new_commit, &Scanned::default(), &mut checks).unwrap();

        let listed = format!("{}:\n   arguments  AWS access key", BEFORE_COMMIT.warned);
        assert_eq!(said.warnings(), [listed]);
        let message = run(dir.path(), &["log", "-1", "--format=%B"]);
        assert!(message.contains(&aws_key()), "{message}");
    }

    /// A binary file too large to check is listed once in a run of commit and push: at the check
    /// before the model, and not again at the commit or the push.
    #[test]
    fn lists_a_binary_file_too_large_to_check_once_in_a_commit_and_its_push() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let _remote = add_remote(&repo);
        let mut bytes = b"\0data\0".to_vec();
        bytes.resize(usize::try_from(MOST_BINARY_BYTES).unwrap() + 1, 0);
        fs::write(dir.join("big.bin"), bytes).unwrap();
        run(dir, &["add", "big.bin"]);

        let said = ui::test_support::listen();
        let before = read_before_the_model(&repo);
        let checks = commit_after(&repo, "feat: data", &before);
        let listed = said.warnings();
        drop(said);

        assert_eq!(listed.len(), 1, "{listed:?}");
        assert!(listed[0].contains("big.bin  (16.0 MiB)"), "{listed:?}");
        assert_eq!(warnings_of_a_push(&repo, &checks), Vec::<String>::new());
    }

    /// When git can't list what the commit made, warn says git-agent couldn't check it, and reject
    /// fails, saying so and what to do.
    #[test]
    fn says_it_couldnt_check_a_commit_whose_commits_it_couldnt_list() {
        let said = ui::test_support::listen();
        let warned = could_not_list_what_was_made(OnSecrets::Warn, anyhow!("no refs"));
        assert!(warned.is_ok());
        assert_eq!(
            said.warnings(),
            ["git-agent couldn't check the commit git made: no refs"]
        );
        drop(said);
        let refused = could_not_list_what_was_made(OnSecrets::Reject, anyhow!("no refs"));
        assert_eq!(
            format!("{:#}", refused.unwrap_err()),
            "git-agent couldn't check the commit git made: no refs\nCheck it for secrets before you \
             push it."
        );
    }

    /// Where HEAD was before a commit made on `parent`, from the branch HEAD is on, with every ref
    /// at `known`.
    fn before_at(repo: &Repo, parent: Option<String>, known: Tips) -> Before {
        Before {
            parent,
            branch: repo.head_branch().unwrap(),
            known,
        }
    }

    /// The commits HEAD and the branches lead to that `known` didn't.
    fn made_since(repo: &Repo, known: &Tips) -> Vec<CommitMade> {
        let now = repo.head_and_branch_tips().unwrap();
        repo.commits_new_since(known, &now).unwrap()
    }

    /// A commit is undone only when it's the one commit made, on the commit HEAD was at, at HEAD
    /// on the same branch, and no other ref that moved meanwhile holds it: undoing it then moves
    /// nothing else.
    #[test]
    fn undoes_only_the_one_commit_made_on_the_old_head() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let first = head(dir).trim().to_owned();
        run(
            dir,
            &["commit", "-q", "--allow-empty", "-m", "landed meanwhile"],
        );
        let landed = head(dir).trim().to_owned();
        run(dir, &["commit", "-q", "--allow-empty", "-m", "made"]);
        let after_landed = tips_of(&[&landed]);
        let reason = |before: &Before, made: &[CommitMade]| {
            undo_target(&repo, before, made).map(|commit| commit.full().to_owned())
        };

        let made = made_since(&repo, &after_landed);
        let on_first = before_at(&repo, Some(first.clone()), tips_of(&[&landed]));
        let on_landed = before_at(&repo, Some(landed.clone()), tips_of(&[&landed]));
        assert_eq!(reason(&on_landed, &made), Ok(head(dir).trim().to_owned()));
        let not_on_it = reason(&on_first, &made).unwrap_err();
        assert!(
            not_on_it.contains("isn't made on the commit HEAD was at"),
            "{not_on_it}"
        );
        let both = made_since(&repo, &tips_of(&[&first]));
        let two = reason(&on_landed, &both).unwrap_err();
        assert!(two.contains("more than one commit"), "{two}");
        let not_at_head = reason(&on_first, &both[1..]).unwrap_err();
        assert!(not_at_head.contains("HEAD isn't at it"), "{not_at_head}");
        run(dir, &["tag", "earlier", &first]);
        assert_eq!(reason(&on_landed, &made), Ok(head(dir).trim().to_owned()));
        run(dir, &["tag", "kept"]);
        let held = reason(&on_landed, &made).unwrap_err();
        assert_eq!(held, "another ref holds it too: refs/tags/kept");
    }

    /// A check that can't list what the commit made keeps it under warn, and under reject refuses
    /// and leaves it, since it can't tell what undoing would move; here the commit's own object is
    /// unreadable.
    #[test]
    fn keeps_a_commit_whose_commits_it_cannot_list() {
        let (dir, repo) = isolated_repo();
        let first = head(dir.path()).trim().to_owned();
        run(dir.path(), &["commit", "-q", "--allow-empty", "-m", "made"]);
        let made = head(dir.path()).trim().to_owned();
        let object = dir
            .path()
            .join(".git/objects")
            .join(&made[..2])
            .join(&made[2..]);
        fs::remove_file(&object).unwrap();
        fs::write(&object, "not a git object").unwrap();
        let before = before_at(&repo, Some(first.clone()), tips_of(&[&first]));

        let said = ui::test_support::listen();
        let mut warning = Checks::new(OnSecrets::Warn);
        assert!(check_made_commit(&repo, &before, &Scanned::default(), &mut warning).is_ok());
        let warned = said.warnings();
        assert!(
            warned[0].starts_with("git-agent couldn't check the commit git made"),
            "{warned:?}"
        );
        drop(said);
        let mut rejecting = Checks::new(OnSecrets::Reject);
        let error =
            check_made_commit(&repo, &before, &Scanned::default(), &mut rejecting).unwrap_err();

        assert!(
            error.to_string().contains("Check it for secrets"),
            "{error}"
        );
        assert_eq!(run(dir.path(), &["rev-parse", "main"]).trim(), made);
    }

    /// A ref at an object the repository lacks, as in a damaged repository, holds nothing, so the
    /// check of the commit made reads it as ever: under reject the key a hook staged is refused, and
    /// the commit undone.
    #[cfg(unix)]
    #[test]
    fn checks_the_commit_made_though_a_ref_points_at_a_missing_object() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        fs::create_dir_all(dir.join(".git/refs/keep")).unwrap();
        fs::write(
            dir.join(".git/refs/keep/missing"),
            format!("{}\n", "1".repeat(40)),
        )
        .unwrap();
        hook(dir, "pre-commit", &stage_a_key());
        fs::write(dir.join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir, &["add", "lib.rs"]);

        let refused = commit_as(&repo, Scope::Staged, "fix: b", OnSecrets::Reject);

        let refusal = refused.unwrap_err().to_string();
        assert!(
            refusal.contains("deploy.env:1  AWS access key"),
            "{refusal}"
        );
        assert!(refusal.contains("undid commit"), "{refusal}");
    }
}
