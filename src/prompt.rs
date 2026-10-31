//! Builds the conversation sent to the model and cleans up its replies.

use crate::fit::{self, FileFit};
use crate::git::Changes;
use crate::llm::Message;
use std::fmt::Write;

/// Rough number of bytes per token. Common tokenizers average 3 to 4 bytes per
/// token for code and English, and about 3 for most other scripts.
const BYTES_PER_TOKEN: usize = 3;
/// Room for the headings around the changes and the chat template's markup.
const FRAMING_BYTES: usize = 256;

/// How the changes were fitted into the model's context.
pub struct Fit {
    /// Estimated size of the conversation as sent.
    pub prompt_tokens: usize,
    /// Estimated size of the conversation with nothing left out.
    pub full_tokens: usize,
    /// How much of each file's diff was sent.
    pub files: Vec<FileFit>,
    /// Whether the unchanged lines around the changes were left out.
    pub context_trimmed: bool,
    /// Files left out of the list of changed files.
    pub list_lines_left_out: usize,
}

impl Fit {
    pub fn is_complete(&self) -> bool {
        self.list_lines_left_out == 0 && self.files.iter().all(FileFit::is_whole)
    }
}

pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(BYTES_PER_TOKEN)
}

/// Tokens kept free for the reply, including any reasoning, and for a round of feedback.
fn reply_reserve(context_length: usize) -> usize {
    (context_length / 4).clamp(1024, 8192)
}

/// The system prompt plus a user message describing `changes`, shortened to
/// fit a context of `context_length` tokens.
pub fn initial_messages(
    system_prompt: &str,
    changes: &Changes,
    hint: Option<&str>,
    context_length: usize,
) -> (Vec<Message>, Fit) {
    let system_prompt = system_prompt.trim();
    let budget = context_length.saturating_sub(reply_reserve(context_length)) * BYTES_PER_TOKEN;
    let room = budget.saturating_sub(system_prompt.len() + FRAMING_BYTES);
    let (user, mut fit) = describe_changes(changes, hint, room);
    fit.prompt_tokens = estimate_tokens(system_prompt) + estimate_tokens(&user);
    let everything = system_prompt.len() + hint.map_or(0, str::len) + changes.stat.len();
    fit.full_tokens = (everything + changes.diff.len() + FRAMING_BYTES).div_ceil(BYTES_PER_TOKEN);
    let messages = vec![Message::system(system_prompt), Message::user(user)];
    (messages, fit)
}

/// Continues `base` with the current message and the user's requested changes.
pub fn refine_messages(base: &[Message], current: &str, feedback: &str) -> Vec<Message> {
    let mut messages = base.to_vec();
    messages.push(Message::assistant(current));
    messages.push(Message::user(format!(
        "Revise the commit message based on this feedback:\n{feedback}\n\n\
         Reply with the revised commit message only."
    )));
    messages
}

/// The user message, at most about `room` bytes, and how much of the changes it holds.
/// The token estimates in the returned [`Fit`] are left for the caller.
fn describe_changes(changes: &Changes, hint: Option<&str>, room: usize) -> (String, Fit) {
    let mut out = String::new();
    if let Some(hint) = hint.map(str::trim).filter(|hint| !hint.is_empty()) {
        let _ = write!(out, "Notes from the author:\n{hint}\n\n");
    }

    // The file list is usually far smaller, but with many files it must leave room for the diff.
    let stat = changes.stat.trim_end();
    let listed = fit::head_lines(stat, room.saturating_sub(out.len()) / 2);
    let mut list_lines_left_out = 0;
    if listed.len() == stat.len() {
        let _ = writeln!(out, "Changed files:\n{stat}");
    } else {
        // The last line sums up the changes ("3 files changed, ..."); keep it.
        let summary = stat.lines().last().unwrap_or_default();
        list_lines_left_out = stat[listed.len()..].lines().count() - 1;
        let _ = write!(out, "Changed files:\n{listed}");
        if list_lines_left_out > 0 {
            let _ = writeln!(out, "[… {list_lines_left_out} more files not listed]");
        }
        let _ = writeln!(out, "{summary}");
    }

    let mut fit = Fit {
        prompt_tokens: 0,
        full_tokens: 0,
        files: Vec::new(),
        context_trimmed: false,
        list_lines_left_out,
    };
    if changes.diff.trim().is_empty() {
        out.push_str("\nNo diff is included: only excluded files such as lockfiles changed.\n");
        return (out, fit);
    }
    let fitted = fit::fit(&changes.diff, room.saturating_sub(out.len()));
    let heading = match (
        fitted.files.iter().all(FileFit::is_whole),
        fitted.context_trimmed,
    ) {
        (true, false) => "Diff:",
        (true, true) => "Diff, without the unchanged lines around the changes to save space:",
        (false, _) => {
            "Diff, shortened to fit: without the unchanged lines around the changes, \
             and with some files cut short or left out (see the notes in the diff). \
             Describe those files from the list of changed files."
        }
    };
    let _ = writeln!(out, "\n{heading}");
    out.push_str(&fitted.text);
    fit.files = fitted.files;
    fit.context_trimmed = fitted.context_trimmed;
    (out, fit)
}

/// Strips reasoning blocks and code fences that models sometimes wrap around the message.
pub fn clean_response(raw: &str) -> String {
    let mut text = raw;
    for close in ["</think>", "</thinking>"] {
        if let Some(end) = text.rfind(close) {
            text = &text[end + close.len()..];
        }
    }
    normalize(strip_code_fence(text.trim()))
}

fn strip_code_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    // Skip the info string, e.g. ```text
    let Some((_, body)) = rest.split_once('\n') else {
        return text;
    };
    body.trim_end().strip_suffix("```").unwrap_or(body)
}

/// Trims trailing whitespace from every line and blank lines around the text.
pub fn normalize(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    lines.join("\n").trim_matches('\n').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Scope;

    fn changes(diff: &str) -> Changes {
        Changes {
            scope: Scope::Staged,
            stat: " src/main.rs | 2 +-\n 1 file changed\n".into(),
            diff: diff.into(),
        }
    }

    #[test]
    fn removes_reasoning_and_fences() {
        let raw =
            "<think>\nThe diff renames...\n</think>\n\n```text\nfix: rename a  \n\nbody\n```\n";
        assert_eq!(clean_response(raw), "fix: rename a\n\nbody");
        assert_eq!(clean_response("feat: x"), "feat: x");
        assert_eq!(
            clean_response("hidden opening tag</think>\nfeat: x"),
            "feat: x"
        );
    }

    fn diff(lines: usize) -> String {
        let mut diff = "diff --git a/src/main.rs b/src/main.rs\n@@ -1 +1 @@\n".to_owned();
        diff.push_str(&"+a changed line of code\n".repeat(lines));
        diff
    }

    #[test]
    fn user_message_contains_hint_stat_and_diff() {
        let (messages, fit) =
            initial_messages("system", &changes(&diff(1)), Some(" fixes #12 "), 4096);
        let user = &messages[1].content;
        assert!(user.starts_with("Notes from the author:\nfixes #12\n"));
        assert!(user.contains("src/main.rs | 2 +-"));
        assert!(user.contains("Diff:\ndiff --git a/src/main.rs"));
        assert!(fit.is_complete());
        assert!(fit.prompt_tokens > 0 && fit.prompt_tokens <= fit.full_tokens);
    }

    #[test]
    fn shortens_changes_to_fit_the_context() {
        let (messages, fit) = initial_messages("s", &changes(&diff(2000)), None, 4096);
        let user = &messages[1].content;
        assert!(!fit.is_complete());
        assert!(user.contains("shortened to fit"));
        assert!(user.contains("more changed lines of this file not shown]"));
        // The prompt leaves room for the reply.
        assert!(fit.prompt_tokens < 4096 - 1024);
        assert!(fit.full_tokens > 4096);

        let (messages, fit) = initial_messages("s", &changes(""), None, 4096);
        assert!(messages[1].content.contains("only excluded files"));
        assert!(fit.is_complete());
    }

    #[test]
    fn shortens_a_long_file_list_but_keeps_its_summary() {
        let mut stat: String = (0..3000)
            .map(|n| format!(" src/module_{n}.rs | 2 +-\n"))
            .collect();
        stat.push_str(" 3000 files changed, 3000 insertions(+), 3000 deletions(-)\n");
        let changes = Changes {
            scope: Scope::Staged,
            stat,
            diff: diff(10),
        };
        let (messages, fit) = initial_messages("s", &changes, None, 4096);
        let user = &messages[1].content;
        assert!(fit.list_lines_left_out > 0 && fit.list_lines_left_out < 3000);
        assert!(user.contains(&format!(
            "[… {} more files not listed]\n 3000 files changed",
            fit.list_lines_left_out
        )));
        assert!(fit.prompt_tokens < 4096 - 1024);
    }

    #[test]
    fn refine_appends_current_message_and_feedback() {
        let (base, _) = initial_messages("s", &changes("+x\n"), None, 4096);
        let messages = refine_messages(&base, "feat: x", "shorter");
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2].content, "feat: x");
        assert!(messages[3].content.contains("shorter"));
    }
}
