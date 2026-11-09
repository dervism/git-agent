//! Committing and pushing: the steps that record and publish what the user accepted.

use crate::config::OnSecrets;
use crate::git::{Repo, Scope};
use crate::secrets;
use crate::ui;
use anyhow::Result;

/// Commits `message` with the changes in `scope`, passing `git_args` on to `git commit` and
/// leaving the work-tree changes of `leave_out` out of a commit of all tracked changes.
pub fn commit(
    repo: &Repo,
    message: &str,
    scope: Scope,
    git_args: &[String],
    leave_out: &[String],
) -> Result<()> {
    repo.commit(message, scope, git_args, leave_out)?;
    ui::info(ui::green("✅ Committed."));
    Ok(())
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
    use crate::git::test_support::{add_remote, commit_elsewhere, isolated_repo, run};
    use std::fs;
    use std::path::Path;

    fn commit_key(dir: &Path) {
        fs::write(
            dir.join("deploy.env"),
            format!("aws AKIA{}\n", "Q".repeat(16)),
        )
        .unwrap();
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
}
