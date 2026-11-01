//! Terminal output and the interactive review prompt.
//!
//! The commit message goes to stdout; status, warnings and prompts go to stderr.

use crate::git::{self, Repo};
use crate::prompt;
use anyhow::{Context, Result};
use std::fmt::Display;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const SUBJECT_LIMIT: usize = 72;

const EDIT_HELP: &str = "\
# Edit the commit message above. Lines starting with '#' are ignored.
# Save and close the editor to return to git-agent.
# An empty message keeps the previous one.
";

/// An HTML comment, since lines starting with '#' are headings in Markdown.
const PULL_REQUEST_EDIT_HELP: &str = "\
<!-- Edit the pull request above: the first line is the title, the rest is the
description. Save and close the editor to return to git-agent. An empty file
keeps the previous version. -->
";

/// What a review shows, and the choices that accept the proposal besides
/// editing, regenerating, feedback and quitting.
pub struct Review<T: 'static> {
    /// Title of the frame around the proposal, e.g. "Proposed commit message".
    pub heading: &'static str,
    /// What the proposal's first line is called, e.g. "subject line".
    pub first_line: &'static str,
    /// What the help calls the proposal, e.g. "message".
    pub noun: &'static str,
    /// What the model is asked to revise, e.g. "commit message".
    pub what: &'static str,
    /// Typed as their first letter or in full; "y" picks the first.
    pub accept: &'static [Choice<T>],
    /// Help for quitting, e.g. "quit without committing".
    pub quit: &'static str,
}

pub struct Choice<T> {
    pub word: &'static str,
    pub help: &'static str,
    pub value: T,
}

fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("NO_COLOR").is_none()
            && io::stderr().is_terminal()
            && io::stdout().is_terminal()
    })
}

fn paint(code: &str, text: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

pub fn bold(text: &str) -> String {
    paint("1", text)
}

pub fn dim(text: &str) -> String {
    paint("2", text)
}

pub fn red(text: &str) -> String {
    paint("31", text)
}

pub fn green(text: &str) -> String {
    paint("32", text)
}

pub fn yellow(text: &str) -> String {
    paint("33", text)
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

pub fn info(message: impl Display) {
    eprintln!("{message}");
}

pub fn warn(message: impl Display) {
    eprintln!("{} {message}", yellow("warning:"));
}

pub fn error(err: &anyhow::Error) {
    eprintln!("{} {err:#}", red("error:"));
}

pub fn show_proposal<T>(review: &Review<T>, text: &str) {
    const WIDTH: usize = 64;
    let title = format!("── {} ", review.heading);
    let title_rule = "─".repeat(WIDTH.saturating_sub(title.chars().count()));
    println!("\n{}", dim(&format!("{title}{title_rule}")));
    println!("{text}");
    println!("{}", dim(&"─".repeat(WIDTH)));
    let first_len = text.lines().next().unwrap_or_default().chars().count();
    if first_len > SUBJECT_LIMIT {
        warn(format!(
            "the {} is {first_len} characters; {SUBJECT_LIMIT} or fewer is conventional",
            review.first_line
        ));
    }
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
        let task = tokio::spawn(async move {
            const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let started = Instant::now();
            let mut ticks = tokio::time::interval(Duration::from_millis(100));
            for frame in FRAMES.iter().cycle() {
                ticks.tick().await;
                let elapsed = started.elapsed().as_secs();
                eprint!("\r{frame} {label} {}", dim(&format!("{elapsed}s")));
            }
        });
        Self(Some(task))
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
            eprint!("\r\x1b[2K");
        }
    }
}

pub enum Action<T> {
    /// One of the review's accept choices.
    Accept(T),
    Edit,
    Regenerate,
    Refine(String),
    Quit,
}

/// Asks what to do with the proposal. End of input counts as quitting.
pub fn ask_action<T: Copy>(review: &Review<T>) -> Result<Action<T>> {
    let words = review.accept.iter().map(|choice| choice.word);
    let mut items: Vec<String> = words
        .chain(["edit", "regenerate", "feedback", "quit"])
        .map(|word| format!("{}{}", bold(&format!("[{}]", &word[..1])), &word[1..]))
        .collect();
    items.push(dim("[?]"));
    let menu = format!("{} ›", items.join("  "));
    loop {
        let Some(input) = read_line(&menu)? else {
            return Ok(Action::Quit);
        };
        let (choice, rest) = input.split_once(' ').unwrap_or((&input, ""));
        let choice = choice.to_lowercase();
        if let Some(value) = accepted(review, &choice) {
            return Ok(Action::Accept(value));
        }
        match choice.as_str() {
            "e" | "edit" => return Ok(Action::Edit),
            "r" | "regenerate" => return Ok(Action::Regenerate),
            "f" | "feedback" => {
                let feedback = match rest.trim() {
                    "" => read_line("What should change? ›")?.unwrap_or_default(),
                    rest => rest.to_owned(),
                };
                if !feedback.is_empty() {
                    return Ok(Action::Refine(feedback));
                }
            }
            "q" | "quit" | "n" | "no" => return Ok(Action::Quit),
            "" => {}
            "?" | "h" | "help" => eprintln!("{}", help(review)),
            other => eprintln!("Unknown choice {other:?}. Type ? for help."),
        }
    }
}

/// The value of the accept choice typed as `choice`.
fn accepted<T: Copy>(review: &Review<T>, choice: &str) -> Option<T> {
    if matches!(choice, "y" | "yes") {
        return review.accept.first().map(|first| first.value);
    }
    review
        .accept
        .iter()
        .find(|accept| choice == accept.word || choice == &accept.word[..1])
        .map(|accept| accept.value)
}

fn help<T>(review: &Review<T>) -> String {
    let mut lines: Vec<String> = review
        .accept
        .iter()
        .map(|choice| format!("  {}  {}", &choice.word[..1], choice.help))
        .collect();
    lines.push(format!("  e  edit the {} in your editor", review.noun));
    lines.push(format!("  r  generate a new {}", review.noun));
    lines.push("  f  tell the model what to change, e.g. `f mention the API change`".to_owned());
    lines.push(format!("  q  {}", review.quit));
    lines.join("\n")
}

/// Prompts on stderr and reads a trimmed line, or `None` at end of input.
fn read_line(prompt: &str) -> Result<Option<String>> {
    eprint!("{prompt} ");
    io::stderr().flush()?;
    let mut line = String::new();
    let read = io::stdin()
        .lock()
        .read_line(&mut line)
        .context("failed to read input")?;
    if read == 0 {
        eprintln!();
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
            _ => eprintln!("Please answer y or n."),
        }
    }
}

/// Lets the user edit `message` in their editor. Returns `None` if they emptied it.
pub fn edit_message(repo: &Repo, message: &str) -> Result<Option<String>> {
    // COMMIT_EDITMSG is git's own scratch file, so editors apply commit message highlighting.
    let path = repo.git_path("COMMIT_EDITMSG")?;
    let edited = edit(&path, &format!("{message}\n\n{EDIT_HELP}"))?;
    let edited = strip_comments(&edited);
    Ok((!edited.is_empty()).then_some(edited))
}

/// Lets the user edit a pull request's title and description in their editor.
/// Returns `None` if they emptied it.
pub fn edit_pull_request(repo: &Repo, text: &str) -> Result<Option<String>> {
    // A .md file, so editors apply Markdown highlighting.
    let path = repo.git_path("GIT_AGENT_PR_EDITMSG.md")?;
    let edited = edit(&path, &format!("{text}\n\n{PULL_REQUEST_EDIT_HELP}"))?;
    let edited = strip_edit_help(&edited);
    Ok((!edited.is_empty()).then_some(edited))
}

/// Writes `text` to `path`, opens it in the user's editor, and returns what they saved.
fn edit(path: &Path, text: &str) -> Result<String> {
    fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))?;
    git::edit_file(path)?;
    fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))
}

/// The edited pull request without the help comment, even if the user changed its end.
fn strip_edit_help(text: &str) -> String {
    let start = PULL_REQUEST_EDIT_HELP.lines().next().unwrap_or_default();
    let kept = text.find(start).map_or(text, |index| &text[..index]);
    prompt::normalize(kept)
}

fn strip_comments(text: &str) -> String {
    let kept: Vec<&str> = text.lines().filter(|line| !line.starts_with('#')).collect();
    prompt::normalize(&kept.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(4096), "4,096");
        assert_eq!(thousands(131072), "131,072");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn strips_comment_lines_and_padding() {
        let edited = format!("fix: thing  \n\nbody line\n\n{EDIT_HELP}");
        assert_eq!(strip_comments(&edited), "fix: thing\n\nbody line");
        assert_eq!(strip_comments(EDIT_HELP), "");
    }

    #[test]
    fn keeps_markdown_headings_but_strips_the_pull_request_help() {
        let edited = format!("feat: x\n\nWhy.\n\n## Changes\n- a\n\n{PULL_REQUEST_EDIT_HELP}");
        assert_eq!(
            strip_edit_help(&edited),
            "feat: x\n\nWhy.\n\n## Changes\n- a"
        );
        assert_eq!(strip_edit_help(PULL_REQUEST_EDIT_HELP), "");
    }

    const REVIEW: Review<u8> = Review {
        heading: "Proposed thing",
        first_line: "title",
        noun: "thing",
        what: "thing",
        accept: &[
            Choice {
                word: "create",
                help: "create it",
                value: 1,
            },
            Choice {
                word: "draft",
                help: "create a draft",
                value: 2,
            },
        ],
        quit: "quit without creating it",
    };

    #[test]
    fn accepts_choices_by_letter_or_word() {
        assert_eq!(accepted(&REVIEW, "c"), Some(1));
        assert_eq!(accepted(&REVIEW, "draft"), Some(2));
        assert_eq!(accepted(&REVIEW, "y"), Some(1));
        assert_eq!(accepted(&REVIEW, "e"), None);
        let help = help(&REVIEW);
        assert!(help.starts_with("  c  create it\n  d  create a draft\n  e  edit the thing"));
        assert!(help.ends_with("  q  quit without creating it"));
    }
}
