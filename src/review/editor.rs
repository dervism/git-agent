//! Editing a proposal in the user's editor.

use crate::git::{self, Repo};
use crate::safe_text;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

const EDIT_HELP: &str = "\
# Edit the commit message above. Lines starting with '#' are ignored.
# Save and close the editor to return to git-agent.
# An empty message keeps the previous one.
";

/// An HTML comment, since lines starting with '#' are headings in Markdown.
const PULL_REQUEST_EDIT_HELP: &str = "\
<!-- Edit the pull request above: the first line is the title, the rest is the
description. Save and close the editor to return to git-agent. An empty file
keeps the previous version. -->
";

/// Lets the user edit `message` in their editor. Returns `None` if they emptied it.
pub fn edit_message(repo: &Repo, message: &str) -> Result<Option<String>> {
    // COMMIT_EDITMSG is git's own scratch file, so editors apply commit message highlighting.
    let path = repo.git_path("COMMIT_EDITMSG")?;
    let edited = edit(&path, &format!("{message}\n\n{EDIT_HELP}"))?;
    let edited = strip_comments(&edited);
    Ok((!edited.is_empty()).then_some(edited))
}

/// Lets the user edit a plan in their editor, in the file `name` in the git
/// directory, and returns what they saved.
pub fn edit_plan(repo: &Repo, name: &str, text: &str) -> Result<String> {
    edit(&repo.git_path(name)?, text)
}

/// Lets the user edit a pull request's title and description in their editor.
/// Returns `None` if they emptied it.
pub fn edit_pull_request(repo: &Repo, text: &str) -> Result<Option<String>> {
    // A .md file, so editors apply Markdown highlighting.
    let path = repo.git_path("GIT_AGENT_PR_EDITMSG.md")?;
    let edited = edit(&path, &format!("{text}\n\n{PULL_REQUEST_EDIT_HELP}"))?;
    let edited = strip_edit_help(&edited);
    Ok((!edited.is_empty()).then_some(edited))
}

/// Writes `text` to `path`, opens it in the user's editor, and returns what they saved.
fn edit(path: &Path, text: &str) -> Result<String> {
    fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))?;
    git::edit_file(path)?;
    fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))
}

/// The edited pull request without the help comment, even if the user changed its end.
fn strip_edit_help(text: &str) -> String {
    let start = PULL_REQUEST_EDIT_HELP.lines().next().unwrap_or_default();
    let kept = text.find(start).map_or(text, |index| &text[..index]);
    safe_text::normalize(kept)
}

fn strip_comments(text: &str) -> String {
    let kept: Vec<&str> = text.lines().filter(|line| !line.starts_with('#')).collect();
    safe_text::normalize(&kept.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_comment_lines_and_padding() {
        let edited = format!("fix: thing  \n\nbody line\n\n{EDIT_HELP}");
        assert_eq!(strip_comments(&edited), "fix: thing\n\nbody line");
        assert_eq!(strip_comments(EDIT_HELP), "");
    }

    #[test]
    fn keeps_markdown_headings_but_strips_the_pull_request_help() {
        let edited = format!("feat: x\n\nWhy.\n\n## Changes\n- a\n\n{PULL_REQUEST_EDIT_HELP}");
        assert_eq!(
            strip_edit_help(&edited),
            "feat: x\n\nWhy.\n\n## Changes\n- a"
        );
        assert_eq!(strip_edit_help(PULL_REQUEST_EDIT_HELP), "");
    }
}
