//! Naming a new branch (`git agent branch`) after a description, a GitHub
//! issue, or the uncommitted changes.

use std::collections::HashSet;

/// Longest name to keep; a longer one is cut at a hyphen.
const MOST_CHARS: usize = 60;
/// Most existing branch names to show the model as examples of the style.
const MOST_EXAMPLES: usize = 15;
/// Branches whose names say nothing about the naming style.
const LONG_LIVED: [&str; 6] = ["main", "master", "develop", "dev", "trunk", "HEAD"];

/// The issue `text` names, as `gh issue view` takes it: a number such as
/// `42` or `#42`, or an issue's URL.
pub fn issue_reference(text: &str) -> Option<String> {
    let text = text.trim();
    let number = text.strip_prefix('#').unwrap_or(text);
    if !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(number.to_owned());
    }
    let is_url = (text.starts_with("https://") || text.starts_with("http://"))
        && text.contains("/issues/")
        && !text.contains(char::is_whitespace);
    is_url.then(|| text.to_owned())
}

/// The model's reply as a branch name: its first line, without a label such
/// as "Branch name:", quotes or characters git doesn't allow, and at most
/// [`MOST_CHARS`] long. `None` if nothing is left.
pub fn clean_name(reply: &str) -> Option<String> {
    let line = reply.lines().map(str::trim).find(|line| !line.is_empty())?;
    let line = match line.rsplit_once(": ") {
        Some((label, name))
            if ["branch", "name"]
                .iter()
                .any(|word| label.to_lowercase().contains(word)) =>
        {
            name
        }
        _ => line,
    };
    let line =
        line.trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | '*') || c.is_whitespace());
    let mut name = String::new();
    for c in line.chars() {
        let c = if c.is_whitespace() { '-' } else { c };
        if !(c.is_alphanumeric() || matches!(c, '-' | '_' | '/' | '.')) {
            continue;
        }
        // No runs of separators, which git rejects ("..", "//") or which look odd ("--").
        if matches!(c, '-' | '/' | '.') && name.ends_with(['-', '/', '.']) {
            if c == '/' {
                name.pop();
            } else {
                continue;
            }
        }
        name.push(c);
    }
    let mut name = name
        .split('/')
        .map(|part| part.trim_matches(['-', '.']))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    while name.ends_with(".lock") {
        name.truncate(name.len() - ".lock".len());
    }
    if name.chars().count() > MOST_CHARS {
        let cut: String = name.chars().take(MOST_CHARS).collect();
        name = match cut.rfind('-') {
            Some(hyphen) if hyphen > cut.find('/').unwrap_or(0) => cut[..hyphen].to_owned(),
            _ => cut,
        };
    }
    let name = name.trim_end_matches(['-', '/', '.']).to_owned();
    (!name.is_empty()).then_some(name)
}

/// `name` without the issue numbers and ticket keys that `work` doesn't
/// mention. Small models copy them from other branches or make them up, and
/// a wrong key can link the branch to someone else's ticket.
pub fn drop_unknown_references(name: &str, work: &str) -> String {
    let (prefix, rest) = match name.split_once('/') {
        Some((prefix, rest)) => (Some(prefix), rest),
        None => (None, name),
    };
    let tokens: HashSet<&str> = work.split(|c: char| !c.is_alphanumeric()).collect();
    let is_number = |word: &str| !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit());
    let is_key = |word: &str| {
        word.len() >= 2
            && word.starts_with(|c: char| c.is_ascii_uppercase())
            && word
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    };
    let words: Vec<&str> = rest.split('-').collect();
    let mut kept = Vec::new();
    let mut index = 0;
    while index < words.len() {
        let word = words[index];
        if is_key(word) && words.get(index + 1).is_some_and(|next| is_number(next)) {
            let key = format!("{word}-{}", words[index + 1]);
            if work.to_uppercase().contains(&key) {
                kept.extend(&words[index..index + 2]);
            }
            index += 2;
            continue;
        }
        if !((is_number(word) || is_key(word)) && !tokens.contains(word)) {
            kept.push(word);
        }
        index += 1;
    }
    if kept.is_empty() {
        return name.to_owned();
    }
    match prefix {
        Some(prefix) => format!("{prefix}/{}", kept.join("-")),
        None => kept.join("-"),
    }
}

/// Recent branch names that show how the repository names its branches,
/// without long-lived ones such as main.
pub fn style_examples(branches: &[String]) -> Vec<&str> {
    branches
        .iter()
        .map(String::as_str)
        .filter(|name| !LONG_LIVED.contains(name))
        .take(MOST_EXAMPLES)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_issue_numbers_and_urls() {
        assert_eq!(issue_reference("42").as_deref(), Some("42"));
        assert_eq!(issue_reference(" #42 ").as_deref(), Some("42"));
        let url = "https://github.com/dervism/git-agent/issues/7";
        assert_eq!(issue_reference(url).as_deref(), Some(url));
        assert_eq!(issue_reference("fix 42"), None);
        assert_eq!(issue_reference("#"), None);
        assert_eq!(
            issue_reference("https://github.com/dervism/git-agent/pull/7"),
            None
        );
    }

    #[test]
    fn cleans_the_reply_into_a_name_git_accepts() {
        let clean = |reply: &str| clean_name(reply);
        assert_eq!(
            clean("fix/login-timeout").as_deref(),
            Some("fix/login-timeout")
        );
        assert_eq!(
            clean("`fix/login-timeout`\n").as_deref(),
            Some("fix/login-timeout")
        );
        assert_eq!(
            clean("\nBranch name: \"feat/add cache\"\nIt adds a cache.").as_deref(),
            Some("feat/add-cache")
        );
        assert_eq!(
            clean("fix//login..timeout--now").as_deref(),
            Some("fix/login.timeout-now")
        );
        assert_eq!(
            clean("-fix/~bad^:name?.lock").as_deref(),
            Some("fix/badname")
        );
        assert_eq!(clean("/.hidden/x/").as_deref(), Some("hidden/x"));
        assert_eq!(
            clean("ABC-123-Fix-login").as_deref(),
            Some("ABC-123-Fix-login")
        );
        assert_eq!(
            clean("fix: login timeout").as_deref(),
            Some("fix-login-timeout")
        );
        assert_eq!(clean("   \n``\n"), None);

        let long = clean(&format!("feat/{}", "word-".repeat(20))).unwrap();
        assert!(long.len() <= MOST_CHARS && long.starts_with("feat/word-"));
        assert!(long.ends_with("word"));
    }

    #[test]
    fn drops_issue_numbers_and_ticket_keys_the_work_does_not_mention() {
        let drop = drop_unknown_references;
        let work = "the export crashes on empty rows";
        assert_eq!(
            drop("bugfix/PROJ-9-export-crashes", work),
            "bugfix/export-crashes"
        );
        assert_eq!(
            drop("feature/PROJ-dark-mode", "add dark mode"),
            "feature/dark-mode"
        );
        assert_eq!(drop("fix/42-export-crashes", work), "fix/export-crashes");
        let work = "PROJ-31 rate limit the login endpoint, see #42 and issue 7";
        assert_eq!(
            drop("feature/PROJ-31-rate-limit", work),
            "feature/PROJ-31-rate-limit"
        );
        assert_eq!(drop("fix/42-rate-limit", work), "fix/42-rate-limit");
        assert_eq!(drop("PROJ-31-rate-limit", work), "PROJ-31-rate-limit");
        assert_eq!(
            drop("fix/login-timeout-after-30s", "after 30 seconds"),
            "fix/login-timeout-after-30s"
        );
        assert_eq!(
            drop("fix/API-timeout", "the API times out"),
            "fix/API-timeout"
        );
        // A name of nothing but references stays as it is.
        assert_eq!(drop("fix/PROJ-9", "something"), "fix/PROJ-9");
    }

    #[test]
    fn shows_recent_branches_but_not_long_lived_ones() {
        let branches: Vec<String> = ["main", "feat/split", "develop", "fix/pr-base"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        assert_eq!(style_examples(&branches), ["feat/split", "fix/pr-base"]);
    }
}
