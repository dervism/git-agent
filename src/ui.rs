//! Terminal output, and reading what the user types.
//!
//! The commit message goes to stdout; status, warnings and prompts go to stderr.
//! This module is the only one that may write to them (`clippy.toml`), so that all of it
//! goes through [`for_terminal`].
#![allow(clippy::disallowed_methods)]

use crate::safe_text;
use anyhow::{Context, Result};
use std::fmt::{self, Display};
use std::io::{self, BufRead, IsTerminal, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Writes a line to stderr, for the user. Unlike `eprintln!`, it doesn't panic
/// when the terminal has gone away, so that a hangup can end the program properly.
macro_rules! say {
    ($($line:tt)*) => {
        to_stderr(&format!($($line)*))
    };
}

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
/// All that [`paint`] writes, and the only escape sequences allowed through to the terminal.
const STYLES: [&str; 6] = [RESET, BOLD, DIM, RED, GREEN, YELLOW];

/// Whether stdout is a terminal rather than a file or a pipe.
pub fn stdout_is_terminal() -> bool {
    io::stdout().is_terminal()
}

/// Whether stderr is a terminal rather than a file or a pipe.
pub fn stderr_is_terminal() -> bool {
    io::stderr().is_terminal()
}

fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("NO_COLOR").is_none()
            && io::stderr().is_terminal()
            && io::stdout().is_terminal()
    })
}

fn paint(style: &str, text: &str) -> String {
    painted(color_enabled(), style, text)
}

/// `text` in `style` when `color` is on, and as it is when it isn't.
fn painted(color: bool, style: &str, text: &str) -> String {
    if color {
        format!("{style}{text}{RESET}")
    } else {
        text.to_owned()
    }
}

pub fn bold(text: &str) -> String {
    paint(BOLD, text)
}

pub fn dim(text: &str) -> String {
    paint(DIM, text)
}

pub fn red(text: &str) -> String {
    paint(RED, text)
}

pub fn green(text: &str) -> String {
    paint(GREEN, text)
}

pub fn yellow(text: &str) -> String {
    paint(YELLOW, text)
}

/// `text` with everything unsafe escaped, so that text from a repository, a model or a
/// server can't change the terminal. With `color`, the styles of [`paint`] stay: they
/// can dim or color text, and can't hide it.
fn for_terminal(text: &str, color: bool) -> Shown {
    if !color {
        return Shown(safe_text::escaped(text));
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((at, style)) = next_style(rest) {
        out.push_str(&safe_text::escaped(&rest[..at]));
        out.push_str(style);
        rest = &rest[at + style.len()..];
    }
    out.push_str(&safe_text::escaped(rest));
    Shown(out)
}

/// Text that [`for_terminal`] made safe, ready to be written. It isn't a `&str`, so it can't
/// be passed on as text by mistake and made safe a second time, which would show the styles
/// it kept as text.
struct Shown(String);

impl Display for Shown {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Where the first of the [`STYLES`] in `text` starts, and which it is.
fn next_style(text: &str) -> Option<(usize, &'static str)> {
    text.match_indices('\x1b').find_map(|(at, _)| {
        STYLES
            .iter()
            .find(|style| text[at..].starts_with(**style))
            .map(|style| (at, *style))
    })
}

/// A stdout for a child process that writes where the user reads status, and not to the
/// reader of git-agent's output. A child that inherits stdout dies of `SIGPIPE` once that
/// reader has gone, even after it has done its work.
pub fn child_stdout_on_stderr() -> io::Result<Stdio> {
    stdio_cloned_from(io::stderr().as_fd())
}

/// A [`Stdio`] that writes to `stream`, which stays open for the caller too.
fn stdio_cloned_from(stream: BorrowedFd<'_>) -> io::Result<Stdio> {
    stream.try_clone_to_owned().map(Stdio::from)
}

/// The one way to stderr for everything the user reads there.
fn to_stderr(line: &str) {
    let _ = write_line(&mut io::stderr(), line, color_enabled());
}

/// The program that reads git-agent's output has closed it, as `head` does once it has read
/// enough. It is no failure to report: git-agent ends without a word, as git does.
#[derive(Debug, PartialEq, Eq)]
pub struct OutputClosed;

impl OutputClosed {
    /// What a shell reports for a program that `SIGPIPE` killed (128 and 13), which is how it
    /// reports git here. It is a status, and not a death by a signal.
    pub const EXIT_STATUS: u8 = 141;
}

impl fmt::Display for OutputClosed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the program reading the output has closed it")
    }
}

impl std::error::Error for OutputClosed {}

/// Writes a line to stdout, after making it safe for the terminal. Fails with
/// [`OutputClosed`] when the reader has gone away.
pub fn out(line: &str) -> Result<()> {
    write_to_stdout(&mut io::stdout().lock(), line, color_enabled())
}

/// Writes text to stdout without styles, for what a pipeline reads: a model's cleaned
/// reply, or the user's own settings. Those are safe already, so escaping changes nothing
/// in them, and it keeps a new caller from printing what isn't. Fails with [`OutputClosed`]
/// when the reader has gone away.
pub fn plain_out(text: &str) -> Result<()> {
    write_to_stdout(&mut io::stdout().lock(), text, false)
}

/// Writes `line` to `stdout`, the one way to it. Only a broken pipe means that the reader has
/// gone away, and is [`OutputClosed`]: a search for that kind of error anywhere would also
/// catch a failed write to `gh` or to git, which is a failure to report.
fn write_to_stdout(stdout: &mut impl Write, line: &str, color: bool) -> Result<()> {
    write_line(stdout, line, color).map_err(|err| match err.kind() {
        io::ErrorKind::BrokenPipe => OutputClosed.into(),
        _ => anyhow::Error::new(err).context("failed to write to stdout"),
    })
}

/// Writes `line` to `to` as [`for_terminal`] makes it, and ends the line.
fn write_line(to: &mut impl Write, line: &str, color: bool) -> io::Result<()> {
    writeln!(to, "{}", for_terminal(line, color))
}

/// Formats a number with thousands separators, e.g. 131,072.
pub fn thousands(number: usize) -> String {
    let digits = number.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Formats a size in bytes, e.g. 220 MB.
pub fn bytes(count: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match unit {
        0 => format!("{count} bytes"),
        _ if value >= 10.0 => format!("{value:.0} {}", UNITS[unit]),
        _ => format!("{value:.1} {}", UNITS[unit]),
    }
}

pub fn info(message: impl Display) {
    say!("{message}");
}

pub fn warn(message: impl Display) {
    say!("{} {message}", yellow("warning:"));
}

pub fn error(err: &anyhow::Error) {
    say!("{} {err:#}", red("error:"));
}

/// Prints `text` between two rules, the first titled `heading`. Fails with [`OutputClosed`]
/// when the reader has gone away.
pub fn show_framed(heading: &str, text: &str) -> Result<()> {
    write_framed(&mut io::stdout().lock(), heading, text, color_enabled())
}

/// Writes `text` between two rules to `stdout`, the first titled `heading`, and stops at the
/// first line that can't be written.
fn write_framed(stdout: &mut impl Write, heading: &str, text: &str, color: bool) -> Result<()> {
    const WIDTH: usize = 64;
    let title = format!("── {heading} ");
    let title_rule = "─".repeat(WIDTH.saturating_sub(title.chars().count()));
    let rule = |text: &str| painted(color, DIM, text);
    let top = format!("\n{}", rule(&format!("{title}{title_rule}")));
    let bottom = rule(&"─".repeat(WIDTH));
    [top.as_str(), text, bottom.as_str()]
        .into_iter()
        .try_for_each(|line| write_to_stdout(stdout, line, color))
}

/// Prints the model's answer: framed under `heading` in a terminal, and as it
/// is when the output goes to a file or a pipe. Fails with [`OutputClosed`] when the reader has
/// gone away.
pub fn show_answer(heading: &str, text: &str) -> Result<()> {
    if io::stdout().is_terminal() {
        show_framed(heading, text)
    } else {
        plain_out(text)
    }
}

/// Asks for a follow-up question, trimmed, or `None` at end of input. An empty one means the
/// user is done, which [`crate::conversation::discuss`] decides.
pub fn ask_follow_up() -> Result<Option<String>> {
    read_line(&format!(
        "Follow-up question {} ›",
        dim("(Enter to finish)")
    ))
}

/// Shows an animated progress line on stderr until dropped.
///
/// Relies on a single-threaded runtime, so the animation cannot draw while it is being cleared.
pub struct Spinner(Option<tokio::task::JoinHandle<()>>);

impl Spinner {
    pub fn start(label: String) -> Self {
        if !io::stderr().is_terminal() {
            info(&label);
            return Self(None);
        }
        let label = for_terminal(&label, color_enabled());
        let task = tokio::spawn(async move {
            const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let started = Instant::now();
            let mut ticks = tokio::time::interval(Duration::from_millis(100));
            for frame in FRAMES.iter().cycle() {
                ticks.tick().await;
                let elapsed = started.elapsed().as_secs();
                let _ = write!(
                    io::stderr(),
                    "\r{frame} {label} {}",
                    dim(&format!("{elapsed}s"))
                );
            }
        });
        Self(Some(task))
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
            let _ = write!(io::stderr(), "\r\x1b[2K");
        }
    }
}

/// Prompts on stderr and reads a trimmed line, or `None` at end of input.
pub fn read_line(prompt: &str) -> Result<Option<String>> {
    let line = prompt_and_read(
        &mut io::stderr(),
        &mut io::stdin().lock(),
        prompt,
        color_enabled(),
    )?;
    if line.is_none() {
        say!("");
    }
    Ok(line)
}

/// Writes `prompt` to `to`, then reads a trimmed line from `from`, or `None` at end of input.
/// A prompt that can't be written ends here with an error: nobody sees it, so nobody answers.
fn prompt_and_read(
    to: &mut impl Write,
    from: &mut impl BufRead,
    prompt: &str,
    color: bool,
) -> Result<Option<String>> {
    write!(to, "{} ", for_terminal(prompt, color))
        .and_then(|()| to.flush())
        .context("failed to show the prompt")?;
    let mut line = String::new();
    let read = from.read_line(&mut line).context("failed to read input")?;
    Ok((read > 0).then(|| line.trim().to_owned()))
}

/// Asks a yes/no question. Enter means yes; end of input means no.
pub fn confirm(question: &str) -> Result<bool> {
    loop {
        let Some(answer) = read_line(&format!("{question} {}", dim("[Y/n] ›")))? else {
            return Ok(false);
        };
        match answer.to_lowercase().as_str() {
            "" | "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => say!("Please answer y or n."),
        }
    }
}

/// Lets the user edit `text` in place on one line. Returns `None` if they
/// emptied it or pressed Esc.
pub fn edit_line(label: &str, text: &str) -> Result<Option<String>> {
    let prompt = inquire::Text::new(label).with_initial_value(text);
    let edited = match prompt.prompt_skippable() {
        // Ctrl-C quits at once, as it does elsewhere in the review.
        Err(inquire::InquireError::OperationInterrupted) => std::process::exit(130),
        result => result?,
    };
    Ok(edited
        .map(|edited| edited.trim().to_owned())
        .filter(|edited| !edited.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What [`for_terminal`] makes of `text`.
    fn shown(text: &str, color: bool) -> String {
        for_terminal(text, color).to_string()
    }

    #[test]
    fn keeps_only_the_styles_it_writes_itself() {
        for style in STYLES {
            let painted = format!("{style}text{RESET}");
            assert_eq!(shown(&painted, true), painted);
            assert_eq!(
                shown(&painted, false),
                safe_text::escaped(&painted),
                "without color, a style is only text"
            );
            assert!(!shown(&painted, false).contains('\x1b'));
        }
    }

    #[test]
    fn escapes_every_other_sequence_with_or_without_color() {
        for color in [true, false] {
            for (text, wanted) in [
                ("\x1b[2Jx", "\\033[2Jx"),
                ("\x1b[8mhidden", "\\033[8mhidden"),
                ("\x1b[1;31mx", "\\033[1;31mx"),
                ("\x1b]0;title\x07", "\\033]0;title\\a"),
                ("\u{9b}2J", "\\302\\2332J"),
                ("end\x1b", "end\\033"),
                ("over\rwrite", "over\\rwrite"),
                ("rev\u{202e}x", "rev\\342\\200\\256x"),
            ] {
                assert_eq!(shown(text, color), wanted, "{text:?}");
            }
        }
    }

    #[test]
    fn handles_an_allowed_style_next_to_a_forbidden_sequence() {
        assert_eq!(
            shown("\x1b[1m\x1b[2Jbold\x1b[0m", true),
            "\x1b[1m\\033[2Jbold\x1b[0m"
        );
        assert_eq!(
            shown("a\x1b[33mæøå\nb\x1b[0m\tc", true),
            "a\x1b[33mæøå\nb\x1b[0m\tc"
        );
    }

    #[test]
    fn keeps_text_that_is_safe_already_with_or_without_color() {
        for color in [true, false] {
            for text in [
                "æøå\nline\tfour",
                "feat: add a greeting\n\nSays hei 👋🏽, and \\033 as text.",
                "",
            ] {
                assert_eq!(shown(text, color), text, "{text:?}");
            }
        }
    }

    /// What [`write_line`] writes for `line`.
    fn written(line: &str, color: bool) -> String {
        let mut to = Vec::new();
        write_line(&mut to, line, color).expect("a Vec takes any write");
        String::from_utf8(to).expect("for_terminal writes UTF-8")
    }

    #[test]
    fn writes_its_own_styles_once_with_color() {
        let rule = format!("{DIM}── Proposed commit message ──{RESET}");
        assert_eq!(written(&rule, true), format!("{rule}\n"));
        let row = format!("* fake  {DIM}loaded, 4,096 tokens of context{RESET}");
        assert_eq!(written(&row, true), format!("{row}\n"));
    }

    #[test]
    fn adds_one_line_end() {
        for color in [true, false] {
            assert_eq!(written("", color), "\n");
            assert_eq!(written("one", color), "one\n");
            assert_eq!(written("one\ntwo", color), "one\ntwo\n");
            assert_eq!(written("one\n", color), "one\n\n");
        }
    }

    /// A writer that takes `room` bytes, and then fails every write with `kind`.
    struct Closing {
        kind: io::ErrorKind,
        room: usize,
        taken: Vec<u8>,
        failures: usize,
    }

    impl Closing {
        fn after(room: usize, kind: io::ErrorKind) -> Self {
            Self {
                kind,
                room,
                taken: Vec::new(),
                failures: 0,
            }
        }

        fn at_once(kind: io::ErrorKind) -> Self {
            Self::after(0, kind)
        }
    }

    impl Write for Closing {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.room == 0 {
                self.failures += 1;
                return Err(io::Error::from(self.kind));
            }
            let taken = bytes.len().min(self.room);
            self.taken.extend_from_slice(&bytes[..taken]);
            self.room -= taken;
            Ok(taken)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A writer that takes everything, and fails to flush it.
    struct FlushFails;

    impl Write for FlushFails {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    const OTHER_FAILURES: [io::ErrorKind; 8] = [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::WriteZero,
        io::ErrorKind::StorageFull,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::ConnectionAborted,
        io::ErrorKind::NotConnected,
        io::ErrorKind::TimedOut,
        io::ErrorKind::Other,
    ];

    #[test]
    fn writes_a_line_to_a_reader_that_is_there() {
        let mut stdout = Vec::new();
        write_to_stdout(&mut stdout, "one\ntwo", false).unwrap();
        assert_eq!(stdout, b"one\ntwo\n");
    }

    #[test]
    fn a_broken_pipe_means_the_reader_has_gone_away() {
        let err = write_to_stdout(&mut Closing::at_once(io::ErrorKind::BrokenPipe), "x", true)
            .unwrap_err();
        assert_eq!(err.downcast_ref::<OutputClosed>(), Some(&OutputClosed));
    }

    #[test]
    fn a_broken_pipe_in_the_middle_of_a_line_means_the_same() {
        let mut stdout = Closing::after(3, io::ErrorKind::BrokenPipe);
        let err = write_to_stdout(&mut stdout, "a long line", false).unwrap_err();
        assert!(err.is::<OutputClosed>());
        assert_eq!(stdout.taken, b"a l");
    }

    #[test]
    fn no_other_failure_means_the_reader_has_gone_away() {
        for kind in OTHER_FAILURES {
            let err = write_to_stdout(&mut Closing::at_once(kind), "x", false).unwrap_err();
            assert!(!err.is::<OutputClosed>(), "{kind}");
            assert_eq!(err.to_string(), "failed to write to stdout", "{kind}");
            let cause = err
                .downcast_ref::<io::Error>()
                .expect("the failure is kept");
            assert_eq!(cause.kind(), kind);
        }
    }

    #[test]
    fn a_closed_output_stays_one_with_context_around_it() {
        let err = write_to_stdout(&mut Closing::at_once(io::ErrorKind::BrokenPipe), "x", false)
            .context("nothing was changed")
            .unwrap_err();
        assert!(err.is::<OutputClosed>());
    }

    /// What [`write_framed`] writes for the frame of "text" under "Head".
    fn frame() -> String {
        let mut stdout = Vec::new();
        write_framed(&mut stdout, "Head", "text", false).unwrap();
        String::from_utf8(stdout).unwrap()
    }

    #[test]
    fn frames_text_between_two_rules_with_the_heading_in_the_first() {
        let top = format!("── Head {}", "─".repeat(64 - "── Head ".chars().count()));
        let rule = "─".repeat(64);
        assert_eq!(frame(), format!("\n{top}\ntext\n{rule}\n"));
    }

    #[test]
    fn dims_the_rules_of_a_frame_with_color_and_only_then() {
        let top = format!("── Head {}", "─".repeat(64 - "── Head ".chars().count()));
        let rule = "─".repeat(64);
        let mut stdout = Vec::new();
        write_framed(&mut stdout, "Head", "text", true).unwrap();
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!("\n{DIM}{top}{RESET}\ntext\n{DIM}{rule}{RESET}\n")
        );
    }

    #[test]
    fn stops_a_frame_at_the_first_line_that_cant_be_written() {
        let whole = frame().len();
        for room in 0..whole {
            let mut stdout = Closing::after(room, io::ErrorKind::BrokenPipe);
            let err = write_framed(&mut stdout, "Head", "text", false).unwrap_err();
            assert!(err.is::<OutputClosed>(), "room for {room} of {whole} bytes");
            assert_eq!(stdout.failures, 1, "room for {room} of {whole} bytes");
        }
        let mut stdout = Closing::after(whole, io::ErrorKind::BrokenPipe);
        write_framed(&mut stdout, "Head", "text", false).unwrap();
        assert_eq!(stdout.failures, 0);
    }

    #[test]
    fn a_frame_fails_for_other_reasons_too_without_saying_the_reader_is_gone() {
        let mut stdout = Closing::at_once(io::ErrorKind::PermissionDenied);
        let err = write_framed(&mut stdout, "Head", "text", false).unwrap_err();
        assert!(!err.is::<OutputClosed>());
    }

    #[test]
    fn exits_with_the_status_a_shell_reports_for_git_killed_by_sigpipe() {
        assert_eq!(OutputClosed::EXIT_STATUS, 128 + 13);
    }

    #[test]
    fn gives_a_child_the_stream_it_clones_and_keeps_that_stream_open() {
        let (mut reader, mut writer) = io::pipe().unwrap();
        let stdout = stdio_cloned_from(writer.as_fd()).unwrap();

        let status = std::process::Command::new("sh")
            .args(["-c", "echo from the child"])
            .stdout(stdout)
            .status()
            .unwrap();
        writeln!(writer, "from the owner").unwrap();
        drop(writer);

        assert!(status.success());
        let mut read = String::new();
        io::Read::read_to_string(&mut reader, &mut read).unwrap();
        assert_eq!(read, "from the child\nfrom the owner\n");
    }

    #[test]
    fn can_give_a_child_stderr_for_its_stdout() {
        assert!(child_stdout_on_stderr().is_ok());
    }

    #[test]
    fn asks_and_reads_a_trimmed_line() {
        for (typed, answer) in [
            ("  yes \n", "yes"),
            ("\n", ""),
            ("no", "no"),
            (" é ø\r\n", "é ø"),
        ] {
            let mut asked = Vec::new();
            let line = prompt_and_read(&mut asked, &mut typed.as_bytes(), "Go?", false).unwrap();
            assert_eq!(line.as_deref(), Some(answer), "{typed:?}");
            assert_eq!(asked, b"Go? ");
        }
    }

    #[test]
    fn reads_nothing_at_the_end_of_input() {
        let mut asked = Vec::new();
        let line = prompt_and_read(&mut asked, &mut "".as_bytes(), "Go?", false).unwrap();
        assert_eq!(line, None);
        assert_eq!(asked, b"Go? ");
    }

    #[test]
    fn escapes_the_prompt_it_shows() {
        let mut asked = Vec::new();
        prompt_and_read(&mut asked, &mut "\n".as_bytes(), "x\x1b[2J", true).unwrap();
        assert_eq!(asked, b"x\\033[2J ");
    }

    #[test]
    fn ends_with_an_error_when_the_prompt_cant_be_written_and_reads_nothing() {
        let kinds = OTHER_FAILURES
            .into_iter()
            .chain([io::ErrorKind::BrokenPipe]);
        for kind in kinds {
            let mut typed = io::Cursor::new("yes\n");
            let err =
                prompt_and_read(&mut Closing::at_once(kind), &mut typed, "Go?", false).unwrap_err();
            assert_eq!(err.to_string(), "failed to show the prompt", "{kind}");
            assert_eq!(typed.position(), 0, "{kind}: the answer was waited for");
        }
    }

    #[test]
    fn ends_with_an_error_when_the_prompt_cant_be_flushed() {
        let mut typed = io::Cursor::new("yes\n");
        let err = prompt_and_read(&mut FlushFails, &mut typed, "Go?", false).unwrap_err();
        assert_eq!(err.to_string(), "failed to show the prompt");
        assert_eq!(typed.position(), 0);
    }

    #[test]
    fn ends_with_an_error_when_the_input_cant_be_read() {
        let mut asked = Vec::new();
        let err =
            prompt_and_read(&mut asked, &mut [0xff, b'\n'].as_slice(), "Go?", false).unwrap_err();
        assert_eq!(err.to_string(), "failed to read input");
    }

    #[test]
    fn separates_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(4096), "4,096");
        assert_eq!(thousands(131072), "131,072");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(bytes(40), "40 bytes");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(220 * 1024 * 1024), "220 MB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
    }
}
