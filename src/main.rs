#![warn(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]

mod api_keys;
mod branch;
mod cli;
mod config;
mod conversation;
mod explain;
mod git;
mod github;
#[cfg(test)]
mod layers;
mod menu;
mod prompt;
mod providers;
mod review;
mod safe_text;
mod secrets;
mod server;
mod split;
mod staging;
mod temp;
mod tidy;
mod ui;
mod why;

use anyhow::{Context, Result, bail};
use cli::{
    BranchArgs, Command, ConfigCommand, ExplainArgs, GenerateArgs, PrArgs, TidyArgs, WhyArgs,
};
use config::{Config, OnSecrets, Overrides};
use conversation::models::{self, Model};
use conversation::{
    commit_message, connect, discuss, generate_message, initial_messages, notes_bytes,
    question_bytes, report_fit, report_model, separated, tokens_in,
};
use git::{Changes, Repo, Scope};
use github::PullRequest;
use prompt::Work;
use review::{Action, Choice, Person, Review, Revisions, Terminal};
use secrets::redact::{self, Finding};
use secrets::{added, check_outgoing, list_findings, reject_added, report_secrets, scan_commits};
use server::{Message, ModelServer, Server};
use split::signals::Stopped;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use ui::thousands;

/// What accepting a commit message does.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CommitAcceptance {
    Commit,
    CommitAndPush,
}

impl CommitAcceptance {
    fn pushes(self) -> bool {
        match self {
            Self::Commit => false,
            Self::CommitAndPush => true,
        }
    }
}

const COMMIT_REVIEW: Review<CommitAcceptance> = Review {
    heading: "Proposed commit message",
    first_line: "subject line",
    noun: "message",
    what: "commit message",
    edit: "edit the message in your editor",
    accept: &[
        Choice {
            word: "commit",
            help: "commit with this message",
            value: CommitAcceptance::Commit,
        },
        Choice {
            word: "push",
            help: "commit with this message, then push",
            value: CommitAcceptance::CommitAndPush,
        },
    ],
    quit: "quit without committing",
};

/// What accepting a pull request does.
#[derive(Debug, Clone, Copy, PartialEq)]
enum PullRequestAcceptance {
    Create,
    CreateDraft,
}

impl PullRequestAcceptance {
    fn is_draft(self) -> bool {
        match self {
            Self::Create => false,
            Self::CreateDraft => true,
        }
    }
}

const PULL_REQUEST_REVIEW: Review<PullRequestAcceptance> = Review {
    heading: "Proposed pull request",
    first_line: "title",
    noun: "pull request",
    what: "pull request title and description",
    edit: "edit the pull request in your editor",
    accept: &[
        Choice {
            word: "create",
            help: "push the branch if needed, then create the pull request",
            value: PullRequestAcceptance::Create,
        },
        Choice {
            word: "draft",
            help: "the same, as a draft pull request",
            value: PullRequestAcceptance::CreateDraft,
        },
    ],
    quit: "quit without creating it",
};

/// Accepting the plan of a tidy-up.
const TIDY_REVIEW: Review<()> = Review {
    heading: "Proposed tidy-up",
    first_line: "first line",
    noun: "plan",
    what: "plan",
    edit: "edit the plan in your editor",
    accept: &[Choice {
        word: "accept",
        help: "write the new messages, reviewing each, then tidy the commits",
        value: (),
    }],
    quit: "quit without changing anything",
};

/// What accepting a new message for a commit being tidied does.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TidyMessageAcceptance {
    UseNew,
    KeepOld,
}

impl TidyMessageAcceptance {
    fn uses_new(self) -> bool {
        match self {
            Self::UseNew => true,
            Self::KeepOld => false,
        }
    }
}

const TIDY_MESSAGE_REVIEW: Review<TidyMessageAcceptance> = Review {
    heading: "Proposed commit message",
    first_line: "subject line",
    noun: "message",
    what: "commit message",
    edit: "edit the message in your editor",
    accept: &[
        Choice {
            word: "use",
            help: "give the commit this message",
            value: TidyMessageAcceptance::UseNew,
        },
        Choice {
            word: "keep",
            help: "keep the commit's old message",
            value: TidyMessageAcceptance::KeepOld,
        },
    ],
    quit: "stop without changing anything",
};

/// Most commits to tidy at a time.
const MOST_TIDIED: usize = 40;

/// Accepting a branch name.
const BRANCH_REVIEW: Review<()> = Review {
    heading: "Proposed branch name",
    first_line: "name",
    noun: "name",
    what: "branch name",
    edit: "edit the name",
    accept: &[Choice {
        word: "create",
        help: "create the branch and switch to it",
        value: (),
    }],
    quit: "quit without creating a branch",
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = cli::parse();
    let overrides = cli.settings.overrides();
    let result = match cli.command {
        None => generate(&cli.generate, &overrides).await,
        Some(Command::Split(args)) => split::flow::run(&args, &overrides).await,
        Some(Command::Pr(args)) => pull_request(&args, &overrides).await,
        Some(Command::Branch(args)) => new_branch(&args, &overrides).await,
        Some(Command::Tidy(args)) => tidy_commits(&args, &overrides).await,
        Some(Command::Why(args)) => explain_lines(&args, &overrides).await,
        Some(Command::Explain(args)) => explain_commits(&args, &overrides).await,
        Some(Command::Models) => list_models(&overrides).await,
        Some(Command::Config { command: None }) => menu::run(&overrides).await,
        Some(Command::Config {
            command: Some(ConfigCommand::Init { force }),
        }) => init_config(&overrides, force),
        Some(Command::Config {
            command: Some(ConfigCommand::Show),
        }) => show_config(&overrides),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            ui::error(&err);
            err.downcast_ref::<Stopped>()
                .map_or(ExitCode::FAILURE, |Stopped(code)| ExitCode::from(*code))
        }
    }
}

async fn generate(args: &GenerateArgs, overrides: &Overrides) -> Result<()> {
    const NOT_PUSHED: &str = "the commit was made, but not pushed";

    let config = Config::load(overrides)?.config;
    let repo = Repo::open(".")?;
    let ask = !args.commit && !args.raw && io::stdin().is_terminal();
    let Some(ToCommit {
        scope,
        mut changes,
        leave_out,
    }) = changes_to_commit(&repo, args, &config, ask)?
    else {
        return Ok(());
    };

    let summary = changes.stat.lines().last().unwrap_or_default().trim();
    match scope {
        Scope::Staged => ui::info(format!("🔍 Staged changes: {summary}")),
        Scope::Tracked => ui::info(format!(
            "🔍 Nothing staged; using all changes to tracked files: {summary}\n   {}",
            ui::dim("Committing will include them (git commit --all).")
        )),
    }
    let untracked = repo.untracked_count()?;
    if untracked > 0 && !args.all {
        ui::info(ui::dim(&format!(
            "   {untracked} untracked path(s) not included; use --all to include them."
        )));
    }
    let redacted = redact::diff(&changes.diff);
    changes.diff = redacted.text;
    if config.on_secrets == OnSecrets::Reject {
        reject_added(
            &redacted.findings,
            "the changes add what look like secrets, so git-agent didn't commit them",
            "Take them out of the changes.",
        )?;
    }
    report_secrets(
        &redacted.findings,
        "If they are real, take them out of the files before you commit.",
    );

    let (server, model) = connect(&config).await?;
    let (conversation, fit) = initial_messages(
        &config.system_prompt,
        &changes,
        args.hint.as_deref(),
        model.budget(),
    );
    report_fit(
        &model,
        &fit,
        "message",
        Some(
            "commit in smaller parts\n   (git agent split, or stage some files and run git agent)",
        ),
    );
    let message = commit_message(&server, &model, &conversation).await?;

    if args.raw {
        ui::plain_out(&message);
        return Ok(());
    }
    review::show_proposal(&COMMIT_REVIEW, &message);
    if args.commit {
        commit(&repo, &message, scope, &args.git_args, &leave_out)?;
        if args.push {
            check_outgoing(&repo, &config).context(NOT_PUSHED)?;
            push(&repo, false).context(NOT_PUSHED)?;
        }
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        ui::info(ui::dim(
            "Input is not a terminal, so not asking for review. \
             Use --commit to commit directly (add --push to also push).",
        ));
        return Ok(());
    }

    let revisions = Revisions {
        server: &server,
        model: &model,
        conversation: &conversation,
        edit: |message: &str| review::edit_message(&repo, message),
        clean: separated,
    };
    let reviewed = review::revise(&COMMIT_REVIEW, message, revisions, &Terminal).await?;
    let Some((acceptance, message)) = reviewed else {
        ui::info("Nothing committed.");
        return Ok(());
    };
    commit(&repo, &message, scope, &args.git_args, &leave_out)?;
    if acceptance.pushes() || args.push {
        check_outgoing(&repo, &config).context(NOT_PUSHED)?;
        push(&repo, true).context(NOT_PUSHED)?;
    }
    Ok(())
}

/// What `git agent` commits: the changes, how, and the submodule pointers it leaves out
/// because the diff hides them.
struct ToCommit {
    scope: Scope,
    changes: Changes,
    leave_out: Vec<String>,
}

/// The changes to commit and how, or `None` if there are none, after everything that
/// doesn't need the model. Unresolved conflicts come first, because `--all` would mark
/// them as resolved.
fn changes_to_commit(
    repo: &Repo,
    args: &GenerateArgs,
    config: &Config,
    ask: bool,
) -> Result<Option<ToCommit>> {
    staging::refuse_unresolved_conflicts(repo, "committing")?;
    if args.all && !staging::stage_all_and_report(repo, ask, false)? {
        return Ok(None);
    }
    let found = repo.changes(&config.exclude)?;
    let commits_all = matches!(found, Some((Scope::Tracked, _)));
    let hidden = staging::hidden_pointers(repo, commits_all)?;
    staging::report_hidden_pointers(&hidden, false);
    if found.is_none() {
        match repo.untracked_count()? {
            0 => ui::info("✨ No changes to commit."),
            // --all already said which untracked files it left out.
            _ if args.all => ui::info("✨ No changes to commit besides the files left out above."),
            n => ui::info(format!(
                "No changes to tracked files, but {n} untracked path(s). Use --all to include them."
            )),
        }
    }
    Ok(found.map(|(scope, changes)| ToCommit {
        scope,
        changes,
        leave_out: hidden.changed,
    }))
}

async fn pull_request(args: &PrArgs, overrides: &Overrides) -> Result<()> {
    let config = Config::load(overrides)?.config;
    let repo = Repo::open(".")?;
    let branch = repo.current_branch()?;
    // Also finds out early whether gh is installed and logged in.
    if !args.raw
        && let Some(url) = github::open_pull_request(repo.workdir(), &branch)?
    {
        ui::info(format!("{branch} already has an open pull request: {url}"));
        return Ok(());
    }
    let base = match &args.base {
        Some(base) => base.clone(),
        None => github::default_branch(repo.workdir())?,
    };
    if branch == base {
        bail!(
            "{branch} is the branch the pull request would merge into; switch to the branch \
             with your changes, or create one with `git switch -c <name>`"
        );
    }

    let base_ref = repo.fetch_branch(&repo.remote()?, &base)?;
    let mut commits = repo.commits_since(&base_ref)?;
    if commits.is_empty() {
        ui::info(format!(
            "✨ {branch} has no commits that {base_ref} doesn't have, so there is nothing to \
             open a pull request for."
        ));
        return Ok(());
    }
    let mut changes = repo.branch_changes(&base_ref, &config.exclude)?;
    let summary = changes.stat.lines().last().unwrap_or_default().trim();
    ui::info(format!(
        "🔍 {} commit(s) on {branch} that {base_ref} doesn't have: {summary}",
        commits.len()
    ));
    if repo.has_uncommitted_changes()? {
        ui::info(ui::dim(
            "   Uncommitted changes are not part of the pull request; commit them to include them.",
        ));
    }

    if config.on_secrets == OnSecrets::Reject {
        // Commit by commit rather than the branch's overall diff: a secret added
        // in one commit and removed in a later one is still in the history a
        // push publishes.
        let patches = repo.commit_patches(&[&format!("{base_ref}..HEAD")], &config.exclude)?;
        reject_added(
            &scan_commits(&patches),
            "the branch's commits add what look like secrets, so git-agent won't open a pull \
             request for it",
            "Remove them from the branch's history, for example with `git rebase -i`, and \
             revoke any that were pushed already.",
        )?;
    }
    let redacted = redact::diff(&changes.diff);
    changes.diff = redacted.text;
    let mut findings = redacted.findings;
    for commit in &mut commits {
        let place = format!("commit {} message", commit.hash);
        let redacted = redact::text(&commit.message, &place);
        commit.message = redacted.text;
        findings.extend(redacted.findings);
    }
    report_secrets(
        &findings,
        "They are in the branch's commits. If they are real, remove them from its history \
         before you push it, and revoke any that were pushed already.",
    );

    let (server, model) = connect(&config).await?;
    let messages: Vec<String> = commits.into_iter().map(|commit| commit.message).collect();
    let (conversation, fit) =
        prompt::pull_request_messages(&messages, &changes, args.hint.as_deref(), model.budget());
    report_fit(&model, &fit, "pull request", None);
    let text = generate_message(&server, &model, &conversation).await?;

    if args.raw {
        ui::plain_out(&text);
        return Ok(());
    }
    review::show_proposal(&PULL_REQUEST_REVIEW, &text);
    let (acceptance, text) = if args.create {
        (PullRequestAcceptance::Create, text)
    } else if !io::stdin().is_terminal() {
        ui::info(ui::dim(
            "Input is not a terminal, so not asking for review. \
             Use --create to create the pull request directly.",
        ));
        return Ok(());
    } else {
        let revisions = Revisions {
            server: &server,
            model: &model,
            conversation: &conversation,
            edit: |text: &str| review::edit_pull_request(&repo, text),
            clean: Ok,
        };
        let reviewed = review::revise(&PULL_REQUEST_REVIEW, text, revisions, &Terminal).await?;
        let Some(reviewed) = reviewed else {
            ui::info("No pull request created.");
            return Ok(());
        };
        reviewed
    };

    let (title, body) = prompt::split_title(&text);
    let pull_request = PullRequest {
        title: &title,
        body: &body,
        base: &base,
        head: &branch,
        draft: acceptance.is_draft() || args.draft,
        extra_args: &args.gh_args,
    };
    create_pull_request(&repo, &config, &pull_request, !args.create)
}

async fn new_branch(args: &BranchArgs, overrides: &Overrides) -> Result<()> {
    let config = Config::load(overrides)?.config;
    let repo = Repo::open(".")?;
    if let Some(from) = &args.from
        && repo.resolve(from)?.is_none()
    {
        bail!("{from} is not a commit or a branch");
    }
    let description = args.description.join(" ");
    let description = description.trim();
    let issue;
    let mut changes;
    let work = if description.is_empty() {
        let Some((_, found)) = repo.changes(&config.exclude)? else {
            bail!(
                "say what the branch is for, e.g. `git agent branch fix login timeout`, or name \
                 an issue, e.g. `git agent branch 42`; with uncommitted changes, git-agent can \
                 name the branch after them"
            );
        };
        changes = found;
        let summary = changes.stat.lines().last().unwrap_or_default().trim();
        ui::info(format!(
            "🔍 Naming the branch after the uncommitted changes: {summary}"
        ));
        let redacted = redact::diff(&changes.diff);
        changes.diff = redacted.text;
        report_secrets(
            &redacted.findings,
            "If they are real, take them out of the files before you commit.",
        );
        Work::Changes(&changes)
    } else if let Some(reference) = branch::issue_reference(description) {
        let mut found = github::issue(repo.workdir(), &reference)?;
        ui::info(format!("🔍 {}", found.summary()));
        let redacted = redact::text(&found.body, &format!("issue #{}", found.number));
        found.body = redacted.text;
        report_secrets(
            &redacted.findings,
            "They are in the issue on GitHub. If they are real, revoke them.",
        );
        issue = found;
        Work::Issue(&issue)
    } else {
        Work::Described(description)
    };

    // What the work mentions, so the name doesn't get references it lacks.
    let mentioned = match &work {
        Work::Described(text) => text.to_string(),
        Work::Issue(issue) => format!("#{} {}\n{}", issue.number, issue.title, issue.body),
        Work::Changes(changes) => format!("{}\n{}", changes.stat, changes.diff),
    };
    let taken = repo.branch_names()?;
    let (server, model) = connect(&config).await?;
    let conversation =
        prompt::branch_messages(&work, &branch::style_examples(&taken), model.budget());
    report_model(&model, tokens_in(&conversation));
    let clean = |reply: String| {
        let name = branch_name(&repo, &reply)?;
        branch_name(&repo, &branch::drop_unknown_references(&name, &mentioned))
    };
    let mut name = clean(generate_message(&server, &model, &conversation).await?)?;
    if args.raw {
        ui::plain_out(&name);
        return Ok(());
    }

    review::show_proposal(&BRANCH_REVIEW, &name);
    let exists = |name: &str| taken.iter().any(|known| known == name);
    if exists(&name) {
        ui::warn(format!("a branch named {name} already exists"));
    }
    if args.create {
        if exists(&name) {
            bail!(
                "a branch named {name} already exists; run git agent branch again to review the name"
            );
        }
    } else if !io::stdin().is_terminal() {
        ui::info(ui::dim(
            "Input is not a terminal, so not asking for review. \
             Use --create to create the branch directly.",
        ));
        return Ok(());
    } else {
        let edit = |name: &str| {
            let edited = ui::edit_line("Branch name:", name)?;
            edited.map(|edited| branch_name(&repo, &edited)).transpose()
        };
        name = loop {
            let revisions = Revisions {
                server: &server,
                model: &model,
                conversation: &conversation,
                edit: &edit,
                clean: &clean,
            };
            let reviewed = review::revise(&BRANCH_REVIEW, name, revisions, &Terminal).await?;
            let Some(((), reviewed)) = reviewed else {
                ui::info("No branch created.");
                return Ok(());
            };
            if !exists(&reviewed) {
                break reviewed;
            }
            ui::warn(format!(
                "a branch named {reviewed} already exists; edit the name or ask for another"
            ));
            name = reviewed;
        };
    }

    repo.create_branch(&name, args.from.as_deref())?;
    if repo.has_uncommitted_changes()? {
        ui::info(ui::dim("   Your uncommitted changes came along."));
    }
    ui::info(ui::dim(
        "   Commit with `git agent`, and open a pull request with `git agent pr` when the \
         branch is ready.",
    ));
    Ok(())
}

/// `text`, a reply from the model or the user's edit, as a branch name git accepts.
fn branch_name(repo: &Repo, text: &str) -> Result<String> {
    branch::clean_name(text)
        .filter(|name| repo.is_valid_branch_name(name))
        .with_context(|| format!("{:?} can't be made into a branch name", text.trim()))
}

async fn tidy_commits(args: &TidyArgs, overrides: &Overrides) -> Result<()> {
    let config = Config::load(overrides)?.config;
    let repo = Repo::open(".")?;
    if let Some(operation) = repo.operation_in_progress()? {
        bail!("{operation} is in progress; finish or abort it before tidying commits");
    }
    let head = repo
        .resolve("HEAD")?
        .context("there are no commits to tidy yet")?;
    let unpushed = repo.unpushed()?;
    let hashes = match &args.base {
        Some(base) => {
            let base = repo
                .resolve(base)?
                .with_context(|| format!("{base} is not a commit"))?;
            let after = repo.commits_after(&base)?;
            if let Some(pushed) = after.iter().find(|hash| !unpushed.contains(hash)) {
                bail!(
                    "commit {} is on a remote already; git-agent only tidies commits that aren't \
                     pushed, so it never has to force-push",
                    &pushed[..7]
                );
            }
            after
        }
        None if !repo.has_remotes()? => bail!(
            "the repository has no remote, so git-agent can't tell which commits are pushed; \
             name the commit to tidy after with --base, e.g. --base HEAD~5"
        ),
        None => unpushed,
    };
    if hashes.is_empty() {
        ui::info("✨ No commits to tidy: the remote has them all.");
        return Ok(());
    }
    if hashes.len() > MOST_TIDIED {
        bail!(
            "there are {} commits to tidy, more than {MOST_TIDIED} at a time; tidy the latest \
             ones with --base HEAD~{MOST_TIDIED}",
            hashes.len()
        );
    }
    let base = repo.parents(&hashes[0])?.first().cloned();
    let range = match &base {
        Some(base) => format!("{base}..HEAD"),
        None => "HEAD".to_owned(),
    };
    if repo.has_merges(&range)? {
        bail!(
            "there is a merge among the commits to tidy; git-agent tidies a straight line of commits"
        );
    }
    let commits: Vec<tidy::Commit> = repo
        .commit_patches(&[&range], &config.exclude)?
        .into_iter()
        .rev()
        .map(|commit| tidy::Commit {
            short: commit.hash,
            message: commit.message,
            patch: commit.patch,
        })
        .collect();
    ui::info(format!(
        "🔍 {} commit(s) to tidy, from {} {} to {} {}",
        commits.len(),
        commits[0].short,
        safe_text::one_line(commits[0].subject()),
        commits[commits.len() - 1].short,
        safe_text::one_line(commits[commits.len() - 1].subject())
    ));
    let findings: Vec<Finding> = commits
        .iter()
        .flat_map(|commit| redact::commit(&commit.short, &commit.message, &commit.patch))
        .collect();
    let added = added(&findings);
    if !added.is_empty() {
        ui::warn(format!(
            "the commits add what look like secrets; the model gets {} in their place:\n{}",
            redact::REDACTED,
            list_findings(&added)
        ));
    }

    let (server, model) = connect(&config).await?;
    let room = model
        .budget()
        .user_message_bytes(prompt::TIDY_PROMPT)
        .saturating_sub(notes_bytes(args.hint.as_deref()));
    let text = redact::text(&tidy::describe(&commits, room), "commits").text;
    let conversation = prompt::tidy_messages(&text, args.hint.as_deref());
    report_model(&model, tokens_in(&conversation));
    let tidier = Tidier {
        repo: &repo,
        server: &server,
        model: &model,
        commits: &commits,
        person: &Terminal,
    };
    let mut plan = tidier.ask_for_plan(&conversation, true).await?;
    if args.apply || !io::stdin().is_terminal() {
        ui::show_framed(TIDY_REVIEW.heading, &tidy::show(&plan, &commits));
        if !plan.changes_anything() {
            ui::info("✨ The commits look tidy already.");
            return Ok(());
        }
        if !args.apply {
            ui::info(ui::dim(
                "Input is not a terminal, so not asking for review. \
                 Use --apply to tidy the commits directly.",
            ));
            return Ok(());
        }
    } else {
        match tidier.review_plan(plan, &conversation).await? {
            Some(reviewed) => plan = reviewed,
            None => {
                ui::info("Nothing changed.");
                return Ok(());
            }
        }
    }

    let mut messages: Vec<Option<PathBuf>> = vec![None; plan.groups.len()];
    let written = tidier
        .write_messages(&plan, &mut messages, &config, args)
        .await;
    let rewritten = match written {
        Ok(true)
            if plan.groups.iter().any(|group| !group.folded.is_empty())
                || messages.iter().any(Option::is_some) =>
        {
            let todo = tidy::todo(&plan, &commits, &messages);
            Some(repo.rewrite(&head, base.as_deref(), &todo))
        }
        Ok(_) => None,
        Err(err) => Some(Err(err)),
    };
    for path in messages.iter().flatten() {
        let _ = fs::remove_file(path);
    }
    let Some(rewritten) = rewritten else {
        ui::info("Nothing changed.");
        return Ok(());
    };
    let new = rewritten.context("nothing was changed")?;
    if repo.tree(&new)? != repo.tree(&head)? {
        bail!(
            "the tidied commits wouldn't end with the same files as before, so git-agent left \
             them as they were"
        );
    }
    repo.move_head(&head, &new, "git-agent tidy")?;
    ui::info(ui::green(&format!(
        "\n✅ Tidied {} commit(s) into {}:",
        commits.len(),
        plan.groups.len()
    )));
    for line in repo.recent_commits(plan.groups.len())?.iter().rev() {
        ui::info(format!("   {line}"));
    }
    ui::info(ui::dim(&format!(
        "   To undo: git reset --keep {}",
        &head[..7]
    )));
    Ok(())
}

/// What tidying commits needs.
struct Tidier<'a> {
    repo: &'a Repo,
    server: &'a Server,
    model: &'a Model,
    commits: &'a [tidy::Commit],
    person: &'a dyn Person,
}

impl Tidier<'_> {
    /// Asks the model for a plan, reminding it once of the form if its first
    /// reply can't be read. See [`tidy::parse_plan`] for `autosquash`.
    async fn ask_for_plan(&self, conversation: &[Message], autosquash: bool) -> Result<tidy::Plan> {
        let reply = generate_message(self.server, self.model, conversation).await?;
        let (plan, notes) = match tidy::parse_plan(&reply, self.commits, autosquash) {
            Ok(parsed) => parsed,
            Err(err) => {
                let retry = prompt::retry_plan(
                    conversation,
                    &reply,
                    &format!("{err:#}"),
                    "with an action for every commit",
                );
                let reply = generate_message(self.server, self.model, &retry).await?;
                tidy::parse_plan(&reply, self.commits, autosquash)
                    .context("the model didn't reply with a plan git-agent could read")?
            }
        };
        for note in notes {
            ui::warn(note);
        }
        Ok(plan)
    }

    /// Lets the user review the plan until they accept or quit it. Returns
    /// the accepted plan, or `None` on quit.
    async fn review_plan(
        &self,
        mut plan: tidy::Plan,
        conversation: &[Message],
    ) -> Result<Option<tidy::Plan>> {
        loop {
            ui::show_framed(TIDY_REVIEW.heading, &tidy::show(&plan, self.commits));
            let revised = match review::ask_action(&TIDY_REVIEW, self.person)? {
                Action::Accept(()) => return Ok(Some(plan)),
                Action::Quit => return Ok(None),
                Action::Edit => review::edit_plan(
                    self.repo,
                    "GIT_AGENT_TIDY_PLAN",
                    &tidy::to_text(&plan, self.commits),
                )
                .and_then(|text| tidy::from_text(&text, self.commits)),
                Action::Regenerate => self.ask_for_plan(conversation, true).await,
                Action::Refine(feedback) => {
                    let refined = prompt::refine_plan(conversation, &plan.to_json(), &feedback);
                    self.ask_for_plan(&refined, false).await
                }
            };
            // A failed edit or generation keeps the current plan so the review can go on.
            match revised {
                Ok(revised) => plan = revised,
                Err(err) => ui::error(&err),
            }
        }
    }

    /// Writes a new message for each commit the plan rewords, into a file
    /// whose path goes into `messages`. Returns `false` if the user stopped.
    async fn write_messages(
        &self,
        plan: &tidy::Plan,
        messages: &mut [Option<PathBuf>],
        config: &Config,
        args: &TidyArgs,
    ) -> Result<bool> {
        for (position, group) in plan.groups.iter().enumerate() {
            if !group.reword {
                continue;
            }
            let commit = &self.commits[group.commit];
            ui::info(format!(
                "\n✏️  A new message for {} {}",
                commit.short,
                safe_text::one_line(commit.subject())
            ));
            let mut changes = self.changes(group, &config.exclude)?;
            changes.diff = redact::diff(&changes.diff).text;
            let mut hint = format!(
                "The commit's current message doesn't say what it does:\n{}",
                redact::text(&commit.message, "message").text
            );
            if let Some(extra) = &args.hint {
                hint = format!("{hint}\n{extra}");
            }
            let (conversation, fit) = initial_messages(
                &config.system_prompt,
                &changes,
                Some(&hint),
                self.model.budget(),
            );
            if !fit.is_complete() {
                report_fit(self.model, &fit, "message", None);
            }
            let message = commit_message(self.server, self.model, &conversation).await?;
            review::show_proposal(&TIDY_MESSAGE_REVIEW, &message);
            let (acceptance, message) = if args.apply {
                (TidyMessageAcceptance::UseNew, message)
            } else {
                let revisions = Revisions {
                    server: self.server,
                    model: self.model,
                    conversation: &conversation,
                    edit: |message: &str| review::edit_message(self.repo, message),
                    clean: separated,
                };
                let reviewed =
                    review::revise(&TIDY_MESSAGE_REVIEW, message, revisions, self.person).await?;
                match reviewed {
                    Some(reviewed) => reviewed,
                    None => return Ok(false),
                }
            };
            if acceptance.uses_new() {
                let path = std::path::absolute(
                    self.repo
                        .git_path(&format!("GIT_AGENT_TIDY_MSG_{position}"))?,
                )?;
                fs::write(&path, format!("{message}\n"))
                    .with_context(|| format!("failed to write {}", path.display()))?;
                messages[position] = Some(path);
            }
        }
        Ok(true)
    }

    /// The changes of the group's commit and of the commits folded into it.
    fn changes(&self, group: &tidy::Group, exclude: &[String]) -> Result<Changes> {
        let mut changes = Changes {
            stat: String::new(),
            diff: String::new(),
        };
        for &index in std::iter::once(&group.commit).chain(&group.folded) {
            let hash = &self.commits[index].short;
            let parent = match self.repo.parents(hash)?.into_iter().next() {
                Some(parent) => parent,
                None => self.repo.empty_tree()?,
            };
            let part = self.repo.changes_between(&[&parent, hash], exclude)?;
            changes.stat.push_str(&part.stat);
            changes.diff.push_str(&part.diff);
        }
        Ok(changes)
    }
}

async fn explain_lines(args: &WhyArgs, overrides: &Overrides) -> Result<()> {
    let config = Config::load(overrides)?.config;
    let repo = Repo::open(".")?;
    let target = why::Target::parse(&args.target, |path| Path::new(path).is_file())?;
    if !repo.in_head(&target.path) {
        ui::info(format!(
            "✨ {} isn't committed yet, so git has no history for it.",
            target.path
        ));
        return Ok(());
    }
    let source =
        fs::read(&target.path).with_context(|| format!("failed to read {}", target.path))?;
    let source = String::from_utf8_lossy(&source);
    let target = target.within(source.lines().count())?;
    // Built-in diff drivers let git find functions inside classes and impl blocks.
    let attributes = repo.git_path("GIT_AGENT_ATTRIBUTES")?;
    repo.write_function_attributes(&attributes)?;
    let found = look_up(&repo.with_attributes(attributes.clone()), &target);
    let _ = fs::remove_file(&attributes);
    let Some(Known {
        blamed,
        mut history,
        complete,
    }) = found?
    else {
        ui::info(format!(
            "✨ {} isn't committed yet, so git has no history for it.",
            target.label()
        ));
        return Ok(());
    };
    let lines = (blamed[0].line, blamed[blamed.len() - 1].line);
    let uncommitted: Vec<usize> = blamed
        .iter()
        .filter(|line| !line.is_committed())
        .map(|line| line.line)
        .collect();

    let mut hashes: Vec<&str> = blamed
        .iter()
        .filter(|line| line.is_committed())
        .map(|line| line.hash.as_str())
        .collect();
    hashes.sort_unstable();
    hashes.dedup();
    let summaries = repo.summaries(&hashes)?;
    let mut status = format!("🔍 {}", target.label());
    // The history is newest first.
    if let Some(latest) = history.first() {
        status.push_str(&format!(
            ", last changed in {} ({}, {})",
            latest.short,
            safe_text::one_line(&latest.author),
            latest.date
        ));
    }
    let count = history.len();
    status.push_str(&format!(
        "; {count}{} commit(s) in its history",
        if complete { "" } else { "+" }
    ));
    ui::info(status);
    if !uncommitted.is_empty() {
        ui::info(ui::dim(&format!(
            "   {} of its lines have uncommitted changes; the history is of the committed version.",
            uncommitted.len()
        )));
    }

    let excerpt = redact::text(&why::excerpt(&source, lines.0, lines.1), &target.path);
    let blame = redact::text(&why::blame_summary(&blamed, &summaries), "git blame");
    let mut findings = [excerpt.findings, blame.findings].concat();
    for change in &mut history {
        let message = redact::text(&change.message, &format!("commit {} message", change.short));
        change.message = message.text;
        let patch = redact::diff(&change.patch);
        change.patch = patch.text;
        findings.extend(message.findings);
        findings.extend(patch.findings.into_iter().map(|finding| Finding {
            place: format!("commit {} {}", change.short, finding.place),
            ..finding
        }));
    }
    report_secrets(
        &findings,
        "If they are real, revoke them: they are in the code or its history.",
    );

    let (server, model) = connect(&config).await?;
    let room = model
        .budget()
        .user_message_bytes(prompt::WHY_PROMPT)
        .saturating_sub(question_bytes(args.question.as_deref()));
    let intro = why::intro(&target, lines, &uncommitted, &excerpt.text, &blame.text);
    let described = why::describe(&intro, &history, complete, room);
    let conversation = prompt::why_messages(&described.text, args.question.as_deref());
    report_model(&model, tokens_in(&conversation));
    if described.left_out > 0 || described.cut > 0 {
        let mut parts = Vec::new();
        if described.left_out > 0 {
            parts.push(format!("left out {} older commits", described.left_out));
        }
        if described.cut > 0 {
            parts.push(format!("cut {} diffs short", described.cut));
        }
        ui::info(ui::dim(&format!(
            "   The history is too long for the context, so git-agent {}.",
            parts.join(" and ")
        )));
    }
    discuss(
        &server,
        &model,
        &format!("Why {}", target.label()),
        conversation,
        &Terminal,
    )
    .await
}

/// What git knows about the lines `git agent why` explains.
struct Known {
    /// The commit that last changed each line; at least one line is committed.
    blamed: Vec<why::Blamed>,
    /// The newest commits that changed the lines, newest first.
    history: Vec<why::Change>,
    /// Whether `history` goes back to the commit that added the lines.
    complete: bool,
}

/// Blames the lines of `target` and reads their history, or returns `None`
/// if none of them are committed.
fn look_up(repo: &Repo, target: &why::Target) -> Result<Option<Known>> {
    let mut patterns = target.ranges().into_iter();
    let (range, blame) = loop {
        let Some(range) = patterns.next() else {
            let why::Lines::Function(name) = &target.lines else {
                unreachable!("a range of lines has a pattern");
            };
            bail!(
                "git found no function {name:?} in {}. It finds functions by their first line; \
                 if the language is one git doesn't know, name the lines instead, e.g. {}:120-140",
                target.path,
                target.path
            );
        };
        match repo.blame(&target.path, &range) {
            Ok(blame) => break (range, blame),
            Err(err) if err.to_string().contains("no match") => continue,
            Err(err) => return Err(err),
        }
    };
    let blamed = why::parse_blame(&blame);
    if !blamed.iter().any(why::Blamed::is_committed) {
        return Ok(None);
    }
    // The history is of HEAD's version of the file, whose line numbers differ
    // where the work tree has uncommitted changes.
    let range = match &target.lines {
        why::Lines::Function(_) => range,
        why::Lines::Range(..) => {
            let hunks = why::parse_hunks(&repo.uncommitted_hunks(&target.path)?);
            let head: Vec<usize> = blamed
                .iter()
                .filter_map(|line| why::head_line(&hunks, line.line))
                .collect();
            let (Some(first), Some(last)) = (head.iter().min(), head.iter().max()) else {
                bail!("could not find {} in HEAD", target.label());
            };
            format!("{first},{last}")
        }
    };
    let mut history =
        why::parse_history(&repo.line_history(&target.path, &range, why::HISTORY_LIMIT + 1)?);
    let complete = history.len() <= why::HISTORY_LIMIT;
    history.truncate(why::HISTORY_LIMIT);
    Ok(Some(Known {
        blamed,
        history,
        complete,
    }))
}

async fn explain_commits(args: &ExplainArgs, overrides: &Overrides) -> Result<()> {
    let config = Config::load(overrides)?.config;
    let repo = Repo::open(".")?;
    let subject = explain::subject(&repo, args)?;
    let log: Vec<&str> = subject.log.iter().map(String::as_str).collect();
    let mut commits = repo.commits(&log)?;
    if commits.is_empty() {
        ui::info(format!("✨ {}", subject.nothing));
        return Ok(());
    }
    let diff: Vec<&str> = subject.diff.iter().map(String::as_str).collect();
    let mut changes = repo.changes_between(&diff, &config.exclude)?;
    let summary = changes.stat.lines().last().unwrap_or_default().trim();
    if subject.single {
        let message = commits[0].message.trim_start_matches("- ");
        let title = safe_text::one_line(message.lines().next().unwrap_or_default());
        ui::info(format!("🔍 {} ({title}): {summary}", subject.heading));
    } else {
        ui::info(format!(
            "🔍 {}: {} commit(s), {summary}",
            subject.heading,
            commits.len()
        ));
    }
    if let Some(target) = &args.target
        && !args.branch
        && repo.is_branch(target)
    {
        ui::info(ui::dim(&format!(
            "   {target} is a branch, so this is its last commit; add --branch to explain the \
             whole branch."
        )));
    }

    let redacted = redact::diff(&changes.diff);
    changes.diff = redacted.text;
    let mut findings = redacted.findings;
    for commit in &mut commits {
        let place = format!("commit {} message", commit.hash);
        let redacted = redact::text(&commit.message, &place);
        commit.message = redacted.text;
        findings.extend(redacted.findings);
    }
    report_secrets(
        &findings,
        "They are in the repository's history. If they are real, revoke them.",
    );

    let (server, model) = connect(&config).await?;
    let listed: Vec<String> = commits.iter().map(explain::listed).collect();
    let (conversation, fit) = prompt::explain_messages(
        &subject.about,
        &listed,
        &changes,
        args.question.as_deref(),
        model.budget(),
    );
    let smaller = (!subject.single).then_some("explain fewer commits at a time");
    report_fit(&model, &fit, "explanation", smaller);
    discuss(&server, &model, &subject.heading, conversation, &Terminal).await
}

/// Pushes the branch if GitHub lacks some of its commits, then creates the
/// pull request. With `ask`, the user confirms a rebase before the push.
fn create_pull_request(
    repo: &Repo,
    config: &Config,
    pull_request: &PullRequest,
    ask: bool,
) -> Result<()> {
    const NOT_PUSHED: &str = "the branch was not pushed, so no pull request was created";
    if repo.has_unpushed_commits()? {
        check_outgoing(repo, config).context(NOT_PUSHED)?;
        ui::info(format!(
            "Pushing {} so GitHub has its commits.",
            pull_request.head
        ));
        let pushed = push(repo, ask).context(NOT_PUSHED)?;
        if !pushed {
            ui::info("No pull request created.");
            return Ok(());
        }
    }
    if let Err(err) = github::create_pull_request(repo.workdir(), pull_request) {
        // Like a failed commit, keep the text so it isn't lost.
        let path = repo.git_path("GIT_AGENT_PR.md")?;
        fs::write(&path, format!("{}\n", pull_request.body))
            .with_context(|| format!("failed to write {}", path.display()))?;
        bail!(
            "{err}; the description was saved, create the pull request with:\n  \
             gh pr create --base {} --title {} --body-file {}",
            pull_request.base,
            git::shell_quote(pull_request.title),
            path.display()
        );
    }
    ui::info(ui::green("✅ Pull request created."));
    Ok(())
}

fn commit(
    repo: &Repo,
    message: &str,
    scope: Scope,
    git_args: &[String],
    leave_out: &[String],
) -> Result<()> {
    repo.commit(message, scope, git_args, leave_out)?;
    ui::info(ui::green("✅ Committed."));
    Ok(())
}

/// Pushes, first rebasing onto commits that reached the remote from elsewhere
/// (e.g. edits made on GitHub). With `ask`, the user confirms the rebase.
/// Returns whether it pushed: the user may decline the rebase.
fn push(repo: &Repo, ask: bool) -> Result<bool> {
    const SHOWN: usize = 5;

    let incoming = repo.incoming_commits()?;
    if !incoming.is_empty() {
        ui::info(format!(
            "The remote has {} commit(s) you don't have yet:",
            incoming.len()
        ));
        for commit in incoming.iter().take(SHOWN) {
            ui::info(format!("   {commit}"));
        }
        if incoming.len() > SHOWN {
            ui::info(format!("   … and {} more", incoming.len() - SHOWN));
        }
        if ask && !ui::confirm("Rebase your commits on top of them and push?")? {
            ui::info("Not pushed. When you're ready, run `git pull --rebase` and `git push`.");
            return Ok(false);
        }
        repo.rebase_onto_upstream()?;
        ui::info("🔀 Rebased onto the remote commits.");
    }
    repo.push()?;
    ui::info(ui::green("✅ Pushed."));
    Ok(true)
}

async fn list_models(overrides: &Overrides) -> Result<()> {
    let config = Config::load(overrides)?.config;
    let models = Server::connect(&config)?.models().await?;
    let chosen = match &config.model {
        Some(model) => Some(model.clone()),
        None => models::choose(models.clone()).ok().map(|model| model.id),
    };
    let shown_id = |model: &server::ModelInfo| safe_text::one_line(&model.id);
    let width = models
        .iter()
        .map(|model| shown_id(model).chars().count())
        .max()
        .unwrap_or(0);
    for model in &models {
        let marker = if chosen.as_ref() == Some(&model.id) {
            "*"
        } else {
            " "
        };
        let mut details = Vec::new();
        if model.loaded == Some(true) {
            details.push("loaded".to_owned());
        }
        if let Some(tokens) = model.context_length {
            details.push(format!("{} tokens of context", thousands(tokens)));
        }
        if details.is_empty() {
            ui::out(&format!("{marker} {}", shown_id(model)));
        } else {
            let details = ui::dim(&details.join(", "));
            ui::out(&format!("{marker} {:width$}  {details}", shown_id(model)));
        }
    }

    match (&config.model, chosen) {
        (Some(model), _) if !models.iter().any(|listed| listed.id == *model) => ui::warn(format!(
            "the configured model {model:?} is not in this list"
        )),
        (None, None) => ui::info(
            "No model is set, and git-agent can't tell which one to use: \
             none or several are loaded.",
        ),
        _ => {}
    }
    ui::info(ui::dim(
        "* marks the model git-agent uses: the one set with `git agent config`, --model or \
         GIT_AGENT_MODEL,\n  or else the one loaded on the server.",
    ));
    Ok(())
}

fn init_config(overrides: &Overrides, force: bool) -> Result<()> {
    let path = config::resolve_path(overrides)
        .context("could not find a config directory; set HOME or pass --config")?;
    if path.exists() && !force {
        bail!(
            "{} already exists; use --force to overwrite it",
            path.display()
        );
    }
    let mut config = Config::default();
    config.apply(overrides);
    config.save(&path)?;
    ui::info(format!("Wrote {}", path.display()));
    Ok(())
}

fn show_config(overrides: &Overrides) -> Result<()> {
    let loaded = Config::load(overrides)?;
    let source = match (&loaded.path, loaded.file_found) {
        (Some(path), true) => format!("# Loaded from {}", path.display()),
        (Some(path), false) => format!("# No file at {}; using defaults", path.display()),
        (None, _) => "# No config location found; using defaults".to_owned(),
    };
    ui::plain_out(&format!(
        "{source}\n# Flags and GIT_AGENT_* environment variables are applied."
    ));
    if loaded.config.model.is_none() {
        ui::plain_out("# `model` is not set: git-agent uses the model loaded on the server.");
    }
    if loaded.config.context_length.is_none() {
        ui::plain_out(&format!(
            "# `context_length` is not set: git-agent asks the server, or assumes {} tokens.",
            config::FALLBACK_CONTEXT_LENGTH
        ));
    }
    let flag_key = overrides
        .api_key
        .as_deref()
        .is_some_and(|key| !key.trim().is_empty());
    ui::plain_out(match (flag_key, loaded.config.api_key.is_some()) {
        (true, _) => "# API key: from --api-key or GIT_AGENT_API_KEY.",
        (false, true) => "# API key: saved in the macOS keychain.",
        (false, false) => "# API key: none.",
    });
    let settings_text = toml::to_string_pretty(&loaded.config)?;
    ui::plain_out(&format!("\n{}", settings_text.trim_end()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support::{isolated_repo, run};
    use crate::server::test_support::FakeServer;
    use conversation::test_support::configured;
    use tempfile::TempDir;

    #[test]
    fn each_acceptance_does_what_its_choice_says() {
        assert!(!CommitAcceptance::Commit.pushes());
        assert!(CommitAcceptance::CommitAndPush.pushes());
        assert!(!PullRequestAcceptance::Create.is_draft());
        assert!(PullRequestAcceptance::CreateDraft.is_draft());
        assert!(TidyMessageAcceptance::UseNew.uses_new());
        assert!(!TidyMessageAcceptance::KeepOld.uses_new());
    }

    #[test]
    fn yes_picks_the_plain_acceptance() {
        for typed in ["y", "yes"] {
            assert_eq!(
                review::accepted(&COMMIT_REVIEW, typed),
                Some(CommitAcceptance::Commit),
                "{typed:?}"
            );
            assert_eq!(
                review::accepted(&PULL_REQUEST_REVIEW, typed),
                Some(PullRequestAcceptance::Create),
                "{typed:?}"
            );
            assert_eq!(review::accepted(&TIDY_REVIEW, typed), Some(()), "{typed:?}");
            assert_eq!(
                review::accepted(&TIDY_MESSAGE_REVIEW, typed),
                Some(TidyMessageAcceptance::UseNew),
                "{typed:?}"
            );
            assert_eq!(
                review::accepted(&BRANCH_REVIEW, typed),
                Some(()),
                "{typed:?}"
            );
        }
    }

    #[test]
    fn every_review_can_be_typed() {
        review::test_support::assert_typable(&COMMIT_REVIEW);
        review::test_support::assert_typable(&PULL_REQUEST_REVIEW);
        review::test_support::assert_typable(&TIDY_REVIEW);
        review::test_support::assert_typable(&TIDY_MESSAGE_REVIEW);
        review::test_support::assert_typable(&BRANCH_REVIEW);
    }

    fn generate_args(arguments: &[&str]) -> GenerateArgs {
        let command = std::iter::once("git-agent").chain(arguments.iter().copied());
        cli::try_parse_from(command).unwrap().generate
    }

    /// A repository where `git stash pop` left `f.txt` and `Kjøre plan.txt` in conflict.
    fn conflicted() -> (TempDir, Repo) {
        let (dir, repo) = isolated_repo();
        let write = |name: &str, text: &str| fs::write(dir.path().join(name), text).unwrap();
        write("f.txt", "base\n");
        write("Kjøre plan.txt", "base\n");
        write("other.txt", "base\n");
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "base"]);
        write("f.txt", "stashed\n");
        write("Kjøre plan.txt", "stashed\n");
        run(dir.path(), &["stash", "-q"]);
        write("f.txt", "theirs\n");
        write("Kjøre plan.txt", "theirs\n");
        run(dir.path(), &["commit", "-q", "-a", "-m", "theirs"]);
        let popped = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["stash", "pop", "-q"])
            .output()
            .unwrap();
        assert!(!popped.status.success(), "the stash should conflict");
        (dir, repo)
    }

    #[test]
    fn refuses_conflicts_in_every_mode_and_keeps_them_unresolved() {
        for arguments in [
            &[][..],
            &["--all"],
            &["--commit"],
            &["--all", "--commit"],
            &["--raw"],
            &["--all", "--raw"],
        ] {
            let (dir, repo) = conflicted();
            let head = repo.resolve("HEAD").unwrap();
            let error =
                changes_to_commit(&repo, &generate_args(arguments), &Config::default(), false)
                    .map(|_| ())
                    .unwrap_err();
            let message = format!("{error:#}");
            assert!(
                message.starts_with("2 files have unresolved conflicts"),
                "{arguments:?}: {message}"
            );
            assert!(message.contains("   Kjøre plan.txt\n   f.txt"), "{message}");
            assert_eq!(repo.resolve("HEAD").unwrap(), head, "{arguments:?}");
            assert_eq!(
                repo.unmerged_paths().unwrap(),
                ["Kjøre plan.txt", "f.txt"],
                "{arguments:?}"
            );
            drop(dir);
        }
    }

    #[test]
    fn commits_a_merge_whose_conflicts_are_resolved() {
        let (dir, repo) = conflicted();
        fs::write(dir.path().join("f.txt"), "resolved\n").unwrap();
        fs::write(dir.path().join("Kjøre plan.txt"), "resolved\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        let found = changes_to_commit(&repo, &generate_args(&[]), &Config::default(), false)
            .unwrap()
            .expect("the resolved changes");
        assert_eq!(found.scope, Scope::Staged);
    }

    #[test]
    fn finds_nothing_to_commit_in_a_clean_repository() {
        let (_dir, repo) = isolated_repo();
        let found =
            changes_to_commit(&repo, &generate_args(&[]), &Config::default(), false).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn all_commits_a_hidden_pointer_at_the_commit_the_user_staged() {
        let (dir, repo, _library, moved) = crate::git::test_support::with_submodule();
        run(dir.path(), &["config", "submodule.library.ignore", "all"]);
        let in_head = run(dir.path(), &["rev-parse", "HEAD:library"]);
        crate::git::test_support::move_pointer(dir.path(), &moved);
        run(dir.path(), &["add", "library"]);
        run(
            &dir.path().join("library"),
            &["checkout", "-q", in_head.trim()],
        );
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();

        let found = changes_to_commit(&repo, &generate_args(&["--all"]), &Config::default(), false)
            .unwrap()
            .expect("the change to lib.rs");
        assert_eq!(found.scope, Scope::Staged);
        assert_eq!(run(dir.path(), &["rev-parse", ":library"]).trim(), moved);
    }

    #[test]
    fn leaves_a_hidden_pointer_out_of_a_commit_of_tracked_changes_only() {
        let (dir, repo, _library, moved) = crate::git::test_support::with_submodule();
        run(dir.path(), &["config", "submodule.library.ignore", "all"]);
        crate::git::test_support::move_pointer(dir.path(), &moved);
        fs::write(dir.path().join("lib.rs"), "fn b() {}\n").unwrap();
        let args = generate_args(&[]);
        let tracked = changes_to_commit(&repo, &args, &Config::default(), false)
            .unwrap()
            .unwrap();
        assert_eq!(tracked.scope, Scope::Tracked);
        assert_eq!(tracked.leave_out, ["library"]);

        run(dir.path(), &["add", "lib.rs"]);
        let staged = changes_to_commit(&repo, &args, &Config::default(), false)
            .unwrap()
            .unwrap();
        assert_eq!(staged.scope, Scope::Staged);
        assert!(staged.leave_out.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn commits_exactly_the_text_the_user_was_shown() {
        let (dir, repo) = isolated_repo();
        fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        let server = FakeServer::replying("feat: add\u{1b}[2J a\u{202e}\n\nbody\rtext\u{9b}");
        let conversation = [Message::user("write a message")];

        let message = commit_message(&server, &configured(100_000), &conversation)
            .await
            .unwrap();

        assert_eq!(message, "feat: add[2J a\n\nbodytext");
        commit(&repo, &message, Scope::Staged, &[], &[]).unwrap();
        assert_eq!(
            run(dir.path(), &["log", "-1", "--format=%B"]).trim_end(),
            message
        );
    }
}
