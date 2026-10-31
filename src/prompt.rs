//! Builds the conversation sent to the model and cleans up its replies.

use crate::git::Changes;
use crate::llm::Message;
use std::fmt::Write;

/// The system prompt plus a user message describing `changes`.
pub fn initial_messages(
    system_prompt: &str,
    changes: &Changes,
    hint: Option<&str>,
    max_diff_chars: usize,
) -> Vec<Message> {
    vec![
        Message::system(system_prompt.trim()),
        Message::user(describe_changes(changes, hint, max_diff_chars)),
    ]
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

fn describe_changes(changes: &Changes, hint: Option<&str>, max_diff_chars: usize) -> String {
    let mut out = String::new();
    if let Some(hint) = hint.map(str::trim).filter(|hint| !hint.is_empty()) {
        let _ = write!(out, "Notes from the author:\n{hint}\n\n");
    }
    let _ = writeln!(out, "Changed files:\n{}", changes.stat.trim_end());
    if changes.diff.trim().is_empty() {
        out.push_str("\nNo diff is included: only excluded files such as lockfiles changed.\n");
    } else {
        let (diff, omitted) = truncate_lines(&changes.diff, max_diff_chars);
        let _ = write!(out, "\nDiff:\n{diff}");
        if omitted > 0 {
            let _ = writeln!(out, "\n[diff truncated: {omitted} more lines not shown]");
        }
    }
    out
}

/// Cuts `text` after the last whole line within `max_chars` characters.
/// Returns the kept text and the number of lines dropped.
fn truncate_lines(text: &str, max_chars: usize) -> (&str, usize) {
    let Some((limit, _)) = text.char_indices().nth(max_chars) else {
        return (text, 0);
    };
    let end = text[..limit]
        .rfind('\n')
        .map_or(limit, |newline| newline + 1);
    (&text[..end], text[end..].lines().count())
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

    #[test]
    fn truncates_at_line_boundaries() {
        let text = "aaa\nbbb\nccc\n";
        assert_eq!(truncate_lines(text, 100), (text, 0));
        assert_eq!(truncate_lines(text, 9), ("aaa\nbbb\n", 1));
        assert_eq!(truncate_lines(text, 6), ("aaa\n", 2));
        // Counts characters, never splitting a multi-byte one.
        assert_eq!(truncate_lines("ææææ", 2), ("ææ", 1));
    }

    #[test]
    fn user_message_contains_hint_stat_and_diff() {
        let messages =
            initial_messages("system", &changes("+new line\n"), Some(" fixes #12 "), 1000);
        let user = &messages[1].content;
        assert!(user.starts_with("Notes from the author:\nfixes #12\n"));
        assert!(user.contains("src/main.rs | 2 +-"));
        assert!(user.contains("Diff:\n+new line"));
        assert!(!user.contains("truncated"));
    }

    #[test]
    fn user_message_reports_truncation_and_excluded_only() {
        let long = "+line\n".repeat(100);
        let user = &initial_messages("s", &changes(&long), None, 60)[1].content;
        assert!(user.contains("[diff truncated: 90 more lines not shown]"));

        let user = &initial_messages("s", &changes(""), None, 60)[1].content;
        assert!(user.contains("only excluded files"));
    }

    #[test]
    fn refine_appends_current_message_and_feedback() {
        let base = initial_messages("s", &changes("+x\n"), None, 100);
        let messages = refine_messages(&base, "feat: x", "shorter");
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2].content, "feat: x");
        assert!(messages[3].content.contains("shorter"));
    }
}
