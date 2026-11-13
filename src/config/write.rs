//! Replaces a file whole, so a reader never sees half of it and nobody else can read it.

use anyhow::{Context, Result, bail};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;

/// The most links followed to find the file a symbolic link stands for.
const MOST_LINKS: usize = 40;

/// Writes `contents` to `path`, creating its folder. The text goes to a new file next to it,
/// readable only by the user, which then takes the place of `path`. A symbolic link at `path`
/// stays, and the file it points to is replaced. A file that is read-only is not replaced.
/// A failure leaves the old file as it was.
pub fn replace_file(path: &Path, contents: &[u8]) -> Result<()> {
    let target = followed(path)?;
    refuse_if_read_only(&target)?;
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }
    let temporary = beside(&target);
    let file = create_private(&temporary)?;
    let written = fill(file, contents)
        .and_then(|()| fs::rename(&temporary, &target).map_err(Into::into))
        .map(|()| sync_folder_of(&target));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// The file at the end of the symbolic links that start at `path`, or `path` itself. A chain
/// of more than [`MOST_LINKS`] links is an error.
fn followed(path: &Path) -> Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..=MOST_LINKS {
        match fs::read_link(&current) {
            Ok(link) => current = current.parent().unwrap_or(Path::new("")).join(link),
            Err(_) => return Ok(current),
        }
    }
    bail!(
        "{} is a chain of more than {MOST_LINKS} symbolic links",
        path.display()
    )
}

fn refuse_if_read_only(target: &Path) -> Result<()> {
    match fs::metadata(target) {
        Ok(metadata) if metadata.permissions().readonly() => {
            bail!("{} is read-only", target.display())
        }
        _ => Ok(()),
    }
}

/// Makes the rename itself survive a crash. The file is already in place, so a folder that
/// can't be synced is not an error.
fn sync_folder_of(target: &Path) {
    #[cfg(unix)]
    if let Some(folder) = target.parent() {
        let folder = if folder.as_os_str().is_empty() {
            Path::new(".")
        } else {
            folder
        };
        let _ = File::open(folder).and_then(|folder| folder.sync_all());
    }
    #[cfg(not(unix))]
    let _ = target;
}

fn beside(target: &Path) -> PathBuf {
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    target.with_file_name(format!(".{name}.{}.tmp", process::id()))
}

/// A new file at `path` that only the user can read. It fails if there is one already.
fn create_private(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))
}

fn fill(mut file: File, contents: &[u8]) -> Result<()> {
    file.write_all(contents)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn writes_the_text_and_creates_the_folder() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a").join("b").join("config.toml");
        replace_file(&path, b"one").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one");
        replace_file(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(leftovers(path.parent().unwrap()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn only_the_user_can_read_the_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        replace_file(&path, b"new").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_stays_and_its_target_is_replaced() {
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("dotfiles").join("real.toml");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, "old").unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let relative = dir.path().join("relative.toml");
        std::os::unix::fs::symlink("dotfiles/real.toml", &relative).unwrap();

        replace_file(&link, b"through the link").unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(fs::read(&real).unwrap(), b"through the link");

        replace_file(&relative, b"through the relative link").unwrap();
        assert!(fs::symlink_metadata(&relative).unwrap().is_symlink());
        assert_eq!(fs::read(&real).unwrap(), b"through the relative link");
        assert!(leftovers(dir.path()).is_empty());
        assert!(leftovers(real.parent().unwrap()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_file_that_does_not_exist_yet_is_followed() {
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real.toml");
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        replace_file(&link, b"new").unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(fs::read(&real).unwrap(), b"new");
    }

    #[cfg(unix)]
    fn chain_of(length: usize, dir: &Path) -> PathBuf {
        let real = dir.join("real.toml");
        fs::write(&real, "old").unwrap();
        let mut previous = real;
        for number in 0..length {
            let link = dir.join(format!("link-{number}"));
            std::os::unix::fs::symlink(&previous, &link).unwrap();
            previous = link;
        }
        previous
    }

    #[cfg(unix)]
    #[test]
    fn a_chain_of_links_within_the_limit_is_followed() {
        let dir = TempDir::new().unwrap();
        let start = chain_of(MOST_LINKS, dir.path());
        replace_file(&start, b"new").unwrap();
        assert_eq!(fs::read(dir.path().join("real.toml")).unwrap(), b"new");
        assert!(fs::symlink_metadata(&start).unwrap().is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn a_chain_of_more_links_than_the_limit_is_refused() {
        let dir = TempDir::new().unwrap();
        let start = chain_of(MOST_LINKS + 5, dir.path());
        let error = replace_file(&start, b"new").unwrap_err();
        assert!(error.to_string().contains("symbolic links"), "{error}");
        assert_eq!(fs::read(dir.path().join("real.toml")).unwrap(), b"old");
        for entry in fs::read_dir(dir.path()).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            let expected = name == "real.toml";
            assert_eq!(entry.file_type().unwrap().is_file(), expected, "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_file_is_not_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        let error = replace_file(&path, b"new").unwrap_err();
        assert!(error.to_string().contains("read-only"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert!(leftovers(dir.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_file_behind_a_link_is_not_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real.toml");
        fs::write(&real, "old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o444)).unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(replace_file(&link, b"new").is_err());
        assert_eq!(fs::read(&real).unwrap(), b"old");
    }

    #[test]
    fn syncing_the_folder_of_a_bare_name_or_a_missing_folder_does_not_fail() {
        sync_folder_of(Path::new("config.toml"));
        sync_folder_of(Path::new("no/such/folder/config.toml"));
    }

    #[test]
    fn a_failed_write_removes_its_temporary_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("inside"), "kept").unwrap();
        assert!(replace_file(&path, b"new").is_err());
        assert!(leftovers(dir.path()).is_empty());
        assert_eq!(fs::read(path.join("inside")).unwrap(), b"kept");
    }

    #[test]
    fn an_existing_temporary_file_is_not_overwritten() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "old").unwrap();
        let in_the_way = beside(&path);
        fs::write(&in_the_way, "someone else's").unwrap();
        assert!(replace_file(&path, b"new").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(fs::read(&in_the_way).unwrap(), b"someone else's");
    }
}
