//! Wrapper around the `git` command line.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Which changes the message describes, and therefore what gets committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The index, committed with `git commit`.
    Staged,
    /// Nothing is staged: all changes to tracked files, committed with `git commit --all`.
    Tracked,
}

#[derive(Debug)]
pub struct Changes {
    pub scope: Scope,
    /// `git diff --stat` for every changed file, including excluded ones.
    pub stat: String,
    /// The diff, without excluded files.
    pub diff: String,
}

pub struct Repo {
    /// Directory git commands run in.
    workdir: PathBuf,
}

impl Repo {
    pub fn open(workdir: impl Into<PathBuf>) -> Result<Self> {
        let repo = Self {
            workdir: workdir.into(),
        };
        let inside = repo.git(&["rev-parse", "--is-inside-work-tree"]);
        if !inside.is_ok_and(|out| out.trim() == "true") {
            bail!("not inside a git work tree");
        }
        Ok(repo)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command.current_dir(&self.workdir).args(args);
        command
    }

    /// Runs git and returns its stdout, failing with its stderr.
    fn git(&self, args: &[&str]) -> Result<String> {
        let output = self
            .command(args)
            .output()
            .context("failed to run git; is it installed?")?;
        if !output.status.success() {
            bail!(
                "`git {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn has_diff(&self, args: &[&str]) -> Result<bool> {
        let output = self
            .command(&["diff", "--quiet"])
            .args(args)
            .output()
            .context("failed to run git; is it installed?")?;
        match output.status.code() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => bail!(
                "`git diff` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
    }

    pub fn stage_all(&self) -> Result<()> {
        self.git(&["add", "--all"]).map(drop)
    }

    /// The changes to describe, or `None` if there are no staged or tracked changes.
    pub fn changes(&self, exclude: &[String]) -> Result<Option<Changes>> {
        let scope = if self.has_diff(&["--cached"])? {
            Scope::Staged
        } else if self.has_diff(&[])? {
            Scope::Tracked
        } else {
            return Ok(None);
        };

        let mut base = vec!["diff", "--no-color", "--no-ext-diff"];
        if scope == Scope::Staged {
            base.push("--cached");
        }
        let stat = self.git(&[base.as_slice(), &["--stat=120"]].concat())?;

        let excludes = exclude_pathspecs(exclude);
        let mut diff_args = base;
        diff_args.extend(["--", ":/"]);
        diff_args.extend(excludes.iter().map(String::as_str));
        let diff = self.git(&diff_args)?;

        Ok(Some(Changes { scope, stat, diff }))
    }

    /// Number of untracked, non-ignored paths (directories count once).
    pub fn untracked_count(&self) -> Result<usize> {
        let status = self.git(&["status", "--porcelain"])?;
        Ok(status.lines().filter(|line| line.starts_with("??")).count())
    }

    /// Path of a file inside the repository's git directory.
    pub fn git_path(&self, name: &str) -> Result<PathBuf> {
        let path = self.git(&["rev-parse", "--git-path", name])?;
        Ok(self.workdir.join(path.trim_end_matches(['\n', '\r'])))
    }

    /// Opens `path` in the editor git would use (GIT_EDITOR, core.editor, VISUAL, EDITOR).
    pub fn edit_file(&self, path: &Path) -> Result<()> {
        let editor = self.git(&["var", "GIT_EDITOR"])?.trim().to_owned();
        // Launched the way git does it, so editors configured with arguments work.
        let status = Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$@\""))
            .arg(&editor)
            .arg(path)
            .status()
            .with_context(|| format!("failed to start editor `{editor}`"))?;
        if !status.success() {
            bail!("editor `{editor}` exited with {status}");
        }
        Ok(())
    }

    /// Commits with `message`. If git fails (e.g. a hook rejects the commit),
    /// the message is left on disk so it isn't lost.
    pub fn commit(&self, message: &str, scope: Scope, extra_args: &[String]) -> Result<()> {
        let path = self.git_path("GIT_AGENT_MSG")?;
        fs::write(&path, format!("{message}\n"))
            .with_context(|| format!("failed to write {}", path.display()))?;

        let mut command = self.command(&["commit", "--file"]);
        command.arg(&path);
        if scope == Scope::Tracked {
            command.arg("--all");
        }
        let status = command
            .args(extra_args)
            .status()
            .context("failed to run git commit")?;
        if !status.success() {
            bail!(
                "git commit failed; the message was saved, reuse it with:\n  git commit --file {}",
                path.display()
            );
        }
        let _ = fs::remove_file(&path);
        Ok(())
    }
}

/// Pathspecs that leave `patterns` out of a diff, matched from the repository root.
fn exclude_pathspecs(patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .map(|pattern| {
            if pattern.contains('/') {
                format!(":(top,exclude,glob){pattern}")
            } else {
                format!(":(top,exclude,glob)**/{pattern}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Repo) {
        let dir = TempDir::new().unwrap();
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .output()
                .unwrap();
            assert!(status.status.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        // Keep the user's global settings (signing, hooks) out of the tests.
        run(&["config", "user.name", "Test"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "commit.gpgsign", "false"]);
        run(&["config", "core.hooksPath", "no-hooks"]);
        fs::write(dir.path().join("lib.rs"), "fn a() {}\n").unwrap();
        run(&["add", "lib.rs"]);
        run(&["commit", "-q", "-m", "init"]);
        let repo = Repo::open(dir.path()).unwrap();
        (dir, repo)
    }

    fn log(repo: &Repo) -> String {
        repo.git(&["log", "--format=%B", "-1"]).unwrap()
    }

    #[test]
    fn rejects_non_repository() {
        let dir = TempDir::new().unwrap();
        assert!(Repo::open(dir.path()).is_err());
    }

    #[test]
    fn no_changes() {
        let (_dir, repo) = setup();
        assert!(repo.changes(&[]).unwrap().is_none());
    }

    #[test]
    fn unstaged_changes_are_committed_with_all() {
        let (dir, repo) = setup();
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        fs::write(dir.path().join("new.rs"), "fn c() {}\n").unwrap();

        let changes = repo.changes(&[]).unwrap().unwrap();
        assert_eq!(changes.scope, Scope::Tracked);
        assert!(changes.diff.contains("+fn b() {}"));
        assert_eq!(repo.untracked_count().unwrap(), 1);

        repo.commit("fix: rename a to b", changes.scope, &[])
            .unwrap();
        assert_eq!(log(&repo).trim(), "fix: rename a to b");
        assert!(repo.changes(&[]).unwrap().is_none());
        assert!(!repo.git_path("GIT_AGENT_MSG").unwrap().exists());
    }

    #[test]
    fn staged_changes_take_priority() {
        let (dir, repo) = setup();
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        fs::write(dir.path().join("staged.rs"), "fn s() {}\n").unwrap();
        repo.git(&["add", "staged.rs"]).unwrap();

        let changes = repo.changes(&[]).unwrap().unwrap();
        assert_eq!(changes.scope, Scope::Staged);
        assert!(changes.diff.contains("staged.rs"));
        assert!(!changes.diff.contains("lib.rs"));
    }

    #[test]
    fn excluded_files_are_listed_but_not_diffed() {
        let (dir, repo) = setup();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/Cargo.lock"), "lock\n").unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.stage_all().unwrap();

        let changes = repo.changes(&["Cargo.lock".into()]).unwrap().unwrap();
        assert!(changes.stat.contains("sub/Cargo.lock"));
        assert!(!changes.diff.contains("Cargo.lock"));
        assert!(changes.diff.contains("main.rs"));
    }

    #[test]
    fn failed_commit_keeps_message() {
        let (dir, repo) = setup();
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        let err = repo
            .commit("feat: x", Scope::Tracked, &["--no-such-flag".into()])
            .unwrap_err();
        let saved = repo.git_path("GIT_AGENT_MSG").unwrap();
        assert!(err.to_string().contains("GIT_AGENT_MSG"));
        assert_eq!(fs::read_to_string(saved).unwrap(), "feat: x\n");
    }
}
