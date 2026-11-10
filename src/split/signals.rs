//! The signals that stop a check, and keeping them working after one.

#[cfg(unix)]
use crate::ui;
use std::fmt;
use std::io;

/// The user or the system stopped git-agent during a check. The value is the
/// exit code a program ended by that signal has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped(pub u8);

impl fmt::Display for Stopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("stopped during the check, so nothing was committed")
    }
}

impl std::error::Error for Stopped {}

/// The signals that should stop a check, listened for from when this is made,
/// so none is missed between two runs of the command. The command is in a
/// process group of its own, so the terminal's signals don't reach it, and
/// git-agent has to pass them on.
pub struct StopSignals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    /// Only when output goes to a terminal: under `nohup`, none does, and listening
    /// would undo it.
    #[cfg(unix)]
    hangup: Option<tokio::signal::unix::Signal>,
}

#[cfg(unix)]
impl StopSignals {
    pub fn new() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: if ui::stdout_is_terminal() || ui::stderr_is_terminal() {
                Some(signal(SignalKind::hangup())?)
            } else {
                None
            },
        })
    }

    pub async fn next(&mut self) -> Stopped {
        tokio::select! {
            _ = self.interrupt.recv() => Stopped(130),
            _ = self.terminate.recv() => Stopped(143),
            _ = hang_up(self.hangup.as_mut()) => Stopped(129),
        }
    }
}

/// Waits for a hangup, or forever if there's no terminal to hang up.
#[cfg(unix)]
async fn hang_up(signal: Option<&mut tokio::signal::unix::Signal>) {
    match signal {
        Some(signal) => {
            signal.recv().await;
        }
        None => std::future::pending().await,
    }
}

#[cfg(not(unix))]
impl StopSignals {
    pub fn new() -> io::Result<Self> {
        Ok(Self {})
    }

    pub async fn next(&mut self) -> Stopped {
        let _ = tokio::signal::ctrl_c().await;
        Stopped(130)
    }
}

/// Makes these signals end the program again once a check is over: a program
/// that has listened for a signal no longer ends by it on its own. This has a
/// thread of its own, because the main one can be waiting for the user's input.
pub fn end_on_stop_signals() {
    std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let Ok(runtime) = runtime else {
            return;
        };
        runtime.block_on(async {
            if let Ok(mut signals) = StopSignals::new() {
                let Stopped(code) = signals.next().await;
                std::process::exit(i32::from(code));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use crate::git::test_support::isolated_repo;
    use crate::git::worktree::TempWorktree;
    use crate::split::check::check_trees;
    use crate::split::command::BuildCheck;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const HELPER_VARIABLE: &str = "GIT_AGENT_TEST_SIGNAL_HELPER";

    /// Not a test by itself: `signals_end_the_program_after_a_check_while_it_waits`
    /// runs it in a process of its own. It runs a check, then waits with its
    /// main thread, as a prompt does.
    #[tokio::test]
    #[ignore = "runs only as a helper of another test"]
    async fn helper() {
        if std::env::var_os(HELPER_VARIABLE).is_none() {
            return;
        }
        {
            let (_dir, repo) = isolated_repo();
            let head = repo.resolve("HEAD").unwrap().unwrap();
            let worktree = TempWorktree::add(&repo, &head).unwrap();
            let check = BuildCheck {
                command: "true".to_owned(),
                timeout: Duration::from_secs(60),
            };
            let tree = repo.tree("HEAD").unwrap();
            check_trees(&worktree, &check, &head, &[tree], |_| {})
                .await
                .unwrap();
        }
        std::thread::sleep(Duration::from_millis(200));
        #[allow(clippy::print_stdout)]
        {
            println!("helper is ready");
        }
        std::thread::sleep(Duration::from_secs(30));
    }

    /// The command that runs the test `name` of this test binary alone, in a process of its own,
    /// with `variable` set: such a test is ignored, and does nothing without it. On macOS,
    /// Gatekeeper can stall a copy of a freshly built test binary at its start, so a test that
    /// uses this is skipped there.
    fn test_alone(name: &str, variable: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "--nocapture", name])
            .env(variable, "1");
        command
    }

    /// Signals reach a whole program, so this runs `helper` in a process of its own: a copy of
    /// the test binary. Gatekeeper on macOS can stall such a copy at its start, so it runs on
    /// Linux, as CI does, and not on macOS.
    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "starts a copy of the test binary, which Gatekeeper on macOS can stall"
    )]
    fn signals_end_the_program_after_a_check_while_it_waits() {
        for (signal, code) in [("INT", 130), ("TERM", 143)] {
            let mut child = test_alone("split::signals::tests::helper", HELPER_VARIABLE)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let stdout = child.stdout.take().unwrap();
            let ready = BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
                .any(|line| line.contains("helper is ready"));
            assert!(ready);
            let started = Instant::now();
            let sent = Command::new("kill")
                .args(["-s", signal, &child.id().to_string()])
                .status()
                .unwrap();
            assert!(sent.success());
            let status = child.wait().unwrap();
            assert_eq!(status.code(), Some(code), "{signal}");
            assert!(started.elapsed() < Duration::from_secs(10));
        }
    }
}
