//! Terminal output, and reading what the user types.
//!
//! The commit message goes to stdout; status, warnings and prompts go to stderr.
//! This module is the only one that may write to them (`clippy.toml`), so that all of it
//! goes through [`for_terminal`].
#![allow(clippy::disallowed_methods)]

use crate::safe_text;
use anyhow::{Context, Result};
use std::fmt::Display;
use std::io::{self, BufRead, IsTerminal, Write};
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
    if color_enabled() {
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

/// The one way to stderr for everything the user reads there.
fn to_stderr(line: &str) {
    let _ = write_line(&mut io::stderr(), line, color_enabled());
}

/// Writes a line to stdout, after making it safe for the terminal.
pub fn out(line: &str) {
    to_stdout(line, color_enabled());
}

/// Writes text to stdout without styles, for what a pipeline reads: a model's cleaned
/// reply, or the user's own settings. Those are safe already, so escaping changes nothing
/// in them, and it keeps a new caller from printing what isn't.
pub fn plain_out(text: &str) {
    to_stdout(text, false);
}

/// The one way to stdout. Like `println!`, it panics when stdout has gone away.
fn to_stdout(line: &str, color: bool) {
    write_line(&mut io::stdout().lock(), line, color)
        .unwrap_or_else(|err| panic!("failed printing to stdout: {err}"));
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

/// Prints `text` between two rules, the first titled `heading`.
pub fn show_framed(heading: &str, text: &str) {
    const WIDTH: usize = 64;
    let title = format!("── {heading} ");
    let title_rule = "─".repeat(WIDTH.saturating_sub(title.chars().count()));
    out(&format!("\n{}", dim(&format!("{title}{title_rule}"))));
    out(text);
    out(&dim(&"─".repeat(WIDTH)));
}

/// Prints the model's answer: framed under `heading` in a terminal, and as it
/// is when the output goes to a file or a pipe.
pub fn show_answer(heading: &str, text: &str) {
    if io::stdout().is_terminal() {
        show_framed(heading, text);
    } else {
        plain_out(text);
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
    let _ = write!(io::stderr(), "{} ", for_terminal(prompt, color_enabled()));
    io::stderr().flush()?;
    let mut line = String::new();
    let read = io::stdin()
        .lock()
        .read_line(&mut line)
        .context("failed to read input")?;
    if read == 0 {
        say!("");
        return Ok(None);
    }
    Ok(Some(line.trim().to_owned()))
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
