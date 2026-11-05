//! A temporary checkout of the repository, for work that shouldn't touch the
//! user's work tree, index or branches.

use super::{NO_HOOKS, Repo};
use crate::temp::{self, PrivateDir, make_writable};
use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What the private folder of a worktree is called, after `git-agent-`.
const FOLDER_LABEL: &str = "worktree";
/// The checkout inside the private folder: `git worktree add` wants a path that
/// doesn't exist yet, and this leaves room beside it for the worktree's owner.
const CHECKOUT: &str = "checkout";

/// A detached worktree in a private folder, removed when this is dropped.
pub struct TempWorktree {
    owner: Repo,
    path: PathBuf,
    folder: PrivateDir,
    repository_variables: Vec<String>,
}

impl TempWorktree {
    /// Adds a worktree of `repo` at `commit`, without running hooks, and removes
    /// any that earlier runs of git-agent couldn't clean up after themselves.
    pub fn add(repo: &Repo, commit: &str) -> Result<Self> {
        let owner = Repo {
            workdir: repo.workdir.clone(),
            index: None,
            attributes: None,
            hooks_off: false,
        };
        remove_leftovers(&owner);
        let folder = PrivateDir::create(FOLDER_LABEL)?;
        let path = folder.path().join(CHECKOUT);
        let path_text = path.to_string_lossy().into_owned();
        owner.git(&[
            "-c", NO_HOOKS, "worktree", "add", "--detach", "--quiet", &path_text, commit,
        ])?;
        let variables = owner.git(&["rev-parse", "--local-env-vars"])?;
        Ok(Self {
            owner,
            path,
            folder,
            repository_variables: variables.lines().map(str::to_owned).collect(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The private folder the checkout is in, for files that belong with it.
    pub fn folder(&self) -> &Path {
        self.folder.path()
    }

    /// A command that runs in the checkout, without inheriting the variables
    /// that tie git-agent to its own repository, such as `GIT_DIR`.
    pub fn isolated_command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.current_dir(&self.path);
        for variable in &self.repository_variables {
            command.env_remove(variable);
        }
        command
    }

    /// Makes the files match `tree`. Ignored and untracked files stay, such as
    /// build output, unless they are in the way of a file of the tree.
    pub fn show_tree(&self, tree: &str) -> Result<()> {
        self.git(&[
            "read-tree",
            "--no-recurse-submodules",
            "-u",
            "--reset",
            tree,
        ])
    }

    /// Whether going from one tree to the other deletes or renames a file.
    pub fn deletes_files(&self, from: &str, to: &str) -> Result<bool> {
        let args = [
            "diff",
            "--quiet",
            "--no-ext-diff",
            "--no-renames",
            "--diff-filter=D",
            from,
            to,
        ];
        let output = self
            .git_command(&args)
            .output()
            .context("failed to run git; is it installed?")?;
        match output.status.code() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => bail!(
                "`git diff` failed in the temporary worktree: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
    }

    /// Removes everything the tree doesn't hold, including ignored files.
    pub fn clean(&self) -> Result<()> {
        make_writable(&self.path);
        self.git(&["clean", "-ffdxq"])
    }

    fn git(&self, args: &[&str]) -> Result<()> {
        let output = self
            .git_command(args)
            .output()
            .context("failed to run git; is it installed?")?;
        if !output.status.success() {
            bail!(
                "`git {}` failed in the temporary worktree: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    /// git in the checkout, without hooks. In a process group of its own, so that
    /// Ctrl-C reaches git-agent, which stops the check, and not a git command
    /// halfway through its work.
    fn git_command(&self, args: &[&str]) -> Command {
        let mut command = self.isolated_command("git");
        command.args(["-c", NO_HOOKS]).args(args);
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        command
    }
}

impl Drop for TempWorktree {
    fn drop(&mut self) {
        make_writable(&self.path);
        let path = self.path.to_string_lossy().into_owned();
        if remove_registered(&self.owner, &path).is_err() {
            let _ = fs::remove_dir_all(&self.path);
            let _ = remove_registered(&self.owner, &path);
        }
    }
}

/// Removes one worktree and its registration, also if its folder is gone. This
/// leaves the user's other worktrees alone, unlike `git worktree prune`, which
/// would also forget one on a disk that isn't mounted right now.
fn remove_registered(owner: &Repo, path: &str) -> Result<String> {
    owner.git(&["worktree", "remove", "--force", path])
}

/// Removes the worktrees of this repository that a git-agent process left
/// behind and is no longer running to remove, such as after a crash. Best
/// effort, because they are in the way of nothing.
fn remove_leftovers(owner: &Repo) {
    // Without a `ps` that works, a running process can't be told from an ended one.
    if !is_running(std::process::id()) {
        return;
    }
    let Ok(list) = owner.git(&["worktree", "list", "--porcelain"]) else {
        return;
    };
    list.lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .filter(|path| is_leftover(Path::new(path), &std::env::temp_dir()))
        .for_each(|path| {
            make_writable(Path::new(path));
            if remove_registered(owner, path).is_ok()
                && let Some(folder) = Path::new(path).parent()
            {
                make_writable(folder);
                let _ = fs::remove_dir_all(folder);
            }
        });
}

/// Whether `path` is a checkout in a private folder directly in `temp_folder`,
/// made by a process that has ended. Anything elsewhere isn't git-agent's, however
/// it's named.
fn is_leftover(path: &Path, temp_folder: &Path) -> bool {
    let Some(folder) = path.parent() else {
        return false;
    };
    let in_temp_folder = folder
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .zip(temp_folder.canonicalize().ok())
        .is_some_and(|(parent, temp_folder)| parent == temp_folder);
    in_temp_folder
        && path.file_name() == Some(OsStr::new(CHECKOUT))
        && folder
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| temp::process_of(name, FOLDER_LABEL))
            .is_some_and(|pid| !is_running(pid))
}

/// Whether a process exists, also if it belongs to another user, which signaling
/// it wouldn't tell. If `ps` can't run, the answer has to be yes, so that a
/// worktree in use is never taken for a leftover.
fn is_running(pid: u32) -> bool {
    Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_or(true, |status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support::{isolated_repo, run};

    /// The process ID of a process that has already ended.
    fn dead_process() -> u32 {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    fn registered(dir: &Path) -> String {
        run(dir, &["worktree", "list", "--porcelain"])
    }

    fn tree_with(dir: &Path, name: &str, text: &str) -> String {
        fs::write(dir.join(name), text).unwrap();
        run(dir, &["add", "--all"]);
        let tree = run(dir, &["write-tree"]).trim().to_owned();
        run(dir, &["reset", "-q", "--hard"]);
        tree
    }

    #[test]
    fn adds_a_checkout_at_the_commit_and_removes_it_again() {
        let (dir, repo) = isolated_repo();
        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        let path = worktree.path().to_owned();
        let folder = worktree.folder().to_owned();
        assert_eq!(
            fs::read_to_string(path.join("lib.rs")).unwrap(),
            "fn a() {}\n"
        );
        assert!(registered(dir.path()).contains(CHECKOUT));
        let mode =
            std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&folder).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o700);

        drop(worktree);
        assert!(!folder.exists());
        assert!(!registered(dir.path()).contains(CHECKOUT));
    }

    #[test]
    fn shows_a_tree_with_its_new_and_deleted_files_and_keeps_build_output() {
        let (dir, repo) = isolated_repo();
        fs::write(dir.path().join(".gitignore"), "out/\n").unwrap();
        fs::write(dir.path().join("gone.txt"), "soon\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "more files"]);
        let tree = {
            fs::remove_file(dir.path().join("gone.txt")).unwrap();
            tree_with(dir.path(), "new.txt", "new\n")
        };
        let head_before = run(dir.path(), &["rev-parse", "HEAD"]);

        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        fs::create_dir(worktree.path().join("out")).unwrap();
        fs::write(worktree.path().join("out/built"), "binary").unwrap();
        worktree.show_tree(&tree).unwrap();
        assert!(worktree.path().join("new.txt").exists());
        assert!(!worktree.path().join("gone.txt").exists());
        assert!(worktree.path().join("out/built").exists());

        worktree.clean().unwrap();
        assert!(!worktree.path().join("out").exists());
        assert!(worktree.path().join("new.txt").exists());

        // The real repository saw none of it.
        assert_eq!(run(dir.path(), &["rev-parse", "HEAD"]), head_before);
        assert!(dir.path().join("gone.txt").exists());
        assert!(!dir.path().join("new.txt").exists());
        assert!(run(dir.path(), &["status", "--porcelain"]).is_empty());
    }

    #[test]
    fn shows_a_tree_while_the_owner_has_a_temporary_index() {
        let (dir, repo) = isolated_repo();
        let tree = tree_with(dir.path(), "new.txt", "new\n");
        let with_index = repo.with_index(dir.path().join(".git/other-index"));
        let worktree = TempWorktree::add(&with_index, "HEAD").unwrap();
        worktree.show_tree(&tree).unwrap();
        assert!(worktree.path().join("new.txt").exists());
    }

    #[test]
    fn doesnt_run_hooks() {
        let (dir, repo) = isolated_repo();
        let hooks = dir.path().join("hooks");
        fs::create_dir(&hooks).unwrap();
        let mark = dir.path().join("hook-ran");
        let script = format!(
            "#!/bin/sh\ntouch {}\n",
            crate::git::shell_quote(&mark.to_string_lossy())
        );
        for hook in ["post-checkout", "post-index-change"] {
            let path = hooks.join(hook);
            fs::write(&path, &script).unwrap();
            let executable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
            fs::set_permissions(&path, executable).unwrap();
        }
        run(dir.path(), &["config", "core.hooksPath", "hooks"]);
        let tree = run(dir.path(), &["rev-parse", "HEAD^{tree}"])
            .trim()
            .to_owned();

        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        worktree.show_tree(&tree).unwrap();
        worktree.clean().unwrap();
        assert!(!mark.exists());
    }

    #[test]
    fn keeps_the_variables_of_git_agents_repository_out_of_its_commands() {
        let (_dir, repo) = isolated_repo();
        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        let command = worktree.isolated_command("true");
        let removed: Vec<String> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        for variable in ["GIT_DIR", "GIT_INDEX_FILE", "GIT_WORK_TREE"] {
            assert!(removed.iter().any(|known| known == variable), "{variable}");
        }
        assert_eq!(command.get_current_dir(), Some(worktree.path()));
    }

    #[test]
    fn cleans_read_only_output_and_nested_repositories() {
        let (dir, repo) = isolated_repo();
        fs::write(dir.path().join(".gitignore"), "out/\nnested/\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "ignore output"]);
        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        let out = worktree.path().join("out");
        fs::create_dir(&out).unwrap();
        fs::write(out.join("built"), "binary").unwrap();
        let locked = std::os::unix::fs::PermissionsExt::from_mode(0o500);
        fs::set_permissions(&out, locked).unwrap();
        let nested = worktree.path().join("nested");
        fs::create_dir(&nested).unwrap();
        run(&nested, &["init", "-q"]);

        worktree.clean().unwrap();
        assert!(!out.exists());
        assert!(!nested.exists());
    }

    #[test]
    fn removes_a_worktree_with_read_only_output() {
        let (dir, repo) = isolated_repo();
        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        let folder = worktree.folder().to_owned();
        let out = worktree.path().join("out");
        fs::create_dir(&out).unwrap();
        let locked = std::os::unix::fs::PermissionsExt::from_mode(0o500);
        fs::set_permissions(&out, locked).unwrap();
        drop(worktree);
        assert!(!folder.exists());
        assert!(!registered(dir.path()).contains(CHECKOUT));
    }

    #[test]
    fn moves_past_a_submodule_when_git_is_set_to_recurse() {
        let (dir, repo) = isolated_repo();
        let library = tempfile::TempDir::new().unwrap();
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
        run(dir.path(), &["config", "submodule.recurse", "true"]);
        run(
            library.path(),
            &["commit", "-q", "--allow-empty", "-m", "two"],
        );
        let moved = run(library.path(), &["rev-parse", "HEAD"]);
        let entry = format!("160000,{},library", moved.trim());
        run(dir.path(), &["update-index", "--cacheinfo", &entry]);
        let tree = run(dir.path(), &["write-tree"]).trim().to_owned();

        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        worktree.show_tree(&tree).unwrap();
    }

    #[test]
    fn removes_a_registration_whose_folder_is_gone() {
        let (dir, repo) = isolated_repo();
        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        fs::remove_dir_all(worktree.path()).unwrap();
        drop(worktree);
        assert!(!registered(dir.path()).contains(CHECKOUT));
    }

    #[test]
    fn removes_what_a_dead_process_left_and_nothing_else() {
        let (dir, repo) = isolated_repo();
        let temp_folder = std::env::temp_dir();
        let unique = std::process::id();
        let add = |parent: &Path, folder: &str, name: &str| {
            let path = parent.join(folder).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let text = path.to_string_lossy().into_owned();
            run(
                dir.path(),
                &["worktree", "add", "-q", "--detach", &text, "HEAD"],
            );
            path
        };
        let elsewhere = tempfile::TempDir::new().unwrap();
        let dead_name = format!("git-agent-worktree-{}-{unique}-dead", dead_process());
        let dead = add(&temp_folder, &dead_name, CHECKOUT);
        let running_name = format!("git-agent-worktree-{unique}-{unique}-live");
        let running = add(&temp_folder, &running_name, CHECKOUT);
        let users_own = add(elsewhere.path(), "my-worktrees", "feature");
        let other_label = add(
            &temp_folder,
            &format!("git-agent-other-{}-{unique}-x", dead_process()),
            CHECKOUT,
        );
        let not_in_temp_folder = add(elsewhere.path(), &dead_name, CHECKOUT);

        drop(TempWorktree::add(&repo, "HEAD").unwrap());

        assert!(!dead.exists());
        assert!(!dead.parent().unwrap().exists());
        let list = registered(dir.path());
        assert!(!list.contains(dead.to_string_lossy().as_ref()));
        for kept in [&running, &users_own, &other_label, &not_in_temp_folder] {
            assert!(kept.exists(), "{}", kept.display());
            assert!(list.contains(kept.to_string_lossy().as_ref()));
        }
        for leftover in [&running, &other_label] {
            fs::remove_dir_all(leftover.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn keeps_the_folder_of_a_leftover_that_cant_be_removed() {
        let (dir, repo) = isolated_repo();
        let name = format!(
            "git-agent-worktree-{}-{}-locked",
            dead_process(),
            std::process::id()
        );
        let folder = std::env::temp_dir().join(name);
        let path = folder.join(CHECKOUT);
        fs::create_dir_all(&folder).unwrap();
        let text = path.to_string_lossy().into_owned();
        run(
            dir.path(),
            &["worktree", "add", "-q", "--detach", &text, "HEAD"],
        );
        run(dir.path(), &["worktree", "lock", &text]);

        drop(TempWorktree::add(&repo, "HEAD").unwrap());
        assert!(
            path.exists(),
            "the registration is still there, so the folder stays"
        );

        run(dir.path(), &["worktree", "unlock", &text]);
        run(dir.path(), &["worktree", "remove", "--force", &text]);
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn removes_a_read_only_leftover() {
        let (dir, repo) = isolated_repo();
        let name = format!(
            "git-agent-worktree-{}-{}-readonly",
            dead_process(),
            std::process::id()
        );
        let folder = std::env::temp_dir().join(name);
        let path = folder.join(CHECKOUT);
        fs::create_dir_all(&folder).unwrap();
        let text = path.to_string_lossy().into_owned();
        run(
            dir.path(),
            &["worktree", "add", "-q", "--detach", &text, "HEAD"],
        );
        let locked = std::os::unix::fs::PermissionsExt::from_mode(0o500);
        fs::set_permissions(&path, locked).unwrap();

        drop(TempWorktree::add(&repo, "HEAD").unwrap());
        assert!(!folder.exists());
        assert!(!registered(dir.path()).contains(&text));
    }

    #[test]
    fn tells_whether_a_later_tree_deletes_files() {
        let (dir, repo) = isolated_repo();
        let first = repo.tree("HEAD").unwrap();
        fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        let with_new = run(dir.path(), &["write-tree"]).trim().to_owned();
        fs::rename(dir.path().join("new.txt"), dir.path().join("moved.txt")).unwrap();
        run(dir.path(), &["add", "--all"]);
        let moved = run(dir.path(), &["write-tree"]).trim().to_owned();
        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        assert!(!worktree.deletes_files(&first, &with_new).unwrap());
        assert!(worktree.deletes_files(&with_new, &first).unwrap());
        assert!(worktree.deletes_files(&with_new, &moved).unwrap());
        assert!(worktree.deletes_files("not-a-tree", &first).is_err());
    }

    #[test]
    fn tells_a_running_process_from_an_ended_one() {
        assert!(is_running(std::process::id()));
        assert!(!is_running(dead_process()));
        assert!(is_running(1), "the first process belongs to another user");
    }

    #[test]
    fn leaves_a_users_worktree_alone_when_its_disk_is_away() {
        let (dir, repo) = isolated_repo();
        let parent = tempfile::TempDir::new().unwrap();
        let away = parent.path().join("away");
        let text = away.to_string_lossy().into_owned();
        run(
            dir.path(),
            &["worktree", "add", "-q", "--detach", &text, "HEAD"],
        );
        fs::remove_dir_all(&away).unwrap();

        drop(TempWorktree::add(&repo, "HEAD").unwrap());
        assert!(registered(dir.path()).contains(&text));
    }
}
