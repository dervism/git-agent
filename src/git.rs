//! Wrapper around the `git` command line.

use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    /// `git diff --stat` for every changed file, including excluded ones.
    pub stat: String,
    /// The diff, without excluded files.
    pub diff: String,
}

/// A commit on a branch.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub hash: String,
    pub author: String,
    /// The author date, e.g. 2025-10-02.
    pub date: String,
    /// The subject, then the body indented: "- subject\n  body".
    pub message: String,
}

/// The short form of a commit, for naming it.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub hash: String,
    pub short: String,
    pub author: String,
    /// The author date, e.g. 2025-10-02.
    pub date: String,
    pub subject: String,
}

/// A `git pull` that changed the current branch, from the branch's reflog.
#[derive(Debug, Clone, PartialEq)]
pub struct Pull {
    /// The branch before the pull.
    pub before: String,
    /// The branch right after it.
    pub after: String,
    /// When it happened, e.g. "2 hours ago".
    pub when: String,
}

/// A commit's message and patch, as pushing it would publish them.
#[derive(Debug)]
pub struct CommitPatch {
    pub hash: String,
    pub message: String,
    pub patch: String,
}

/// git's built-in diff drivers for common languages, by file extension. With
/// them, `git blame -L :name` and `git log -L` find functions inside classes
/// and impl blocks, and hunk headers name the enclosing function.
const FUNCTION_DRIVERS: &str = "\
*.rs diff=rust
*.py diff=python
*.go diff=golang
*.java diff=java
*.kt diff=kotlin
*.kts diff=kotlin
*.cs diff=csharp
*.c diff=cpp
*.h diff=cpp
*.cc diff=cpp
*.cpp diff=cpp
*.cxx diff=cpp
*.hpp diff=cpp
*.m diff=objc
*.mm diff=objc
*.rb diff=ruby
*.php diff=php
*.pl diff=perl
*.pm diff=perl
*.sh diff=bash
*.bash diff=bash
*.ex diff=elixir
*.exs diff=elixir
*.css diff=css
*.html diff=html
*.md diff=markdown
*.tex diff=tex
";

pub struct Repo {
    /// Directory git commands run in.
    workdir: PathBuf,
    /// An index file to use instead of the repository's own.
    index: Option<PathBuf>,
    /// An attributes file to use instead of the user's own.
    attributes: Option<PathBuf>,
}

impl Repo {
    pub fn open(workdir: impl Into<PathBuf>) -> Result<Self> {
        let repo = Self {
            workdir: workdir.into(),
            index: None,
            attributes: None,
        };
        let inside = repo.git(&["rev-parse", "--is-inside-work-tree"]);
        if !inside.is_ok_and(|out| out.trim() == "true") {
            bail!("not inside a git work tree");
        }
        Ok(repo)
    }

    /// The same repository with the index file at `index`, so commits can be
    /// built without touching the real index.
    pub fn with_index(&self, index: PathBuf) -> Repo {
        Repo {
            workdir: self.workdir.clone(),
            index: Some(index),
            attributes: self.attributes.clone(),
        }
    }

    /// The same repository with the attributes file at `attributes` in place
    /// of the user's own (see [`Repo::write_function_attributes`]).
    pub fn with_attributes(&self, attributes: PathBuf) -> Repo {
        Repo {
            workdir: self.workdir.clone(),
            index: self.index.clone(),
            attributes: Some(attributes),
        }
    }

    /// Writes an attributes file to `path` that sets git's built-in diff
    /// drivers for common languages, followed by the user's own attributes,
    /// which take precedence, as do the repository's `.gitattributes`.
    pub fn write_function_attributes(&self, path: &Path) -> Result<()> {
        let mut text = FUNCTION_DRIVERS.to_owned();
        if let Some(own) = self.user_attributes()
            && let Ok(own) = fs::read_to_string(own)
        {
            text.push_str(&own);
        }
        fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
    }

    /// The user's attributes file: `core.attributesFile`, else git's default.
    fn user_attributes(&self) -> Option<PathBuf> {
        if let Ok(path) = self.git(&["config", "--path", "--get", "core.attributesFile"]) {
            return Some(PathBuf::from(path.trim()));
        }
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))?;
        Some(config.join("git/attributes"))
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command.current_dir(&self.workdir);
        if let Some(attributes) = &self.attributes {
            command
                .arg("-c")
                .arg(format!("core.attributesFile={}", attributes.display()));
        }
        command.args(args);
        if let Some(index) = &self.index {
            command.env("GIT_INDEX_FILE", index);
        }
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

    /// The changes to describe and how to commit them, or `None` if there are
    /// no staged or tracked changes.
    pub fn changes(&self, exclude: &[String]) -> Result<Option<(Scope, Changes)>> {
        let scope = if self.has_diff(&["--cached"])? {
            Scope::Staged
        } else if self.has_diff(&[])? {
            Scope::Tracked
        } else {
            return Ok(None);
        };
        let range: &[&str] = match scope {
            Scope::Staged => &["--cached"],
            Scope::Tracked => &[],
        };
        Ok(Some((scope, self.diff(range, exclude)?)))
    }

    /// The changes in `scope` as a patch that `git apply` can replay: with
    /// binary files, and with fixed prefixes and context, whatever the user's
    /// diff settings are.
    pub fn patch(&self, scope: Scope) -> Result<String> {
        let mut args = vec![
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "--unified=3",
            "--find-renames",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ];
        if scope == Scope::Staged {
            args.push("--cached");
        }
        self.git(&args)
    }

    /// The paths in `scope` whose diffs aren't left out by `exclude`. A
    /// renamed file is named by its new path.
    pub fn included_paths(&self, scope: Scope, exclude: &[String]) -> Result<HashSet<String>> {
        let excludes = exclude_pathspecs(exclude);
        let mut args = vec!["diff", "--name-only", "--find-renames", "-z"];
        if scope == Scope::Staged {
            args.push("--cached");
        }
        args.extend(["--", ":/"]);
        args.extend(excludes.iter().map(String::as_str));
        let names = self.git(&args)?;
        Ok(names
            .split('\0')
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Replaces the index with the tree of `tree_ish`, e.g. `HEAD`.
    pub fn read_tree(&self, tree_ish: &str) -> Result<()> {
        self.git(&["read-tree", tree_ish]).map(drop)
    }

    /// Applies `patch` to the index, leaving the work tree alone.
    pub fn apply_to_index(&self, patch: &str) -> Result<()> {
        let mut child = self
            .command(&["apply", "--cached", "--whitespace=nowarn", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to run git apply")?;
        // Dropping stdin closes it, so git sees the end of the patch.
        let written = child
            .stdin
            .take()
            .context("git apply has no input")?
            .write_all(patch.as_bytes());
        let output = child
            .wait_with_output()
            .context("failed to wait for git apply")?;
        if !output.status.success() {
            bail!(
                "`git apply --cached` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        written.context("failed to send the patch to git apply")
    }

    /// Resets the index to HEAD, leaving the work tree alone.
    pub fn reset_index(&self) -> Result<()> {
        self.git(&["reset", "--quiet"]).map(drop)
    }

    pub fn has_staged_changes(&self) -> Result<bool> {
        self.has_diff(&["--cached"])
    }

    /// The merge, rebase, cherry-pick or revert that is in progress, if any.
    pub fn operation_in_progress(&self) -> Result<Option<&'static str>> {
        const OPERATIONS: [(&str, &str); 5] = [
            ("MERGE_HEAD", "a merge"),
            ("rebase-merge", "a rebase"),
            ("rebase-apply", "a rebase"),
            ("CHERRY_PICK_HEAD", "a cherry-pick"),
            ("REVERT_HEAD", "a revert"),
        ];
        for (name, operation) in OPERATIONS {
            if self.git_path(name)?.exists() {
                return Ok(Some(operation));
            }
        }
        Ok(None)
    }

    /// The last `count` commits as `<hash> <subject>` lines, newest first.
    pub fn recent_commits(&self, count: usize) -> Result<Vec<String>> {
        let log = self.git(&["log", "--format=%h %s", &format!("--max-count={count}")])?;
        Ok(log.lines().map(str::to_owned).collect())
    }

    /// The changes HEAD makes since it branched off `base`.
    pub fn branch_changes(&self, base: &str, exclude: &[String]) -> Result<Changes> {
        self.diff(&[&format!("{base}...HEAD")], exclude)
    }

    /// The changes `git diff` shows with `range` (e.g. `--cached`), without the
    /// diffs of files matching `exclude`.
    fn diff(&self, range: &[&str], exclude: &[String]) -> Result<Changes> {
        let base = ["diff", "--no-color", "--no-ext-diff"];
        let stat = self.git(&[&base[..], range, &["--stat=120"]].concat())?;

        let excludes = exclude_pathspecs(exclude);
        // A deleted file's old content says little about the change; its name is enough.
        let mut diff_args = [&base[..], range, &["--irreversible-delete", "--", ":/"]].concat();
        diff_args.extend(excludes.iter().map(String::as_str));
        let diff = self.git(&diff_args)?;

        Ok(Changes { stat, diff })
    }

    /// The commits in `range` (`git log` arguments, e.g. `main..HEAD`) with
    /// their patches, without the diffs of files matching `exclude`.
    pub fn commit_patches(&self, range: &[&str], exclude: &[String]) -> Result<Vec<CommitPatch>> {
        let excludes = exclude_pathspecs(exclude);
        let mut args = vec![
            "log",
            "--patch",
            "--no-color",
            "--no-ext-diff",
            "--irreversible-delete",
            "--format=%x1e%h%x1f%B%x1f",
        ];
        args.extend_from_slice(range);
        args.extend(["--", ":/"]);
        args.extend(excludes.iter().map(String::as_str));
        let log = self.git(&args)?;
        let commits = log.split('\x1e').filter_map(|record| {
            let mut parts = record.splitn(3, '\x1f');
            let hash = parts.next()?.trim();
            (!hash.is_empty()).then(|| CommitPatch {
                hash: hash.to_owned(),
                message: parts.next().unwrap_or_default().trim().to_owned(),
                patch: parts.next().unwrap_or_default().to_owned(),
            })
        });
        Ok(commits.collect())
    }

    /// The commits a push of HEAD would send: those no remote-tracking branch has.
    pub fn outgoing_patches(&self, exclude: &[String]) -> Result<Vec<CommitPatch>> {
        self.commit_patches(&["HEAD", "--not", "--remotes"], exclude)
    }

    /// The commits on HEAD that `base` lacks, oldest first, without merges.
    pub fn commits_since(&self, base: &str) -> Result<Vec<Commit>> {
        self.commits(&[&format!("{base}..HEAD")])
    }

    /// The commits `git log` lists for `revisions` (e.g. `main..HEAD`), oldest
    /// first, without merges.
    pub fn commits(&self, revisions: &[&str]) -> Result<Vec<Commit>> {
        let mut args = vec![
            "log",
            "--reverse",
            "--no-merges",
            "--format=%x1e%h%n%an%n%as%n%s%n%b",
        ];
        args.extend_from_slice(revisions);
        args.push("--");
        let log = self.git(&args)?;
        let commits = log.split('\x1e').filter_map(|record| {
            let mut lines = record.trim().lines();
            let hash = lines.next().filter(|hash| !hash.is_empty())?;
            let author = lines.next().unwrap_or_default();
            let date = lines.next().unwrap_or_default();
            let mut message = format!("- {}", lines.next().unwrap_or_default());
            for line in lines {
                message.push('\n');
                if !line.trim().is_empty() {
                    message.push_str("  ");
                    message.push_str(line.trim_end());
                }
            }
            Some(Commit {
                hash: hash.to_owned(),
                author: author.to_owned(),
                date: date.to_owned(),
                message: message.trim_end().to_owned(),
            })
        });
        Ok(commits.collect())
    }

    /// The changes between two commits, or what `git diff` shows for a range
    /// such as `main...feature`, without the diffs of files matching `exclude`.
    pub fn changes_between(&self, revisions: &[&str], exclude: &[String]) -> Result<Changes> {
        self.diff(revisions, exclude)
    }

    /// The full hash of the commit `revision` names, or `None` if it names none.
    pub fn resolve(&self, revision: &str) -> Result<Option<String>> {
        if revision.starts_with('-') {
            return Ok(None);
        }
        let output = self
            .command(&["rev-parse", "--verify", "--quiet"])
            .arg(format!("{revision}^{{commit}}"))
            .output()
            .context("failed to run git; is it installed?")?;
        let hash = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Ok((output.status.success() && !hash.is_empty()).then_some(hash))
    }

    /// The parents of `commit`: none for a first commit, two or more for a merge.
    pub fn parents(&self, commit: &str) -> Result<Vec<String>> {
        let line = self.git(&["rev-list", "--parents", "--max-count=1", commit])?;
        Ok(line.split_whitespace().skip(1).map(str::to_owned).collect())
    }

    /// The hash of the empty tree, which a first commit is compared with.
    pub fn empty_tree(&self) -> Result<String> {
        // `output()` gives git an empty stdin.
        let hash = self.git(&["hash-object", "-t", "tree", "--stdin"])?;
        Ok(hash.trim().to_owned())
    }

    /// The short hash, author, date and subject of each of `hashes`.
    pub fn summaries(&self, hashes: &[&str]) -> Result<Vec<Summary>> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec![
            "show",
            "--no-patch",
            "--format=%H%x1f%h%x1f%an%x1f%as%x1f%s",
        ];
        args.extend_from_slice(hashes);
        args.push("--");
        let shown = self.git(&args)?;
        let summaries = shown.lines().filter_map(|line| {
            let mut parts = line.split('\x1f');
            let mut next = || parts.next().unwrap_or_default().to_owned();
            let summary = Summary {
                hash: next(),
                short: next(),
                author: next(),
                date: next(),
                subject: next(),
            };
            (!summary.hash.is_empty()).then_some(summary)
        });
        Ok(summaries.collect())
    }

    /// Whether `branch` is a local branch.
    pub fn is_branch(&self, branch: &str) -> bool {
        self.git(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .is_ok()
    }

    /// The branch new branches most likely start from: the remote's default
    /// branch (e.g. `origin/main`), else `main` or `master`.
    pub fn default_base(&self) -> Result<String> {
        let remote = self.remote().or_else(|_| self.push_remote()).ok();
        let mut candidates = Vec::new();
        if let Some(remote) = &remote {
            let head = format!("refs/remotes/{remote}/HEAD");
            if let Ok(name) = self.git(&["symbolic-ref", "--quiet", "--short", &head]) {
                return Ok(name.trim().to_owned());
            }
            candidates.extend([format!("{remote}/main"), format!("{remote}/master")]);
        }
        candidates.extend(["main".to_owned(), "master".to_owned()]);
        candidates
            .into_iter()
            .find(|candidate| self.resolve(candidate).is_ok_and(|hash| hash.is_some()))
            .context("could not tell which branch to compare with; name it with --base")
    }

    /// The last `git pull` that changed `branch`, if its reflog has one.
    pub fn last_pull(&self, branch: &str) -> Result<Option<Pull>> {
        let reflog = self.git(&[
            "log",
            "--walk-reflogs",
            "--date=relative",
            "--format=%H%x1f%gd%x1f%gs",
            &format!("refs/heads/{branch}"),
            "--",
        ])?;
        let entries: Vec<Vec<&str>> = reflog
            .lines()
            .map(|line| line.split('\x1f').collect())
            .collect();
        let Some(index) = entries.iter().position(|entry| {
            entry
                .get(2)
                .is_some_and(|action| action.starts_with("pull"))
        }) else {
            return Ok(None);
        };
        let Some(before) = entries.get(index + 1) else {
            // The pull created the branch, so there is nothing to compare with.
            return Ok(None);
        };
        let selector = entries[index].get(1).copied().unwrap_or_default();
        let when = selector
            .split_once("@{")
            .map_or("", |(_, rest)| rest.trim_end_matches('}'));
        Ok(Some(Pull {
            before: before[0].to_owned(),
            after: entries[index][0].to_owned(),
            when: when.to_owned(),
        }))
    }

    /// Whether HEAD has a file at `path`, which may be absolute or relative
    /// to the directory git runs in.
    pub fn in_head(&self, path: &str) -> bool {
        self.git(&["ls-tree", "--name-only", "HEAD", "--", path])
            .is_ok_and(|listed| !listed.trim().is_empty())
    }

    /// `git blame --porcelain` for the lines `range` (a `-L` value such as
    /// `120,140` or `:name`) of `path` as it is in the work tree. Whitespace
    /// changes and moved or copied lines don't count as changes.
    pub fn blame(&self, path: &str, range: &str) -> Result<String> {
        self.git(&[
            "blame",
            "--porcelain",
            "-w",
            "-M",
            "-C",
            "-L",
            range,
            "--",
            path,
        ])
    }

    /// The lines of `path` that differ from HEAD, as `git diff --unified=0` hunks.
    pub fn uncommitted_hunks(&self, path: &str) -> Result<String> {
        self.git(&[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--unified=0",
            "HEAD",
            "--",
            path,
        ])
    }

    /// The newest `limit` commits that changed the lines `range` of `path` at
    /// HEAD (`git log -L`), each followed by its diff of those lines.
    pub fn line_history(&self, path: &str, range: &str, limit: usize) -> Result<String> {
        self.git(&[
            "log",
            "--no-color",
            "--no-ext-diff",
            &format!("--max-count={limit}"),
            "--format=%x1e%h%x1f%an%x1f%as%x1f%B%x1f",
            &format!("-L{range}:{path}"),
        ])
    }

    /// The checked-out branch.
    pub fn current_branch(&self) -> Result<String> {
        let name = self
            .git(&["symbolic-ref", "--quiet", "--short", "HEAD"])
            .map_err(|_| anyhow!("HEAD is detached; check out a branch first"))?;
        Ok(name.trim().to_owned())
    }

    /// The remote the current branch pulls from, else the one it would push to.
    pub fn remote(&self) -> Result<String> {
        let branch = self.current_branch()?;
        match self.git(&["config", &format!("branch.{branch}.remote")]) {
            Ok(remote) if remote.trim() != "." => Ok(remote.trim().to_owned()),
            _ => self.push_remote(),
        }
    }

    /// Fetches `branch` from `remote` and returns its remote-tracking ref, e.g.
    /// `origin/main`, so HEAD can be compared with the remote's latest version.
    pub fn fetch_branch(&self, remote: &str, branch: &str) -> Result<String> {
        let tracking = format!("{remote}/{branch}");
        let refspec = format!("+refs/heads/{branch}:refs/remotes/{tracking}");
        // Uses the terminal so git can show errors and ask for credentials.
        let status = self
            .command(&["fetch", "--quiet", remote, &refspec])
            .status()
            .context("failed to run git fetch")?;
        if !status.success() {
            bail!("`git fetch {remote} {branch}` failed ({status})");
        }
        Ok(tracking)
    }

    /// Whether the current branch has no upstream, or commits its upstream lacks.
    pub fn has_unpushed_commits(&self) -> Result<bool> {
        if !self.has_upstream() {
            return Ok(true);
        }
        let ahead = self.git(&["rev-list", "--max-count=1", "@{upstream}..HEAD"])?;
        Ok(!ahead.trim().is_empty())
    }

    /// Whether tracked files have staged or unstaged changes.
    pub fn has_uncommitted_changes(&self) -> Result<bool> {
        Ok(self.has_diff(&["--cached"])? || self.has_diff(&[])?)
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
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

    /// Pushes the current branch. A branch without an upstream is pushed to the
    /// default remote and its upstream is set, like `git push --set-upstream`.
    pub fn push(&self) -> Result<()> {
        let mut command = self.command(&["push"]);
        if !self.has_upstream() {
            let remote = self.push_remote()?;
            command.args(["--set-upstream", remote.as_str(), "HEAD"]);
        }
        let status = command.status().context("failed to run git push")?;
        if !status.success() {
            bail!("`git push` failed ({status})");
        }
        Ok(())
    }

    /// Fetches the upstream and returns the commits on it that the current branch
    /// lacks, as `<hash> <subject>` lines. Empty if the branch has no upstream.
    pub fn incoming_commits(&self) -> Result<Vec<String>> {
        if !self.has_upstream() {
            return Ok(Vec::new());
        }
        // Uses the terminal so git can show errors and ask for credentials.
        let status = self
            .command(&["fetch", "--quiet"])
            .status()
            .context("failed to run git fetch")?;
        if !status.success() {
            bail!("`git fetch` failed ({status})");
        }
        let log = self.git(&["log", "--format=%h %s", "HEAD..@{upstream}"])?;
        Ok(log.lines().map(str::to_owned).collect())
    }

    /// Replays local commits on top of the upstream, setting uncommitted changes
    /// aside meanwhile. If that fails the rebase is aborted, leaving the branch as it was.
    pub fn rebase_onto_upstream(&self) -> Result<()> {
        let output = self
            .command(&["rebase", "--autostash", "@{upstream}"])
            .output()
            .context("failed to run git rebase")?;
        if output.status.success() {
            return Ok(());
        }
        let _ = self.command(&["rebase", "--abort"]).output();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let conflicts: Vec<&str> = stdout
            .lines()
            .chain(stderr.lines())
            .filter(|line| line.starts_with("CONFLICT"))
            .collect();
        if conflicts.is_empty() {
            bail!("`git rebase` failed: {}", stderr.trim());
        }
        bail!(
            "your changes conflict with the new remote commits, so the rebase was cancelled:\n  {}\n\
             Run `git pull --rebase`, resolve the conflicts, then `git push`",
            conflicts.join("\n  ")
        );
    }

    fn has_upstream(&self) -> bool {
        self.git(&[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ])
        .is_ok()
    }

    /// `remote.pushDefault` if set, else the only remote, else `origin`.
    fn push_remote(&self) -> Result<String> {
        if let Ok(remote) = self.git(&["config", "remote.pushDefault"]) {
            return Ok(remote.trim().to_owned());
        }
        let remotes = self.git(&["remote"])?;
        let remotes: Vec<&str> = remotes.lines().collect();
        match remotes.as_slice() {
            [] => bail!(
                "there is no remote to push to; add one with `git remote add origin <url>`, \
                 or create a GitHub repository with `gh repo create --source . --push`"
            ),
            [remote] => Ok(remote.to_string()),
            _ if remotes.contains(&"origin") => Ok("origin".to_owned()),
            _ => bail!(
                "the branch has no upstream and there are several remotes; \
                 push it once with `git push --set-upstream <remote> HEAD`"
            ),
        }
    }
}

/// Opens `path` in the editor git would use (GIT_EDITOR, core.editor, VISUAL, EDITOR).
/// Works outside a repository too, with the user's global git settings.
pub fn edit_file(path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["var", "GIT_EDITOR"])
        .output()
        .context("failed to run git; is it installed?")?;
    if !output.status.success() {
        bail!(
            "`git var GIT_EDITOR` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let editor = String::from_utf8_lossy(&output.stdout).trim().to_owned();
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

    /// Runs git in `dir`, panicking on failure, and returns its stdout.
    fn run(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Keeps the user's global settings (signing, hooks) out of the tests.
    fn isolate(dir: &Path) {
        run(dir, &["config", "user.name", "Test"]);
        run(dir, &["config", "user.email", "test@example.com"]);
        run(dir, &["config", "commit.gpgsign", "false"]);
        run(dir, &["config", "core.hooksPath", "no-hooks"]);
    }

    fn setup() -> (TempDir, Repo) {
        let dir = TempDir::new().unwrap();
        run(dir.path(), &["init", "-q", "--initial-branch", "main"]);
        isolate(dir.path());
        fs::write(dir.path().join("lib.rs"), "fn a() {}\n").unwrap();
        run(dir.path(), &["add", "lib.rs"]);
        run(dir.path(), &["commit", "-q", "-m", "init"]);
        let repo = Repo::open(dir.path()).unwrap();
        (dir, repo)
    }

    /// Adds a bare repository as `origin` and pushes to it, setting the upstream.
    fn add_remote(repo: &Repo) -> TempDir {
        let remote = TempDir::new().unwrap();
        run(
            remote.path(),
            &["init", "--bare", "-q", "--initial-branch", "main"],
        );
        repo.git(&["remote", "add", "origin", remote.path().to_str().unwrap()])
            .unwrap();
        repo.push().unwrap();
        remote
    }

    /// Pushes a commit to `remote` from another clone, like an edit made on GitHub.
    fn commit_elsewhere(remote: &TempDir, file: &str, content: &str, subject: &str) {
        let clone = TempDir::new().unwrap();
        run(
            clone.path(),
            &["clone", "-q", remote.path().to_str().unwrap(), "."],
        );
        isolate(clone.path());
        fs::write(clone.path().join(file), content).unwrap();
        run(clone.path(), &["add", "--all"]);
        run(clone.path(), &["commit", "-q", "-m", subject]);
        run(clone.path(), &["push", "-q"]);
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

        let (scope, changes) = repo.changes(&[]).unwrap().unwrap();
        assert_eq!(scope, Scope::Tracked);
        assert!(changes.diff.contains("+fn b() {}"));
        assert_eq!(repo.untracked_count().unwrap(), 1);

        repo.commit("fix: rename a to b", scope, &[]).unwrap();
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

        let (scope, changes) = repo.changes(&[]).unwrap().unwrap();
        assert_eq!(scope, Scope::Staged);
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

        let (_, changes) = repo.changes(&["Cargo.lock".into()]).unwrap().unwrap();
        assert!(changes.stat.contains("sub/Cargo.lock"));
        assert!(!changes.diff.contains("Cargo.lock"));
        assert!(changes.diff.contains("main.rs"));
    }

    #[test]
    fn deleted_files_show_only_their_name() {
        let (dir, repo) = setup();
        fs::remove_file(dir.path().join("lib.rs")).unwrap();

        let (_, changes) = repo.changes(&[]).unwrap().unwrap();
        assert!(changes.diff.contains("deleted file"));
        assert!(!changes.diff.contains("fn a()"));
    }

    #[test]
    fn push_sets_upstream_then_reuses_it() {
        let (dir, repo) = setup();
        let remote = add_remote(&repo);
        assert!(repo.has_upstream());

        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        repo.commit("feat: second", Scope::Tracked, &[]).unwrap();
        assert!(repo.incoming_commits().unwrap().is_empty());
        repo.push().unwrap();
        let pushed = run(remote.path(), &["log", "--format=%s", "-1", "main"]);
        assert_eq!(pushed.trim(), "feat: second");
    }

    #[test]
    fn push_without_remote_explains_how_to_add_one() {
        let (_dir, repo) = setup();
        assert!(repo.incoming_commits().unwrap().is_empty());
        let err = repo.push().unwrap_err();
        assert!(err.to_string().contains("gh repo create"));
    }

    #[test]
    fn rebases_onto_remote_commits_keeping_uncommitted_changes() {
        let (dir, repo) = setup();
        let remote = add_remote(&repo);
        commit_elsewhere(&remote, "LICENSE", "MIT\n", "docs: add license");

        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        repo.commit("feat: local", Scope::Tracked, &[]).unwrap();
        fs::write(dir.path().join("lib.rs"), "fn uncommitted() {}\n").unwrap();

        let incoming = repo.incoming_commits().unwrap();
        assert_eq!(incoming.len(), 1);
        assert!(incoming[0].ends_with(" docs: add license"));
        repo.rebase_onto_upstream().unwrap();
        repo.push().unwrap();

        let history = run(remote.path(), &["log", "--format=%s", "main"]);
        assert_eq!(history, "feat: local\ndocs: add license\ninit\n");
        let lib = fs::read_to_string(dir.path().join("lib.rs")).unwrap();
        assert_eq!(lib, "fn uncommitted() {}\n");
    }

    #[test]
    fn conflicting_rebase_is_cancelled() {
        let (dir, repo) = setup();
        let remote = add_remote(&repo);
        commit_elsewhere(&remote, "lib.rs", "fn remote() {}\n", "fix: remote");

        fs::write(dir.path().join("lib.rs"), "fn local() {}\n").unwrap();
        repo.commit("fix: local", Scope::Tracked, &[]).unwrap();
        let before = repo.git(&["rev-parse", "HEAD"]).unwrap();

        assert_eq!(repo.incoming_commits().unwrap().len(), 1);
        let err = repo.rebase_onto_upstream().unwrap_err();
        assert!(
            err.to_string().contains("Merge conflict in lib.rs"),
            "{err}"
        );
        assert_eq!(repo.git(&["rev-parse", "HEAD"]).unwrap(), before);
        assert!(!repo.git_path("rebase-merge").unwrap().exists());
        assert!(repo.changes(&[]).unwrap().is_none());
    }

    #[test]
    fn describes_a_branch_since_it_left_the_remote_base() {
        let (dir, repo) = setup();
        let remote = add_remote(&repo);
        run(dir.path(), &["switch", "-q", "-c", "feature"]);
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        repo.commit(
            "feat: add b\n\nb replaces a.\n\nIt is faster.",
            Scope::Tracked,
            &[],
        )
        .unwrap();
        fs::write(dir.path().join("new.rs"), "fn c() {}\n").unwrap();
        repo.stage_all().unwrap();
        repo.commit("test: cover c", Scope::Staged, &[]).unwrap();
        // The base moves on without the branch; its commit is not the branch's.
        commit_elsewhere(&remote, "LICENSE", "MIT\n", "docs: add license");

        assert_eq!(repo.current_branch().unwrap(), "feature");
        assert_eq!(repo.remote().unwrap(), "origin");
        let base = repo.fetch_branch("origin", "main").unwrap();
        assert_eq!(base, "origin/main");
        let commits = repo.commits_since(&base).unwrap();
        let messages: Vec<&str> = commits.iter().map(|c| c.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "- feat: add b\n  b replaces a.\n\n  It is faster.",
                "- test: cover c"
            ]
        );
        assert!(commits.iter().all(|c| c.hash.len() >= 7));

        let changes = repo.branch_changes(&base, &[]).unwrap();
        assert!(changes.diff.contains("+fn b() {}") && changes.diff.contains("new.rs"));
        assert!(!changes.diff.contains("LICENSE"));
        assert!(changes.stat.contains("2 files changed"));

        let patches = repo
            .commit_patches(&[&format!("{base}..HEAD")], &[])
            .unwrap();
        let summary: Vec<_> = patches
            .iter()
            .map(|commit| (commit.message.as_str(), commit.patch.contains("+fn c() {}")))
            .collect();
        assert_eq!(
            summary,
            [
                ("test: cover c", true),
                ("feat: add b\n\nb replaces a.\n\nIt is faster.", false)
            ]
        );
        assert!(patches[1].patch.contains("diff --git a/lib.rs b/lib.rs"));
        assert_eq!(repo.outgoing_patches(&[]).unwrap().len(), 2);

        assert!(repo.has_unpushed_commits().unwrap());
        repo.push().unwrap();
        assert!(!repo.has_unpushed_commits().unwrap());
        assert!(repo.outgoing_patches(&[]).unwrap().is_empty());
        assert!(!repo.has_uncommitted_changes().unwrap());
        fs::write(dir.path().join("lib.rs"), "fn d() {}\n").unwrap();
        assert!(repo.has_uncommitted_changes().unwrap());
    }

    #[test]
    fn traces_the_history_of_lines_through_a_rename() {
        use crate::why;
        let (dir, repo) = setup();
        let write = |content: &str| fs::write(dir.path().join("core.rs"), content).unwrap();
        fs::write(
            dir.path().join("lib.rs"),
            "fn a() {}\nfn b() {}\nfn c() {}\n",
        )
        .unwrap();
        repo.commit("feat: add b and c", Scope::Tracked, &[])
            .unwrap();
        run(dir.path(), &["mv", "lib.rs", "core.rs"]);
        repo.commit("refactor: rename lib to core", Scope::Staged, &[])
            .unwrap();
        write("fn a() {}\nfn b2() {}\nfn c() {}\n");
        repo.commit("fix: rename b", Scope::Tracked, &[]).unwrap();
        write("// new\nfn a() {}\nfn b2() {}\nfn c() {}\n");
        assert!(repo.in_head("core.rs") && !repo.in_head("lib.rs"));

        let blamed = why::parse_blame(&repo.blame("core.rs", "1,3").unwrap());
        let lines: Vec<(usize, bool)> = blamed
            .iter()
            .map(|line| (line.line, line.is_committed()))
            .collect();
        assert_eq!(lines, [(1, false), (2, true), (3, true)]);
        let summaries = repo.summaries(&[&blamed[1].hash, &blamed[2].hash]).unwrap();
        let subjects: Vec<&str> = summaries.iter().map(|s| s.subject.as_str()).collect();
        assert_eq!(subjects, ["init", "fix: rename b"]);
        assert!(summaries[0].short.len() >= 7 && summaries[0].date.len() == 10);

        let hunks = why::parse_hunks(&repo.uncommitted_hunks("core.rs").unwrap());
        assert_eq!(why::head_line(&hunks, 3), Some(2));
        let history = why::parse_history(&repo.line_history("core.rs", "2,2", 10).unwrap());
        let subjects: Vec<&str> = history.iter().map(|c| c.message.as_str()).collect();
        assert_eq!(subjects.first(), Some(&"fix: rename b"));
        assert_eq!(subjects.last(), Some(&"feat: add b and c"));
        assert!(history[0].patch.contains("-fn b() {}\n+fn b2() {}"));
        // Before the rename, the lines were in lib.rs.
        assert!(history.last().unwrap().patch.contains("lib.rs"));
        assert_eq!(history[0].author, "Test");
    }

    #[test]
    fn finds_methods_with_the_built_in_diff_drivers() {
        let (dir, repo) = setup();
        let code = "struct Repo;\n\nimpl Repo {\n    fn has_open(&self) {}\n\n    \
                    pub fn open() -> Self {\n        Repo\n    }\n}\n";
        fs::write(dir.path().join("lib.rs"), code).unwrap();
        repo.commit("feat: open", Scope::Tracked, &[]).unwrap();
        let word = crate::why::Target {
            path: "lib.rs".into(),
            lines: crate::why::Lines::Function("open".into()),
        }
        .ranges()
        .remove(0);
        // Without a driver, git only finds what starts at the first column.
        let err = repo.blame("lib.rs", &word).unwrap_err();
        assert!(err.to_string().contains("no match"), "{err}");

        let attributes = repo.git_path("test-attributes").unwrap();
        repo.write_function_attributes(&attributes).unwrap();
        let drivers = repo.with_attributes(attributes);
        let blamed = crate::why::parse_blame(&drivers.blame("lib.rs", &word).unwrap());
        let lines: Vec<usize> = blamed.iter().map(|line| line.line).collect();
        assert_eq!(lines, [6, 7, 8, 9]);
        let history = drivers.line_history("lib.rs", &word, 5).unwrap();
        assert!(history.contains("feat: open") && history.contains("+    pub fn open()"));
    }

    #[test]
    fn resolves_commits_and_their_parents() {
        let (dir, repo) = setup();
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        repo.commit("feat: b\n\nWhy b.", Scope::Tracked, &[])
            .unwrap();
        let head = repo.resolve("HEAD").unwrap().unwrap();
        let first = repo.resolve("HEAD~1").unwrap().unwrap();
        assert_eq!(repo.parents(&head).unwrap(), std::slice::from_ref(&first));
        assert!(repo.parents(&first).unwrap().is_empty());
        assert!(repo.resolve("no-such-branch").unwrap().is_none());
        assert!(repo.resolve("--all").unwrap().is_none());
        assert!(repo.is_branch("main") && !repo.is_branch("HEAD~1"));

        let empty = repo.empty_tree().unwrap();
        let changes = repo.changes_between(&[&empty, &first], &[]).unwrap();
        assert!(changes.diff.contains("+fn a() {}"));
        let commits = repo.commits(&["--max-count=1", &head]).unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].message, "- feat: b\n  Why b.");
        assert_eq!(commits[0].author, "Test");
        // Without a remote, branches start from main.
        assert_eq!(repo.default_base().unwrap(), "main");
    }

    #[test]
    fn detached_head_has_no_branch() {
        let (dir, repo) = setup();
        run(dir.path(), &["switch", "-q", "--detach"]);
        let err = repo.current_branch().unwrap_err();
        assert!(err.to_string().contains("detached"));
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
