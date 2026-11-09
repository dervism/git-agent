//! The prompts of each command, until each moves into its feature.

use crate::conversation::fit;
use crate::conversation::{Fit, asks, describe_changes, notes, room, with_commits};
use crate::git::Changes;
use crate::github::Issue;
use crate::server::Message;
use std::fmt::Write;

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

/// Instructions for naming a branch (`git agent branch`).
pub const BRANCH_PROMPT: &str = r#"You name a new git branch for the work a developer is about to do.

Rules for the name:
- Start with the kind of work and a slash. If existing branches are listed, use the prefixes they use, spelled the same way: feature/ rather than feat/ if they use feature/. Otherwise use feat/, fix/, docs/, refactor/, perf/, test/ or chore/.
- Then two to five words that say what the work is, in lowercase, separated by hyphens.
- Put an issue number or ticket key right after the slash only when the work names one, e.g. fix/42-login-timeout. Never make one up.
- Use only letters, digits, hyphens and slashes, and keep the name under 50 characters.

Reply with the branch name only: no quotes, no code fences and no explanation."#;

/// Most bytes of an issue's description to send.
const ISSUE_BYTES: usize = 3000;
/// Most bytes of the changes to send when naming a branch after them: a name
/// needs only their gist.
const BRANCH_CHANGES_BYTES: usize = 12_000;

/// What a new branch is for.
pub enum Work<'a> {
    /// The developer's own description.
    Described(&'a str),
    Issue(&'a Issue),
    /// The uncommitted changes.
    Changes(&'a Changes),
}

/// The conversation asking for the name of a branch for `work`, in the
/// style of the repository's `examples` of branch names.
pub fn branch_messages(
    work: &Work,
    examples: &[&str],
    context_length: usize,
    max_output_tokens: usize,
) -> Vec<Message> {
    let mut user = String::new();
    if !examples.is_empty() {
        let _ = writeln!(
            user,
            "Existing branches, most recent first:\n{}\n",
            examples.join("\n")
        );
    }
    match work {
        Work::Described(text) => {
            let _ = writeln!(user, "The work: {text}");
        }
        Work::Issue(issue) => {
            let _ = writeln!(
                user,
                "The work is GitHub issue #{}: {}",
                issue.number, issue.title
            );
            if !issue.labels.is_empty() {
                let _ = writeln!(user, "Labels: {}", issue.labels.join(", "));
            }
            let body = format!("{}\n", issue.body.trim());
            if !body.trim().is_empty() {
                let shown = fit::head_lines(&body, ISSUE_BYTES);
                let _ = write!(user, "\nIssue description:\n{shown}");
                if shown.len() < body.len() {
                    user.push_str("[… rest of the description not shown]\n");
                }
            }
        }
        Work::Changes(changes) => {
            user.push_str("The work is in these uncommitted changes.\n\n");
            let room = room(BRANCH_PROMPT, context_length, max_output_tokens);
            user = describe_changes(user, changes, room.min(BRANCH_CHANGES_BYTES)).0;
        }
    }
    vec![Message::system(BRANCH_PROMPT), Message::user(user)]
}

/// Instructions for planning a tidy-up of unpushed commits (`git agent tidy`).
/// The new messages come later, one commit at a time, from the commit message prompt.
pub const TIDY_PROMPT: &str = r#"You help a developer tidy their commits before they push them. You get the commits that aren't pushed yet, oldest first and numbered, each with its message and its changes. Long changes are cut short.

Choose an action for each commit:
- "keep": the message says what the commit does. Most commits should be kept as they are.
- "reword": the message doesn't say what the commit does, such as "wip", "fix", "update", "changes" or "asdf". A new message will be written from the commit's changes.
- "fold": the commit only corrects or finishes an earlier commit in the list, such as "fix typo", "oops" or "address review" changing the same lines or files as that commit. Give the earlier commit's number as "into". The two become one commit with the earlier commit's message.

Don't fold a commit that does something of its own, however small it is.

Reply with JSON only, without any other text, in this form:
{"commits": [{"commit": 1, "action": "keep"}, {"commit": 2, "action": "reword"}, {"commit": 3, "action": "fold", "into": 1}]}"#;

/// The conversation asking for a tidy-up plan for the numbered `commits`.
pub fn tidy_messages(commits: &str, hint: Option<&str>) -> Vec<Message> {
    let user = format!("{}Commits, oldest first:\n{commits}", notes(hint));
    vec![Message::system(TIDY_PROMPT), Message::user(user)]
}

/// What the model is told about one commit of a split, for its message.
pub fn split_commit_hint(title: &str, author_hint: Option<&str>) -> String {
    let hint =
        format!("This commit is one of several that split a larger change. It should: {title}");
    match author_hint {
        Some(author_hint) => format!("{hint}\n{author_hint}"),
        None => hint,
    }
}

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

/// Continues `base` with a plan that left out the `missing` units (and put the
/// `repeated` ones in more than one commit), asking for it again with every unit
/// in it once. Numbers are 1-based, as the model sees them.
pub fn complete_plan(
    base: &[Message],
    plan: &str,
    missing: &[usize],
    repeated: &[usize],
) -> Vec<Message> {
    let list = |numbers: &[usize]| {
        let numbers: Vec<String> = numbers.iter().map(usize::to_string).collect();
        numbers.join(", ")
    };
    let mut request = format!("Your plan leaves out units {}.", list(missing));
    if !repeated.is_empty() {
        let _ = write!(
            request,
            " It also puts units {} in more than one commit.",
            list(repeated)
        );
    }
    request.push_str(
        " Put each missing unit in the commit it belongs with, or in a new commit if none fits. \
         Reply with the whole plan again, as JSON in the same form, with every unit in exactly \
         one commit.",
    );
    let mut messages = base.to_vec();
    messages.push(Message::assistant(plan));
    messages.push(Message::user(request));
    messages
}

/// Continues `base` with a reply that wasn't a readable plan, why, and the
/// `rule` the plan must follow, e.g. "with every unit in exactly one commit".
pub fn retry_plan(base: &[Message], reply: &str, problem: &str, rule: &str) -> Vec<Message> {
    let mut messages = base.to_vec();
    messages.push(Message::assistant(reply));
    messages.push(Message::user(format!(
        "That reply could not be read as a plan ({problem}). Reply with the JSON only, in the \
         form shown, {rule}."
    )));
    messages
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

/// The conversation asking why the lines that `history` describes (see
/// `why::describe`) are the way they are.
pub fn why_messages(history: &str, question: Option<&str>) -> Vec<Message> {
    vec![
        Message::system(WHY_PROMPT),
        Message::user(format!("{}{history}", asks(question))),
    ]
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
    (title.to_owned(), crate::safe_text::normalize(description))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_MAX_OUTPUT_TOKENS as REPLY;
    use crate::conversation::test_support::{changes, diff};

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
    fn asks_why_with_the_question_first() {
        let messages = why_messages("Explain line 3 of a.rs.\n", None);
        assert_eq!(messages[0].content, WHY_PROMPT);
        assert_eq!(messages[1].content, "Explain line 3 of a.rs.\n");
        let asked = why_messages("history\n", Some("why 3?"));
        assert_eq!(asked[1].content, "The developer asks:\nwhy 3?\n\nhistory\n");
    }

    #[test]
    fn asks_for_a_branch_name_in_the_repository_style() {
        let messages = branch_messages(
            &Work::Described("fix login timeout"),
            &["feature/split", "bugfix/pr-base"],
            8192,
            REPLY,
        );
        assert_eq!(messages[0].content, BRANCH_PROMPT);
        assert_eq!(
            messages[1].content,
            "Existing branches, most recent first:\nfeature/split\nbugfix/pr-base\n\n\
             The work: fix login timeout\n"
        );

        let issue = Issue {
            number: 42,
            title: "Login times out".into(),
            body: format!("It hangs.\n{}", "More detail.\n".repeat(400)),
            labels: vec!["bug".into()],
        };
        let user = &branch_messages(&Work::Issue(&issue), &[], 8192, REPLY)[1].content;
        assert!(user.starts_with(
            "The work is GitHub issue #42: Login times out\nLabels: bug\n\n\
             Issue description:\nIt hangs.\n"
        ));
        assert!(user.ends_with("[… rest of the description not shown]\n"));
        assert!(user.len() < ISSUE_BYTES + 200);

        let big = changes(&diff(5000));
        let user = &branch_messages(&Work::Changes(&big), &[], 131_072, REPLY)[1].content;
        assert!(user.starts_with("The work is in these uncommitted changes.\n\nChanged files:\n"));
        assert!(user.len() <= BRANCH_CHANGES_BYTES + 200);
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
        let completed = complete_plan(&messages, "{\"commits\": []}", &[3, 5, 8], &[2]);
        assert_eq!(completed.len(), 4);
        assert!(completed[3].content.starts_with(
            "Your plan leaves out units 3, 5, 8. It also puts units 2 in more than one commit."
        ));
        assert!(
            complete_plan(&messages, "{}", &[4], &[])[3]
                .content
                .starts_with("Your plan leaves out units 4. Put")
        );
        let retried = retry_plan(
            &messages,
            "oops",
            "not JSON",
            "with every unit in one commit",
        );
        assert!(retried[3].content.contains("(not JSON)"));
        assert!(
            retried[3]
                .content
                .ends_with("form shown, with every unit in one commit.")
        );
        let tidy = tidy_messages("Commit 1 (a1b2c3d):\n    wip\n", Some("fixes #3"));
        assert_eq!(tidy[0].content, TIDY_PROMPT);
        assert_eq!(
            tidy[1].content,
            "Notes from the author:\nfixes #3\n\nCommits, oldest first:\nCommit 1 (a1b2c3d):\n    wip\n"
        );
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

    #[test]
    fn tells_the_model_what_a_commit_of_a_split_should_do() {
        let hint = split_commit_hint("Add a cache", None);
        assert_eq!(
            hint,
            "This commit is one of several that split a larger change. It should: Add a cache"
        );
    }

    #[test]
    fn adds_the_authors_own_hint_last() {
        let hint = split_commit_hint("Add a cache", Some("It's for the parser."));
        assert!(
            hint.ends_with("It should: Add a cache\nIt's for the parser."),
            "{hint}"
        );
    }
}
