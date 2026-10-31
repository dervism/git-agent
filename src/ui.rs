//! Terminal output and the interactive review prompt.
//!
//! The commit message goes to stdout; status, warnings and prompts go to stderr.

use crate::git::Repo;
use crate::prompt;
use anyhow::{Context, Result};
use std::fmt::Display;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const SUBJECT_LIMIT: usize = 72;

const EDIT_HELP: &str = "\
# Edit the commit message above. Lines starting with '#' are ignored.
# Save and close the editor to return to git-agent.
# An empty message keeps the previous one.
";

const ACTION_HELP: &str = "\
  c  commit with this message
  e  edit the message in your editor
  r  generate a new message
  f  tell the model what to change, e.g. `f mention the API change`
  q  quit without committing";

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

pub fn info(message: impl Display) {
    eprintln!("{message}");
}

pub fn warn(message: impl Display) {
    eprintln!("{} {message}", yellow("warning:"));
}

pub fn error(err: &anyhow::Error) {
    eprintln!("{} {err:#}", red("error:"));
}

pub fn show_message(message: &str) {
    const WIDTH: usize = 64;
    let title = "── Proposed commit message ";
    let title_rule = "─".repeat(WIDTH - title.chars().count());
    println!("\n{}", dim(&format!("{title}{title_rule}")));
    println!("{message}");
    println!("{}", dim(&"─".repeat(WIDTH)));
    let subject_len = message.lines().next().unwrap_or_default().chars().count();
    if subject_len > SUBJECT_LIMIT {
        warn(format!(
            "the subject line is {subject_len} characters; {SUBJECT_LIMIT} or fewer is conventional"
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

pub enum Action {
    Commit,
    Edit,
    Regenerate,
    Refine(String),
    Quit,
}

/// Asks what to do with the proposed message. End of input counts as quitting.
pub fn ask_action() -> Result<Action> {
    loop {
        let menu = format!(
            "{}ommit  {}dit  {}egenerate  {}eedback  {}uit  {} ›",
            bold("[c]"),
            bold("[e]"),
            bold("[r]"),
            bold("[f]"),
            bold("[q]"),
            dim("[?]"),
        );
        let Some(input) = read_line(&menu)? else {
            return Ok(Action::Quit);
        };
        let (choice, rest) = input.split_once(' ').unwrap_or((&input, ""));
        match choice.to_lowercase().as_str() {
            "c" | "commit" | "y" | "yes" => return Ok(Action::Commit),
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
            "?" | "h" | "help" => eprintln!("{ACTION_HELP}"),
            other => eprintln!("Unknown choice {other:?}. Type ? for help."),
        }
    }
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

/// Lets the user edit `message` in their editor. Returns `None` if they emptied it.
pub fn edit_message(repo: &Repo, message: &str) -> Result<Option<String>> {
    // COMMIT_EDITMSG is git's own scratch file, so editors apply commit message highlighting.
    let path = repo.git_path("COMMIT_EDITMSG")?;
    fs::write(&path, format!("{message}\n\n{EDIT_HELP}"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    repo.edit_file(&path)?;
    let edited =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let edited = strip_comments(&edited);
    Ok((!edited.is_empty()).then_some(edited))
}

fn strip_comments(text: &str) -> String {
    let kept: Vec<&str> = text.lines().filter(|line| !line.starts_with('#')).collect();
    prompt::normalize(&kept.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_comment_lines_and_padding() {
        let edited = format!("fix: thing  \n\nbody line\n\n{EDIT_HELP}");
        assert_eq!(strip_comments(&edited), "fix: thing\n\nbody line");
        assert_eq!(strip_comments(EDIT_HELP), "");
    }
}
