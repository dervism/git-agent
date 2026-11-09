//! Committing and pushing: the steps that record and publish what the user accepted.

use crate::git::{Repo, Scope};
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
/// (e.g. edits made on GitHub). With `ask`, the user confirms the rebase.
/// Returns whether it pushed: the user may decline the rebase.
pub fn push(repo: &Repo, ask: bool) -> Result<bool> {
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
    repo.push()?;
    ui::info(ui::green("✅ Pushed."));
    Ok(true)
}
