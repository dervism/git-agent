//! How a run that failed ends: the status git-agent exits with, and whether the user is told
//! why.

use crate::split::signals::Stopped;
use crate::ui::OutputClosed;

/// The status of a run that failed for a reason of its own, as opposed to a signal that
/// stopped it or the reader of its output going away.
pub const FAILURE: u8 = 1;

/// Whether `err` is shown to the user. It isn't when the reader of git-agent's output went
/// away: git ends without a word then too.
pub fn is_reported(err: &anyhow::Error) -> bool {
    !err.is::<OutputClosed>()
}

/// The status git-agent exits with after `err`: that of the signal that stopped it, 141 when
/// the reader of its output went away, and [`FAILURE`] for anything else.
pub fn status(err: &anyhow::Error) -> u8 {
    match (err.downcast_ref::<Stopped>(), err.is::<OutputClosed>()) {
        (Some(Stopped(code)), _) => *code,
        (None, true) => OutputClosed::EXIT_STATUS,
        (None, false) => FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, anyhow};
    use std::io;

    #[test]
    fn exits_with_141_without_a_word_when_the_reader_of_the_output_went_away() {
        let closed = anyhow::Error::from(OutputClosed);
        assert_eq!(status(&closed), 141);
        assert!(!is_reported(&closed));
    }

    #[test]
    fn exits_the_same_with_context_around_the_closed_output() {
        let closed = Err::<(), _>(OutputClosed)
            .context("nothing was changed")
            .context("git-agent stopped")
            .unwrap_err();
        assert_eq!(status(&closed), 141);
        assert!(!is_reported(&closed));
    }

    #[test]
    fn maps_a_broken_pipe_to_gh_to_the_failure_status_and_reports_it() {
        let to_gh = Err::<(), _>(io::Error::from(io::ErrorKind::BrokenPipe))
            .context("failed to send the description to gh")
            .unwrap_err();
        assert_eq!(status(&to_gh), FAILURE);
        assert!(is_reported(&to_gh));
    }

    #[test]
    fn maps_other_errors_to_the_failure_status_and_reports_them() {
        for err in [
            anyhow!("git commit failed"),
            anyhow::Error::from(io::Error::other("disk full")),
        ] {
            assert_eq!(status(&err), FAILURE);
            assert!(is_reported(&err));
        }
        assert_eq!(FAILURE, 1);
    }

    #[test]
    fn exits_with_the_status_of_the_signal_that_stopped_it_and_says_so() {
        for code in [129, 130, 143] {
            let err = anyhow::Error::from(Stopped(code)).context("the check was stopped");
            assert_eq!(status(&err), code);
            assert!(is_reported(&err));
        }
    }
}
