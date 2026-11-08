//! Wrapper around the `git` command line.

pub mod lossless;
pub mod names;
pub mod worktree;

use crate::safe_text::one_line;
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use worktree::TempWorktree;

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

/// A path the index holds differently from `HEAD`, as `git diff --cached --raw` shows it: the
/// mode and object a commit would hold, or mode `000000` and a zero object for a removal.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexEntry {
    pub mode: String,
    pub object: String,
    pub path: String,
}

impl IndexEntry {
    /// Whether the index holds no entry at the path: it's staged for removal.
    pub fn is_removal(&self) -> bool {
        self.mode.bytes().all(|digit| digit == b'0')
    }
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

/// For what people and models read: file names as they are, whatever the
/// user's setting is. It's passed per command, because `-c` values also reach
/// `git commit` hooks.
const READABLE_NAMES: [&str; 2] = ["-c", "core.quotePath=false"];

/// For a patch that goes back to `git apply` after being read as text: a name
/// that isn't valid UTF-8 only survives that in git's ASCII quoting.
const QUOTED_NAMES: [&str; 2] = ["-c", "core.quotePath=true"];

/// Makes `git diff` and `git log` cover the whole work tree, and name paths from its
/// top, whatever `diff.relative` says. Like [`READABLE_NAMES`], it's passed to each read
/// and not to `git commit`, because `-c` values reach the user's hooks.
const WHOLE_TREE: [&str; 2] = ["-c", "diff.relative=false"];

pub struct Repo {
    /// Directory git commands run in.
    workdir: PathBuf,
    /// An index file to use instead of the repository's own.
    index: Option<PathBuf>,
    /// An attributes file to use instead of the user's own.
    attributes: Option<PathBuf>,
    /// Whether git commands run without the user's hooks.
    hooks_off: bool,
}

impl Repo {
    pub fn open(workdir: impl Into<PathBuf>) -> Result<Self> {
        let repo = Self {
            workdir: workdir.into(),
            index: None,
            attributes: None,
            hooks_off: false,
        };
        let inside = repo.git(&["rev-parse", "--is-inside-work-tree"]);
        if !inside.is_ok_and(|out| out.trim() == "true") {
            bail!("not inside a git work tree");
        }
        Ok(repo)
    }

    /// The repository around `dir`, with git commands running at the top of its
    /// work tree. Commands that use another index need this, because git reads
    /// a relative index path from the top, and `git apply --cached` skips
    /// paths outside the folder it runs in.
    pub fn open_at_top(dir: impl Into<PathBuf>) -> Result<Self> {
        let repo = Self::open(dir)?;
        let workdir = repo.toplevel()?;
        Ok(Self { workdir, ..repo })
    }

    /// The same repository with git running at the top of the work tree, where the paths
    /// git reads mean the same as the paths it lists, whatever folder the user is in.
    pub fn at_top(&self) -> Result<Repo> {
        Ok(Repo {
            workdir: self.toplevel()?,
            index: self.index.clone(),
            attributes: self.attributes.clone(),
            hooks_off: self.hooks_off,
        })
    }

    /// The same repository with the index file at `index`, so commits can be
    /// built without touching the real index.
    pub fn with_index(&self, index: PathBuf) -> Repo {
        Repo {
            workdir: self.workdir.clone(),
            index: Some(index),
            attributes: self.attributes.clone(),
            hooks_off: self.hooks_off,
        }
    }

    /// The same repository for work the user shouldn't hear about from their
    /// hooks, such as on a temporary index.
    pub fn without_hooks(self) -> Repo {
        Repo {
            hooks_off: true,
            ..self
        }
    }

    /// The same repository with the attributes file at `attributes` in place
    /// of the user's own (see [`Repo::write_function_attributes`]).
    pub fn with_attributes(&self, attributes: PathBuf) -> Repo {
        Repo {
            workdir: self.workdir.clone(),
            index: self.index.clone(),
            attributes: Some(attributes),
            hooks_off: self.hooks_off,
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
        if self.hooks_off {
            command.args(["-c", NO_HOOKS]);
        }
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
        let output = self.git_bytes(args)?;
        Ok(String::from_utf8_lossy(&output).into_owned())
    }

    /// Runs git and returns its stdout as it is, failing with its stderr.
    fn git_bytes(&self, args: &[&str]) -> Result<Vec<u8>> {
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
        Ok(output.stdout)
    }

    /// Runs git for a `-z` list, whose names are in the form [`lossless`] describes, so
    /// that they can reach git again unchanged.
    fn nul_list(&self, args: &[&str]) -> Result<Vec<String>> {
        Ok(self
            .git_bytes(args)?
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .map(lossless::to_text)
            .collect())
    }

    /// Runs a `git diff` for a `-z` list of names, as [`Repo::nul_list`] reads it. The
    /// names are from the top of the work tree, and include those outside the current
    /// folder, whatever `diff.relative` says.
    fn diff_names(&self, args: &[&str]) -> Result<Vec<String>> {
        self.nul_list(&[&WHOLE_TREE[..], args].concat())
    }

    /// Runs git with `input` on its standard input, failing with its stderr.
    fn git_with_input(&self, args: &[&str], input: &[u8]) -> Result<()> {
        self.git_with_input_and_output(args, input, &[0]).map(drop)
    }

    /// Runs git with `input` on its standard input and returns its stdout. An exit code
    /// outside `success` fails with its stderr.
    fn git_with_input_and_output(
        &self,
        args: &[&str],
        input: &[u8],
        success: &[i32],
    ) -> Result<Vec<u8>> {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to run git; is it installed?")?;
        // A thread writes the input, so git can't block on a full stdout meanwhile.
        let mut stdin = child.stdin.take().context("git has no input")?;
        let input = input.to_vec();
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let output = child.wait_with_output().context("failed to wait for git")?;
        let written = writer
            .join()
            .map_err(|_| anyhow::anyhow!("writing to git panicked"))?;
        if !output
            .status
            .code()
            .is_some_and(|code| success.contains(&code))
        {
            bail!(
                "`git {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        written.context("failed to send git its input")?;
        Ok(output.stdout)
    }

    /// Those of `paths` that git ignores, by every `.gitignore` and exclude file there is.
    pub fn ignored(&self, paths: &[String]) -> Result<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        // check-ignore reads paths from the current folder, and these are from the top.
        let output = self.at_top()?.git_with_input_and_output(
            &["check-ignore", "-z", "--stdin"],
            &nul_joined(paths),
            &[0, 1],
        )?;
        Ok(output
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .map(lossless::to_text)
            .collect())
    }

    /// The paths that are staged, also before the first commit.
    pub fn staged_paths(&self) -> Result<Vec<String>> {
        self.diff_names(&["diff", "--cached", "--name-only", "-z", "--no-renames"])
    }

    /// Takes new files out of the index again, leaving them in the work tree. Only for
    /// files HEAD doesn't have: on a committed file, this would stage its deletion.
    pub fn unstage_new_files(&self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        // `git rm` refuses a pathspec file outside the top of the work tree.
        self.at_top()?.git_with_input(
            &[
                "rm",
                "--cached",
                // Only the index changes, so this can't lose work. Without it, git keeps an
                // entry whose content differs from both the file and HEAD.
                "--force",
                "--quiet",
                "--ignore-unmatch",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ],
            &top_literal_pathspecs(paths),
        )
    }

    /// Runs git like [`Repo::git`], for output that names files to people or models.
    fn git_readable(&self, args: &[&str]) -> Result<String> {
        self.git(&[&READABLE_NAMES[..], &WHOLE_TREE, args].concat())
    }

    fn has_diff(&self, args: &[&str]) -> Result<bool> {
        let output = self
            .command(&[&WHOLE_TREE[..], &["diff", "--quiet", "--no-ext-diff"]].concat())
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

    /// Stages all changes, as `git add --all` does, except what the `exclude` pathspecs
    /// name.
    pub fn stage_all_except(&self, exclude: &[String]) -> Result<()> {
        let pathspecs: Vec<Vec<u8>> = std::iter::once(":/")
            .chain(exclude.iter().map(String::as_str))
            .map(lossless::to_bytes)
            .collect();
        self.git_with_input(
            &[
                "add",
                "--all",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ],
            &pathspecs.join(&0),
        )
    }

    /// The untracked files that no ignore rule covers, each file listed on its
    /// own and relative to the top of the work tree.
    pub fn untracked_files(&self) -> Result<Vec<String>> {
        let status = self.nul_list(&["status", "--porcelain", "-z", "--untracked-files=all"])?;
        let mut files = Vec::new();
        let mut entries = status.iter();
        while let Some(entry) = entries.next() {
            if let Some(path) = entry.strip_prefix("?? ") {
                files.push(path.to_owned());
            } else if entry.get(..2).is_some_and(|code| code.contains(['R', 'C'])) {
                // A rename or copy is followed by the path it came from.
                entries.next();
            }
        }
        Ok(files)
    }

    /// The top directory of the work tree.
    pub fn toplevel(&self) -> Result<PathBuf> {
        let top = self.git(&["rev-parse", "--show-toplevel"])?;
        Ok(PathBuf::from(top.trim_end_matches(['\n', '\r'])))
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
    /// diff settings are. Bytes that aren't valid UTF-8 stay in it, in the form
    /// [`lossless`] describes, which [`Repo::apply_to_index`] takes.
    pub fn patch(&self, scope: Scope) -> Result<String> {
        let mut args = [&QUOTED_NAMES[..], &WHOLE_TREE].concat();
        args.extend([
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "--unified=3",
            "--find-renames",
            "--submodule=short",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ]);
        if scope == Scope::Staged {
            args.push("--cached");
        }
        let patch = lossless::to_text(&self.git_bytes(&args)?);
        Ok(without_dirty_submodules(&patch))
    }

    /// The paths in `scope` whose diffs aren't left out by `exclude`, in the
    /// form [`names::of_diff`] gives. A renamed file is named by its new path.
    pub fn included_paths(&self, scope: Scope, exclude: &[String]) -> Result<HashSet<String>> {
        let excludes = exclude_pathspecs(exclude);
        let mut args = [
            &WHOLE_TREE[..],
            &["diff", "--name-only", "--find-renames", "-z"],
        ]
        .concat();
        if scope == Scope::Staged {
            args.push("--cached");
        }
        args.extend(["--", ":/"]);
        args.extend(excludes.iter().map(String::as_str));
        let names = self.git(&args)?;
        Ok(names
            .split('\0')
            .filter(|name| !name.is_empty())
            .map(names::shown)
            .collect())
    }

    /// Writes the index as a tree and returns its hash.
    pub fn write_tree(&self) -> Result<String> {
        Ok(self.git(&["write-tree"])?.trim().to_owned())
    }

    /// Whether the changes in `scope` differ from what they'd be if `tree` were
    /// committed: the index for staged changes, and the work tree for tracked
    /// ones. It goes by the same settings as the changes themselves, so a file
    /// marked to be left out counts as it does there.
    pub fn differs_from_tree(&self, scope: Scope, tree: &str) -> Result<bool> {
        match scope {
            Scope::Staged => self.has_diff(&["--cached", tree]),
            // The work tree can't show which commit a submodule should be at: only
            // the index says that, and a moved one isn't in the index yet.
            Scope::Tracked => self.has_diff(&["--ignore-submodules=all", tree]),
        }
    }

    /// The files in the index with conflicts that aren't resolved yet.
    pub fn unmerged_paths(&self) -> Result<Vec<String>> {
        self.diff_names(&["diff", "--name-only", "--diff-filter=U", "-z"])
    }

    /// The paths that differ between two commits or trees, with both paths of a rename,
    /// and submodules whatever their `ignore` setting says.
    pub fn changed_between(&self, from: &str, to: &str) -> Result<Vec<String>> {
        self.nul_list(&[
            "diff-tree",
            "-r",
            "-z",
            "--name-only",
            "--no-renames",
            "--no-commit-id",
            "--ignore-submodules=none",
            from,
            to,
        ])
    }

    /// The paths at which the index differs from `commit`.
    pub fn staged_against(&self, commit: &str) -> Result<Vec<String>> {
        self.diff_names(&[
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--no-renames",
            "--ignore-submodules=none",
            commit,
        ])
    }

    /// Sets `paths` in the index to what `commit` holds, leaving the rest of the index and
    /// the work tree alone. No paths means nothing happens: `git reset` with an empty
    /// list of paths would reset everything.
    pub fn reset_paths(&self, commit: &str, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        self.git_with_input(
            &[
                "reset",
                "--quiet",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
                commit,
            ],
            &top_literal_pathspecs(paths),
        )
    }

    /// Staged submodule pointers that `git diff --cached` doesn't show, because of an
    /// `ignore` setting. A plain `git commit` commits them all the same.
    pub fn hidden_staged_pointers(&self) -> Result<Vec<String>> {
        Ok(self
            .hidden_staged_entries()?
            .into_iter()
            .map(|entry| entry.path)
            .collect())
    }

    /// The index entries of the [hidden staged pointers](Repo::hidden_staged_pointers),
    /// so that they can be put back as they are.
    pub fn hidden_staged_entries(&self) -> Result<Vec<IndexEntry>> {
        let all = raw_entries(&self.diff_names(&[
            "diff",
            "--cached",
            "--raw",
            "--no-abbrev",
            "-z",
            "--no-renames",
            "--ignore-submodules=none",
        ])?)?;
        let shown = self.diff_names(&["diff", "--cached", "--name-only", "-z", "--no-renames"])?;
        Ok(all
            .into_iter()
            .filter(|entry| !shown.contains(&entry.path))
            .collect())
    }

    /// Puts `entries` into the index as they are: a pointer at its object, and a removal
    /// as no entry at all.
    pub fn restore_index_entries(&self, entries: &[IndexEntry]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let records: Vec<u8> = entries
            .iter()
            .flat_map(|entry| {
                let record = format!("{} {}\t{}", entry.mode, entry.object, entry.path);
                lossless::to_bytes(&record).into_iter().chain([0])
            })
            .collect();
        self.git_with_input(&["update-index", "-z", "--index-info"], &records)
    }

    /// Paths whose work tree differs from the index, with a submodule whose checkout moved,
    /// whatever its `ignore` setting says, but not one that's only dirty.
    pub fn changed_in_work_tree(&self) -> Result<Vec<String>> {
        self.diff_names(&["diff", "--name-only", "-z", "--ignore-submodules=dirty"])
    }

    /// Paths whose work tree holds the same kind of thing as the index, but changed: a
    /// submodule whose checkout moved counts, one that's gone or only dirty doesn't.
    pub fn modified_in_work_tree(&self) -> Result<Vec<String>> {
        self.diff_names(&[
            "diff",
            "--name-only",
            "-z",
            "--ignore-submodules=dirty",
            "--diff-filter=M",
        ])
    }

    /// Moved submodule pointers in the work tree that `git commit --all` commits though
    /// `git diff` doesn't show them. That's so for `submodule.<name>.ignore=all`, and not
    /// for `diff.ignoreSubmodules=all`, under which `git commit --all` leaves them out.
    pub fn hidden_changed_pointers(&self) -> Result<Vec<String>> {
        let moved = self.changed_in_work_tree()?;
        let shown = self.diff_names(&[
            "-c",
            "diff.ignoreSubmodules=none",
            "diff",
            "--name-only",
            "-z",
        ])?;
        Ok(moved
            .into_iter()
            .filter(|path| !shown.contains(path))
            .collect())
    }

    /// Replaces the index with the tree of `tree_ish`, e.g. `HEAD`.
    pub fn read_tree(&self, tree_ish: &str) -> Result<()> {
        self.git(&["read-tree", tree_ish]).map(drop)
    }

    /// Applies `patch` to the index, leaving the work tree alone.
    pub fn apply_to_index(&self, patch: &str) -> Result<()> {
        self.git_with_input(
            &["apply", "--cached", "--whitespace=nowarn", "-"],
            &lossless::to_bytes(patch),
        )
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
        Ok(log.lines().map(one_line).collect())
    }

    /// The changes HEAD makes since it branched off `base`.
    pub fn branch_changes(&self, base: &str, exclude: &[String]) -> Result<Changes> {
        self.diff(&[&format!("{base}...HEAD")], exclude)
    }

    /// The changes `git diff` shows with `range` (e.g. `--cached`), without the
    /// diffs of files matching `exclude`.
    fn diff(&self, range: &[&str], exclude: &[String]) -> Result<Changes> {
        let base = ["diff", "--no-color", "--no-ext-diff"];
        let stat = self.git_readable(&[&base[..], range, &["--stat=120"]].concat())?;

        let excludes = exclude_pathspecs(exclude);
        // A deleted file's old content says little about the change; its name is enough.
        let mut diff_args = [&base[..], range, &["--irreversible-delete", "--", ":/"]].concat();
        diff_args.extend(excludes.iter().map(String::as_str));
        let diff = self.git_readable(&diff_args)?;

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
        let log = self.git_readable(&args)?;
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

    /// The names of the local and remote branches, those committed to most
    /// recently first, each once and without the remote's name.
    pub fn branch_names(&self) -> Result<Vec<String>> {
        let remotes = self.git(&["remote"])?;
        let prefixes: Vec<String> = remotes
            .lines()
            .map(|remote| format!("refs/remotes/{remote}/"))
            .collect();
        let refs = self.git(&[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes",
        ])?;
        let mut names: Vec<String> = Vec::new();
        for full in refs.lines() {
            let name = full.strip_prefix("refs/heads/").or_else(|| {
                prefixes
                    .iter()
                    .find_map(|prefix| full.strip_prefix(prefix.as_str()))
            });
            if let Some(name) = name
                && name != "HEAD"
                && !names.iter().any(|known| known == name)
            {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }

    /// Whether git accepts `name` as the name of a branch.
    pub fn is_valid_branch_name(&self, name: &str) -> bool {
        !name.starts_with('-') && self.git(&["check-ref-format", "--branch", name]).is_ok()
    }

    /// Creates the branch `name` at `start`, or at HEAD, and switches to it.
    /// Uncommitted changes come along. The branch doesn't track `start`, so
    /// its first push sets an upstream of its own name.
    pub fn create_branch(&self, name: &str, start: Option<&str>) -> Result<()> {
        let mut command = self.command(&["switch", "--no-track", "--create", name]);
        if let Some(start) = start {
            command.arg(start);
        }
        // Uses the terminal, so git reports the switch, or what stands in its way.
        let status = command.status().context("failed to run git switch")?;
        if !status.success() {
            bail!("`git switch --create {name}` failed ({status})");
        }
        Ok(())
    }

    /// The commits HEAD has that no remote-tracking branch has, oldest first.
    pub fn unpushed(&self) -> Result<Vec<String>> {
        let list = self.git(&["rev-list", "--reverse", "HEAD", "--not", "--remotes"])?;
        Ok(list.lines().map(str::to_owned).collect())
    }

    /// The commits on HEAD after `base`, oldest first.
    pub fn commits_after(&self, base: &str) -> Result<Vec<String>> {
        let list = self.git(&["rev-list", "--reverse", &format!("{base}..HEAD")])?;
        Ok(list.lines().map(str::to_owned).collect())
    }

    pub fn has_remotes(&self) -> Result<bool> {
        Ok(!self.git(&["remote"])?.trim().is_empty())
    }

    /// Whether any commit in `range` is a merge.
    pub fn has_merges(&self, range: &str) -> Result<bool> {
        let merges = self.git(&["rev-list", "--merges", "--max-count=1", range])?;
        Ok(!merges.trim().is_empty())
    }

    /// The hash of the tree of `commit`.
    pub fn tree(&self, commit: &str) -> Result<String> {
        Ok(self
            .git(&["rev-parse", &format!("{commit}^{{tree}}")])?
            .trim()
            .to_owned())
    }

    /// Replays the commits from `head` back to `base` (or to the first commit
    /// if `base` is `None`) as the `git rebase -i` list `todo` says, and
    /// returns the new tip. The rebase runs in a temporary worktree without
    /// hooks, so this repository's work tree, index and branches are left alone.
    pub fn rewrite(&self, head: &str, base: Option<&str>, todo: &str) -> Result<String> {
        let todo_path = std::path::absolute(self.git_path("GIT_AGENT_TODO")?)?;
        fs::write(&todo_path, todo)
            .with_context(|| format!("failed to write {}", todo_path.display()))?;
        let result = TempWorktree::add(self, head)
            .and_then(|worktree| rebase(worktree.path(), base, &todo_path));
        let _ = fs::remove_file(&todo_path);
        result
    }

    /// Moves the current branch, or a detached HEAD, from `old` to `new`,
    /// failing if it has moved from `old` meanwhile.
    pub fn move_head(&self, old: &str, new: &str, reason: &str) -> Result<()> {
        let moved = match self.current_branch() {
            Ok(branch) => self.git(&[
                "update-ref",
                "-m",
                reason,
                &format!("refs/heads/{branch}"),
                new,
                old,
            ]),
            Err(_) => self.git(&["update-ref", "-m", reason, "--no-deref", "HEAD", new, old]),
        };
        moved.map(drop)
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
        self.git_readable(&[
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
        self.git_readable(&[
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
        self.git_readable(&[
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
    pub fn commit(
        &self,
        message: &str,
        scope: Scope,
        extra_args: &[String],
        leave_out: &[String],
    ) -> Result<()> {
        let path = self.git_path("GIT_AGENT_MSG")?;
        fs::write(&path, format!("{message}\n"))
            .with_context(|| format!("failed to write {}", path.display()))?;

        let mut command = self.command(&["commit", "--file"]);
        command.arg(&path);
        if scope == Scope::Tracked {
            // What `--all` commits, apart from the work-tree changes in `leave_out`:
            // `--include` adds the work-tree contents of the paths given to what's
            // staged, so a version of a left-out path the user staged is committed.
            command.arg(if leave_out.is_empty() {
                "--all"
            } else {
                "--include"
            });
        }
        command.args(extra_args);
        if scope == Scope::Tracked && !leave_out.is_empty() {
            command.args(["--", ":/"]);
            command.args(leave_out.iter().map(|path| {
                let mut pathspec = OsString::from(":(top,exclude,literal)");
                pathspec.push(lossless::to_path(path));
                pathspec
            }));
        }
        let status = command.status().context("failed to run git commit")?;
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
        Ok(log.lines().map(one_line).collect())
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

/// Turns hooks off for the commands git-agent runs on its own behalf, such as
/// a rebase in a temporary worktree; git passes it on to the commands those run.
const NO_HOOKS: &str = "core.hooksPath=/dev/null";

/// Runs the interactive rebase of the worktree at `dir` with `todo_path` as
/// its list, and returns the new HEAD. A failed rebase is aborted.
fn rebase(dir: &Path, base: Option<&str>, todo_path: &Path) -> Result<String> {
    let mut command = Command::new("git");
    command.current_dir(dir).args([
        "-c",
        NO_HOOKS,
        "-c",
        "rebase.updateRefs=false",
        "rebase",
        "--interactive",
        "--empty=keep",
    ]);
    match base {
        Some(base) => command.arg(base),
        None => command.arg("--root"),
    };
    // The "editor" puts git-agent's list in place of the one git wrote.
    let editor = format!("cp {}", shell_quote(&todo_path.to_string_lossy()));
    let output = command
        .env("GIT_SEQUENCE_EDITOR", editor)
        .env("GIT_EDITOR", "true")
        .output()
        .context("failed to run git rebase")?;
    if !output.status.success() {
        let _ = Command::new("git")
            .current_dir(dir)
            .args(["rebase", "--abort"])
            .output();
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
            "moving the commits as planned runs into conflicts:\n  {}",
            conflicts.join("\n  ")
        );
    }
    let head = Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .context("failed to run git rev-parse")?;
    Ok(String::from_utf8_lossy(&head.stdout).trim().to_owned())
}

/// `text` quoted for a POSIX shell.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
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

/// `patch` without what a commit can't hold: that a submodule has uncommitted work
/// of its own, which git writes as `-dirty` after its commit. The patch stays as
/// git wrote it otherwise, which respects the user's settings for submodules.
/// `paths` for git's standard input, each ended by a NUL.
fn nul_joined(paths: &[String]) -> Vec<u8> {
    paths
        .iter()
        .flat_map(|path| {
            let mut bytes = lossless::to_bytes(path);
            bytes.push(0);
            bytes
        })
        .collect()
}

/// Reads `git diff --raw -z --no-renames`, split at its NULs, into entries: each is a header
/// such as `:160000 160000 <old> <new> M`, then the path.
fn raw_entries(fields: &[String]) -> Result<Vec<IndexEntry>> {
    fields
        .chunks(2)
        .map(|pair| match pair {
            [header, path] => raw_entry(header, path),
            _ => bail!("`git diff --raw` ended in the middle of an entry"),
        })
        .collect()
}

fn raw_entry(header: &str, path: &str) -> Result<IndexEntry> {
    let parts: Vec<&str> = header.strip_prefix(':').unwrap_or("").split(' ').collect();
    match parts[..] {
        [_, mode, _, object, _] => Ok(IndexEntry {
            mode: mode.to_owned(),
            object: object.to_owned(),
            path: path.to_owned(),
        }),
        _ => bail!("`git diff --raw` gave a line git-agent can't read: {header:?}"),
    }
}

/// `paths` as pathspecs that name exactly those paths from the top of the work tree,
/// wherever git runs, for git's standard input.
fn top_literal_pathspecs(paths: &[String]) -> Vec<u8> {
    let pathspecs: Vec<String> = paths
        .iter()
        .map(|path| format!(":(top,literal){path}"))
        .collect();
    nul_joined(&pathspecs)
}

fn without_dirty_submodules(patch: &str) -> String {
    crate::fit::split_files(patch)
        .into_iter()
        .filter_map(without_dirty_submodule)
        .collect()
}

/// The part of a patch about one file, without `-dirty` if it's a submodule, or
/// `None` if that's all that changed.
fn without_dirty_submodule(file: &str) -> Option<String> {
    let line_after = |start: &str| file.lines().find_map(|line| line.strip_prefix(start));
    let is_submodule = line_after("index ").is_none_or(|index| index.ends_with(" 160000"));
    let new = line_after("+Subproject commit ").filter(|_| is_submodule);
    let Some((new, commit)) = new.and_then(|new| Some((new, new.strip_suffix("-dirty")?))) else {
        return Some(file.to_owned());
    };
    (line_after("-Subproject commit ") != Some(commit)).then(|| file.replace(new, commit))
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

/// What the tests of this module and its submodules share.
#[cfg(test)]
pub mod test_support {
    use super::*;
    use tempfile::TempDir;

    /// Runs git in `dir`, panicking on failure, and returns its stdout.
    pub fn run(dir: &Path, args: &[&str]) -> String {
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

    /// Moves the pointer of the submodule at `library` in the work tree of `dir` to `moved`.
    pub fn move_pointer(dir: &Path, moved: &str) {
        let library = dir.join("library");
        run(
            &library,
            &["-c", "protocol.file.allow=always", "fetch", "-q", "origin"],
        );
        run(&library, &["checkout", "-q", moved]);
    }

    /// A text of `count` numbered lines.
    pub fn numbered_lines(count: usize) -> String {
        (1..=count).map(|n| format!("line {n}\n")).collect()
    }

    /// A repository with lib.rs committed, without the user's global settings
    /// for signing and hooks.
    pub fn isolated_repo() -> (TempDir, Repo) {
        let dir = TempDir::new().unwrap();
        run(dir.path(), &["init", "-q", "--initial-branch", "main"]);
        run(dir.path(), &["config", "user.name", "Test"]);
        run(dir.path(), &["config", "user.email", "test@example.com"]);
        run(dir.path(), &["config", "commit.gpgsign", "false"]);
        run(dir.path(), &["config", "core.hooksPath", "no-hooks"]);
        fs::write(dir.path().join("lib.rs"), "fn a() {}\n").unwrap();
        run(dir.path(), &["add", "lib.rs"]);
        run(dir.path(), &["commit", "-q", "-m", "init"]);
        let repo = Repo::open(dir.path()).unwrap();
        (dir, repo)
    }

    /// A repository with a submodule, and that submodule's folder and a new commit
    /// there, for moving it.
    pub fn with_submodule() -> (TempDir, Repo, TempDir, String) {
        let (dir, repo) = isolated_repo();
        let library = TempDir::new().unwrap();
        run(library.path(), &["init", "-q", "--initial-branch", "main"]);
        run(library.path(), &["config", "user.name", "Test"]);
        run(
            library.path(),
            &["config", "user.email", "test@example.com"],
        );
        run(
            library.path(),
            &["commit", "-q", "--allow-empty", "-m", "one"],
        );
        let url = library.path().to_string_lossy().into_owned();
        run(
            dir.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                &url,
                "library",
            ],
        );
        run(dir.path(), &["commit", "-q", "-m", "add the library"]);
        run(
            library.path(),
            &["commit", "-q", "--allow-empty", "-m", "two"],
        );
        let moved = run(library.path(), &["rev-parse", "HEAD"])
            .trim()
            .to_owned();
        (dir, repo, library, moved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fields(raw: &[&str]) -> Vec<String> {
        raw.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn reads_each_kind_of_raw_entry() {
        let zero = "0".repeat(40);
        let (old, new) = ("a".repeat(40), "b".repeat(40));
        let raw = fields(&[
            &format!(":160000 160000 {old} {new} M"),
            "library",
            &format!(":000000 160000 {zero} {new} A"),
            "a\nb",
            &format!(":160000 000000 {old} {zero} D"),
            "gone lib",
        ]);
        let entry = |mode: &str, object: &str, path: &str| IndexEntry {
            mode: mode.to_owned(),
            object: object.to_owned(),
            path: path.to_owned(),
        };
        assert_eq!(
            raw_entries(&raw).unwrap(),
            [
                entry("160000", &new, "library"),
                entry("160000", &new, "a\nb"),
                entry("000000", &zero, "gone lib"),
            ]
        );
        assert!(raw_entries(&[]).unwrap().is_empty());
    }

    #[test]
    fn refuses_raw_output_it_cant_read() {
        let half = fields(&[":160000 160000 a b M"]);
        assert!(raw_entries(&half).is_err());
        let short = fields(&[":160000 a M", "library"]);
        assert!(raw_entries(&short).is_err());
        let no_colon = fields(&["160000 160000 a b M", "library"]);
        assert!(raw_entries(&no_colon).is_err());
    }

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

    /// Commits a file with Norwegian letters in its name, then changes it.
    fn change_norwegian_file(dir: &Path) {
        fs::write(dir.join("Kjøre.txt"), "en\n").unwrap();
        run(dir, &["add", "--all"]);
        run(dir, &["commit", "-q", "-m", "add the file"]);
        fs::write(dir.join("Kjøre.txt"), "to\n").unwrap();
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

        repo.commit("fix: rename a to b", scope, &[], &[]).unwrap();
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
        repo.stage_all_except(&[]).unwrap();

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
        repo.commit("feat: second", Scope::Tracked, &[], &[])
            .unwrap();
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
        repo.commit("feat: local", Scope::Tracked, &[], &[])
            .unwrap();
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
        repo.commit("fix: local", Scope::Tracked, &[], &[]).unwrap();
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
            &[],
        )
        .unwrap();
        fs::write(dir.path().join("new.rs"), "fn c() {}\n").unwrap();
        repo.stage_all_except(&[]).unwrap();
        repo.commit("test: cover c", Scope::Staged, &[], &[])
            .unwrap();
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
        repo.commit("feat: add b and c", Scope::Tracked, &[], &[])
            .unwrap();
        run(dir.path(), &["mv", "lib.rs", "core.rs"]);
        repo.commit("refactor: rename lib to core", Scope::Staged, &[], &[])
            .unwrap();
        write("fn a() {}\nfn b2() {}\nfn c() {}\n");
        repo.commit("fix: rename b", Scope::Tracked, &[], &[])
            .unwrap();
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
        repo.commit("feat: open", Scope::Tracked, &[], &[]).unwrap();
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
        repo.commit("feat: b\n\nWhy b.", Scope::Tracked, &[], &[])
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
    fn creates_a_branch_that_takes_the_changes_along() {
        let (dir, repo) = setup();
        let remote = add_remote(&repo);
        commit_elsewhere(&remote, "LICENSE", "MIT\n", "docs: add license");
        run(dir.path(), &["fetch", "-q"]);
        run(dir.path(), &["branch", "-q", "fix/old"]);
        // Each once, though main is also origin/main.
        let mut names = repo.branch_names().unwrap();
        names.sort();
        assert_eq!(names, ["fix/old", "main"]);
        assert!(repo.is_valid_branch_name("feat/add-cache"));
        assert!(!repo.is_valid_branch_name("feat/add cache"));
        assert!(!repo.is_valid_branch_name("-x"));

        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        repo.create_branch("feat/b", Some("origin/main")).unwrap();
        assert_eq!(repo.current_branch().unwrap(), "feat/b");
        // It starts from the remote's latest commit, and the change came along.
        assert!(dir.path().join("LICENSE").exists());
        assert!(repo.has_uncommitted_changes().unwrap());
        // It doesn't track origin/main, so its first push makes origin/feat/b.
        assert!(!repo.has_upstream());
        repo.commit("feat: b", Scope::Tracked, &[], &[]).unwrap();
        repo.push().unwrap();
        let pushed = run(remote.path(), &["log", "--format=%s", "-1", "feat/b"]);
        assert_eq!(pushed.trim(), "feat: b");
        assert_eq!(repo.branch_names().unwrap().len(), 3);
        assert!(repo.create_branch("feat/b", None).is_err());
    }

    #[test]
    fn rewrites_commits_without_touching_the_work_tree() {
        let (dir, repo) = setup();
        let _remote = add_remote(&repo);
        let write = |name: &str, content: &str| fs::write(dir.path().join(name), content).unwrap();
        write("lib.rs", "fn a() {}\nfn b() {}\n");
        repo.commit("feat: add b", Scope::Tracked, &[], &[])
            .unwrap();
        write("notes.md", "notes\n");
        repo.stage_all_except(&[]).unwrap();
        repo.commit("wip", Scope::Staged, &[], &[]).unwrap();
        write("lib.rs", "fn a() {}\nfn b() { }\n");
        repo.commit("fix typo", Scope::Tracked, &[], &[]).unwrap();
        // Uncommitted work that the rewrite must leave alone.
        write("lib.rs", "fn a() {}\nfn b() { }\nfn c() {}\n");
        write("draft.txt", "draft\n");

        let unpushed = repo.unpushed().unwrap();
        assert_eq!(unpushed.len(), 3);
        assert_eq!(repo.commits_after("HEAD~3").unwrap(), unpushed);
        assert!(repo.has_remotes().unwrap());
        assert!(!repo.has_merges("HEAD~3..HEAD").unwrap());
        let old = repo.resolve("HEAD").unwrap().unwrap();
        let base = repo.resolve("HEAD~3").unwrap().unwrap();

        let message = std::path::absolute(dir.path().join("message.txt")).unwrap();
        fs::write(&message, "docs: add notes\n").unwrap();
        let todo = format!(
            "pick {} feat: add b\nfixup {} fix typo\npick {} wip\nexec git commit --amend --quiet \
             --no-verify --allow-empty --file {}\n",
            unpushed[0],
            unpushed[2],
            unpushed[1],
            shell_quote(&message.to_string_lossy())
        );
        let new = repo.rewrite(&old, Some(&base), &todo).unwrap();
        assert_eq!(repo.tree(&new).unwrap(), repo.tree(&old).unwrap());
        // Nothing moved yet.
        assert_eq!(repo.resolve("HEAD").unwrap().unwrap(), old);
        repo.move_head(&old, &new, "git-agent tidy").unwrap();
        let log = repo.git(&["log", "--format=%s", "-3"]).unwrap();
        assert_eq!(log, "docs: add notes\nfeat: add b\ninit\n");
        assert!(repo.has_uncommitted_changes().unwrap());
        assert_eq!(
            fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
            "fn a() {}\nfn b() { }\nfn c() {}\n"
        );
        assert!(dir.path().join("draft.txt").exists());
        assert!(repo.git(&["worktree", "list"]).unwrap().lines().count() == 1);
        // A stale `old` doesn't move the branch.
        assert!(repo.move_head(&old, &new, "again").is_err());

        // A plan that conflicts leaves everything as it was: renaming c
        // before adding it can't work.
        let head = repo.resolve("HEAD").unwrap().unwrap();
        repo.commit("feat: c", Scope::Tracked, &[], &[]).unwrap();
        write("lib.rs", "fn a() {}\nfn b() { }\nfn c2() {}\n");
        repo.commit("refactor: rename c", Scope::Tracked, &[], &[])
            .unwrap();
        let commits = repo.commits_after(&head).unwrap();
        let tip = repo.resolve("HEAD").unwrap().unwrap();
        let swapped = format!("pick {}\npick {}\n", commits[1], commits[0]);
        let err = repo.rewrite(&tip, Some(&head), &swapped).unwrap_err();
        assert!(err.to_string().contains("conflicts"), "{err}");
        assert_eq!(repo.resolve("HEAD").unwrap().unwrap(), tip);
        assert!(repo.git(&["worktree", "list"]).unwrap().lines().count() == 1);
    }

    #[test]
    fn stages_everything_but_the_untracked_files_left_out() {
        let (dir, repo) = setup();
        fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        fs::write(dir.path().join("target/debug/app"), "binary").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/a b.rs"), "fn a() {}\n").unwrap();
        fs::write(dir.path().join("lib.rs"), "fn changed() {}\n").unwrap();
        run(dir.path(), &["mv", "lib.rs", "core.rs"]);
        fs::write(dir.path().join("x.o"), "object").unwrap();

        let mut untracked = repo.untracked_files().unwrap();
        untracked.sort();
        assert_eq!(
            untracked,
            ["Cargo.toml", "src/a b.rs", "target/debug/app", "x.o"]
        );
        assert_eq!(
            repo.toplevel().unwrap().canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );

        let findings = crate::untracked::check(&repo.toplevel().unwrap(), &untracked);
        repo.stage_all_except(&crate::untracked::exclude(&findings))
            .unwrap();
        let staged = run(dir.path(), &["diff", "--cached", "--name-only"]);
        // The rename shows by its new name.
        assert_eq!(staged, "Cargo.toml\ncore.rs\nsrc/a b.rs\n");
        assert_eq!(repo.untracked_files().unwrap().len(), 2);

        // Once ignored, they stay out of a plain `git add --all` too.
        crate::untracked::add_to_gitignore(&repo.toplevel().unwrap(), &findings).unwrap();
        repo.stage_all_except(&[]).unwrap();
        assert!(repo.untracked_files().unwrap().is_empty());
        let staged = run(dir.path(), &["diff", "--cached", "--name-only"]);
        assert!(staged.contains(".gitignore") && !staged.contains("target"));
    }

    #[test]
    fn detached_head_has_no_branch() {
        let (dir, repo) = setup();
        run(dir.path(), &["switch", "-q", "--detach"]);
        let err = repo.current_branch().unwrap_err();
        assert!(err.to_string().contains("detached"));
    }

    #[test]
    fn shows_file_names_as_they_are_whatever_the_setting_is() {
        for setting in [None, Some("true"), Some("false")] {
            let (dir, repo) = setup();
            if let Some(setting) = setting {
                run(dir.path(), &["config", "core.quotePath", setting]);
            }
            change_norwegian_file(dir.path());
            let (_, changes) = repo.changes(&[]).unwrap().unwrap();
            assert!(
                changes.diff.contains("a/Kjøre.txt b/Kjøre.txt"),
                "{setting:?}"
            );
            assert!(changes.stat.contains("Kjøre.txt"), "{setting:?}");
            assert!(!changes.stat.contains("303"), "{setting:?}");
            let hunks = repo.uncommitted_hunks("Kjøre.txt").unwrap();
            assert!(hunks.contains("a/Kjøre.txt b/Kjøre.txt"), "{setting:?}");
            run(dir.path(), &["add", "--all"]);
            run(dir.path(), &["commit", "-q", "-m", "change the file"]);
            let patches = repo.commit_patches(&["-1"], &[]).unwrap();
            assert!(
                patches[0].patch.contains("a/Kjøre.txt b/Kjøre.txt"),
                "{setting:?}"
            );
            let between = repo.changes_between(&["HEAD~1", "HEAD"], &[]).unwrap();
            assert!(
                between.diff.contains("a/Kjøre.txt b/Kjøre.txt"),
                "{setting:?}"
            );
            let history = repo.line_history("Kjøre.txt", "1,1", 1).unwrap();
            assert!(history.contains("a/Kjøre.txt b/Kjøre.txt"), "{setting:?}");
        }
    }

    #[test]
    fn keeps_a_patch_in_ascii_whatever_the_setting_is() {
        for setting in [None, Some("true"), Some("false")] {
            let (dir, repo) = setup();
            if let Some(setting) = setting {
                run(dir.path(), &["config", "core.quotePath", setting]);
            }
            change_norwegian_file(dir.path());
            let patch = repo.patch(Scope::Tracked).unwrap();
            assert!(
                patch.starts_with("diff --git \"a/Kj\\303\\270re.txt\" \"b/Kj\\303\\270re.txt\""),
                "{setting:?}: {patch}"
            );
        }
    }

    #[test]
    fn lists_included_paths_in_the_form_a_parsed_patch_names_them() {
        let (dir, repo) = setup();
        for name in ["Kjøre.txt", "tab\there.txt", "skip.lock"] {
            fs::write(dir.path().join(name), "en\n").unwrap();
        }
        run(dir.path(), &["add", "--all"]);
        let included = repo
            .included_paths(Scope::Staged, &["*.lock".to_owned()])
            .unwrap();
        let expected = ["Kjøre.txt", "tab\\there.txt"].map(str::to_owned);
        assert_eq!(included, HashSet::from(expected));
    }

    #[test]
    #[cfg(unix)]
    fn leaves_the_quoting_setting_alone_for_commit_hooks() {
        let (dir, repo) = setup();
        let hooks = dir.path().join("hooks");
        fs::create_dir(&hooks).unwrap();
        let seen = dir.path().join("seen.txt");
        let script = format!(
            "#!/bin/sh\nprintf '%s' \"$GIT_CONFIG_PARAMETERS\" > {}\n",
            shell_quote(&seen.to_string_lossy())
        );
        let hook = hooks.join("pre-commit");
        fs::write(&hook, script).unwrap();
        let executable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        fs::set_permissions(&hook, executable).unwrap();
        run(dir.path(), &["config", "core.hooksPath", "hooks"]);
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir.path(), &["add", "lib.rs"]);
        repo.commit("change", Scope::Staged, &[], &[]).unwrap();
        assert!(!fs::read_to_string(seen).unwrap().contains("quotePath"));
    }

    #[test]
    fn leaves_only_a_submodules_commit_in_a_patch() {
        let old = "a".repeat(40);
        let new = "b".repeat(40);
        let header = "diff --git a/library b/library\n";
        let moved = format!(
            "{header}index aaaaaaa..bbbbbbb 160000\n--- a/library\n+++ b/library\n@@ -1 +1 @@\n\
             -Subproject commit {old}\n+Subproject commit {new}\n"
        );
        let dirty_too = moved.replace(
            &format!("+Subproject commit {new}"),
            &format!("+Subproject commit {new}-dirty"),
        );
        let only_dirty = format!(
            "{header}--- a/library\n+++ b/library\n@@ -1 +1 @@\n\
             -Subproject commit {old}\n+Subproject commit {old}-dirty\n"
        );
        let other = "diff --git a/a.txt b/a.txt\nindex 1..2 100644\n--- a/a.txt\n+++ b/a.txt\n\
                     @@ -1 +1 @@\n-a\n+b\n";
        // A file whose text only looks like this stays as it is.
        let lookalike = format!(
            "diff --git a/n.txt b/n.txt\nindex 1..2 100644\n--- a/n.txt\n+++ b/n.txt\n@@ -1 +1 @@\n\
             -Subproject commit {old}\n+Subproject commit {old}-dirty\n"
        );
        assert_eq!(without_dirty_submodules(&moved), moved);
        assert_eq!(without_dirty_submodules(&dirty_too), moved);
        assert_eq!(without_dirty_submodules(&only_dirty), "");
        assert_eq!(without_dirty_submodules(other), other);
        assert_eq!(without_dirty_submodules(&lookalike), lookalike);
        assert_eq!(
            without_dirty_submodules(&format!("{only_dirty}{other}{dirty_too}")),
            format!("{other}{moved}")
        );
        assert_eq!(without_dirty_submodules(""), "");
    }

    #[test]
    fn tells_when_a_conflict_is_unresolved() {
        let (dir, repo) = setup();
        assert!(repo.unmerged_paths().unwrap().is_empty());
        let blob = run(dir.path(), &["rev-parse", "HEAD:lib.rs"]);
        let entry = format!("100644,{},lib.rs", blob.trim());
        run(dir.path(), &["update-index", "--force-remove", "lib.rs"]);
        run(
            dir.path(),
            &["update-index", "--add", "--cacheinfo", &entry],
        );
        assert!(repo.unmerged_paths().unwrap().is_empty());
        let mut child = Command::new("git")
            .current_dir(dir.path())
            .args(["update-index", "--index-info"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let unmerged = format!(
            "0 {} 0\tlib.rs\n100644 {} 2\tlib.rs\n",
            "0".repeat(40),
            blob.trim()
        );
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(unmerged.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(repo.unmerged_paths().unwrap(), ["lib.rs"]);
    }

    #[test]
    fn sees_changes_whatever_an_external_diff_says() {
        let (dir, repo) = setup();
        run(dir.path(), &["config", "diff.external", "true"]);
        run(dir.path(), &["config", "diff.trustExitCode", "true"]);
        assert!(repo.changes(&[]).unwrap().is_none());
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        assert!(repo.changes(&[]).unwrap().is_some());
        let tree = repo.tree("HEAD").unwrap();
        assert!(repo.differs_from_tree(Scope::Tracked, &tree).unwrap());
    }

    #[test]
    fn keeps_hooks_off_for_a_scratch_index() {
        let (dir, repo) = setup();
        let hooks = dir.path().join("hooks");
        fs::create_dir(&hooks).unwrap();
        let mark = dir.path().join("hook-ran");
        let hook = hooks.join("post-index-change");
        let script = format!(
            "#!/bin/sh\ntouch {}\n",
            shell_quote(&mark.to_string_lossy())
        );
        fs::write(&hook, script).unwrap();
        let executable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        fs::set_permissions(&hook, executable).unwrap();
        run(dir.path(), &["config", "core.hooksPath", "hooks"]);
        let scratch = repo
            .with_index(dir.path().join(".git/scratch"))
            .without_hooks();
        scratch.read_tree("HEAD").unwrap();
        assert!(!mark.exists());
        repo.with_index(dir.path().join(".git/other"))
            .read_tree("HEAD")
            .unwrap();
        assert!(mark.exists());
    }

    #[test]
    fn failed_commit_keeps_message() {
        let (dir, repo) = setup();
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        let err = repo
            .commit("feat: x", Scope::Tracked, &["--no-such-flag".into()], &[])
            .unwrap_err();
        let saved = repo.git_path("GIT_AGENT_MSG").unwrap();
        assert!(err.to_string().contains("GIT_AGENT_MSG"));
        assert_eq!(fs::read_to_string(saved).unwrap(), "feat: x\n");
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn resets_only_the_paths_it_is_given() {
        let (dir, repo) = setup();
        for name in ["a*", "ab", "blå bær", ":colon"] {
            fs::write(dir.path().join(name), "one\n").unwrap();
        }
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "add them"]);
        for name in ["a*", "ab", "blå bær", ":colon"] {
            fs::write(dir.path().join(name), "two\n").unwrap();
        }
        run(dir.path(), &["add", "--all"]);

        repo.reset_paths("HEAD", &[]).unwrap();
        assert_eq!(repo.staged_against("HEAD").unwrap().len(), 4);

        repo.reset_paths("HEAD", &names(&["a*", "blå bær", ":colon"]))
            .unwrap();
        assert_eq!(repo.staged_against("HEAD").unwrap(), ["ab"]);
        assert_eq!(fs::read_to_string(dir.path().join("a*")).unwrap(), "two\n");
    }

    #[test]
    fn lists_both_paths_of_a_rename_as_changed() {
        let (dir, repo) = setup();
        fs::write(dir.path().join("old.txt"), "the same text\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "add old"]);
        run(dir.path(), &["mv", "old.txt", "new.txt"]);
        run(dir.path(), &["commit", "-q", "-m", "rename"]);
        assert_eq!(
            repo.changed_between("HEAD~", "HEAD").unwrap(),
            ["new.txt", "old.txt"]
        );
        assert!(repo.changed_between("HEAD", "HEAD").unwrap().is_empty());
    }

    #[test]
    fn stages_all_but_what_it_is_told_to_leave_out_from_a_subfolder() {
        let (dir, _repo) = setup();
        fs::create_dir(dir.path().join("sub")).unwrap();
        for name in [
            "keep.txt",
            "key*.pem",
            "key1.pem",
            "sub/deep.o",
            "sub/keep.txt",
        ] {
            fs::write(dir.path().join(name), "x\n").unwrap();
        }
        let repo = Repo::open(dir.path().join("sub")).unwrap();
        repo.stage_all_except(&names(&[
            ":(top,exclude,literal)key*.pem",
            ":(top,exclude,glob)**/*.o",
        ]))
        .unwrap();
        let staged = run(dir.path(), &["diff", "--cached", "--name-only"]);
        assert_eq!(staged, "keep.txt\nkey1.pem\nsub/keep.txt\n");
    }

    #[test]
    fn stages_everything_with_nothing_to_leave_out() {
        let (dir, repo) = setup();
        fs::write(dir.path().join("a.txt"), "x\n").unwrap();
        repo.stage_all_except(&[]).unwrap();
        assert_eq!(repo.staged_against("HEAD").unwrap(), ["a.txt"]);
    }

    #[test]
    fn lists_conflicts_outside_the_folder_whatever_diff_relative_says() {
        let (dir, _repo) = setup();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/s.txt"), "s\n").unwrap();
        fs::write(dir.path().join("f.txt"), "base\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "base"]);
        fs::write(dir.path().join("f.txt"), "stashed\n").unwrap();
        run(dir.path(), &["stash", "-q"]);
        fs::write(dir.path().join("f.txt"), "theirs\n").unwrap();
        run(dir.path(), &["commit", "-q", "-a", "-m", "theirs"]);
        let popped = Command::new("git")
            .current_dir(dir.path())
            .args(["stash", "pop", "-q"])
            .output()
            .unwrap();
        assert!(!popped.status.success());
        run(dir.path(), &["config", "diff.relative", "true"]);
        let repo = Repo::open(dir.path().join("sub")).unwrap();
        assert_eq!(repo.unmerged_paths().unwrap(), ["f.txt"]);
        fs::write(dir.path().join("sub/s.txt"), "s2\n").unwrap();
        run(dir.path(), &["add", "sub/s.txt"]);
        assert_eq!(repo.staged_paths().unwrap(), ["f.txt", "sub/s.txt"]);
    }

    #[test]
    fn reads_all_the_changes_from_a_subfolder_whatever_diff_relative_says() {
        let (dir, _repo) = setup();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/s.txt"), "s\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "add sub"]);
        run(dir.path(), &["config", "diff.relative", "true"]);
        let repo = Repo::open(dir.path().join("sub")).unwrap();

        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        let (scope, changes) = repo.changes(&[]).unwrap().expect("a change outside sub/");
        assert_eq!(scope, Scope::Tracked);
        assert!(changes.diff.contains("+++ b/lib.rs"), "{}", changes.diff);
        assert!(changes.stat.contains("lib.rs"), "{}", changes.stat);

        fs::write(dir.path().join("sub/s.txt"), "s2\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        let (scope, changes) = repo.changes(&[]).unwrap().unwrap();
        assert_eq!(scope, Scope::Staged);
        for name in ["lib.rs", "sub/s.txt"] {
            assert!(
                changes.diff.contains(&format!("+++ b/{name}")),
                "{}",
                changes.diff
            );
        }
        let patch = repo.patch(Scope::Staged).unwrap();
        assert!(patch.contains("+++ b/lib.rs") && patch.contains("+++ b/sub/s.txt"));
        assert_eq!(
            repo.included_paths(Scope::Staged, &[]).unwrap(),
            HashSet::from(["lib.rs".to_owned(), "sub/s.txt".to_owned()])
        );
    }

    #[test]
    fn commits_tracked_changes_without_a_pointer_it_leaves_out() {
        let (dir, repo, _library, moved) = test_support::with_submodule();
        run(dir.path(), &["config", "submodule.library.ignore", "all"]);
        fs::write(dir.path().join("gone.txt"), "x\n").unwrap();
        run(dir.path(), &["add", "gone.txt"]);
        run(dir.path(), &["commit", "-q", "-m", "add gone"]);
        test_support::move_pointer(dir.path(), &moved);
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        fs::remove_file(dir.path().join("gone.txt")).unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let from_sub = Repo::open(dir.path().join("sub")).unwrap();

        from_sub
            .commit("feat: b", Scope::Tracked, &[], &["library".to_owned()])
            .unwrap();
        assert_eq!(
            repo.changed_between("HEAD~", "HEAD").unwrap(),
            ["gone.txt", "lib.rs"]
        );
        assert_eq!(repo.hidden_changed_pointers().unwrap(), ["library"]);
    }

    /// A repository with a moved submodule pointer under `ignore=all` and an edited lib.rs.
    fn with_a_hidden_pointer() -> (TempDir, Repo, TempDir) {
        let (dir, repo, library, moved) = test_support::with_submodule();
        run(dir.path(), &["config", "submodule.library.ignore", "all"]);
        test_support::move_pointer(dir.path(), &moved);
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        (dir, repo, library)
    }

    #[test]
    #[cfg(unix)]
    fn leaving_a_pointer_out_keeps_what_a_formatting_hook_did() {
        let (dir, repo, _library) = with_a_hidden_pointer();
        let hooks = dir.path().join("hooks");
        fs::create_dir(&hooks).unwrap();
        fs::write(
            hooks.join("pre-commit"),
            "#!/bin/sh\nprintf 'fn b() { }\\n' > lib.rs\ngit add lib.rs\n",
        )
        .unwrap();
        let executable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        fs::set_permissions(hooks.join("pre-commit"), executable).unwrap();
        run(dir.path(), &["config", "core.hooksPath", "hooks"]);

        repo.commit("feat: b", Scope::Tracked, &[], &["library".to_owned()])
            .unwrap();
        assert_eq!(run(dir.path(), &["show", "HEAD:lib.rs"]), "fn b() { }\n");
        assert!(repo.staged_against("HEAD").unwrap().is_empty());
    }

    #[test]
    fn leaving_a_pointer_out_doesnt_commit_an_assume_unchanged_file() {
        let (dir, repo, _library) = with_a_hidden_pointer();
        fs::write(dir.path().join("quiet.txt"), "one\n").unwrap();
        run(dir.path(), &["add", "quiet.txt"]);
        run(dir.path(), &["commit", "-q", "-m", "add quiet"]);
        run(
            dir.path(),
            &["update-index", "--assume-unchanged", "quiet.txt"],
        );
        fs::write(dir.path().join("quiet.txt"), "two\n").unwrap();
        fs::write(dir.path().join("lib.rs"), "fn c() {}\n").unwrap();

        repo.commit("feat: c", Scope::Tracked, &[], &["library".to_owned()])
            .unwrap();
        assert_eq!(run(dir.path(), &["show", "HEAD:quiet.txt"]), "one\n");
        assert_eq!(run(dir.path(), &["show", "HEAD:lib.rs"]), "fn c() {}\n");
    }

    #[test]
    fn commits_the_version_of_a_pointer_the_user_staged_and_not_a_later_one() {
        let (dir, repo, library, moved) = test_support::with_submodule();
        run(dir.path(), &["config", "submodule.library.ignore", "all"]);
        test_support::move_pointer(dir.path(), &moved);
        run(dir.path(), &["add", "library"]);
        run(
            library.path(),
            &["commit", "-q", "--allow-empty", "-m", "three"],
        );
        let later = run(library.path(), &["rev-parse", "HEAD"])
            .trim()
            .to_owned();
        test_support::move_pointer(dir.path(), &later);
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        assert_eq!(repo.hidden_changed_pointers().unwrap(), ["library"]);

        repo.commit("feat: b", Scope::Tracked, &[], &["library".to_owned()])
            .unwrap();
        assert_eq!(
            run(dir.path(), &["rev-parse", "HEAD:library"]).trim(),
            moved
        );
        assert_eq!(run(dir.path(), &["show", "HEAD:lib.rs"]), "fn b() {}\n");
        assert_eq!(repo.hidden_changed_pointers().unwrap(), ["library"]);
    }

    #[test]
    fn lists_a_conflicted_submodule_under_any_ignore_setting() {
        let (dir, repo, _library, moved) = test_support::with_submodule();
        let base = run(dir.path(), &["rev-parse", "HEAD:library"]);
        let info = format!(
            "0 {zero} 0\tlibrary\n160000 {base} 1\tlibrary\n160000 {moved} 2\tlibrary\n\
             160000 {base} 3\tlibrary\n",
            zero = "0".repeat(40),
            base = base.trim()
        );
        let mut child = Command::new("git")
            .current_dir(dir.path())
            .args(["update-index", "--index-info"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(info.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
        for (key, value) in [
            ("submodule.library.ignore", "all"),
            ("diff.ignoreSubmodules", "all"),
        ] {
            run(dir.path(), &["config", key, value]);
            assert_eq!(repo.unmerged_paths().unwrap(), ["library"], "{key}");
        }
    }

    #[test]
    fn lists_unmerged_files_once_each_with_their_real_names() {
        let (dir, repo) = setup();
        fs::write(dir.path().join("Kjøre.txt"), "base\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "base"]);
        fs::write(dir.path().join("Kjøre.txt"), "stashed\n").unwrap();
        run(dir.path(), &["stash", "-q"]);
        fs::write(dir.path().join("Kjøre.txt"), "theirs\n").unwrap();
        run(dir.path(), &["commit", "-q", "-a", "-m", "theirs"]);
        let popped = Command::new("git")
            .current_dir(dir.path())
            .args(["stash", "pop", "-q"])
            .output()
            .unwrap();
        assert!(!popped.status.success());
        assert_eq!(repo.unmerged_paths().unwrap(), ["Kjøre.txt"]);
    }

    #[test]
    fn finds_pointers_the_diff_hides_under_a_submodule_setting() {
        let (dir, repo, _library, moved) = test_support::with_submodule();
        run(dir.path(), &["config", "submodule.library.ignore", "all"]);
        test_support::move_pointer(dir.path(), &moved);
        assert!(!repo.has_diff(&[]).unwrap());
        assert_eq!(repo.hidden_changed_pointers().unwrap(), ["library"]);
        assert!(repo.hidden_staged_pointers().unwrap().is_empty());

        run(dir.path(), &["add", "--all"]);
        assert!(!repo.has_diff(&["--cached"]).unwrap());
        assert_eq!(repo.hidden_staged_pointers().unwrap(), ["library"]);
    }

    #[test]
    fn finds_only_staged_pointers_under_the_diff_setting() {
        let (dir, repo, _library, moved) = test_support::with_submodule();
        run(dir.path(), &["config", "diff.ignoreSubmodules", "all"]);
        test_support::move_pointer(dir.path(), &moved);
        assert!(repo.hidden_changed_pointers().unwrap().is_empty());
        assert!(repo.hidden_staged_pointers().unwrap().is_empty());

        run(dir.path(), &["add", "--all"]);
        assert_eq!(repo.hidden_staged_pointers().unwrap(), ["library"]);
    }

    #[test]
    fn finds_no_hidden_pointers_when_the_diff_shows_them_or_nothing_moved() {
        let (dir, repo, _library, moved) = test_support::with_submodule();
        assert!(repo.hidden_changed_pointers().unwrap().is_empty());
        fs::write(dir.path().join("library/loose.txt"), "dirty\n").unwrap();
        run(dir.path(), &["config", "submodule.library.ignore", "all"]);
        assert!(repo.hidden_changed_pointers().unwrap().is_empty());
        run(
            dir.path(),
            &["config", "--unset", "submodule.library.ignore"],
        );
        fs::remove_file(dir.path().join("library/loose.txt")).unwrap();
        test_support::move_pointer(dir.path(), &moved);
        assert!(repo.hidden_changed_pointers().unwrap().is_empty());
        run(dir.path(), &["add", "--all"]);
        assert!(repo.hidden_staged_pointers().unwrap().is_empty());
    }

    #[test]
    fn lists_a_hidden_pointer_a_commit_changed() {
        let (dir, repo, _library, moved) = test_support::with_submodule();
        run(dir.path(), &["config", "submodule.library.ignore", "all"]);
        test_support::move_pointer(dir.path(), &moved);
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "move the library"]);
        assert_eq!(repo.changed_between("HEAD~", "HEAD").unwrap(), ["library"]);
    }
}
