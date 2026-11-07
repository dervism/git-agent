//! Runs the `--check` command once in a temporary worktree, and stops it again
//! if it takes too long or the user asks.

use super::signals::Stopped;
use crate::git::worktree::TempWorktree;
use crate::safe_text::one_line;
use anyhow::{Context, Result, bail};
use regex_lite::Regex;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::sync::LazyLock;
use std::time::Duration;
use tokio::process::{Child, Command};

/// How long a stopped command gets to finish before it's killed.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// How much of the end of the output to read, and how many lines of it to show.
const OUTPUT_TAIL_BYTES: u64 = 64 * 1024;
const OUTPUT_LINES: usize = 20;
/// What the shell exits with when it can't run a command.
const CANNOT_EXECUTE: i32 = 126;
const NOT_FOUND: i32 = 127;
/// What a shell exits with when a program it ran was killed by these signals.
const KILLED_BY_SIGKILL: i32 = 128 + 9;
const KILLED_BY_SIGTERM: i32 = 128 + 15;

pub struct BuildCheck {
    pub command: String,
    /// How long one run of the command may take.
    pub timeout: Duration,
}

/// How one run of the command ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Ran {
    Pass,
    /// It ran and reported failure, with the end of its output.
    Fail(String),
    TimedOut,
    /// The shell couldn't run it, with the end of its output.
    CouldNotStart(String),
    /// A signal ended it, such as when the system ran out of memory, which says
    /// nothing about the code it was checking.
    Killed,
}

impl Ran {
    fn from_exit(status: ExitStatus, output: String) -> Self {
        match status.code() {
            Some(0) => Self::Pass,
            Some(CANNOT_EXECUTE | NOT_FOUND) => Self::CouldNotStart(output),
            Some(KILLED_BY_SIGKILL | KILLED_BY_SIGTERM) | None => Self::Killed,
            Some(_) => Self::Fail(output),
        }
    }
}

/// Runs the command in the worktree. If `stop` finishes first, the command is
/// stopped and this fails with what `stop` gave.
pub async fn run_command<F: Future<Output = Stopped>>(
    worktree: &TempWorktree,
    check: &BuildCheck,
    stop: Pin<&mut F>,
) -> Result<Ran> {
    let mut output = output_file(&worktree.folder().join("check-output.txt"))?;
    let mut child = match Command::from(shell_command(worktree, check, &output)?).spawn() {
        Ok(child) => child,
        Err(error) => return Ok(Ran::CouldNotStart(error.to_string())),
    };
    let group = child.id();
    let ended = tokio::select! {
        status = child.wait() => Some(status.context("failed to wait for the check command")?),
        () = tokio::time::sleep(check.timeout) => None,
        stopped = stop => {
            stop_group(&mut child, group).await;
            bail!(stopped);
        }
    };
    let Some(status) = ended else {
        stop_group(&mut child, group).await;
        return Ok(Ran::TimedOut);
    };
    // What the command started in the background must not outlive it, because
    // it would keep running in a folder that's about to go.
    signal_group(group, "KILL");
    Ok(Ran::from_exit(status, end_of_output(&mut output)?))
}

/// A new file for the command's output, which it can't fill memory with or
/// hold open as a pipe. It's new each time, because an earlier run may have left
/// something else at the name. Reading through this handle afterward means that
/// if the command replaces the file, git-agent doesn't print what it was replaced with.
fn output_file(path: &Path) -> Result<File> {
    let _ = fs::remove_file(path);
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))
}

/// The command in the worktree, in its own process group, with no input, and
/// without what ties git-agent to its repository and its model server.
fn shell_command(
    worktree: &TempWorktree,
    check: &BuildCheck,
    output: &File,
) -> Result<std::process::Command> {
    let mut command = worktree.isolated_command("sh");
    command
        .arg("-c")
        .arg(&check.command)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output.try_clone()?)
        .env_remove("GIT_AGENT_API_KEY");
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    Ok(command)
}

/// Asks the command and what it started to stop, then makes them.
async fn stop_group(child: &mut Child, group: Option<u32>) {
    signal_group(group, "TERM");
    let stopped = tokio::time::timeout(STOP_GRACE, child.wait()).await;
    signal_group(group, "KILL");
    if stopped.is_err() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

/// Sends a signal to a process group. Through the shell's `kill`, because the
/// standard library has no way to signal a group.
fn signal_group(group: Option<u32>, signal: &str) {
    if let Some(group) = group {
        let _ = std::process::Command::new("sh")
            .args(["-c", &format!("kill -s {signal} -- -{group} 2>/dev/null")])
            .status();
    }
}

/// The terminal codes a build tool colors its output with.
static COLOR_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("\x1b\\[[0-9;]*[A-Za-z]").expect("the pattern is valid"));

/// The last lines of the output, without colors and safe to print.
fn end_of_output(file: &mut File) -> Result<String> {
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(OUTPUT_TAIL_BYTES)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let shown = &lines[lines.len().saturating_sub(OUTPUT_LINES)..];
    Ok(shown
        .iter()
        .map(|line| one_line(&COLOR_CODE.replace_all(line, "")))
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support::isolated_repo;
    use std::future::pending;
    use std::pin::pin;
    use std::time::Instant;

    fn check(command: &str) -> BuildCheck {
        BuildCheck {
            command: command.to_owned(),
            timeout: Duration::from_secs(60),
        }
    }

    fn never() -> impl Future<Output = Stopped> {
        pending()
    }

    /// Runs `command` in a new temporary worktree, and returns how it ended and
    /// the worktree, which is still there.
    async fn run_in_worktree(
        command: &str,
        timeout: Duration,
    ) -> (Ran, TempWorktree, tempfile::TempDir) {
        let (dir, repo) = isolated_repo();
        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        let check = BuildCheck {
            command: command.to_owned(),
            timeout,
        };
        let ran = run_command(&worktree, &check, pin!(never())).await.unwrap();
        (ran, worktree, dir)
    }

    #[tokio::test]
    async fn tells_a_pass_from_a_failure_and_keeps_the_end_of_the_output() {
        let long = Duration::from_secs(60);
        assert_eq!(run_in_worktree("true", long).await.0, Ran::Pass);
        let (ran, _, _) = run_in_worktree("echo out; echo err >&2; exit 3", long).await;
        assert_eq!(ran, Ran::Fail("out\nerr".to_owned()));
    }

    #[tokio::test]
    async fn tells_a_command_that_cant_start_from_one_that_fails() {
        let long = Duration::from_secs(60);
        for command in ["no-such-command-at-all", "./lib.rs"] {
            let (ran, _, _) = run_in_worktree(command, long).await;
            assert!(matches!(ran, Ran::CouldNotStart(_)), "{ran:?}");
        }
    }

    #[tokio::test]
    async fn runs_in_the_checkout_with_no_input() {
        let long = Duration::from_secs(60);
        let (ran, _, _) = run_in_worktree("test -f lib.rs && test -f .git && cat", long).await;
        assert_eq!(ran, Ran::Pass);
    }

    #[tokio::test]
    async fn shows_only_the_end_of_huge_output_and_makes_it_safe_to_print() {
        let long = Duration::from_secs(60);
        let (ran, _, _) = run_in_worktree(
            "yes filler | head -c 5000000; printf '\\033[31mred\\033[0m \\033]0;title\\n'; exit 1",
            long,
        )
        .await;
        let Ran::Fail(output) = ran else {
            panic!("{ran:?}");
        };
        assert_eq!(output.lines().count(), OUTPUT_LINES);
        assert!(output.ends_with("red \\033]0;title"), "{output}");
        assert!(!output.contains('\u{1b}'));
    }

    #[tokio::test]
    async fn doesnt_print_a_file_the_command_put_in_place_of_the_output() {
        let (dir, repo) = isolated_repo();
        let secret = dir.path().join("secret.txt");
        fs::write(&secret, "the secret\n").unwrap();
        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        let output = worktree.folder().join("check-output.txt");
        let command = format!(
            "echo real output; rm -f {output}; ln -s {secret} {output}; exit 1",
            output = crate::git::shell_quote(&output.to_string_lossy()),
            secret = crate::git::shell_quote(&secret.to_string_lossy()),
        );
        let ran = run_command(&worktree, &check(&command), pin!(never()))
            .await
            .unwrap();
        assert_eq!(ran, Ran::Fail("real output".to_owned()));
    }

    fn is_running(pid: &str) -> bool {
        std::process::Command::new("sh")
            .args(["-c", &format!("ps -o stat= -p {pid} | grep -qv Z")])
            .status()
            .unwrap()
            .success()
    }

    /// Waits for the processes whose IDs `worktree` wrote to `pids` to be gone.
    fn all_gone(worktree: &TempWorktree) -> bool {
        let pids = fs::read_to_string(worktree.path().join("pids")).unwrap();
        (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(100));
            !pids.split_whitespace().any(is_running)
        })
    }

    #[tokio::test]
    async fn gives_a_command_the_chance_to_stop_by_itself_at_the_limit() {
        let (ran, worktree, _dir) = run_in_worktree(
            "trap 'echo cleaned up > marker; exit 0' TERM; sleep 300 & wait",
            Duration::from_millis(500),
        )
        .await;
        assert_eq!(ran, Ran::TimedOut);
        assert!(worktree.path().join("marker").exists());
    }

    #[tokio::test]
    async fn tells_a_command_a_signal_ended_from_one_that_failed() {
        let long = Duration::from_secs(60);
        assert_eq!(run_in_worktree("kill -9 $$", long).await.0, Ran::Killed);
        let child_killed = r#"sh -c 'kill -9 $$'; exit $?"#;
        assert_eq!(run_in_worktree(child_killed, long).await.0, Ran::Killed);
        let child_stopped = r#"sh -c 'kill -15 $$'; exit $?"#;
        assert_eq!(run_in_worktree(child_stopped, long).await.0, Ran::Killed);
    }

    #[tokio::test]
    async fn doesnt_write_through_what_an_earlier_run_left_at_the_output_name() {
        let (dir, repo) = isolated_repo();
        let target = dir.path().join("target.txt");
        fs::write(&target, "keep\n").unwrap();
        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        let output = worktree.folder().join("check-output.txt");
        std::os::unix::fs::symlink(&target, &output).unwrap();
        let ran = run_command(&worktree, &check("echo new"), pin!(never()))
            .await
            .unwrap();
        assert_eq!(ran, Ran::Pass);
        assert_eq!(fs::read_to_string(target).unwrap(), "keep\n");
    }

    #[tokio::test]
    async fn stops_a_command_and_what_it_started_at_the_limit() {
        let started = Instant::now();
        let (ran, worktree, _dir) = run_in_worktree(
            "sleep 300 & echo $$ > pids; echo $! >> pids; wait",
            Duration::from_millis(500),
        )
        .await;
        assert_eq!(ran, Ran::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(20));
        assert!(all_gone(&worktree));
    }

    #[tokio::test]
    async fn isnt_held_up_by_what_a_command_left_running() {
        let started = Instant::now();
        let (ran, worktree, _dir) = run_in_worktree(
            "sleep 300 & echo $! > pids; exit 0",
            Duration::from_secs(60),
        )
        .await;
        assert_eq!(ran, Ran::Pass);
        assert!(started.elapsed() < Duration::from_secs(20));
        assert!(all_gone(&worktree));
    }

    #[tokio::test]
    async fn stops_the_command_when_asked_and_passes_on_why() {
        let (_dir, repo) = isolated_repo();
        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        let check = check("sleep 300 & echo $$ > pids; echo $! >> pids; wait");
        let stop = pin!(async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Stopped(143)
        });
        let error = run_command(&worktree, &check, stop).await.err().unwrap();
        assert_eq!(error.downcast_ref::<Stopped>(), Some(&Stopped(143)));
        assert!(format!("{error:#}").contains("nothing was committed"));
        assert!(all_gone(&worktree));
    }

    #[test]
    fn keeps_what_ties_git_agent_to_its_repository_out_of_the_command() {
        let (_dir, repo) = isolated_repo();
        let worktree = TempWorktree::add(&repo, "HEAD").unwrap();
        let output = output_file(&worktree.folder().join("output")).unwrap();
        let command = shell_command(&worktree, &check("true"), &output).unwrap();
        let removed: Vec<String> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        for name in ["GIT_DIR", "GIT_INDEX_FILE", "GIT_AGENT_API_KEY"] {
            assert!(removed.iter().any(|known| known == name), "{name}");
        }
        assert_eq!(command.get_current_dir(), Some(worktree.path()));
    }
}
