//! Tidying the commits that aren't pushed yet (`git agent tidy`): giving
//! commits whose messages don't say what they do a new message, and folding
//! fixes into the commits they fix.
//!
//! The model proposes an action for each commit, and the plan becomes a
//! `git rebase -i` todo list. Commits named `fixup! <subject>` or
//! `squash! <subject>` always fold into their target, as `git rebase
//! --autosquash` would do.

use crate::fit;
use crate::safe_text::{escaped, one_line};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fmt::Write;
use std::path::PathBuf;

/// Help at the top of the plan when the user edits it.
const EDIT_HELP: &str = "\
# Edit the plan. Each line is a commit, oldest first:
#   keep    leave the commit as it is
#   reword  give the commit a new message, written from its changes
#   fold    fold the commit into the commit on the line above, keeping that
#           commit's message
# Move a fold line under the commit it fixes. Every commit must stay in the
# plan. Lines starting with '#' are ignored.
";

/// A commit to tidy.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub short: String,
    /// The whole message.
    pub message: String,
    /// The patch, without the diffs of excluded files.
    pub patch: String,
}

impl Commit {
    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or_default()
    }
}

/// The commits after tidying, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub groups: Vec<Group>,
}

/// A commit after tidying: one of the original commits, perhaps with a new
/// message, and the commits folded into it.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// Index of the commit in the list of commits.
    pub commit: usize,
    pub reword: bool,
    pub folded: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Action {
    Keep,
    Reword,
    /// Fold into the commit at this index.
    Fold(usize),
}

impl Plan {
    /// Whether the plan changes any commit.
    pub fn changes_anything(&self) -> bool {
        self.groups
            .iter()
            .any(|group| group.reword || !group.folded.is_empty())
    }

    /// The plan as JSON in the form the model writes, for asking it to revise the plan.
    pub fn to_json(&self) -> String {
        let mut entries = Vec::new();
        for group in &self.groups {
            let action = if group.reword { "reword" } else { "keep" };
            entries.push(json!({"commit": group.commit + 1, "action": action}));
            for &folded in &group.folded {
                entries.push(
                    json!({"commit": folded + 1, "action": "fold", "into": group.commit + 1}),
                );
            }
        }
        json!({ "commits": entries }).to_string()
    }

    /// One line per commit, in the new order, as `show` and the editor list them.
    fn lines(&self, commits: &[Commit]) -> String {
        let mut out = String::new();
        let line = |out: &mut String, verb: &str, index: usize| {
            let commit = &commits[index];
            let _ = writeln!(
                out,
                "{verb:<7}{} {}",
                commit.short,
                one_line(commit.subject())
            );
        };
        for group in &self.groups {
            line(
                &mut out,
                if group.reword { "reword" } else { "keep" },
                group.commit,
            );
            for &folded in &group.folded {
                line(&mut out, "fold", folded);
            }
        }
        out
    }
}

/// The plan as the review shows it: a line per commit and what comes of them.
pub fn show(plan: &Plan, commits: &[Commit]) -> String {
    let folded: usize = plan.groups.iter().map(|group| group.folded.len()).sum();
    let reworded = plan.groups.iter().filter(|group| group.reword).count();
    let mut parts = Vec::new();
    if folded > 0 {
        parts.push(format!("{folded} folded into others"));
    }
    if reworded > 0 {
        parts.push(format!("{reworded} with a new message"));
    }
    let mut out = plan.lines(commits);
    let _ = write!(
        out,
        "\n{} commits become {}",
        commits.len(),
        plan.groups.len()
    );
    if !parts.is_empty() {
        let _ = write!(out, ": {}", parts.join(", "));
    }
    out
}

/// The plan as text to edit.
pub fn to_text(plan: &Plan, commits: &[Commit]) -> String {
    format!("{EDIT_HELP}\n{}", escaped(&plan.lines(commits)))
}

/// Reads an edited plan. Every commit must appear once; a fold line folds
/// into the commit on the nearest keep or reword line above it.
pub fn from_text(text: &str, commits: &[Commit]) -> Result<Plan> {
    let mut groups: Vec<Group> = Vec::new();
    let mut seen = vec![false; commits.len()];
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let verb = words.next().unwrap_or_default().to_lowercase();
        let hash = words.next().unwrap_or_default();
        let index = commits
            .iter()
            .position(|commit| {
                !hash.is_empty()
                    && (commit.short.starts_with(hash) || hash.starts_with(&commit.short))
            })
            .with_context(|| format!("the line {line:?} names no commit of the plan"))?;
        if std::mem::replace(&mut seen[index], true) {
            bail!("commit {hash} is in the plan twice");
        }
        match verb.as_str() {
            "keep" | "k" | "pick" | "p" => groups.push(Group {
                commit: index,
                reword: false,
                folded: Vec::new(),
            }),
            "reword" | "r" => groups.push(Group {
                commit: index,
                reword: true,
                folded: Vec::new(),
            }),
            "fold" | "f" | "fixup" | "squash" | "s" => match groups.last_mut() {
                Some(group) => group.folded.push(index),
                None => {
                    bail!("the first line can't fold: there is no commit above it to fold into")
                }
            },
            other => bail!("unknown action {other:?}; use keep, reword or fold"),
        }
    }
    if let Some(missing) = seen.iter().position(|seen| !seen) {
        bail!(
            "commit {} is missing from the plan; every commit must stay",
            commits[missing].short
        );
    }
    Ok(Plan { groups })
}

/// The model's plan, read from its JSON reply and made consistent (see
/// [`settle`]). With `autosquash`, fixup! and squash! commits fold into their
/// targets whatever the model says; a plan revised at the user's request
/// keeps the model's choice. Also returns notes on what had to be changed.
pub fn parse_plan(
    reply: &str,
    commits: &[Commit],
    autosquash: bool,
) -> Result<(Plan, Vec<String>)> {
    let start = reply.find(['{', '[']).context("the reply has no JSON")?;
    let end = reply.rfind(['}', ']']).context("the reply has no JSON")?;
    if end < start {
        bail!("the reply has no JSON");
    }
    let value: Value =
        serde_json::from_str(&reply[start..=end]).context("the reply isn't valid JSON")?;
    let entries = value
        .get("commits")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .context("the JSON has no list of commits")?;
    let number = |value: &Value| -> Option<usize> {
        let number = value
            .as_u64()
            .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))?;
        usize::try_from(number).ok()?.checked_sub(1)
    };
    let mut actions = vec![Action::Keep; commits.len()];
    for entry in entries {
        let Some(index) = number(&entry["commit"]).filter(|&index| index < commits.len()) else {
            continue;
        };
        let action = entry["action"].as_str().unwrap_or_default().to_lowercase();
        actions[index] = match action.as_str() {
            "reword" => Action::Reword,
            "fold" | "fixup" | "squash" => match number(&entry["into"]) {
                Some(into) => Action::Fold(into),
                // Without a target, a fold usually means the commit just above.
                None => Action::Fold(index.wrapping_sub(1)),
            },
            _ => Action::Keep,
        };
    }
    if autosquash {
        autosquash_fixups(&mut actions, commits);
    }
    Ok(settle(actions, commits))
}

/// Folds fixup! and squash! commits into their targets, as `git rebase
/// --autosquash` does, and rewords those whose targets aren't in the list.
fn autosquash_fixups(actions: &mut [Action], commits: &[Commit]) {
    for (index, commit) in commits.iter().enumerate() {
        let Some(target) = fixup_target(commit.subject()) else {
            continue;
        };
        let found = (0..index)
            .rev()
            .find(|&earlier| commits[earlier].subject().starts_with(target));
        actions[index] = match found {
            Some(earlier) => Action::Fold(earlier),
            // Its commit is pushed already, so it needs a message of its own.
            None => Action::Reword,
        };
    }
}

/// Makes the actions consistent: a fold must go into an earlier commit, and
/// a fold into a commit that is itself folded goes into that commit's target.
fn settle(mut actions: Vec<Action>, commits: &[Commit]) -> (Plan, Vec<String>) {
    let mut notes = Vec::new();
    for index in 0..actions.len() {
        if let Action::Fold(into) = actions[index] {
            if into >= index {
                notes.push(format!(
                    "the model folded {} into a later commit; git-agent keeps it",
                    commits[index].short
                ));
                actions[index] = Action::Keep;
                continue;
            }
            let mut target = into;
            while let Action::Fold(next) = actions[target] {
                target = next;
            }
            actions[index] = Action::Fold(target);
        }
    }
    let mut groups: Vec<Group> = Vec::new();
    for (index, action) in actions.into_iter().enumerate() {
        match action {
            Action::Fold(into) => {
                if let Some(group) = groups.iter_mut().find(|group| group.commit == into) {
                    group.folded.push(index);
                }
            }
            _ => groups.push(Group {
                commit: index,
                reword: action == Action::Reword,
                folded: Vec::new(),
            }),
        }
    }
    (Plan { groups }, notes)
}

/// The subject a `fixup! <subject>` or `squash! <subject>` commit fixes.
fn fixup_target(subject: &str) -> Option<&str> {
    let mut rest = subject;
    while let Some(after) = ["fixup! ", "squash! "]
        .iter()
        .find_map(|prefix| rest.strip_prefix(prefix))
    {
        rest = after;
    }
    (rest.len() < subject.len() && !rest.trim().is_empty()).then_some(rest)
}

/// The commits for the model, numbered and oldest first, in about `room`
/// bytes. Messages are shown whole; the changes share what room is left.
pub fn describe(commits: &[Commit], room: usize) -> String {
    let heads: Vec<String> = commits
        .iter()
        .enumerate()
        .map(|(index, commit)| {
            let mut head = format!("Commit {} ({}):\n", index + 1, commit.short);
            for line in commit.message.lines() {
                let _ = writeln!(head, "    {line}");
            }
            head.push_str("Changes:\n");
            head
        })
        .collect();
    let heads_size: usize = heads.iter().map(String::len).sum();
    let sizes: Vec<usize> = commits.iter().map(|commit| commit.patch.len()).collect();
    let left = room.saturating_sub(heads_size);
    let shares = if sizes.iter().sum::<usize>() <= left {
        sizes
    } else {
        fit::share(&sizes, left)
    };
    let mut out = String::new();
    for ((head, commit), share) in heads.iter().zip(commits).zip(shares) {
        out.push_str(head);
        if commit.patch.trim().is_empty() {
            out.push_str("(no changes shown)\n");
        } else {
            out.push_str(&fit::fit(&commit.patch, share).text);
        }
        out.push('\n');
    }
    out
}

/// The `git rebase -i` todo list that carries out `plan`. `messages` holds,
/// for each group in order, the file with its new message, if it gets one.
pub fn todo(plan: &Plan, commits: &[Commit], messages: &[Option<PathBuf>]) -> String {
    let mut out = String::new();
    for (group, message) in plan.groups.iter().zip(messages) {
        let commit = &commits[group.commit];
        let _ = writeln!(out, "pick {} {}", commit.short, commit.subject());
        for &folded in &group.folded {
            let commit = &commits[folded];
            let _ = writeln!(out, "fixup {} {}", commit.short, commit.subject());
        }
        if let Some(path) = message {
            let _ = writeln!(
                out,
                "exec git commit --amend --quiet --no-verify --allow-empty --file {}",
                crate::git::shell_quote(&path.to_string_lossy())
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commits(subjects: &[&str]) -> Vec<Commit> {
        subjects
            .iter()
            .enumerate()
            .map(|(index, subject)| Commit {
                short: format!("c{index:06}"),
                message: subject.to_string(),
                patch: format!(
                    "diff --git a/f{index}.rs b/f{index}.rs\n--- a/f{index}.rs\n+++ b/f{index}.rs\n\
                     @@ -1 +1 @@\n-old\n+new {index}\n"
                ),
            })
            .collect()
    }

    fn group(commit: usize, reword: bool, folded: &[usize]) -> Group {
        Group {
            commit,
            reword,
            folded: folded.to_vec(),
        }
    }

    #[test]
    fn reads_the_models_plan() {
        let commits = commits(&["feat: add cache", "wip", "docs: readme", "fix typo"]);
        let reply = "Here is the plan:\n```json\n{\"commits\": [{\"commit\": 1, \"action\": \"keep\"}, \
                     {\"commit\": 2, \"action\": \"reword\"}, {\"commit\": \"4\", \"action\": \"fold\", \
                     \"into\": 1}]}\n```";
        let (plan, notes) = parse_plan(reply, &commits, true).unwrap();
        assert!(notes.is_empty());
        assert_eq!(
            plan.groups,
            [
                group(0, false, &[3]),
                group(1, true, &[]),
                group(2, false, &[])
            ]
        );
        assert!(plan.changes_anything());
        assert!(parse_plan("no plan", &commits, true).is_err());
        let (untouched, _) = parse_plan("{\"commits\": []}", &commits, true).unwrap();
        assert!(!untouched.changes_anything());
    }

    #[test]
    fn settles_folds_the_model_got_wrong() {
        let commits = commits(&["feat: a", "fix: b", "oops", "more"]);
        // Into a later commit, into itself, and into a folded commit.
        let reply = r#"[{"commit": 1, "action": "fold", "into": 3}, {"commit": 3, "action": "fold", "into": 2},
                        {"commit": 4, "action": "fold", "into": 3}, {"commit": 2, "action": "fold"}]"#;
        let (plan, notes) = parse_plan(reply, &commits, true).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(plan.groups, [group(0, false, &[1, 2, 3])]);
    }

    #[test]
    fn folds_fixup_commits_into_their_targets() {
        let commits = commits(&[
            "feat: add cache",
            "docs: readme",
            "fixup! feat: add cache",
            "fixup! fixup! docs: readme",
            "squash! feat: something pushed already",
        ]);
        let (plan, _) = parse_plan(r#"{"commits": []}"#, &commits, true).unwrap();
        assert_eq!(
            plan.groups,
            [
                group(0, false, &[2]),
                group(1, false, &[3]),
                group(4, true, &[])
            ]
        );
        // After the user's feedback, the model's choice stands.
        let (plan, _) = parse_plan(r#"{"commits": []}"#, &commits, false).unwrap();
        assert!(!plan.changes_anything());
        assert_eq!(fixup_target("fixup! "), None);
        assert_eq!(fixup_target("feat: x"), None);
    }

    #[test]
    fn shows_and_edits_the_plan_as_lines() {
        let commits = commits(&["feat: add cache", "wip", "fix typo"]);
        let plan = Plan {
            groups: vec![group(0, false, &[2]), group(1, true, &[])],
        };
        let shown = show(&plan, &commits);
        assert_eq!(
            shown,
            "keep   c000000 feat: add cache\nfold   c000002 fix typo\nreword c000001 wip\n\n\
             3 commits become 2: 1 folded into others, 1 with a new message"
        );
        let text = to_text(&plan, &commits);
        assert!(text.starts_with("# Edit the plan."));
        assert_eq!(from_text(&text, &commits).unwrap(), plan);

        let edited = "pick c000000 x\nr c000002\nf c000001 wip\n";
        assert_eq!(
            from_text(edited, &commits).unwrap().groups,
            [group(0, false, &[]), group(2, true, &[1])]
        );
        let error = |text: &str| from_text(text, &commits).unwrap_err().to_string();
        assert!(error("keep c000000\nkeep c000001\n").contains("c000002 is missing"));
        assert!(
            error("fold c000000\nkeep c000001\nkeep c000002").contains("first line can't fold")
        );
        assert!(error("keep c000000\nkeep c000000").contains("twice"));
        assert!(error("drop c000000\nkeep c000001\nkeep c000002").contains("unknown action"));
        assert!(error("keep nope").contains("names no commit"));
    }

    #[test]
    fn describes_the_commits_within_the_room() {
        let mut commits = commits(&["feat: add cache\n\nIt is faster.", "wip"]);
        assert!(describe(&commits, 100_000).starts_with(
            "Commit 1 (c000000):\n    feat: add cache\n    \n    It is faster.\nChanges:\ndiff --git"
        ));
        commits[1].patch = format!(
            "diff --git a/big.rs b/big.rs\n@@ -1 +1,2000 @@\n{}",
            "+a long line of new code\n".repeat(2000)
        );
        let text = describe(&commits, 6000);
        assert!(text.len() < 6000 + 500, "{} bytes", text.len());
        assert!(text.contains("Commit 2 (c000001):\n    wip\n"));
        assert!(text.contains("+new 0"));
    }

    #[test]
    fn writes_the_rebase_todo_list() {
        let commits = commits(&["feat: add cache", "wip", "fix typo"]);
        let plan = Plan {
            groups: vec![group(0, false, &[2]), group(1, true, &[])],
        };
        let todo = todo(&plan, &commits, &[None, Some(PathBuf::from("/tmp/it's"))]);
        assert_eq!(
            todo,
            "pick c000000 feat: add cache\nfixup c000002 fix typo\npick c000001 wip\n\
             exec git commit --amend --quiet --no-verify --allow-empty --file '/tmp/it'\\''s'\n"
        );
        assert!(
            plan.to_json()
                .contains(r#"{"action":"fold","commit":3,"into":1}"#)
        );
    }
}
