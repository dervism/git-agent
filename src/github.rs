//! Pull requests and issues through the GitHub CLI (`gh`), which brings its own login.

use crate::safe_text::one_line;
use anyhow::{Context, Result, anyhow, bail};
use std::io::{self, ErrorKind, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// A GitHub issue, as `gh issue view` reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
}

impl Issue {
    /// The issue on one line, whatever its title holds.
    pub fn summary(&self) -> String {
        format!("Issue #{}: {}", self.number, one_line(&self.title))
    }
}

/// A pull request to create.
pub struct PullRequest<'a> {
    pub title: &'a str,
    pub body: &'a str,
    /// The branch it merges into.
    pub base: &'a str,
    /// The branch with the changes.
    pub head: &'a str,
    pub draft: bool,
    /// More arguments for `gh pr create`, e.g. `--label bug`.
    pub extra_args: &'a [String],
}

fn gh(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("gh");
    command.current_dir(dir).args(args);
    command
}

fn start_error(err: io::Error) -> anyhow::Error {
    if err.kind() == ErrorKind::NotFound {
        anyhow!(
            "git-agent works with GitHub through the GitHub CLI, which is not installed; get it \
             from https://cli.github.com, then run `gh auth login`"
        )
    } else {
        anyhow::Error::new(err).context("failed to run gh")
    }
}

/// Runs gh and returns its trimmed stdout, failing with its stderr.
fn output(dir: &Path, args: &[&str]) -> Result<String> {
    let output = gh(dir, args).output().map_err(start_error)?;
    if !output.status.success() {
        bail!(
            "`gh {}` failed: {}",
            args[..2].join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The repository's default branch, e.g. `main`.
pub fn default_branch(dir: &Path) -> Result<String> {
    let name = output(
        dir,
        &[
            "repo",
            "view",
            "--json",
            "defaultBranchRef",
            "--jq",
            ".defaultBranchRef.name",
        ],
    )
    .context("could not ask GitHub for the default branch; name the base branch with --base")?;
    if name.is_empty() {
        bail!("GitHub reported no default branch; name the base branch with --base");
    }
    Ok(name)
}

/// The issue `reference` names: a number, or an issue's URL.
pub fn issue(dir: &Path, reference: &str) -> Result<Issue> {
    let json = output(
        dir,
        &[
            "issue",
            "view",
            reference,
            "--json",
            "number,title,body,labels",
        ],
    )?;
    parse_issue(&json).with_context(|| format!("could not read issue {reference} from gh"))
}

fn parse_issue(json: &str) -> Result<Issue> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
    let labels = value["labels"].as_array().map_or_else(Vec::new, |labels| {
        labels
            .iter()
            .filter_map(|label| label["name"].as_str().map(str::to_owned))
            .collect()
    });
    Ok(Issue {
        number: value["number"].as_u64().context("no issue number")?,
        title: text("title"),
        body: text("body"),
        labels,
    })
}

/// The URL of `branch`'s open pull request, if it has one.
pub fn open_pull_request(dir: &Path, branch: &str) -> Result<Option<String>> {
    let url = output(
        dir,
        &[
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "open",
            "--json",
            "url",
            "--jq",
            ".[0].url // empty",
        ],
    )?;
    Ok((!url.is_empty()).then_some(url))
}

/// Creates the pull request. gh prints its progress to the user, and returns the new pull
/// request's URL, which is for the caller to print: gh's own stdout isn't git-agent's, so that
/// with the reader of that output gone, gh doesn't die of `SIGPIPE` after it has made the
/// pull request.
pub fn create_pull_request(dir: &Path, pull_request: &PullRequest) -> Result<String> {
    let mut command = gh(
        dir,
        &[
            "pr",
            "create",
            "--title",
            pull_request.title,
            "--body-file",
            "-",
            "--base",
            pull_request.base,
            "--head",
            pull_request.head,
        ],
    );
    if pull_request.draft {
        command.arg("--draft");
    }
    command.args(pull_request.extra_args);
    send_description(command, pull_request.body)
}

/// Runs `command` with `description` on its standard input, and returns its trimmed standard
/// output. Its standard error stays the user's.
fn send_description(mut command: Command, description: &str) -> Result<String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(start_error)?;
    // Dropping stdin closes it, so gh sees the end of the description.
    let written = child
        .stdin
        .take()
        .context("gh has no input")?
        .write_all(description.as_bytes());
    let output = child.wait_with_output().context("failed to wait for gh")?;
    written.context("failed to send the description to gh")?;
    if !output.status.success() {
        bail!("`gh pr create` failed ({})", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_an_issue_on_one_line() {
        let issue = Issue {
            number: 7,
            title: "Fiks å lagre\n✅ Pushed.\u{1b}[2J".to_owned(),
            body: String::new(),
            labels: Vec::new(),
        };
        assert_eq!(
            issue.summary(),
            "Issue #7: Fiks å lagre\\n✅ Pushed.\\033[2J"
        );
    }

    fn script(text: &str) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(text);
        command
    }

    #[test]
    fn sends_the_description_and_returns_what_gh_prints_trimmed() {
        let gh = script("body=$(cat); echo \"  https://github.example/pull/7 ($body)  \"");
        assert_eq!(
            send_description(gh, "the body").unwrap(),
            "https://github.example/pull/7 (the body)"
        );
    }

    #[test]
    fn returns_nothing_when_gh_prints_nothing() {
        assert_eq!(send_description(script("cat >/dev/null"), "x").unwrap(), "");
    }

    #[test]
    fn fails_with_gh_s_status_and_no_url_when_gh_fails() {
        let gh = script("cat >/dev/null; echo not-a-url; exit 3");
        let err = send_description(gh, "x").unwrap_err();
        assert_eq!(err.to_string(), "`gh pr create` failed (exit status: 3)");
    }

    #[test]
    fn fails_with_an_error_to_report_when_the_description_cant_be_sent() {
        let err = send_description(Command::new("true"), &"x".repeat(1 << 20)).unwrap_err();
        assert_eq!(err.to_string(), "failed to send the description to gh");
        assert!(!err.is::<crate::ui::OutputClosed>());
    }

    #[test]
    fn says_what_to_install_when_gh_is_missing() {
        let err = send_description(Command::new("gh-that-is-not-installed"), "x").unwrap_err();
        assert!(err.to_string().contains("GitHub CLI"), "{err}");
    }

    #[test]
    fn reads_an_issue() {
        let json = r#"{"body":"It hangs.","labels":[{"id":"x","name":"bug"},{"name":"auth"}],"number":42,"title":"Login times out"}"#;
        assert_eq!(
            parse_issue(json).unwrap(),
            Issue {
                number: 42,
                title: "Login times out".into(),
                body: "It hangs.".into(),
                labels: vec!["bug".into(), "auth".into()],
            }
        );
        assert!(parse_issue("{}").is_err());
    }
}
