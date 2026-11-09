//! A blob's content, read from git as it comes, so a large one needn't be held whole.

use std::io::{self, Read};
use std::process::{Child, ChildStdout};

/// The content of a blob, read from `git cat-file` as git writes it. Reading to the end fails if
/// git failed, so a blob git couldn't read never passes for an empty one. Dropping it stops git.
pub struct Blob {
    child: Child,
    stdout: ChildStdout,
    hash: String,
}

impl Blob {
    /// The blob `hash` that `child`, a `git cat-file` with its output piped, writes.
    pub(super) fn new(mut child: Child, hash: &str) -> io::Result<Self> {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("git cat-file was started without its output"))?;
        Ok(Self {
            child,
            stdout,
            hash: hash.to_owned(),
        })
    }
}

impl Read for Blob {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.stdout.read(buffer)?;
        if read == 0 && !buffer.is_empty() && !self.child.wait()?.success() {
            return Err(io::Error::other(format!(
                "git couldn't read the blob {}",
                self.hash
            )));
        }
        Ok(read)
    }
}

impl Drop for Blob {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
