//! Cleaning up a model's reply, so that what's shown is what gets committed.

use crate::safe_text::{self, Cleaned, without_hidden};

/// A model's reply, without the reasoning and code fences that models sometimes wrap
/// around it, and without hidden characters, so that what's shown is what gets committed.
/// Those go before the fence, because a BOM ahead of a fence would otherwise keep it.
pub fn clean_reply(raw: &str) -> Cleaned {
    let mut text = raw;
    for close in ["</think>", "</thinking>"] {
        if let Some(end) = text.rfind(close) {
            text = &text[end + close.len()..];
        }
    }
    let cleaned = without_hidden(text);
    Cleaned {
        text: safe_text::normalize(strip_code_fence(cleaned.text.trim())),
        removed: cleaned.removed,
    }
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

/// `message` with a blank line between its subject and its body, which git
/// needs to tell them apart. Models sometimes leave it out.
pub fn separate_subject(message: &str) -> String {
    match message.split_once('\n') {
        Some((subject, body)) if !body.starts_with('\n') && !body.trim().is_empty() => {
            format!("{subject}\n\n{body}")
        }
        _ => message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_reasoning_and_fences() {
        let raw =
            "<think>\nThe diff renames...\n</think>\n\n```text\nfix: rename a  \n\nbody\n```\n";
        assert_eq!(clean_reply(raw).text, "fix: rename a\n\nbody");
        assert_eq!(clean_reply("feat: x").text, "feat: x");
        assert_eq!(
            clean_reply("hidden opening tag</think>\nfeat: x").text,
            "feat: x"
        );
    }

    #[test]
    fn removes_hidden_characters_from_a_reply_and_counts_them() {
        let cleaned = clean_reply("fix: a\u{1b}[2Jb\n\nbody\u{202e} text\r more");
        assert_eq!(cleaned.text, "fix: a[2Jb\n\nbody text more");
        assert_eq!(cleaned.removed, 3);
    }

    #[test]
    fn doesnt_count_hidden_characters_in_the_reasoning() {
        let cleaned = clean_reply("<think>\u{1b}[2J\u{202e}</think>\nfeat: x");
        assert_eq!(cleaned.text, "feat: x");
        assert_eq!(cleaned.removed, 0);
    }

    #[test]
    fn a_reply_of_only_hidden_characters_is_empty() {
        let cleaned = clean_reply("\u{1b}\u{202e}\u{200b}\n\u{feff}");
        assert_eq!(cleaned.text, "");
        assert_eq!(cleaned.removed, 4);
    }

    #[test]
    fn strips_a_fence_behind_a_bom() {
        let cleaned = clean_reply("\u{feff}```text\nfeat: x\n```\n");
        assert_eq!(cleaned.text, "feat: x");
        assert_eq!(cleaned.removed, 1);
    }

    #[test]
    fn keeps_trailing_whitespace_from_hiding_behind_a_hidden_character() {
        assert_eq!(clean_reply("feat: x  \u{200b}\nbody").text, "feat: x\nbody");
    }

    #[test]
    fn separates_the_subject_from_the_body() {
        assert_eq!(separate_subject("feat: a\nWhy a."), "feat: a\n\nWhy a.");
        assert_eq!(separate_subject("feat: a\n\nWhy a."), "feat: a\n\nWhy a.");
        assert_eq!(separate_subject("feat: a"), "feat: a");
    }
}
