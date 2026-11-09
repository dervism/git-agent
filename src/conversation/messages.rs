//! Building the conversation for the model, and sizing it to fit the model's context.

use super::fit::{self, FileFit};
use crate::git::Changes;
use crate::server::Message;
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
    /// Whether commits were listed by their subjects only.
    pub commit_bodies_left_out: bool,
    /// Commits left out of the list of commits.
    pub commits_left_out: usize,
}

impl Fit {
    pub fn is_complete(&self) -> bool {
        self.list_lines_left_out == 0
            && !self.commit_bodies_left_out
            && self.commits_left_out == 0
            && self.files.iter().all(FileFit::is_whole)
    }
}

pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(BYTES_PER_TOKEN)
}

/// Tokens kept free for the reply, including any reasoning, and for a round of
/// feedback: up to the output limit, but at most a quarter of the context, so a
/// small context still has room for the changes.
fn reply_reserve(context_length: usize, max_output_tokens: usize) -> usize {
    (context_length / 4).min(max_output_tokens).max(1024)
}

/// Bytes the user message may take in a context of `context_length` tokens,
/// with room for a reply of up to `max_output_tokens`.
pub fn room(system_prompt: &str, context_length: usize, max_output_tokens: usize) -> usize {
    let reserve = reply_reserve(context_length, max_output_tokens);
    let budget = context_length.saturating_sub(reserve) * BYTES_PER_TOKEN;
    budget.saturating_sub(system_prompt.trim().len() + FRAMING_BYTES)
}

/// The system prompt plus a user message describing `changes`, shortened to
/// fit a context of `context_length` tokens with room for a reply of up to
/// `max_output_tokens`.
pub fn initial_messages(
    system_prompt: &str,
    changes: &Changes,
    hint: Option<&str>,
    context_length: usize,
    max_output_tokens: usize,
) -> (Vec<Message>, Fit) {
    let intro = notes(hint);
    let intro_bytes = intro.len();
    let room = room(system_prompt, context_length, max_output_tokens);
    build(system_prompt, intro, intro_bytes, changes, room)
}

/// The system prompt and a user message of `intro`, the `commits` under
/// `heading`, and the changes, in about `room` bytes.
pub fn with_commits(
    system_prompt: &str,
    mut intro: String,
    heading: &str,
    commits: &[String],
    changes: &Changes,
    room: usize,
) -> (Vec<Message>, Fit) {
    let intro_bytes =
        intro.len() + heading.len() + commits.iter().map(|c| c.len() + 1).sum::<usize>();
    // Commit messages often say why the changes were made, which the diff
    // can't, so they get up to a third of the room.
    let listed = list_commits(commits, room / 3);
    let _ = writeln!(intro, "{heading}{}", listed.text);
    let (messages, mut fit) = build(system_prompt, intro, intro_bytes, changes, room);
    fit.commit_bodies_left_out = listed.bodies_left_out;
    fit.commits_left_out = listed.left_out;
    (messages, fit)
}

/// Bytes the developer's question takes in the user message.
pub fn question_bytes(question: Option<&str>) -> usize {
    asks(question).len()
}

/// Continues `base` with the model's `answer` and the developer's next `question`.
pub fn follow_up(base: &[Message], answer: &str, question: &str) -> Vec<Message> {
    let mut messages = base.to_vec();
    messages.push(Message::assistant(answer));
    messages.push(Message::user(question));
    messages
}

/// Whether `messages` leave room in the context for another reply.
pub fn has_room(messages: &[Message], context_length: usize, max_output_tokens: usize) -> bool {
    let used: usize = messages
        .iter()
        .map(|message| estimate_tokens(&message.content))
        .sum();
    let reply = (reply_reserve(context_length, max_output_tokens) / 2).max(1024);
    used + reply <= context_length
}

/// The developer's question, if there is one.
pub fn asks(question: Option<&str>) -> String {
    match question
        .map(str::trim)
        .filter(|question| !question.is_empty())
    {
        Some(question) => format!("The developer asks:\n{question}\n\n"),
        None => String::new(),
    }
}

/// "Notes from the author" from the hint, if there is one.
pub fn notes(hint: Option<&str>) -> String {
    match hint.map(str::trim).filter(|hint| !hint.is_empty()) {
        Some(hint) => format!("Notes from the author:\n{hint}\n\n"),
        None => String::new(),
    }
}

struct Listed {
    text: String,
    bodies_left_out: bool,
    left_out: usize,
}

/// The commits in at most about `room` bytes: whole if they fit, else by
/// their subjects, and if even those don't fit, the first ones.
fn list_commits(commits: &[String], room: usize) -> Listed {
    let whole: usize = commits.iter().map(|commit| commit.len() + 1).sum();
    let bodies_left_out = whole > room;
    let mut text = String::new();
    for (index, commit) in commits.iter().enumerate() {
        let entry = if bodies_left_out {
            commit.lines().next().unwrap_or_default()
        } else {
            commit
        };
        if text.len() + entry.len() + 1 > room {
            let left_out = commits.len() - index;
            let _ = writeln!(text, "[… {left_out} more commits not listed]");
            return Listed {
                text,
                bodies_left_out,
                left_out,
            };
        }
        let _ = writeln!(text, "{entry}");
    }
    if bodies_left_out {
        text.push_str("[Commit message bodies left out to save space]\n");
    }
    Listed {
        text,
        bodies_left_out,
        left_out: 0,
    }
}

/// The system prompt and a user message of `intro` followed by the changes,
/// shortened to about `room` bytes. `intro_bytes` is the intro's size before
/// any shortening.
fn build(
    system_prompt: &str,
    intro: String,
    intro_bytes: usize,
    changes: &Changes,
    room: usize,
) -> (Vec<Message>, Fit) {
    let system_prompt = system_prompt.trim();
    let (user, mut fit) = describe_changes(intro, changes, room);
    fit.prompt_tokens = estimate_tokens(system_prompt) + estimate_tokens(&user);
    let everything = system_prompt.len() + intro_bytes + changes.stat.len() + changes.diff.len();
    fit.full_tokens = (everything + FRAMING_BYTES).div_ceil(BYTES_PER_TOKEN);
    let messages = vec![Message::system(system_prompt), Message::user(user)];
    (messages, fit)
}

/// Continues `base` with the current proposal and the user's requested
/// changes. `what` names the proposal, e.g. "commit message".
pub fn refine_messages(
    base: &[Message],
    current: &str,
    feedback: &str,
    what: &str,
) -> Vec<Message> {
    let mut messages = base.to_vec();
    messages.push(Message::assistant(current));
    messages.push(Message::user(format!(
        "Revise the {what} based on this feedback:\n{feedback}\n\n\
         Reply with the revised {what} only."
    )));
    messages
}

/// The user message, `intro` followed by the changes in at most about `room`
/// bytes, and how much of the changes it holds. The token estimates in the
/// returned [`Fit`] are left for the caller.
pub fn describe_changes(intro: String, changes: &Changes, room: usize) -> (String, Fit) {
    let mut out = intro;

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
        commit_bodies_left_out: false,
        commits_left_out: 0,
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

/// Whether escaping made `conversation` too large for the model's context, with room left
/// for its reply. What's sent is sized before it's escaped, so only text full of such
/// characters can get here.
pub fn outgrew_context(
    conversation: &[Message],
    outgoing: &[Message],
    context_length: usize,
    max_output_tokens: usize,
) -> bool {
    let tokens = |messages: &[Message]| -> usize {
        messages
            .iter()
            .map(|message| estimate_tokens(&message.content))
            .sum()
    };
    let (before, after) = (tokens(conversation), tokens(outgoing));
    let room = context_length.saturating_sub(reply_reserve(context_length, max_output_tokens));
    before <= room && after > room
}

/// Bytes the author's notes take in the user message.
pub fn notes_bytes(hint: Option<&str>) -> usize {
    notes(hint).len()
}

#[cfg(test)]
pub mod test_support {
    use crate::git::Changes;

    pub fn changes(diff: &str) -> Changes {
        Changes {
            stat: " src/main.rs | 2 +-\n 1 file changed\n".into(),
            diff: diff.into(),
        }
    }

    pub fn diff(lines: usize) -> String {
        let mut diff = "diff --git a/src/main.rs b/src/main.rs\n@@ -1 +1 @@\n".to_owned();
        diff.push_str(&"+a changed line of code\n".repeat(lines));
        diff
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{changes, diff};
    use super::*;
    use crate::config::DEFAULT_MAX_OUTPUT_TOKENS as REPLY;
    use crate::server::Request;

    #[test]
    fn notices_only_escaping_that_outgrows_the_context() {
        let escaped =
            |conversation: &[Message]| Request::new("m", conversation).messages().to_vec();
        let plain = [Message::user("x".repeat(4000))];
        assert!(!outgrew_context(&plain, &escaped(&plain), 10, REPLY));

        let hidden = [Message::user("\u{1b}".repeat(4000))];
        let outgoing = escaped(&hidden);
        assert!(outgrew_context(&hidden, &outgoing, 3000, REPLY));
        assert!(!outgrew_context(&hidden, &outgoing, 100_000, REPLY));
        // 4,000 tokens fit in 5,000, but not with 1,250 kept for the reply.
        assert!(outgrew_context(&hidden, &outgoing, 5000, REPLY));
        // A conversation over the room already is the server's to judge.
        let long = [Message::user(format!("{}\u{1b}", "x".repeat(40_000)))];
        assert!(!outgrew_context(&long, &escaped(&long), 5000, REPLY));
    }

    #[test]
    fn user_message_contains_hint_stat_and_diff() {
        let (messages, fit) = initial_messages(
            "system",
            &changes(&diff(1)),
            Some(" fixes #12 "),
            4096,
            REPLY,
        );
        let user = &messages[1].content;
        assert!(user.starts_with("Notes from the author:\nfixes #12\n"));
        assert!(user.contains("src/main.rs | 2 +-"));
        assert!(user.contains("Diff:\ndiff --git a/src/main.rs"));
        assert!(fit.is_complete());
        assert!(fit.prompt_tokens > 0 && fit.prompt_tokens <= fit.full_tokens);
    }

    #[test]
    fn shortens_changes_to_fit_the_context() {
        let (messages, fit) = initial_messages("s", &changes(&diff(2000)), None, 4096, REPLY);
        let user = &messages[1].content;
        assert!(!fit.is_complete());
        assert!(user.contains("shortened to fit"));
        assert!(user.contains("more changed lines of this file not shown]"));
        // The prompt leaves room for the reply.
        assert!(fit.prompt_tokens < 4096 - 1024);
        assert!(fit.full_tokens > 4096);

        let (messages, fit) = initial_messages("s", &changes(""), None, 4096, REPLY);
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
            stat,
            diff: diff(10),
        };
        let (messages, fit) = initial_messages("s", &changes, None, 4096, REPLY);
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
        let (base, _) = initial_messages("s", &changes("+x\n"), None, 4096, REPLY);
        let messages = refine_messages(&base, "feat: x", "shorter", "commit message");
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2].content, "feat: x");
        assert!(messages[3].content.contains("shorter"));
        assert!(messages[3].content.contains("revised commit message only"));
    }

    #[test]
    fn lists_the_first_commits_that_fit_and_counts_the_rest() {
        let commits: Vec<String> = (0..400)
            .map(|n| {
                format!(
                    "- fix: change number {n}\n  {}",
                    "A long explanation. ".repeat(10)
                )
            })
            .collect();
        let listed = list_commits(&commits, 300);
        assert!(listed.bodies_left_out);
        assert!(listed.left_out > 0 && listed.left_out < 400);
        assert!(listed.text.ends_with(&format!(
            "[… {} more commits not listed]\n",
            listed.left_out
        )));
        assert!(listed.text.len() <= 300 + 40);
    }
}
