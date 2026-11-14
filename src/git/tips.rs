//! Where HEAD and the refs point, before a commit and after it, and the commits new between them:
//! what a commit, and the hooks it runs, made.

use super::{CommitId, LogMarks, Repo, UNREADABLE_LOG, object_hash, stream_of};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead};

/// Where HEAD and some refs pointed at one moment, and what a merge in progress took in, by full
/// object hashes.
#[cfg_attr(test, derive(Default))]
pub struct Tips {
    head: Option<String>,
    /// Each ref by its full name as git wrote it, which needn't be UTF-8.
    refs: BTreeMap<Vec<u8>, String>,
    /// The commits `MERGE_HEAD` named.
    merging: Vec<String>,
}

/// A ref that moved between two snapshots: its name, readable, and the commit it points at now.
pub struct MovedRef<'a> {
    pub name: String,
    pub hash: &'a str,
}

impl Tips {
    /// The tips of `head`, `refs` and `merging`. Fails unless each is a full object hash, so that
    /// none can be read as an option.
    fn of(
        head: Option<String>,
        refs: BTreeMap<Vec<u8>, String>,
        merging: Vec<String>,
    ) -> Result<Self> {
        let tips = Self {
            head,
            refs,
            merging,
        };
        tips.hashes()
            .try_for_each(|hash| object_hash(hash).map(drop))?;
        Ok(tips)
    }

    /// Whether nothing pointed at anything.
    pub fn is_empty(&self) -> bool {
        self.hashes().next().is_none()
    }

    /// Whether a merge was in progress.
    pub fn is_merging(&self) -> bool {
        !self.merging.is_empty()
    }

    /// The refs that point somewhere else than at `before`, or weren't there then, other than the
    /// branch `except` names.
    pub fn refs_moved_since<'a>(
        &'a self,
        before: &Tips,
        except: Option<&str>,
    ) -> Vec<MovedRef<'a>> {
        self.refs
            .iter()
            .filter(|(name, hash)| before.refs.get(*name) != Some(*hash))
            .map(|(name, hash)| MovedRef {
                name: String::from_utf8_lossy(name).into_owned(),
                hash,
            })
            .filter(|moved| Some(moved.name.as_str()) != except)
            .collect()
    }

    fn hashes(&self) -> impl Iterator<Item = &String> {
        self.head
            .iter()
            .chain(self.refs.values())
            .chain(&self.merging)
    }

    /// These tips and none of `before`'s history, as `git log --stdin` reads revisions.
    fn revisions_new_since(&self, before: &Tips) -> String {
        let hashes = |tips: &Tips| tips.hashes().cloned().collect::<BTreeSet<_>>();
        let now = hashes(self).into_iter().map(|tip| format!("{tip}\n"));
        let not = hashes(before).into_iter().map(|tip| format!("^{tip}\n"));
        now.chain(not).collect()
    }
}

/// A commit made since an earlier moment, with the parents git stores for it.
pub struct CommitMade {
    pub id: CommitId,
    pub parents: Vec<String>,
}

impl CommitMade {
    /// Whether it was made on `parent`: its first parent is `parent`, or it has none when `parent`
    /// is `None`.
    pub fn made_on(&self, parent: Option<&str>) -> bool {
        self.parents.first().map(String::as_str) == parent
    }
}

impl Repo {
    /// Where HEAD and every ref point, with the commits a merge in progress takes in: the commits
    /// there are already, before a commit.
    pub fn every_tip(&self) -> Result<Tips> {
        Tips::of(
            self.resolve("HEAD")?,
            self.refs_at(&[])?,
            self.merge_heads()?,
        )
    }

    /// Where HEAD and the branches point: where a commit, and its hooks, leave what they make.
    pub fn head_and_branch_tips(&self) -> Result<Tips> {
        Tips::of(
            self.resolve("HEAD")?,
            self.refs_at(&["refs/heads/"])?,
            Vec::new(),
        )
    }

    /// Each ref `patterns` match, by its name as git wrote it, and the hash it points at.
    fn refs_at(&self, patterns: &[&str]) -> Result<BTreeMap<Vec<u8>, String>> {
        let mut args = vec!["for-each-ref", "--format=%(objectname) %(refname)"];
        args.extend_from_slice(patterns);
        let refs = self.git_bytes(&args)?;
        let named = refs.split(|byte| *byte == b'\n').filter_map(|line| {
            let space = line.iter().position(|byte| *byte == b' ')?;
            let hash = String::from_utf8_lossy(&line[..space]).into_owned();
            Some((line[space + 1..].to_vec(), hash))
        });
        Ok(named.collect())
    }

    /// The commits a merge in progress takes in, from `MERGE_HEAD`; none without one.
    fn merge_heads(&self) -> Result<Vec<String>> {
        let path = self.git_path("MERGE_HEAD")?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let hashes = text.lines().map(str::trim).filter(|line| !line.is_empty());
        Ok(hashes.map(str::to_owned).collect())
    }

    /// Whether `tip` reaches `commit`: it is `commit`, or `commit` is in its history, as git stores
    /// them. Fails with git's first line of error when git can't tell, as for a tip that is a tree
    /// or a file rather than a commit.
    pub fn reaches(&self, tip: &str, commit: &str) -> Result<bool> {
        let args = [
            "merge-base",
            "--is-ancestor",
            object_hash(commit)?,
            object_hash(tip)?,
        ];
        let (mut command, _) = self.published_command(&args);
        let output = command
            .output()
            .context("failed to run git; is it installed?")?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => {
                let error = String::from_utf8_lossy(&output.stderr);
                bail!("{}", error.lines().next().unwrap_or("git failed").trim())
            }
        }
    }

    /// The commits `now` leads to and `before` didn't, newest first, with their parents, as git
    /// stores them, read a line at a time; none when `now` holds no tip, without asking git, which
    /// would read HEAD when it gets no revision at all. A tip at an object the repository lacks is
    /// left out: it holds nothing to read, and nothing to leave out.
    pub fn commits_new_since(&self, before: &Tips, now: &Tips) -> Result<Vec<CommitMade>> {
        if now.is_empty() {
            return Ok(Vec::new());
        }
        let revisions = now.revisions_new_since(before);
        let (command, args) = self.published_command(&[
            "log",
            "--no-show-signature",
            "--format=%H %h %P",
            "--ignore-missing",
            "--stdin",
            "--",
        ]);
        stream_of(command, &args, Some(revisions.as_bytes()), |log| {
            log.lines().map(|line| new_commit(&line?)).collect()
        })
    }

    /// `commits`, in the order given, read as [`Repo::published_commits`] reads a range, with the
    /// hashes on git's standard input. Fails when there are none, since git would read HEAD then.
    pub fn read_commits<T>(
        &self,
        commits: &[CommitId],
        read: impl FnOnce(&mut dyn BufRead, &LogMarks) -> Result<T>,
    ) -> Result<T> {
        if commits.is_empty() {
            bail!("there are no commits to read");
        }
        let revisions: String = commits
            .iter()
            .map(|commit| format!("{}\n", commit.full()))
            .collect();
        self.published_log(
            &["--no-walk=unsorted", "--stdin"],
            Some(revisions.as_bytes()),
            read,
        )
    }
}

/// The commit a line of `%H %h %P` names, with its parents.
fn new_commit(line: &str) -> Result<CommitMade> {
    let mut fields = line.split(' ');
    let id = fields
        .next()
        .zip(fields.next())
        .and_then(|(full, short)| CommitId::new(full, short))
        .context(UNREADABLE_LOG)?;
    let parents = fields
        .filter(|hash| !hash.is_empty())
        .map(str::to_owned)
        .collect();
    Ok(CommitMade { id, parents })
}

#[cfg(test)]
pub mod test_support {
    use super::Tips;

    /// Tips at `hashes`, as a snapshot of refs would hold them, each under a name of its own.
    pub fn tips_of(hashes: &[&str]) -> Tips {
        let named = hashes.iter().enumerate().map(|(number, hash)| {
            (
                format!("refs/tags/t{number}").into_bytes(),
                (*hash).to_owned(),
            )
        });
        Tips::of(None, named.collect(), Vec::new()).expect("full hashes")
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{isolate, isolated_repo, run, short_head};
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    /// Commits an empty commit with `message` on a new branch `branch` at `start`, and returns
    /// its short hash.
    fn commit_on(dir: &Path, branch: &str, start: &str, message: &str) -> String {
        run(dir, &["switch", "-q", "-c", branch, start]);
        run(dir, &["commit", "-q", "--allow-empty", "-m", message]);
        short_head(dir)
    }

    /// The short hashes of the commits new since `before` on HEAD and the branches, newest first.
    fn new_since(repo: &Repo, before: &Tips) -> Vec<String> {
        let now = repo.head_and_branch_tips().unwrap();
        let new = repo.commits_new_since(before, &now).unwrap();
        new.iter()
            .map(|commit| commit.id.short().to_owned())
            .collect()
    }

    /// What's new since a snapshot of every ref is what HEAD and the branches lead to now and no
    /// ref did then: on a branch a hook made or switched to too, behind a merge's second parent
    /// too, and not a commit a branch, a tag or a remote-tracking branch held already.
    #[test]
    fn finds_the_commits_new_since_a_snapshot() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let first = repo.resolve("HEAD").unwrap().unwrap();
        commit_on(dir, "other", &first, "on other");
        let tagged = commit_on(dir, "tagged", &first, "tagged");
        run(dir, &["tag", "-a", "-m", "a tag", "v1"]);
        run(dir, &["switch", "-q", "--detach", &first]);
        run(dir, &["branch", "-q", "-D", "tagged"]);
        run(dir, &["switch", "-q", "main"]);
        let before = repo.every_tip().unwrap();
        run(dir, &["commit", "-q", "--allow-empty", "-m", "made"]);
        let made = short_head(dir);
        let side = commit_on(dir, "side", &first, "on the side");
        run(dir, &["switch", "-q", "main"]);
        run(dir, &["merge", "-q", "--no-ff", "-m", "merge", "side"]);
        let merge = short_head(dir);
        run(dir, &["branch", "-q", "-D", "side"]);
        run(dir, &["branch", "-q", "again", "v1^{}"]);
        run(dir, &["switch", "-q", "other"]);

        let mut new = new_since(&repo, &before);
        new.sort();
        let mut wanted = vec![made, side, merge];
        wanted.sort();
        assert_eq!(new, wanted);
        assert!(!new.contains(&tagged));
        let now = repo.head_and_branch_tips().unwrap();
        assert!(repo.commits_new_since(&now, &now).unwrap().is_empty());
    }

    /// On a branch without commits, nothing is new until the first commit, made on no parent.
    #[test]
    fn finds_the_first_commit_made_on_a_branch_without_commits() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), &["init", "-q", "--initial-branch", "main"]);
        isolate(dir.path());
        let repo = Repo::open(dir.path()).unwrap();
        let before = repo.every_tip().unwrap();
        assert!(before.is_empty());
        assert!(repo.commits_new_since(&before, &before).unwrap().is_empty());
        run(
            dir.path(),
            &["commit", "-q", "--allow-empty", "-m", "first"],
        );

        let now = repo.head_and_branch_tips().unwrap();
        let new = repo.commits_new_since(&before, &now).unwrap();

        assert_eq!(new.len(), 1);
        assert!(new[0].made_on(None));
        assert!(!new[0].made_on(Some(new[0].id.full())));
    }

    /// Tips are full hashes, so that none can be read as an option. With no tip now nothing is
    /// new; that git, given no revision at all, would read HEAD is what the test of a branch
    /// without commits pins.
    #[test]
    fn reads_only_from_tips_that_are_hashes() {
        let (_dir, repo) = isolated_repo();
        let now = repo.head_and_branch_tips().unwrap();
        let named = |hash: &str| BTreeMap::from([(b"refs/heads/x".to_vec(), hash.to_owned())]);
        assert!(Tips::of(None, named("--all"), Vec::new()).is_err());
        assert!(Tips::of(Some("HEAD".to_owned()), BTreeMap::new(), Vec::new()).is_err());
        assert!(Tips::of(None, BTreeMap::new(), vec!["not-a-hash".to_owned()]).is_err());
        let none = repo.commits_new_since(&now, &Tips::default()).unwrap();
        assert!(none.is_empty());
        assert!(repo.read_commits(&[], |_, _| Ok(())).is_err());
    }

    /// Read in the order given, exactly the commits asked for.
    #[test]
    fn reads_the_commits_asked_for_in_their_order() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let first = short_head(dir);
        run(dir, &["commit", "-q", "--allow-empty", "-m", "second"]);
        run(dir, &["commit", "-q", "--allow-empty", "-m", "third"]);
        let before = Tips::default();
        let now = repo.head_and_branch_tips().unwrap();
        let new = repo.commits_new_since(&before, &now).unwrap();
        let asked = [new[2].id.clone(), new[0].id.clone()];

        let read = repo
            .read_commits(&asked, |log, _| {
                let mut text = String::new();
                log.read_to_string(&mut text)?;
                Ok(text)
            })
            .unwrap();

        let at = |short: &str| read.find(short).unwrap();
        assert_eq!(asked[0].short(), first);
        assert!(at(asked[0].short()) < at(asked[1].short()), "{read}");
        assert!(!read.contains(new[1].id.short()), "{read}");
    }

    /// What moved between two snapshots: refs that point elsewhere, and refs that are new; not
    /// those that stayed, nor those that went, nor HEAD, nor the branch left out.
    #[test]
    fn tells_which_refs_moved_between_two_snapshots() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        run(dir, &["branch", "stays"]);
        run(dir, &["branch", "goes"]);
        let before = repo.every_tip().unwrap();
        run(dir, &["commit", "-q", "--allow-empty", "-m", "second"]);
        run(dir, &["branch", "-q", "-D", "goes"]);
        run(dir, &["tag", "new"]);

        let now = repo.every_tip().unwrap();

        let names = |except: Option<&str>| -> Vec<String> {
            let moved = now.refs_moved_since(&before, except);
            moved.into_iter().map(|moved| moved.name).collect()
        };
        assert_eq!(names(None), ["refs/heads/main", "refs/tags/new"]);
        assert_eq!(names(Some("refs/heads/main")), ["refs/tags/new"]);
        let main = repo.resolve("main").unwrap().unwrap();
        assert_eq!(now.refs_moved_since(&before, None)[0].hash, main);
        assert!(now.refs_moved_since(&now, None).is_empty());
    }

    /// A merge in progress takes in commits no ref may hold, as after a fetch of a URL: they're
    /// there already, so a commit of the merge makes only the merge.
    #[test]
    fn counts_what_a_merge_in_progress_takes_in_as_there_already() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let first = repo.resolve("HEAD").unwrap().unwrap();
        let fetched = commit_on(dir, "fetched", &first, "fetched work");
        let fetched_full = repo.resolve("HEAD").unwrap().unwrap();
        run(dir, &["switch", "-q", "main"]);
        run(dir, &["branch", "-q", "-D", "fetched"]);
        run(dir, &["commit", "-q", "--allow-empty", "-m", "on main"]);
        run(
            dir,
            &["merge", "-q", "--no-ff", "--no-commit", &fetched_full],
        );
        let before = repo.every_tip().unwrap();
        assert_eq!(before.merging, [fetched_full]);
        run(dir, &["commit", "-q", "-m", "merge"]);

        let new = new_since(&repo, &before);

        assert_eq!(new, [short_head(dir)]);
        assert_ne!(new, [fetched]);
    }

    /// A merge of two histories at once names both; a `MERGE_HEAD` git-agent can't read, or a line
    /// in it that isn't a full object hash, fails the snapshot rather than count as none.
    #[test]
    fn reads_every_commit_a_merge_names_and_fails_on_one_it_cant_read() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let first = repo.resolve("HEAD").unwrap().unwrap();
        commit_on(dir, "one", &first, "one");
        commit_on(dir, "two", &first, "two");
        run(dir, &["switch", "-q", "main"]);
        run(dir, &["merge", "-q", "--no-commit", "one", "two"]);
        assert_eq!(repo.every_tip().unwrap().merging.len(), 2);

        let merge_head = repo.git_path("MERGE_HEAD").unwrap();
        fs::write(&merge_head, "not-a-hash\n").unwrap();
        assert!(repo.every_tip().is_err());
        fs::remove_file(&merge_head).unwrap();
        fs::create_dir(&merge_head).unwrap();
        assert!(repo.every_tip().is_err());
    }

    /// Refs whose names differ only in bytes that aren't UTF-8 stay apart, though they read alike.
    #[test]
    fn keeps_apart_refs_whose_names_differ_only_in_bytes_that_arent_utf8() {
        let (dir, repo) = isolated_repo();
        let dir = dir.path();
        let first = repo.resolve("HEAD").unwrap().unwrap();
        run(dir, &["commit", "-q", "--allow-empty", "-m", "second"]);
        let second = repo.resolve("HEAD").unwrap().unwrap();
        let mut packed = b"# pack-refs with: peeled fully-peeled sorted \n".to_vec();
        for (name, hash) in [
            (&b"refs/heads/a\xfe"[..], &first),
            (b"refs/heads/a\xff", &second),
        ] {
            packed.extend([hash.as_bytes(), b" ", name, b"\n"].concat());
        }
        fs::write(repo.git_path("packed-refs").unwrap(), packed).unwrap();

        let tips = repo.every_tip().unwrap();

        assert_eq!(tips.refs.len(), 3);
        let moved = tips.refs_moved_since(&Tips::default(), Some("refs/heads/main"));
        let hashes: Vec<&str> = moved.iter().map(|moved| moved.hash).collect();
        assert_eq!(hashes, [first.as_str(), second.as_str()]);
    }

    /// A tip reaches itself and its history, and nothing after it.
    #[test]
    fn tells_whether_a_tip_reaches_a_commit() {
        let (dir, repo) = isolated_repo();
        let first = repo.resolve("HEAD").unwrap().unwrap();
        run(
            dir.path(),
            &["commit", "-q", "--allow-empty", "-m", "second"],
        );
        let second = repo.resolve("HEAD").unwrap().unwrap();

        assert!(repo.reaches(&second, &first).unwrap());
        assert!(repo.reaches(&second, &second).unwrap());
        assert!(!repo.reaches(&first, &second).unwrap());
        assert!(repo.reaches("--all", &first).is_err());
    }

    /// Read from a line of `%H %h %P`: the commit and its parents, and what it was made on.
    #[test]
    fn reads_a_new_commit_from_a_line_of_a_log() {
        let (full, parent, other) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        let made = new_commit(&format!("{full} aaaaaaa {parent} {other}")).unwrap();
        assert_eq!(made.id, CommitId::new(&full, "aaaaaaa").unwrap());
        assert_eq!(made.parents, [parent.clone(), other.clone()]);
        assert!(made.made_on(Some(&parent)) && !made.made_on(Some(&other)));
        assert!(!made.made_on(None));
        let first = new_commit(&format!("{full} aaaaaaa ")).unwrap();
        assert!(first.parents.is_empty() && first.made_on(None));
        assert!(!first.made_on(Some(&parent)));
        assert!(new_commit("not a commit").is_err());
    }
}
