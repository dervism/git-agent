//! Pushing a branch: where a bare `git push` of it would go, what each place it goes to has
//! already, the commits that came in from its upstream, and a push that sends one checked commit
//! there and nothing else.

use super::Repo;
use super::settings::git_bool;
use crate::safe_text::one_line;
use crate::ui;
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::process::{Command, Stdio};

/// Where the current branch is pushed: a remote, and the branch there, and what the push does
/// with submodules.
#[derive(Debug, Clone, PartialEq)]
pub struct Destination {
    pub remote: String,
    pub branch: String,
    local: String,
    upstream: Option<Upstream>,
    submodules: Submodules,
}

impl Destination {
    /// The local branch pushed.
    pub fn local(&self) -> &str {
        &self.local
    }

    /// Whether the push is to make the branch track the destination: it has no upstream yet.
    pub fn sets_upstream(&self) -> bool {
        self.upstream.is_none()
    }
}

/// The branch a local branch pulls from, as `branch.<name>.remote` and `branch.<name>.merge`
/// name it.
#[derive(Debug, Clone, PartialEq)]
struct Upstream {
    remote: String,
    branch: String,
}

/// What a bare `git push` does by `push.default`, `simple`, git's default, when it isn't set.
#[derive(Debug, Clone, Copy, PartialEq)]
enum PushDefault {
    Nothing,
    Current,
    Upstream,
    Simple,
    Matching,
}

impl PushDefault {
    /// `push.default` set to `value`, or not set. A value git doesn't take fails, as git push
    /// would, before anything is fetched or checked.
    fn from_setting(value: Option<&str>) -> Result<Self> {
        Ok(match value {
            None | Some("simple") => Self::Simple,
            Some("nothing") => Self::Nothing,
            Some("current") => Self::Current,
            Some("upstream" | "tracking") => Self::Upstream,
            Some("matching") => Self::Matching,
            Some(other) => bail!(
                "push.default is {other:?}, which git doesn't take; set it to nothing, matching, \
                 simple, upstream or current"
            ),
        })
    }
}

/// What a push does with the commits of submodules. git-agent never pushes them: where the user's
/// settings ask git to push or check them, git checks that their remotes have them, and otherwise
/// they're left alone, as git leaves them.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Submodules {
    Checked,
    Unchecked,
}

/// The settings that say what git push does with submodules, as `git config` names them.
const SUBMODULE_SETTINGS: &str = r"^(push\.recursesubmodules|submodule\.recurse)$";

impl Submodules {
    /// What the `settings` ask of a push, each a name as `git config` lists it with its value,
    /// `None` for a name set without one, in the order git reads them. git push takes each in
    /// turn, so the last decides, and since git 2.45 it fails on any value it doesn't take; so
    /// does this, where older gits read only the last value of `push.recurseSubmodules`.
    fn asked_by<'a>(
        settings: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    ) -> Result<Self> {
        settings
            .into_iter()
            .try_fold(Self::Unchecked, |_, (name, value)| {
                Self::asked_by_one(name, value)
            })
    }

    /// What `name` set to `value` asks: `check`, `on-demand` and `only` ask git to push or check
    /// submodules, and so does `submodule.recurse` when it's true.
    fn asked_by_one(name: &str, value: Option<&str>) -> Result<Self> {
        let of_push = name == "push.recursesubmodules";
        let asks = match value {
            Some("check" | "on-demand" | "only") if of_push => Some(true),
            value if of_push => git_bool(value).filter(|asks| !asks),
            value => git_bool(value),
        };
        let shown = value.map_or("set without a value".to_owned(), |value| {
            format!("{value:?}")
        });
        match asks {
            Some(true) => Ok(Self::Checked),
            Some(false) => Ok(Self::Unchecked),
            None if of_push => bail!(
                "push.recurseSubmodules is {shown}, which git push doesn't take; set it to check, \
                 on-demand, only or no"
            ),
            None => bail!(
                "submodule.recurse is {shown}, which git doesn't take; set it to true or false"
            ),
        }
    }

    /// The `git push` option that does this.
    fn option(self) -> &'static str {
        match self {
            Self::Checked => "--recurse-submodules=check",
            Self::Unchecked => "--recurse-submodules=no",
        }
    }
}

impl Repo {
    /// Where a bare `git push` of the current branch would send it, by `push.default`,
    /// `branch.<name>.pushRemote` and `remote.pushDefault`, narrowed to the branch itself where
    /// `matching` would push others too. A branch without an upstream goes, as on its first push,
    /// under its own name to its push remote, else to the only remote, or `origin`. A setting git
    /// push wouldn't take fails here, before anything is fetched.
    pub fn push_destination(&self) -> Result<Destination> {
        let local = self.current_branch()?;
        let default = PushDefault::from_setting(self.setting("push.default")?.as_deref())?;
        let submodules = self.submodules_on_push()?;
        let upstream = self.upstream_of(&local)?;
        let remote = match self.push_remote_of(&local)? {
            Some(remote) => remote,
            None => match &upstream {
                Some(upstream) => upstream.remote.clone(),
                None => self.push_remote()?,
            },
        };
        let branch = match &upstream {
            Some(upstream) => branch_pushed(default, &local, &remote, upstream)?,
            None => local.clone(),
        };
        Ok(Destination {
            remote,
            branch,
            local,
            upstream,
            submodules,
        })
    }

    /// Fetches what a push of `destination` needs to know: the destination's remote, pruned, so
    /// that its remote-tracking refs are the branches it has now, and the upstream's remote when
    /// that's another one, for the commits that came in there. Local tags stay, whatever
    /// `fetch.pruneTags` says.
    pub fn fetch_for_push(&self, destination: &Destination) -> Result<()> {
        let pruned = ["--prune", "--no-prune-tags"];
        self.fetch_from(&destination.remote, &pruned, &[])?;
        match &destination.upstream {
            Some(upstream) if upstream.remote != destination.remote => {
                self.fetch_from(&upstream.remote, &[], &[])
            }
            _ => Ok(()),
        }
    }

    /// What each place a push of `destination` goes to has published already: for each of its
    /// push URLs, the tips of its branches that are here, each once. Where the push goes to the
    /// URL the fetch comes from, and the fetch takes every branch as git sets a remote up,
    /// [`Repo::fetch_for_push`] has left them in the remote-tracking refs. Any other push URL is
    /// asked with `git ls-remote`, since then those refs say nothing of what it has; a tip that
    /// isn't here can't be walked, so the check reads more, never less.
    pub fn published_tips(&self, destination: &Destination) -> Result<Vec<Vec<String>>> {
        let remote = &destination.remote;
        let pushed_to = self.urls(remote, true)?;
        let fetched_from = self.urls(remote, false)?;
        if pushed_to[..] == fetched_from[..fetched_from.len().min(1)]
            && self.fetches_every_branch(remote)?
        {
            return Ok(vec![self.branch_tips(remote)?]);
        }
        pushed_to.iter().map(|url| self.tips_at(url)).collect()
    }

    /// The commits `remote`'s branches are at, as its remote-tracking refs hold them, each once.
    /// A remote whose name starts another's, such as `origin` and `origin/mirror`, keeps its
    /// refs apart.
    pub fn branch_tips(&self, remote: &str) -> Result<Vec<String>> {
        let own = format!("refs/remotes/{remote}/");
        let nested: Vec<String> = self
            .git(&["remote"])?
            .lines()
            .filter(|name| name.starts_with(&format!("{remote}/")))
            .map(|name| format!("refs/remotes/{name}/"))
            .collect();
        let refs = self.git(&[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/remotes/",
        ])?;
        let tips: BTreeSet<String> = refs
            .lines()
            .filter_map(|line| line.split_once(' '))
            .filter(|(_, name)| {
                name.starts_with(&own) && !nested.iter().any(|other| name.starts_with(other))
            })
            .map(|(hash, _)| hash.to_owned())
            .collect();
        Ok(tips.into_iter().collect())
    }

    /// The commits on the current branch's upstream that the branch lacks, as `<hash> <subject>`
    /// lines, as [`Repo::fetch_for_push`] left them. Empty without an upstream, or when its branch
    /// is gone from the remote.
    pub fn incoming_commits(&self) -> Result<Vec<String>> {
        if !self.has_upstream() {
            return Ok(Vec::new());
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

    /// Pushes `commit` to `destination`, and nothing else: no tags, whatever `push.followTags`
    /// says, and no submodule's commits, which are only checked where the user's settings ask
    /// git to push or check them.
    pub fn push_commit(&self, commit: &str, destination: &Destination) -> Result<()> {
        let stdout = ui::child_stdout_on_stderr().context("failed to give git push a stdout")?;
        let status = self
            .push_command(commit, destination, stdout)?
            .status()
            .context("failed to run git push")?;
        if !status.success() {
            bail!("`git push` failed ({status})");
        }
        Ok(())
    }

    /// The `git push` of [`Repo::push_commit`], with `stdout` for git's standard output. It isn't
    /// git-agent's own: a `pre-push` hook writes there. With the reader of that output gone, the
    /// hook would die, and the push with it.
    fn push_command(
        &self,
        commit: &str,
        destination: &Destination,
        stdout: Stdio,
    ) -> Result<Command> {
        let commit = super::object_hash(commit)?;
        let refspec = format!("{commit}:refs/heads/{}", destination.branch);
        let mut command = self.command(&[
            "push",
            "--no-follow-tags",
            destination.submodules.option(),
            "--",
            &destination.remote,
            &refspec,
        ]);
        command.stdout(stdout);
        Ok(command)
    }

    /// Makes the branch of `destination` track it, as `git push --set-upstream` does, once a push
    /// that [`Destination::sets_upstream`] has gone.
    pub fn set_upstream(&self, destination: &Destination) -> Result<()> {
        let key = |name: &str| format!("branch.{}.{name}", destination.local);
        let merge = format!("refs/heads/{}", destination.branch);
        self.git(&["config", &key("remote"), &destination.remote])?;
        self.git(&["config", &key("merge"), &merge])?;
        Ok(())
    }

    /// The URLs a push to `remote` goes to, with `push`, or those it's fetched from, the first
    /// one being where a fetch goes.
    fn urls(&self, remote: &str, push: bool) -> Result<Vec<String>> {
        let mut args = vec!["remote", "get-url", "--all"];
        if push {
            args.push("--push");
        }
        args.extend(["--", remote]);
        Ok(self.git(&args)?.lines().map(str::to_owned).collect())
    }

    /// Whether `remote` fetches every branch into its own remote-tracking refs, as git sets a
    /// remote up, and nothing else: then a pruned fetch leaves those refs as its branches.
    fn fetches_every_branch(&self, remote: &str) -> Result<bool> {
        let refspecs = self.settings(&format!("remote.{remote}.fetch"))?;
        Ok(refspecs == [format!("+refs/heads/*:refs/remotes/{remote}/*")])
    }

    /// The commits the branches at `url` are at, as `git ls-remote` lists them, those that are
    /// here, each once.
    fn tips_at(&self, url: &str) -> Result<Vec<String>> {
        let listed = self.git(&["ls-remote", "--heads", "--", url])?;
        let tips: BTreeSet<&str> = listed
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .map(|(hash, _)| hash)
            .collect();
        self.commits_here(&tips.into_iter().collect::<Vec<_>>())
    }

    /// The hashes among `hashes` of the commits this repository has.
    fn commits_here(&self, hashes: &[&str]) -> Result<Vec<String>> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let input: String = hashes
            .iter()
            .map(|hash| super::object_hash(hash).map(|hash| format!("{hash}\n")))
            .collect::<Result<_>>()?;
        let args = [
            "--no-replace-objects",
            "cat-file",
            "--batch-check=%(objectname) %(objecttype)",
        ];
        let output = self.git_with_input_and_output(&args, input.as_bytes(), &[0])?;
        Ok(String::from_utf8_lossy(&output)
            .lines()
            .filter_map(|line| line.strip_suffix(" commit"))
            .map(str::to_owned)
            .collect())
    }

    /// What a push does with submodules, by the user's settings.
    fn submodules_on_push(&self) -> Result<Submodules> {
        let settings = self.settings_matching(SUBMODULE_SETTINGS)?;
        Submodules::asked_by(
            settings
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_deref())),
        )
    }

    /// The upstream of `local`, if it has one. A branch that tracks another branch of this
    /// repository has no remote to push to.
    fn upstream_of(&self, local: &str) -> Result<Option<Upstream>> {
        let remote = self.setting(&format!("branch.{local}.remote"))?;
        let merge = self.setting(&format!("branch.{local}.merge"))?;
        let (Some(remote), Some(merge)) = (remote, merge) else {
            return Ok(None);
        };
        if remote == "." {
            bail!("{local} tracks a branch of this repository, so it has no remote to push to");
        }
        let branch = merge
            .strip_prefix("refs/heads/")
            .unwrap_or(&merge)
            .to_owned();
        Ok(Some(Upstream { remote, branch }))
    }

    /// The remote a bare `git push` of `local` would use: its `pushRemote`, `remote.pushDefault`,
    /// or the remote it pulls from.
    fn push_remote_of(&self, local: &str) -> Result<Option<String>> {
        let branch = format!("refs/heads/{local}");
        let remote = self.git(&["for-each-ref", "--format=%(push:remotename)", &branch])?;
        Ok(Some(remote.trim().to_owned()).filter(|remote| !remote.is_empty()))
    }

    pub(super) fn has_upstream(&self) -> bool {
        self.git(&[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ])
        .is_ok()
    }

    /// `remote.pushDefault` if set, else the only remote, else `origin`.
    pub(super) fn push_remote(&self) -> Result<String> {
        if let Some(remote) = self.setting("remote.pushDefault")? {
            return Ok(remote);
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

/// The branch on `remote` that a bare `git push` of `local`, whose upstream is `upstream`, goes to
/// by `default`, as git chooses it, for this branch alone. Where git would push nothing, so does
/// this.
fn branch_pushed(
    default: PushDefault,
    local: &str,
    remote: &str,
    upstream: &Upstream,
) -> Result<String> {
    let to_upstream = remote == upstream.remote;
    match default {
        PushDefault::Nothing => bail!(
            "push.default is nothing, so git pushes a branch only when told which; push it with \
             `git push {remote} {local}`"
        ),
        PushDefault::Current | PushDefault::Matching => Ok(local.to_owned()),
        PushDefault::Upstream if to_upstream => Ok(upstream.branch.clone()),
        PushDefault::Upstream => bail!(
            "push.default is upstream, but {local} would go to {remote}, and its upstream is on \
             {}; push it with `git push {remote} {local}`",
            upstream.remote
        ),
        PushDefault::Simple if !to_upstream || upstream.branch == local => Ok(local.to_owned()),
        PushDefault::Simple => bail!(
            "{local}'s upstream, {remote}/{}, has another name, so git won't push it there with \
             push.default simple; push it with `git push {remote} HEAD:{}`, or set push.default \
             to upstream",
            upstream.branch,
            upstream.branch
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Scope;
    #[cfg(unix)]
    use crate::git::test_support::hook;
    use crate::git::test_support::{
        add_remote, commit_elsewhere, isolate, isolated_repo, push, run, with_submodule,
    };
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

    fn path(remote: &TempDir) -> &str {
        remote.path().to_str().unwrap()
    }

    /// A repository with `origin` as a bare repository that holds nothing yet, so that the
    /// branch has no upstream to push to.
    fn with_an_empty_remote() -> (TempDir, Repo, TempDir) {
        let (dir, repo) = isolated_repo();
        let remote = bare();
        run(dir.path(), &["remote", "add", "origin", path(&remote)]);
        (dir, repo, remote)
    }

    fn remote_has(remote: &TempDir, branch: &str) -> bool {
        !run(remote.path(), &["branch", "--list", branch]).is_empty()
    }

    fn head(repo: &Repo) -> String {
        repo.head_commit().unwrap().unwrap()
    }

    fn commit(dir: &Path, repo: &Repo, content: &str, subject: &str) -> String {
        fs::write(dir.join("lib.rs"), content).unwrap();
        repo.commit(subject, Scope::Tracked, &[], &[]).unwrap();
        head(repo)
    }

    #[test]
    fn push_sets_upstream_then_reuses_it() {
        let (dir, repo) = isolated_repo();
        let remote = add_remote(&repo);
        assert!(repo.has_upstream());
        commit(dir.path(), &repo, "fn b() {}\n", "feat: second");

        assert!(repo.incoming_commits().unwrap().is_empty());
        assert!(!repo.push_destination().unwrap().sets_upstream());
        push(&repo);

        let pushed = run(remote.path(), &["log", "--format=%s", "-1", "main"]);
        assert_eq!(pushed.trim(), "feat: second");
    }

    /// A first push goes, under the branch's own name, to the only remote, and the branch tracks
    /// it afterwards.
    #[test]
    fn a_first_push_sets_the_upstream_to_where_it_went() {
        let (dir, repo, remote) = with_an_empty_remote();
        let destination = repo.push_destination().unwrap();

        repo.push_commit(&head(&repo), &destination).unwrap();
        repo.set_upstream(&destination).unwrap();

        assert!(destination.sets_upstream());
        let to = (destination.remote.as_str(), destination.branch.as_str());
        assert_eq!(to, ("origin", "main"));
        assert!(remote_has(&remote, "main"));
        let upstream = run(dir.path(), &["rev-parse", "--abbrev-ref", "@{upstream}"]);
        assert_eq!(upstream.trim(), "origin/main");
    }

    /// The commit pushed is the one it's given, the one that was checked, even when HEAD has
    /// moved on since.
    #[test]
    fn pushes_the_commit_it_is_given_and_not_where_head_is_now() {
        let (dir, repo, remote) = with_an_empty_remote();
        let checked = head(&repo);
        commit(
            dir.path(),
            &repo,
            "fn later() {}\n",
            "feat: after the check",
        );

        repo.push_commit(&checked, &repo.push_destination().unwrap())
            .unwrap();

        assert_eq!(run(remote.path(), &["rev-parse", "main"]).trim(), checked);
    }

    /// A pre-push hook writes to git's stdout, so with the reader of that output gone the hook
    /// would die, and the push with it: that's why git is given a stdout of its own.
    #[cfg(unix)]
    #[test]
    fn a_pre_push_hook_that_writes_to_a_stdout_nobody_reads_stops_the_push() {
        let (dir, repo, remote) = with_an_empty_remote();
        hook(dir.path(), "pre-push", "echo checking\n");
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let destination = repo.push_destination().unwrap();

        let status = repo
            .push_command(&head(&repo), &destination, Stdio::from(writer))
            .unwrap()
            .status()
            .unwrap();

        assert!(!status.success());
        assert!(!remote_has(&remote, "main"));
    }

    #[cfg(unix)]
    #[test]
    fn a_pre_push_hook_writes_to_the_stream_it_is_given() {
        let (dir, repo, remote) = with_an_empty_remote();
        hook(dir.path(), "pre-push", "echo checking\n");
        let (mut reader, writer) = std::io::pipe().unwrap();
        let destination = repo.push_destination().unwrap();

        let status = repo
            .push_command(&head(&repo), &destination, Stdio::from(writer))
            .unwrap()
            .status()
            .unwrap();

        assert!(status.success());
        assert!(remote_has(&remote, "main"));
        let mut written = String::new();
        std::io::Read::read_to_string(&mut reader, &mut written).unwrap();
        assert_eq!(written, "checking\n");
    }

    #[test]
    fn a_push_writes_nothing_to_stdout_so_it_survives_a_closed_one() {
        let (_dir, repo, remote) = with_an_empty_remote();
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let destination = repo.push_destination().unwrap();

        let status = repo
            .push_command(&head(&repo), &destination, Stdio::from(writer))
            .unwrap()
            .status()
            .unwrap();

        assert!(status.success());
        assert!(remote_has(&remote, "main"));
    }

    #[test]
    fn push_without_remote_explains_how_to_add_one() {
        let (_dir, repo) = isolated_repo();
        assert!(repo.incoming_commits().unwrap().is_empty());
        let error = repo.push_destination().unwrap_err();
        assert!(error.to_string().contains("gh repo create"), "{error}");
    }

    #[test]
    fn rebases_onto_remote_commits_keeping_uncommitted_changes() {
        let (dir, repo) = isolated_repo();
        let remote = add_remote(&repo);
        commit_elsewhere(&remote, "LICENSE", "MIT\n", "docs: add license");
        commit(dir.path(), &repo, "fn b() {}\n", "feat: local");
        fs::write(dir.path().join("lib.rs"), "fn uncommitted() {}\n").unwrap();

        repo.fetch_for_push(&repo.push_destination().unwrap())
            .unwrap();
        let incoming = repo.incoming_commits().unwrap();
        assert_eq!(incoming.len(), 1);
        assert!(incoming[0].ends_with(" docs: add license"));
        repo.rebase_onto_upstream().unwrap();
        push(&repo);

        let history = run(remote.path(), &["log", "--format=%s", "main"]);
        assert_eq!(history, "feat: local\ndocs: add license\ninit\n");
        let lib = fs::read_to_string(dir.path().join("lib.rs")).unwrap();
        assert_eq!(lib, "fn uncommitted() {}\n");
    }

    #[test]
    fn conflicting_rebase_is_cancelled() {
        let (dir, repo) = isolated_repo();
        let remote = add_remote(&repo);
        commit_elsewhere(&remote, "lib.rs", "fn remote() {}\n", "fix: remote");
        let before = commit(dir.path(), &repo, "fn local() {}\n", "fix: local");

        repo.fetch_for_push(&repo.push_destination().unwrap())
            .unwrap();
        assert_eq!(repo.incoming_commits().unwrap().len(), 1);
        let error = repo.rebase_onto_upstream().unwrap_err();

        assert!(
            error.to_string().contains("Merge conflict in lib.rs"),
            "{error}"
        );
        assert_eq!(head(&repo), before);
        assert!(!repo.git_path("rebase-merge").unwrap().exists());
        assert!(repo.changes(&[]).unwrap().is_none());
    }

    /// The branch a bare push goes to under each `push.default`, for `feature` with an upstream
    /// on `origin`, pushed to `origin` or to `fork`. Where git would push nothing, so does this.
    #[test]
    fn chooses_the_branch_a_bare_push_would_go_to_under_each_push_default() {
        let same = Upstream {
            remote: "origin".to_owned(),
            branch: "feature".to_owned(),
        };
        let other = Upstream {
            branch: "other".to_owned(),
            ..same.clone()
        };
        let pushed = |default, remote, upstream: &Upstream| {
            branch_pushed(default, "feature", remote, upstream).ok()
        };
        let feature = Some("feature".to_owned());
        let (simple, upstream) = (PushDefault::Simple, PushDefault::Upstream);

        assert_eq!(pushed(simple, "origin", &same), feature);
        assert_eq!(pushed(simple, "origin", &other), None);
        assert_eq!(pushed(simple, "fork", &other), feature);
        assert_eq!(pushed(PushDefault::Current, "origin", &other), feature);
        assert_eq!(pushed(PushDefault::Matching, "origin", &other), feature);
        assert_eq!(pushed(upstream, "origin", &other), Some("other".to_owned()));
        assert_eq!(pushed(upstream, "fork", &other), None);
        assert_eq!(pushed(PushDefault::Nothing, "origin", &same), None);
    }

    /// `push.default` is read as git reads it, `tracking` being `upstream`, and a value git
    /// doesn't take fails before anything is fetched.
    #[test]
    fn reads_push_default_as_git_does() {
        let read = |value| PushDefault::from_setting(value).ok();

        assert_eq!(read(None), Some(PushDefault::Simple));
        assert_eq!(read(Some("tracking")), Some(PushDefault::Upstream));
        assert_eq!(read(Some("nothing")), Some(PushDefault::Nothing));
        for wrong in ["bogus", "Simple", ""] {
            assert_eq!(read(Some(wrong)), None, "{wrong:?}");
        }
    }

    /// A setting's name, as `git config` lists it, and its value, `None` for a name set alone.
    type Setting<'a> = (&'a str, Option<&'a str>);

    /// Submodules are checked where the last of the settings asks git to push or check them,
    /// never pushed, and a value git push doesn't take fails, wherever it is.
    #[test]
    fn checks_submodules_where_the_last_setting_asks_to_push_or_check_them() {
        const PUSH: &str = "push.recursesubmodules";
        const RECURSE: &str = "submodule.recurse";
        let (checked, unchecked) = (Some(Submodules::Checked), Some(Submodules::Unchecked));
        let cases: [(&[Setting], Option<Submodules>); 23] = [
            (&[], unchecked),
            (&[(PUSH, Some("check"))], checked),
            (&[(PUSH, Some("on-demand"))], checked),
            (&[(PUSH, Some("only"))], checked),
            (&[(PUSH, Some("no"))], unchecked),
            (&[(PUSH, Some("false"))], unchecked),
            (&[(PUSH, Some("Off"))], unchecked),
            (&[(PUSH, Some("0"))], unchecked),
            (&[(PUSH, Some(""))], unchecked),
            (&[(PUSH, Some("0x0"))], unchecked),
            (&[(PUSH, Some("Check"))], None),
            (&[(PUSH, Some("yes"))], None),
            (&[(PUSH, Some("true"))], None),
            (&[(PUSH, Some("1"))], None),
            (&[(PUSH, None)], None),
            (&[(RECURSE, Some("true"))], checked),
            (&[(RECURSE, None)], checked),
            (&[(RECURSE, Some("off"))], unchecked),
            (&[(RECURSE, Some("maybe"))], None),
            (&[(PUSH, Some("no")), (RECURSE, Some("1"))], checked),
            (&[(RECURSE, Some("1")), (PUSH, Some("no"))], unchecked),
            (&[(PUSH, Some("bogus")), (PUSH, Some("check"))], None),
            (&[(RECURSE, Some("maybe")), (RECURSE, Some("on"))], None),
        ];

        for (settings, expected) in cases {
            let asked = Submodules::asked_by(settings.iter().copied()).ok();
            assert_eq!(asked, expected, "{settings:?}");
        }
    }

    /// What `git push --dry-run` in `dir` does with submodules: `Some(true)` where it pushes or
    /// checks them, `Some(false)` where it leaves them alone, and `None` where it refuses its
    /// settings.
    fn what_git_does_with_submodules(dir: &Path) -> Option<bool> {
        let output = std::process::Command::new("git")
            .current_dir(dir)
            .env("LC_ALL", "C")
            .args(["push", "--dry-run", "origin", "HEAD:refs/heads/main"])
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&output.stderr);
        if said.contains("fatal: bad ") {
            None
        } else if said.contains("submodule") {
            Some(true)
        } else {
            assert!(output.status.success(), "{said}");
            Some(false)
        }
    }

    /// `entries`, each `name=value` or a name alone, as a settings file that sets them in that
    /// order.
    fn settings_file(entries: &[&str]) -> String {
        entries
            .iter()
            .map(|entry| {
                let (name, value) = entry
                    .split_once('=')
                    .map_or((*entry, None), |(name, value)| (name, Some(value)));
                let (section, key) = name.split_once('.').unwrap();
                match value {
                    Some(value) => format!("[{section}]\n\t{key} = {value}\n"),
                    None => format!("[{section}]\n\t{key}\n"),
                }
            })
            .collect()
    }

    /// A repository whose last commit moves its submodule to a commit the submodule's remote
    /// lacks, with `origin` to push to.
    fn with_a_submodule_commit_not_pushed() -> (TempDir, Repo, TempDir, TempDir) {
        let (dir, repo, library, _moved) = with_submodule();
        let remote = add_remote(&repo);
        let checkout = dir.path().join("library");
        isolate(&checkout);
        run(&checkout, &["commit", "-q", "--allow-empty", "-m", "three"]);
        run(dir.path(), &["add", "library"]);
        repo.commit("chore: move the library", Scope::Staged, &[], &[])
            .unwrap();
        (dir, repo, library, remote)
    }

    /// Settings for submodules, in the order they're set, each `name=value` or a name alone, and
    /// whether git push checks or pushes submodules with them: `None` where it refuses them.
    /// Only cases every git git-agent runs with agrees on; the pure test has the others.
    const SUBMODULE_CASES: [(&[&str], Option<bool>); 16] = [
        (&[], Some(false)),
        (&["push.recurseSubmodules=check"], Some(true)),
        (&["push.recurseSubmodules=on-demand"], Some(true)),
        (&["push.recurseSubmodules=only"], Some(true)),
        (&["push.recurseSubmodules=no"], Some(false)),
        (&["push.recurseSubmodules="], Some(false)),
        (&["push.recurseSubmodules"], None),
        (&["push.recurseSubmodules=yes"], None),
        (&["push.recurseSubmodules=Check"], None),
        (&["submodule.recurse=true"], Some(true)),
        (&["submodule.recurse=0x0"], Some(false)),
        (&["submodule.recurse=maybe"], None),
        (
            &["push.recurseSubmodules=no", "submodule.recurse=true"],
            Some(true),
        ),
        (
            &["submodule.recurse=true", "push.recurseSubmodules=no"],
            Some(false),
        ),
        (
            &["push.recurseSubmodules=check", "submodule.recurse=false"],
            Some(false),
        ),
        (&["submodule.recurse=maybe", "submodule.recurse=true"], None),
    ];

    /// git-agent reads the settings for submodules as `git push --dry-run` does: in order, the last
    /// one deciding, and refusing any value git push refuses.
    #[test]
    fn reads_the_submodule_settings_as_git_push_does() {
        let (dir, repo, _library, _remote) = with_a_submodule_commit_not_pushed();
        let dir = dir.path();
        run(dir, &["config", "include.path", "case.config"]);

        for (entries, expected) in SUBMODULE_CASES {
            fs::write(dir.join(".git/case.config"), settings_file(entries)).unwrap();
            let ours = repo
                .push_destination()
                .ok()
                .map(|to| to.submodules == Submodules::Checked);
            let git = what_git_does_with_submodules(dir);
            assert_eq!((ours, git), (expected, expected), "{entries:?}");
        }
    }

    /// Where `git push --dry-run` of `feature` would go, as `remote/branch`, or `None` when git
    /// wouldn't push it.
    fn where_git_would_push(dir: &Path, remotes: &[(&str, &str)]) -> Option<String> {
        let args = ["push", "--dry-run", "--porcelain"];
        let output = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        let mut remote = None;
        for line in text.lines() {
            if let Some(url) = line.strip_prefix("To ") {
                remote = remotes
                    .iter()
                    .find(|(_, at)| *at == url)
                    .map(|(name, _)| *name);
            }
            let refspec = line.split('\t').nth(1).unwrap_or_default();
            if let Some(branch) = refspec.strip_prefix("refs/heads/feature:refs/heads/") {
                let pushed = remote.map(|remote| format!("{remote}/{branch}"));
                return pushed.filter(|_| output.status.success());
            }
        }
        None
    }

    /// Where git-agent and `git push --dry-run` send `feature`, whose upstream is `upstream`, with
    /// `settings`, given the `remotes`' paths, as `remote/branch`, or `None` for nowhere.
    fn destinations(
        repo: &Repo,
        remotes: &[(&str, &str)],
        upstream: &str,
        settings: &[(&str, &str)],
    ) -> (Option<String>, Option<String>) {
        let dir = repo.workdir();
        run(dir, &["branch", "-q", "-u", upstream]);
        for (key, value) in settings {
            run(dir, &["config", key, value]);
        }
        let ours = repo
            .push_destination()
            .ok()
            .map(|to| format!("{}/{}", to.remote, to.branch));
        let git = where_git_would_push(dir, remotes);
        for (key, _) in settings {
            run(dir, &["config", "--unset", key]);
        }
        (ours, git)
    }

    /// A case of where `feature` goes: its upstream, the settings, and where it goes with them.
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], Option<&'a str>);

    /// A repository on `feature`, with `origin` holding `feature` and `other`, and `fork` empty.
    fn with_origin_and_fork() -> (TempDir, Repo, TempDir, TempDir) {
        let (dir, repo) = isolated_repo();
        let (origin, fork) = (bare(), bare());
        let at = dir.path();
        run(at, &["remote", "add", "origin", path(&origin)]);
        run(at, &["remote", "add", "fork", path(&fork)]);
        run(at, &["switch", "-q", "-c", "feature"]);
        run(
            at,
            &["push", "-q", "origin", "feature:feature", "feature:other"],
        );
        (dir, repo, origin, fork)
    }

    /// `feature` goes where `git push --dry-run` sends it, under every setting that changes that,
    /// whichever of its branches `origin` has, and nowhere where git wouldn't push it.
    #[test]
    fn goes_where_a_bare_git_push_would_go() {
        let (_dir, repo, origin, fork) = with_origin_and_fork();
        let remotes = [("origin", path(&origin)), ("fork", path(&fork))];
        let fork_upstream = [
            ("push.default", "upstream"),
            ("branch.feature.pushRemote", "fork"),
        ];
        let default = |value| [("push.default", value)];
        let cases: [Case; 10] = [
            ("origin/feature", &[], Some("origin/feature")),
            ("origin/other", &[], None),
            (
                "origin/other",
                &[("branch.feature.pushRemote", "fork")],
                Some("fork/feature"),
            ),
            (
                "origin/feature",
                &[("remote.pushDefault", "fork")],
                Some("fork/feature"),
            ),
            ("origin/other", &default("current"), Some("origin/feature")),
            ("origin/other", &default("upstream"), Some("origin/other")),
            ("origin/other", &fork_upstream, None),
            ("origin/other", &default("matching"), Some("origin/feature")),
            ("origin/feature", &default("nothing"), None),
            ("origin/feature", &default("bogus"), None),
        ];

        for (upstream, settings, expected) in cases {
            let expected = expected.map(str::to_owned);
            let found = destinations(&repo, &remotes, upstream, settings);
            assert_eq!(
                found,
                (expected.clone(), expected),
                "{upstream} {settings:?}"
            );
        }
    }

    /// A first push goes to the branch's push remote when it has one, as git's would, and the
    /// branch tracks it afterwards.
    #[test]
    fn a_first_push_goes_to_the_branch_s_push_remote() {
        let (dir, repo, _origin) = with_an_empty_remote();
        let fork = bare();
        run(dir.path(), &["remote", "add", "fork", path(&fork)]);
        run(dir.path(), &["config", "branch.main.pushRemote", "fork"]);

        let destination = repo.push_destination().unwrap();

        let to = (destination.remote.as_str(), destination.branch.as_str());
        assert_eq!(to, ("fork", "main"));
        assert!(destination.sets_upstream());
    }

    /// Where the push goes to the URL the fetch comes from, by git's own fetch refspec, what the
    /// push target has is read from the remote-tracking refs.
    #[test]
    fn reads_the_remote_tracking_refs_when_the_push_goes_where_the_fetch_comes_from() {
        let (_dir, repo) = isolated_repo();
        let _origin = add_remote(&repo);

        let tips = repo.published_tips(&repo.push_destination().unwrap());

        assert_eq!(tips.unwrap(), [vec![head(&repo)]]);
    }

    /// A remote with a second URL pushes to both, so each is asked what it has, one list each.
    #[test]
    fn asks_each_url_a_push_goes_to_what_it_has() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let _origin = add_remote(&repo);
        let first = head(&repo);
        let later = commit(dir, &repo, "fn b() {}\n", "feat: b");
        run(dir, &["push", "-q", "origin", "main"]);
        let second = bare();
        let refspec = format!("{first}:refs/heads/main");
        run(dir, &["push", "-q", path(&second), &refspec]);
        run(
            dir,
            &["config", "--add", "remote.origin.url", path(&second)],
        );

        let tips = repo.published_tips(&repo.push_destination().unwrap());

        assert_eq!(tips.unwrap(), [vec![later], vec![first]]);
    }

    /// The fetch makes the remote-tracking refs the remote's branches as they are now: a branch
    /// deleted on the remote no longer counts as one it has. Its tips come once each, though
    /// `origin/HEAD` names `origin/main` again.
    #[test]
    fn fetches_the_branches_the_remote_has_now() {
        let (dir, repo) = isolated_repo();
        let remote = add_remote(&repo);
        let main = head(&repo);
        commit(dir.path(), &repo, "fn old() {}\n", "feat: old");
        run(dir.path(), &["push", "-q", "origin", "HEAD:refs/heads/old"]);
        run(remote.path(), &["branch", "-q", "-D", "old"]);
        assert_eq!(repo.branch_tips("origin").unwrap().len(), 2);

        repo.fetch_for_push(&repo.push_destination().unwrap())
            .unwrap();

        assert_eq!(repo.branch_tips("origin").unwrap(), [main]);
    }

    /// The branches of a remote named after another, such as `origin/mirror`, sit under
    /// `refs/remotes/origin/` too, but don't count as `origin`'s.
    #[test]
    fn keeps_apart_the_branches_of_a_remote_whose_name_starts_another_s() {
        let (dir, repo) = isolated_repo();
        let _origin = add_remote(&repo);
        let first = head(&repo);
        let mirror = bare();
        run(
            dir.path(),
            &["remote", "add", "origin/mirror", path(&mirror)],
        );
        let later = commit(dir.path(), &repo, "fn b() {}\n", "feat: b");
        run(
            dir.path(),
            &["push", "-q", "origin/mirror", "HEAD:refs/heads/main"],
        );

        assert_eq!(repo.branch_tips("origin").unwrap(), [first]);
        assert_eq!(repo.branch_tips("origin/mirror").unwrap(), [later]);
    }

    /// The branch alone is pushed: no other branch, though `push.default` is `matching`, and no
    /// tag, though `push.followTags` asks for them.
    #[test]
    fn pushes_only_the_branch_whatever_push_default_and_follow_tags_say() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let remote = add_remote(&repo);
        run(dir, &["push", "-q", "origin", "main:side"]);
        let side = head(&repo);
        run(dir, &["switch", "-q", "-c", "side"]);
        commit(dir, &repo, "fn side() {}\n", "feat: side");
        run(dir, &["switch", "-q", "main"]);
        let main = commit(dir, &repo, "fn b() {}\n", "feat: b");
        run(dir, &["tag", "-a", "v1", "-m", "v1"]);
        run(dir, &["config", "push.default", "matching"]);
        run(dir, &["config", "push.followTags", "true"]);

        push(&repo);

        assert_eq!(run(remote.path(), &["rev-parse", "main"]).trim(), main);
        assert_eq!(run(remote.path(), &["rev-parse", "side"]).trim(), side);
        assert!(run(remote.path(), &["tag"]).is_empty());
    }

    /// A commit of a submodule that its remote lacks stops the push where the user's settings
    /// ask git to push or check submodules, and is never pushed itself; without such settings
    /// the push goes, as git's does.
    #[test]
    fn checks_submodules_only_where_the_settings_ask_git_to() {
        let (dir, repo, _library, _remote) = with_a_submodule_commit_not_pushed();
        let dir = dir.path();
        let checkout = dir.join("library");
        let three = run(&checkout, &["rev-parse", "HEAD"]).trim().to_owned();
        run(dir, &["config", "push.recurseSubmodules", "on-demand"]);

        let checked = repo.push_destination().unwrap();
        let error = repo.push_commit(&head(&repo), &checked).unwrap_err();

        assert!(error.to_string().contains("git push"), "{error}");
        let on_remote = run(&checkout, &["branch", "-r", "--contains", &three]);
        assert!(on_remote.is_empty(), "{on_remote}");
        run(dir, &["config", "--unset", "push.recurseSubmodules"]);
        let unchecked = repo.push_destination().unwrap();
        repo.push_commit(&head(&repo), &unchecked).unwrap();
    }
}
