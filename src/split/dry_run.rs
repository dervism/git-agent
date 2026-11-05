//! Finds out what a plan's commits will hold, before any of them is made.
//!
//! The changes are fixed when they are read: a [`Snapshot`] holds the tree they
//! add up to. Each commit's tree is then built in a temporary index, from HEAD
//! and all the units planned so far. That costs no build and no model time, so a
//! plan that can't be carried out stops here instead of leaving a split half done.

use super::{
    Committed, FilePatch, Merged, Plan, Unit, merge_failed, merge_notes, merged_plan, patch,
};
use crate::git::{Repo, Scope};
use crate::temp::PrivateDir;
use anyhow::{Context, Result, bail};

/// The changes as they were when they were read, before the model's long wait.
/// Whatever the user saves after that stays uncommitted.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// The commit the changes apply to.
    pub head: String,
    /// The tree of `head` with all the changes in it.
    pub tree: String,
}

impl Snapshot {
    /// Builds the tree of `patch`, which holds the changes in `scope`, and makes
    /// sure the patch says all there is to say about them.
    pub fn take(repo: &Repo, scope: Scope, patch: &str) -> Result<Self> {
        let head = repo
            .resolve("HEAD")?
            .context("the branch has no commits yet; make a first commit, then split the rest")?;
        let folder = PrivateDir::create("split")?;
        let index = scratch_index(repo, &folder);
        index.read_tree(&head)?;
        index
            .apply_to_index(patch)
            .context("git-agent can't rebuild these changes from their patch")?;
        let tree = index.write_tree()?;
        if repo.differs_from_tree(scope, &tree)? {
            bail!(
                "git-agent can't rebuild these changes from their patch, so it can't split them; \
                 commit them with `git agent`"
            );
        }
        Ok(Self { head, tree })
    }
}

/// A temporary index that doesn't set off the user's hooks.
fn scratch_index(repo: &Repo, folder: &PrivateDir) -> Repo {
    repo.with_index(folder.path().join("index")).without_hooks()
}

/// The commits to make, and what they will hold.
pub struct CommitPlan {
    /// The plan, with commits that couldn't stand alone merged into the next.
    pub plan: Plan,
    /// The tree each commit of `plan` leaves.
    pub trees: Vec<String>,
    /// What was merged, for the user.
    pub merge_notes: Vec<String>,
}

impl CommitPlan {
    /// `plan` with `merges` made, with the tree of each commit from `tree_after`,
    /// which gets the index of a commit of `plan`.
    fn merged(
        plan: &Plan,
        merges: &[Merged],
        tree_after: impl Fn(usize) -> Option<String>,
    ) -> Self {
        Self {
            plan: merged_plan(merges),
            trees: merges
                .iter()
                .filter_map(|merge| tree_after(merge.from.end - 1))
                .collect(),
            merge_notes: merge_notes(plan, merges),
        }
    }

    /// The same with the commits that didn't pass a check merged into the next.
    pub fn after_check(&self, passed: &[bool]) -> Self {
        let merges = merge_failed(&self.plan, passed);
        Self::merged(&self.plan, &merges, |commit| {
            self.trees.get(commit).cloned()
        })
    }
}

/// Builds the tree after each commit of `plan`, and checks that the last one is
/// the change in `snapshot`. The user's index, work tree and HEAD stay as they are.
pub fn dry_run(
    repo: &Repo,
    snapshot: &Snapshot,
    plan: &Plan,
    files: &[FilePatch],
    units: &[Unit],
) -> Result<CommitPlan> {
    let folder = PrivateDir::create("split")?;
    let index = scratch_index(repo, &folder);
    let built: Vec<Result<String>> = (1..=plan.commits.len())
        .map(|count| {
            let chosen: Vec<usize> = plan.commits[..count]
                .iter()
                .flat_map(|commit| commit.units.iter().copied())
                .collect();
            let everything_so_far = patch(files, units, &chosen, &Committed::default());
            index.read_tree(&snapshot.head)?;
            index.apply_to_index(&everything_so_far.text)?;
            index.write_tree()
        })
        .collect();
    match built.last() {
        Some(Ok(tree)) if *tree == snapshot.tree => {}
        Some(Ok(_)) => bail!("the plan doesn't add up to the changes, so nothing was committed"),
        Some(Err(error)) => bail!("the plan can't be carried out: {error:#}"),
        None => bail!("the plan has no commits"),
    }
    let passed: Vec<bool> = built.iter().map(Result::is_ok).collect();
    let merges = merge_failed(plan, &passed);
    Ok(CommitPlan::merged(plan, &merges, |commit| {
        built[commit].as_ref().ok().cloned()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support::{isolated_repo, numbered_lines, run, with_submodule};
    use crate::split::{Planned, describe, parse};
    use std::collections::HashSet;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn commit_file(dir: &Path, name: &str, text: &str) {
        fs::write(dir.join(name), text).unwrap();
        run(dir, &["add", "--all"]);
        run(dir, &["commit", "-q", "-m", "add a file"]);
    }

    /// The change in `scope`, cut into units with nothing excluded, and its snapshot.
    fn cut(repo: &Repo, scope: Scope) -> (Vec<FilePatch>, Vec<Unit>, Snapshot) {
        let included: HashSet<String> = repo.included_paths(scope, &[]).unwrap();
        let text = repo.patch(scope).unwrap();
        let files = parse(&text, &included);
        let units = describe(&files, 1_000_000).unwrap().units;
        let snapshot = Snapshot::take(repo, scope, &text).unwrap();
        (files, units, snapshot)
    }

    /// A plan with one commit per list of units.
    fn plan(commits: &[&[usize]]) -> Plan {
        Plan {
            commits: commits
                .iter()
                .enumerate()
                .map(|(number, units)| Planned {
                    title: format!("Commit {}", number + 1),
                    units: units.to_vec(),
                })
                .collect(),
        }
    }

    fn state(dir: &Path) -> (String, String, String) {
        (
            run(dir, &["rev-parse", "HEAD"]),
            run(dir, &["diff", "--cached"]),
            run(dir, &["diff"]),
        )
    }

    /// A staged change with an edit in two places of a file.
    fn staged_two_places() -> (TempDir, Repo) {
        let (dir, repo) = isolated_repo();
        commit_file(dir.path(), "lib.txt", &numbered_lines(40));
        let changed = numbered_lines(40)
            .replace("line 2\n", "line 2 changed\nline 2b\n")
            .replace("line 37\n", "line 37 changed\n");
        fs::write(dir.path().join("lib.txt"), changed).unwrap();
        run(dir.path(), &["add", "--all"]);
        (dir, repo)
    }

    #[test]
    fn gives_the_tree_after_each_commit_and_touches_nothing() {
        let (dir, repo) = staged_two_places();
        let (files, units, snapshot) = cut(&repo, Scope::Staged);
        let before = state(dir.path());
        let result = dry_run(&repo, &snapshot, &plan(&[&[1], &[0]]), &files, &units).unwrap();
        assert_eq!(state(dir.path()), before);
        assert_eq!(snapshot.head, before.0.trim());
        assert_eq!(result.trees.len(), 2);
        assert_eq!(result.plan.commits.len(), 2);
        assert!(result.merge_notes.is_empty());
        assert_eq!(result.trees[1], repo.write_tree().unwrap());

        let first = run(
            dir.path(),
            &["cat-file", "-p", &format!("{}:lib.txt", result.trees[0])],
        );
        assert!(first.contains("line 37 changed") && !first.contains("line 2 changed"));
    }

    #[test]
    fn merges_a_commit_that_cant_be_built_from_its_patch() {
        let (dir, repo) = isolated_repo();
        commit_file(dir.path(), "x", "a file that a folder takes the place of\n");
        fs::remove_file(dir.path().join("x")).unwrap();
        fs::create_dir(dir.path().join("x")).unwrap();
        fs::write(dir.path().join("x/inner"), "something else entirely\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        let (files, units, snapshot) = cut(&repo, Scope::Staged);
        assert_eq!(units.len(), 2);
        let adding_first = if files[units[0].file].path == "x/inner" {
            plan(&[&[0], &[1]])
        } else {
            plan(&[&[1], &[0]])
        };

        let result = dry_run(&repo, &snapshot, &adding_first, &files, &units).unwrap();
        assert_eq!(result.plan.commits.len(), 1);
        assert_eq!(result.merge_notes.len(), 1);
        assert_eq!(result.trees, [repo.write_tree().unwrap()]);
    }

    #[test]
    fn stops_when_the_plan_doesnt_add_up_to_the_change() {
        let (dir, repo) = staged_two_places();
        let (files, units, snapshot) = cut(&repo, Scope::Staged);
        let before = state(dir.path());
        let error = dry_run(&repo, &snapshot, &plan(&[&[1]]), &files, &units)
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("doesn't add up"), "{error:#}");
        assert_eq!(state(dir.path()), before);
    }

    #[test]
    fn stops_when_the_last_commit_cant_be_built() {
        let (dir, repo) = staged_two_places();
        let (files, units, snapshot) = cut(&repo, Scope::Staged);
        // The change no longer fits once HEAD has moved past it.
        run(dir.path(), &["stash", "-q"]);
        commit_file(dir.path(), "lib.txt", "something else\n");
        let moved = Snapshot {
            head: repo.resolve("HEAD").unwrap().unwrap(),
            tree: snapshot.tree,
        };
        let error = dry_run(&repo, &moved, &plan(&[&[0, 1]]), &files, &units)
            .err()
            .unwrap();
        assert!(
            format!("{error:#}").contains("can't be carried out"),
            "{error:#}"
        );
    }

    #[test]
    fn says_so_when_there_are_no_commits_yet() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), &["init", "-q", "--initial-branch", "main"]);
        fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        let repo = Repo::open(dir.path()).unwrap();
        let error = Snapshot::take(&repo, Scope::Staged, "").err().unwrap();
        assert!(format!("{error:#}").contains("no commits yet"), "{error:#}");
    }

    #[test]
    fn wont_take_a_snapshot_of_a_patch_that_leaves_something_out() {
        let (dir, repo) = staged_two_places();
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        let text = repo.patch(Scope::Staged).unwrap();
        let second_file = text.rfind("diff --git ").unwrap();
        let error = Snapshot::take(&repo, Scope::Staged, &text[..second_file])
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("can't rebuild"), "{error:#}");
    }

    #[test]
    fn accepts_files_git_was_told_to_treat_differently() {
        let (dir, repo) = isolated_repo();
        commit_file(dir.path(), "lib.txt", &numbered_lines(40));
        commit_file(dir.path(), "skip.conf", "committed\n");
        commit_file(dir.path(), "assume.conf", "committed\n");
        run(
            dir.path(),
            &["update-index", "--skip-worktree", "skip.conf"],
        );
        run(
            dir.path(),
            &["update-index", "--assume-unchanged", "assume.conf"],
        );
        fs::write(dir.path().join("skip.conf"), "local\n").unwrap();
        fs::write(dir.path().join("assume.conf"), "local\n").unwrap();
        fs::write(dir.path().join("planned.txt"), "later\n").unwrap();
        run(dir.path(), &["add", "-N", "planned.txt"]);
        fs::write(
            dir.path().join("lib.txt"),
            numbered_lines(40).replace("line 7\n", "seven\n"),
        )
        .unwrap();
        let text = repo.patch(Scope::Tracked).unwrap();
        Snapshot::take(&repo, Scope::Tracked, &text).unwrap();
    }

    #[test]
    fn takes_a_snapshot_with_a_moved_submodule_whatever_the_diff_setting_is() {
        let (dir, repo, _library, moved) = with_submodule();
        run(dir.path(), &["config", "diff.submodule", "log"]);
        let entry = format!("160000,{moved},library");
        run(dir.path(), &["update-index", "--cacheinfo", &entry]);
        let text = repo.patch(Scope::Staged).unwrap();
        let snapshot = Snapshot::take(&repo, Scope::Staged, &text).unwrap();
        assert_eq!(snapshot.tree, repo.write_tree().unwrap());
    }

    #[test]
    fn takes_a_snapshot_with_a_moved_submodule_that_isnt_staged() {
        let (dir, repo, _library, _) = with_submodule();
        let inside = dir.path().join("library");
        run(
            &inside,
            &[
                "-c",
                "protocol.file.allow=always",
                "pull",
                "-q",
                "origin",
                "main",
            ],
        );
        fs::write(dir.path().join("lib.rs"), "fn edited() {}\n").unwrap();
        let text = repo.patch(Scope::Tracked).unwrap();
        let snapshot = Snapshot::take(&repo, Scope::Tracked, &text).unwrap();
        let listed = run(dir.path(), &["ls-tree", &snapshot.tree, "library"]);
        let moved = run(&inside, &["rev-parse", "HEAD"]);
        assert!(listed.contains(moved.trim()), "{listed}");
    }

    #[test]
    fn leaves_out_a_moved_submodule_the_settings_hide() {
        for (key, value) in [
            ("submodule.library.ignore", "all"),
            ("diff.ignoreSubmodules", "all"),
        ] {
            let (dir, repo, _library, _) = with_submodule();
            let inside = dir.path().join("library");
            run(
                &inside,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "pull",
                    "-q",
                    "origin",
                    "main",
                ],
            );
            run(dir.path(), &["config", key, value]);
            fs::write(dir.path().join("lib.rs"), "fn edited() {}\n").unwrap();
            let text = repo.patch(Scope::Tracked).unwrap();
            assert!(!text.contains("library"), "{key}: {text}");
            let snapshot = Snapshot::take(&repo, Scope::Tracked, &text).unwrap();
            let listed = run(dir.path(), &["ls-tree", &snapshot.tree, "library"]);
            let committed = run(dir.path(), &["ls-tree", "HEAD", "library"]);
            assert_eq!(listed, committed, "{key}");
        }
    }

    #[test]
    fn takes_a_snapshot_when_a_submodule_is_only_dirty() {
        let (dir, repo, _library, _) = with_submodule();
        let inside = dir.path().join("library");
        fs::write(inside.join("loose.txt"), "x\n").unwrap();
        fs::write(dir.path().join("lib.rs"), "fn edited() {}\n").unwrap();
        let text = repo.patch(Scope::Tracked).unwrap();
        assert!(!text.contains("library"), "{text}");
        Snapshot::take(&repo, Scope::Tracked, &text).unwrap();

        fs::write(dir.path().join("lib.rs"), "fn a() {}\n").unwrap();
        let only_dirty = repo.patch(Scope::Tracked).unwrap();
        assert!(only_dirty.is_empty(), "{only_dirty}");
    }

    #[test]
    fn doesnt_set_off_hooks_while_it_rehearses() {
        let (dir, repo) = staged_two_places();
        let hooks = dir.path().join("hooks");
        fs::create_dir(&hooks).unwrap();
        let mark = dir.path().join("hook-ran");
        let hook = hooks.join("post-index-change");
        let script = format!(
            "#!/bin/sh\ntouch {}\n",
            crate::git::shell_quote(&mark.to_string_lossy())
        );
        fs::write(&hook, script).unwrap();
        let executable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        fs::set_permissions(&hook, executable).unwrap();
        run(dir.path(), &["config", "core.hooksPath", "hooks"]);
        let (files, units, snapshot) = cut(&repo, Scope::Staged);
        dry_run(&repo, &snapshot, &plan(&[&[0], &[1]]), &files, &units).unwrap();
        assert!(!mark.exists());
    }

    #[test]
    fn merges_the_commits_a_check_failed_and_keeps_the_right_trees() {
        let (dir, repo) = staged_two_places();
        let (files, units, snapshot) = cut(&repo, Scope::Staged);
        let result = dry_run(&repo, &snapshot, &plan(&[&[1], &[0]]), &files, &units).unwrap();
        let checked = result.after_check(&[false, true]);
        assert_eq!(checked.plan.commits.len(), 1);
        assert_eq!(checked.plan.commits[0].units, [1, 0]);
        assert_eq!(checked.trees, std::slice::from_ref(&snapshot.tree));
        assert_eq!(checked.merge_notes.len(), 1);
        assert!(checked.merge_notes[0].contains("\"Commit 1\" and \"Commit 2\""));
        let kept = result.after_check(&[true, true]);
        assert_eq!(kept.trees, result.trees);
        assert!(kept.merge_notes.is_empty());
        drop(dir);
    }

    #[test]
    fn starts_from_the_snapshot_and_not_from_where_head_is_now() {
        let (dir, repo) = staged_two_places();
        let (files, units, snapshot) = cut(&repo, Scope::Staged);
        run(
            dir.path(),
            &["commit", "-q", "--allow-empty", "-m", "elsewhere"],
        );
        let result = dry_run(&repo, &snapshot, &plan(&[&[0, 1]]), &files, &units).unwrap();
        assert_eq!(result.trees, [snapshot.tree]);
    }

    #[test]
    fn reads_tracked_changes_from_the_work_tree_without_staging_them() {
        let (dir, repo) = isolated_repo();
        commit_file(dir.path(), "lib.txt", &numbered_lines(40));
        fs::write(
            dir.path().join("lib.txt"),
            numbered_lines(40).replace("line 2\n", "two\n"),
        )
        .unwrap();
        fs::write(dir.path().join("untracked.txt"), "stays out\n").unwrap();
        let (files, units, snapshot) = cut(&repo, Scope::Tracked);
        let before = state(dir.path());
        let result = dry_run(&repo, &snapshot, &plan(&[&[0]]), &files, &units).unwrap();
        assert_eq!(state(dir.path()), before);
        let listed = run(dir.path(), &["ls-tree", "--name-only", &result.trees[0]]);
        assert!(!listed.contains("untracked.txt"));
        let text = run(
            dir.path(),
            &["cat-file", "-p", &format!("{}:lib.txt", result.trees[0])],
        );
        assert!(text.contains("two\n"));
    }

    #[test]
    fn counts_only_what_is_staged_when_edits_are_left_over() {
        let (dir, repo) = staged_two_places();
        fs::write(dir.path().join("lib.rs"), "fn unstaged() {}\n").unwrap();
        let (files, units, snapshot) = cut(&repo, Scope::Staged);
        let before = state(dir.path());
        let result = dry_run(&repo, &snapshot, &plan(&[&[0], &[1]]), &files, &units).unwrap();
        assert_eq!(state(dir.path()), before);
        assert_eq!(result.trees[1], repo.write_tree().unwrap());
    }

    #[test]
    fn handles_renames_deletions_binaries_and_modes_at_a_boundary() {
        let (dir, repo) = isolated_repo();
        let long = numbered_lines(30);
        commit_file(dir.path(), "old.txt", &long);
        commit_file(dir.path(), "gone.txt", "going away\n");
        commit_file(dir.path(), "script.sh", "#!/bin/sh\n");
        fs::write(dir.path().join("image.bin"), [0u8, 159, 146, 150]).unwrap();
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "binary"]);

        fs::rename(dir.path().join("old.txt"), dir.path().join("renamed.txt")).unwrap();
        fs::write(
            dir.path().join("renamed.txt"),
            long.replace("line 5\n", "line five\n"),
        )
        .unwrap();
        fs::remove_file(dir.path().join("gone.txt")).unwrap();
        fs::write(dir.path().join("image.bin"), [1u8, 2, 3, 0, 255]).unwrap();
        let executable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        fs::set_permissions(dir.path().join("script.sh"), executable).unwrap();
        run(dir.path(), &["add", "--all"]);

        let (files, units, snapshot) = cut(&repo, Scope::Staged);
        assert_eq!(units.len(), 4);
        let one_each: Vec<Vec<usize>> = (0..4).map(|unit| vec![unit]).collect();
        let commits: Vec<&[usize]> = one_each.iter().map(Vec::as_slice).collect();
        let forward = dry_run(&repo, &snapshot, &plan(&commits), &files, &units).unwrap();
        assert_eq!(forward.plan.commits.len(), 4);
        let backward: Vec<&[usize]> = commits.iter().rev().copied().collect();
        let reverse = dry_run(&repo, &snapshot, &plan(&backward), &files, &units).unwrap();
        assert_eq!(reverse.plan.commits.len(), 4);
        assert_eq!(forward.trees.last(), reverse.trees.last());
        assert_eq!(forward.trees.last(), Some(&repo.write_tree().unwrap()));
    }

    #[test]
    fn keeps_latin1_contents_byte_for_byte() {
        let (dir, repo) = isolated_repo();
        let before = numbered_lines(40).replace("line 2\n", "linje \u{f8}\n");
        let latin1 = |text: &str| -> Vec<u8> { text.chars().map(|c| c as u8).collect() };
        fs::write(dir.path().join("l1.properties"), latin1(&before)).unwrap();
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "latin1"]);
        let after = before
            .replace("linje \u{f8}\n", "linje \u{e6}\n")
            .replace("line 37\n", "linje \u{e5}\n");
        fs::write(dir.path().join("l1.properties"), latin1(&after)).unwrap();
        run(dir.path(), &["add", "--all"]);

        let (files, units, snapshot) = cut(&repo, Scope::Staged);
        assert_eq!(units.len(), 2);
        let result = dry_run(&repo, &snapshot, &plan(&[&[1], &[0]]), &files, &units).unwrap();
        assert_eq!(result.plan.commits.len(), 2);
        assert_eq!(result.trees[1], repo.write_tree().unwrap());
    }

    #[test]
    fn works_from_a_subfolder_and_from_a_linked_worktree() {
        let (dir, _) = staged_two_places();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let repo = Repo::open_at_top(dir.path().join("sub")).unwrap();
        let (files, units, snapshot) = cut(&repo, Scope::Staged);
        let result = dry_run(&repo, &snapshot, &plan(&[&[0], &[1]]), &files, &units).unwrap();
        assert_eq!(result.trees[1], repo.write_tree().unwrap());

        let linked_parent = TempDir::new().unwrap();
        let linked = linked_parent.path().join("linked");
        let linked_text = linked.to_string_lossy().into_owned();
        run(
            dir.path(),
            &["worktree", "add", "-q", "--detach", &linked_text, "HEAD"],
        );
        fs::write(
            linked.join("lib.txt"),
            numbered_lines(40).replace("line 7\n", "seven\n"),
        )
        .unwrap();
        run(&linked, &["add", "--all"]);
        let repo = Repo::open_at_top(&linked).unwrap();
        let (files, units, snapshot) = cut(&repo, Scope::Staged);
        let before = state(&linked);
        let result = dry_run(&repo, &snapshot, &plan(&[&[0]]), &files, &units).unwrap();
        assert_eq!(state(&linked), before);
        assert_eq!(result.trees[0], repo.write_tree().unwrap());
    }
}
