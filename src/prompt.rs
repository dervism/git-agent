//! Builds the conversation sent to the model and cleans up its replies.

use crate::fit::{self, FileFit};
use crate::git::Changes;
use crate::server::Message;
use std::fmt::Write;

/// Rough number of bytes per token. Common tokenizers average 3 to 4 bytes per
/// token for code and English, and about 3 for most other scripts.
const BYTES_PER_TOKEN: usize = 3;
/// Room for the headings around the changes and the chat template's markup.
const FRAMING_BYTES: usize = 256;

/// Instructions for writing a pull request. Unlike the commit message prompt,
/// this one is not a setting.
pub const PULL_REQUEST_PROMPT: &str = r###"You write the title and description of a GitHub pull request for a branch a developer is about to open one for.

Format:
- The first line is the title: at most 72 characters, no trailing period, no Markdown. If the branch's commit subjects use Conventional Commits (e.g. "fix(parser): handle empty input"), write the title the same way.
- Then a blank line and the description in GitHub Markdown: a short paragraph on what the pull request changes and why, then a "## Changes" section with "- " bullets for the notable changes. Leave out the section when there is only one change.
- Point out breaking changes, new settings, and anything reviewers should check, but only when the commits or the diff show them.

Guidelines:
- Describe the intent and effect of the changes, not a file-by-file narration of the diff.
- The commits are listed oldest first. Describe the end result, not the order in which the work was done.
- Do not invent motivation, issue numbers, test results or screenshots that the commits, the diff and the author's notes do not support.

Reply with the title and description only: no introduction, no "Title:" label, no code fences."###;

/// Instructions for explaining lines of code (`git agent why`).
pub const WHY_PROMPT: &str = r#"You explain to a developer why some lines of code are the way they are, from what git knows about them.

You get:
- The lines to explain, marked with ">", and some of the code around them.
- The commit that last changed each line (git blame).
- The history of the lines: each commit that changed them, newest first, with its message and its diff of the lines.

How to answer:
- Start with the answer: why the lines are as they are, and what they are for.
- Then tell how they got here: the commit that added them and the changes since, as far as they matter, naming each commit by its short hash in parentheses, e.g. (a1b2c3d).
- Base the answer on the commit messages, the diffs and the code. When they don't say why something was done, say so instead of guessing, and keep what you infer from the code apart from what the commits say.
- If the developer asks a question, answer it.
- Be brief: a few short paragraphs, or "- " bullets. Write plain text for a terminal, without headings, tables or bold."#;

/// Instructions for explaining commits (`git agent explain`).
pub const EXPLAIN_PROMPT: &str = r#"You explain commits to a developer who wants to understand them: one commit, a range of commits, a branch, or the commits a pull brought in.

You get the commits, oldest first, each with its short hash, subject, author and date and the rest of its message, then the files they change and the diff of everything they change together. Long diffs are shortened.

How to answer:
- Start with a sentence or two on what the commits do and why, as far as their messages and the diff say.
- Then the notable changes as "- " bullets, grouped by topic rather than by commit or by file. When there are several commits, name the ones behind each point by their short hashes in parentheses, e.g. (a1b2c3d).
- Point out what the developer may need to act on, such as breaking changes, new or changed settings, new dependencies, migrations, or changes to how something is used. Leave this out when there is nothing.
- Base the answer on the commits and the diff. Do not invent motivation, issue numbers or test results, and say so when the reason for a change isn't clear.
- If the developer asks a question, answer it.
- Write plain text for a terminal, without headings, tables or bold."#;

/// Instructions for grouping a change into commits (`git agent split`). The
/// messages come later, one commit at a time, from the commit message prompt.
pub const SPLIT_PROMPT: &str = r#"You split a developer's uncommitted changes into a series of small, logical commits, the way a careful developer would before pushing.

The changes are listed as numbered units. A unit is one hunk of a file, or a whole file. For each unit you see its file and its lines as in a diff: added lines start with "+", removed lines with "-", and unchanged lines around them with a space. Long units are cut short.

Group the units into commits:
- Each commit does one thing, such as a feature, a fix, a refactoring, a dependency update or documentation.
- Don't split a feature into its parts: a new module, the code that starts using it, the dependency it needs and its documentation belong in one commit.
- Keep together what only works together, such as a function and the calls that use its new signature, or a dependency and its lockfile.
- Put tests with the change they test, unless they stand on their own.
- Order the commits so each one builds on those before it, for example a refactoring before the feature that needs it.
- Use as few commits as make sense. If everything belongs together, return a single commit.
- Every unit goes in exactly one commit.

Reply with JSON only, without any other text, in this form:
{"commits": [{"title": "Add a cache for parsed configs", "units": [2, 3, 5, 8]}, {"title": "Fix the config path on Windows", "units": [1]}, {"title": "Correct typos in comments", "units": [4, 6, 7]}]}

Each title is a short summary of its commit in the imperative mood."#;

/// The conversation asking for a plan for the numbered `units`.
pub fn split_messages(units: &str, hint: Option<&str>) -> Vec<Message> {
    let user = format!("{}Changes:\n{units}", notes(hint));
    vec![Message::system(SPLIT_PROMPT), Message::user(user)]
}

/// Continues `base` with the current plan, as JSON, and the user's requested changes.
pub fn refine_plan(base: &[Message], plan: &str, feedback: &str) -> Vec<Message> {
    let mut messages = base.to_vec();
    messages.push(Message::assistant(plan));
    messages.push(Message::user(format!(
        "Revise the plan based on this feedback:\n{feedback}\n\n\
         Reply with the revised plan only, as JSON in the same form."
    )));
    messages
}

/// Continues `base` with a reply that wasn't a readable plan, and why.
pub fn retry_plan(base: &[Message], reply: &str, problem: &str) -> Vec<Message> {
    let mut messages = base.to_vec();
    messages.push(Message::assistant(reply));
    messages.push(Message::user(format!(
        "That reply could not be read as a plan ({problem}). Reply with the JSON only, in the \
         form shown, with every unit in exactly one commit."
    )));
    messages
}

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

/// Like [`initial_messages`], for a pull request of a branch with `commits`
/// (each "- subject\n  body", oldest first) and `changes`.
pub fn pull_request_messages(
    commits: &[String],
    changes: &Changes,
    hint: Option<&str>,
    context_length: usize,
    max_output_tokens: usize,
) -> (Vec<Message>, Fit) {
    with_commits(
        PULL_REQUEST_PROMPT,
        notes(hint),
        "Commits on the branch, oldest first:\n",
        commits,
        changes,
        room(PULL_REQUEST_PROMPT, context_length, max_output_tokens),
    )
}

/// Like [`pull_request_messages`], asking to explain the `commits` (see
/// `explain::listed`) and the `changes` they make together. `about` names
/// them, e.g. "commit a1b2c3d".
pub fn explain_messages(
    about: &str,
    commits: &[String],
    changes: &Changes,
    question: Option<&str>,
    context_length: usize,
    max_output_tokens: usize,
) -> (Vec<Message>, Fit) {
    with_commits(
        EXPLAIN_PROMPT,
        format!("{}Explain {about}.\n\n", asks(question)),
        "Commits, oldest first:\n",
        commits,
        changes,
        room(EXPLAIN_PROMPT, context_length, max_output_tokens),
    )
}

/// The system prompt and a user message of `intro`, the `commits` under
/// `heading`, and the changes, in about `room` bytes.
fn with_commits(
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

/// The conversation asking why the lines that `history` describes (see
/// `why::describe`) are the way they are.
pub fn why_messages(history: &str, question: Option<&str>) -> Vec<Message> {
    vec![
        Message::system(WHY_PROMPT),
        Message::user(format!("{}{history}", asks(question))),
    ]
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
fn asks(question: Option<&str>) -> String {
    match question
        .map(str::trim)
        .filter(|question| !question.is_empty())
    {
        Some(question) => format!("The developer asks:\n{question}\n\n"),
        None => String::new(),
    }
}

/// "Notes from the author" from the hint, if there is one.
fn notes(hint: Option<&str>) -> String {
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

/// Splits a generated pull request into its title (the first line) and description.
pub fn split_title(text: &str) -> (String, String) {
    let text = text.trim();
    let (title, description) = text.split_once('\n').unwrap_or((text, ""));
    let mut title = title.trim().trim_start_matches('#').trim();
    if title
        .get(..6)
        .is_some_and(|label| label.eq_ignore_ascii_case("title:"))
    {
        title = &title[6..];
    }
    let title = title.trim().trim_matches('*').trim();
    (title.to_owned(), normalize(description))
}

/// The user message, `intro` followed by the changes in at most about `room`
/// bytes, and how much of the changes it holds. The token estimates in the
/// returned [`Fit`] are left for the caller.
fn describe_changes(intro: String, changes: &Changes, room: usize) -> (String, Fit) {
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
    use crate::config::DEFAULT_MAX_OUTPUT_TOKENS as REPLY;

    fn changes(diff: &str) -> Changes {
        Changes {
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
    fn pull_request_message_lists_commits_before_the_changes() {
        let commits = [
            "- feat: add b\n  b replaces a.".to_owned(),
            "- test: cover b".to_owned(),
        ];
        let (messages, fit) =
            pull_request_messages(&commits, &changes(&diff(1)), Some("fixes #7"), 8192, REPLY);
        assert_eq!(messages[0].content, PULL_REQUEST_PROMPT);
        let user = &messages[1].content;
        assert!(user.starts_with(
            "Notes from the author:\nfixes #7\n\nCommits on the branch, oldest first:\n\
             - feat: add b\n  b replaces a.\n- test: cover b\n\nChanged files:\n"
        ));
        assert!(user.contains("Diff:\ndiff --git a/src/main.rs"));
        assert!(fit.is_complete());
    }

    #[test]
    fn shortens_the_commit_list_to_its_share_of_the_room() {
        let commits: Vec<String> = (0..400)
            .map(|n| {
                format!(
                    "- fix: change number {n}\n  {}",
                    "A long explanation. ".repeat(10)
                )
            })
            .collect();
        let (messages, fit) =
            pull_request_messages(&commits, &changes(&diff(1)), None, 8192, REPLY);
        let user = &messages[1].content;
        assert!(fit.commit_bodies_left_out && !fit.is_complete());
        assert!(!user.contains("A long explanation"));
        assert!(user.contains("- fix: change number 0\n"));
        // The diff still gets the rest of the room.
        assert!(user.contains("+a changed line of code"));
        assert!(fit.prompt_tokens < 8192 - 2048);

        let listed = list_commits(&commits, 300);
        assert!(listed.left_out > 0 && listed.left_out < 400);
        assert!(listed.text.ends_with(&format!(
            "[… {} more commits not listed]\n",
            listed.left_out
        )));
        assert!(listed.text.len() <= 300 + 40);
    }

    #[test]
    fn asks_to_explain_commits_with_the_question_first() {
        let commits = ["- a1b2c3d feat: add b (Ann, 2025-10-02)".to_owned()];
        let (messages, fit) = explain_messages(
            "commit a1b2c3d",
            &commits,
            &changes(&diff(1)),
            Some("does it break anything?"),
            8192,
            REPLY,
        );
        assert_eq!(messages[0].content, EXPLAIN_PROMPT);
        assert!(messages[1].content.starts_with(
            "The developer asks:\ndoes it break anything?\n\nExplain commit a1b2c3d.\n\n\
             Commits, oldest first:\n- a1b2c3d feat: add b (Ann, 2025-10-02)\n\nChanged files:\n"
        ));
        assert!(fit.is_complete());
    }

    #[test]
    fn asks_why_and_follows_up_while_there_is_room() {
        let messages = why_messages("Explain line 3 of a.rs.\n", None);
        assert_eq!(messages[0].content, WHY_PROMPT);
        assert_eq!(messages[1].content, "Explain line 3 of a.rs.\n");
        assert_eq!(question_bytes(Some("  ")), 0);
        let asked = why_messages("history\n", Some("why 3?"));
        assert_eq!(asked[1].content, "The developer asks:\nwhy 3?\n\nhistory\n");
        assert_eq!(
            question_bytes(Some("why 3?")),
            "The developer asks:\nwhy 3?\n\n".len()
        );

        let next = follow_up(&messages, "Because.", "And line 4?");
        assert_eq!(next.len(), 4);
        assert_eq!(next[3].content, "And line 4?");
        assert!(has_room(&next, 4096, REPLY));
        let long = follow_up(&messages, &"word ".repeat(3000), "more?");
        assert!(!has_room(&long, 4096, REPLY));
    }

    #[test]
    fn asks_for_a_plan_and_continues_it() {
        let messages = split_messages("[1] a.rs (+1)\n+x\n", Some("cleanup"));
        assert_eq!(messages[0].content, SPLIT_PROMPT);
        assert_eq!(
            messages[1].content,
            "Notes from the author:\ncleanup\n\nChanges:\n[1] a.rs (+1)\n+x\n"
        );
        let refined = refine_plan(&messages, "{\"commits\": []}", "fewer commits");
        assert_eq!(refined.len(), 4);
        assert!(refined[3].content.contains("fewer commits"));
        let retried = retry_plan(&messages, "oops", "not JSON");
        assert!(retried[3].content.contains("(not JSON)"));
    }

    #[test]
    fn splits_the_title_from_the_description() {
        assert_eq!(
            split_title("feat: add b\n\nIt replaces a.\n\n## Changes\n- b"),
            (
                "feat: add b".to_owned(),
                "It replaces a.\n\n## Changes\n- b".to_owned()
            )
        );
        assert_eq!(
            split_title("# Title: **feat: add b**  \n"),
            ("feat: add b".to_owned(), String::new())
        );
    }
}
