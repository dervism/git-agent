//! Private temporary folders.
//!
//! The system temp folder can be shared with other users, who could otherwise
//! redirect, read or change what git-agent keeps there. A new folder that only
//! the owner can enter keeps that out, also when an editor saves a file inside
//! it by replacing it.

use anyhow::{Context, Result};
use std::fs::{self, DirBuilder};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many names to try before giving up, in case some are taken.
const ATTEMPTS: u32 = 16;

const PREFIX: &str = "git-agent-";

/// A new folder that only the owner can enter, removed when this is dropped.
pub struct PrivateDir(PathBuf);

impl PrivateDir {
    /// Creates a folder in the system temp folder. `label` says what it's for.
    pub fn create(label: &str) -> Result<Self> {
        Self::create_under(&std::env::temp_dir(), label)
    }

    fn create_under(parent: &Path, label: &str) -> Result<Self> {
        let pid = process::id();
        let mut created = Self::create_in(parent, &folder_name(label, pid, 0));
        for attempt in 1..ATTEMPTS {
            match created {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    created = Self::create_in(parent, &folder_name(label, pid, attempt));
                }
                _ => break,
            }
        }
        created.with_context(|| {
            format!(
                "failed to create a private temporary folder in {}",
                parent.display()
            )
        })
    }

    /// Fails if anything is already at the name, including a symlink, which
    /// isn't followed.
    fn create_in(parent: &Path, name: &str) -> io::Result<Self> {
        // Absolute, because git reads a relative index path from the top of the
        // work tree, whatever folder git-agent was started in.
        let path = std::path::absolute(parent.join(name))?;
        owner_only(&mut DirBuilder::new()).create(&path)?;
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        if fs::remove_dir_all(&self.0).is_err() {
            make_writable(&self.0);
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

/// Makes everything in `path` writable, so it can be removed. Builds sometimes
/// leave read-only files and folders behind.
pub fn make_writable(path: &Path) {
    let mut command = Command::new("chmod");
    command.arg("-R").arg("u+w").arg(path).stderr(Stdio::null());
    // In a process group of its own, so Ctrl-C doesn't stop it halfway and turn
    // into a confusing failure of what comes after it.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let _ = command.status();
}

#[cfg(unix)]
fn owner_only(builder: &mut DirBuilder) -> &mut DirBuilder {
    std::os::unix::fs::DirBuilderExt::mode(builder, 0o700)
}

#[cfg(not(unix))]
fn owner_only(builder: &mut DirBuilder) -> &mut DirBuilder {
    builder
}

/// A name no earlier folder is likely to have, which also says which process
/// made it, so [`process_of`] can tell leftovers from folders in use.
fn folder_name(label: &str, pid: u32, attempt: u32) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{PREFIX}{label}-{pid}-{nanos}-{attempt}")
}

/// The process that made the folder `name` for `label`, if git-agent named it.
pub fn process_of(name: &str, label: &str) -> Option<u32> {
    let rest = name.strip_prefix(PREFIX)?.strip_prefix(label)?;
    let rest = rest.strip_prefix('-')?;
    rest.split('-').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn makes_a_folder_only_the_owner_can_enter_and_removes_it() {
        let parent = TempDir::new().unwrap();
        let dir = PrivateDir::create_under(parent.path(), "test").unwrap();
        let path = dir.path().to_owned();
        assert!(path.starts_with(parent.path()));
        let mode =
            std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&path).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o700);

        fs::write(path.join("inside.txt"), "text").unwrap();
        drop(dir);
        assert!(!path.exists());
    }

    #[test]
    fn gives_an_absolute_path_for_a_relative_temp_folder() {
        // The crate on crates.io has no tmp/ folder.
        fs::create_dir_all("tmp").unwrap();
        let dir = PrivateDir::create_under(Path::new("tmp"), "test").unwrap();
        assert!(dir.path().is_absolute(), "{}", dir.path().display());
        assert!(dir.path().is_dir());
    }

    #[test]
    fn removes_read_only_contents() {
        let parent = TempDir::new().unwrap();
        let dir = PrivateDir::create_under(parent.path(), "test").unwrap();
        let path = dir.path().to_owned();
        let inside = path.join("read-only");
        fs::create_dir(&inside).unwrap();
        fs::write(inside.join("file"), "text").unwrap();
        let locked = std::os::unix::fs::PermissionsExt::from_mode(0o500);
        fs::set_permissions(&inside, locked).unwrap();
        drop(dir);
        assert!(!path.exists());
    }

    #[test]
    fn makes_a_new_folder_each_time() {
        let parent = TempDir::new().unwrap();
        let first = PrivateDir::create_under(parent.path(), "test").unwrap();
        let second = PrivateDir::create_under(parent.path(), "test").unwrap();
        assert_ne!(first.path(), second.path());
    }

    #[test]
    fn wont_use_a_folder_that_is_already_there() {
        let parent = TempDir::new().unwrap();
        fs::create_dir(parent.path().join("taken")).unwrap();
        let error = PrivateDir::create_in(parent.path(), "taken").err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn wont_follow_a_symlink_at_the_name() {
        let parent = TempDir::new().unwrap();
        let target = parent.path().join("target");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, parent.path().join("to-folder")).unwrap();
        std::os::unix::fs::symlink(
            parent.path().join("nothing"),
            parent.path().join("dangling"),
        )
        .unwrap();
        for name in ["to-folder", "dangling"] {
            let error = PrivateDir::create_in(parent.path(), name).err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{name}");
        }
        assert!(!parent.path().join("nothing").exists());
        assert!(target.exists());
    }

    #[test]
    fn dropping_doesnt_follow_a_symlink_inside() {
        let parent = TempDir::new().unwrap();
        let outside = parent.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep.txt"), "text").unwrap();
        let dir = PrivateDir::create_in(parent.path(), "private").unwrap();
        std::os::unix::fs::symlink(&outside, dir.path().join("link")).unwrap();
        drop(dir);
        assert!(outside.join("keep.txt").exists());
    }

    #[test]
    fn fails_rather_than_falling_back_when_it_cant_create_the_folder() {
        let parent = TempDir::new().unwrap();
        let missing = parent.path().join("missing");
        let error = PrivateDir::create_under(&missing, "test").err().unwrap();
        assert!(format!("{error:#}").contains("private temporary folder"));
    }

    #[test]
    fn tells_which_process_made_a_folder() {
        let name = folder_name("worktree", 1234, 0);
        assert_eq!(process_of(&name, "worktree"), Some(1234));
        assert_eq!(process_of(&name, "edit"), None);
        assert_eq!(process_of("git-agent-worktree-x-1", "worktree"), None);
        assert_eq!(process_of("somebody-elses-folder", "worktree"), None);
        assert_eq!(process_of("git-agent-worktrees-1-2", "worktree"), None);
    }
}
