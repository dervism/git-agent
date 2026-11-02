mod branch;
mod cli;
mod config;
mod explain;
mod fit;
mod git;
mod github;
mod menu;
mod models;
mod prompt;
mod providers;
mod redact;
mod secrets;
mod server;
mod split;
mod tidy;
mod ui;
mod why;

use anyhow::{Context, Result, bail};
use clap::Parser;
use cli::{
    BranchArgs, Cli, Command, ConfigCommand, ExplainArgs, GenerateArgs, PrArgs, SettingArgs,
    SplitArgs, TidyArgs, WhyArgs,
};
use config::{Config, OnSecrets};
use fit::Kept;
use git::{Changes, CommitPatch, Repo, Scope};
use github::PullRequest;
use models::{ContextSource, Model, Pick};
use prompt::{Fit, Work};
use redact::{Change, Finding};
use server::{Message, ModelServer, Server};
use split::{FilePatch, Plan, Unit};
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use ui::{Action, Choice, Review, thousands};

/// Accepting a commit message: the value says whether to push afterwards.
const COMMIT_REVIEW: Review<bool> = Review {
    heading: "Proposed commit message",
    first_line: "subject line",
    noun: "message",
    what: "commit message",
    edit: "edit the message in your editor",
    accept: &[
        Choice {
            word: "commit",
            help: "commit with this message",
            value: false,
        },
        Choice {
            word: "push",
            help: "commit with this message, then push",
            value: true,
        },
    ],
    quit: "quit without committing",
};

/// Accepting a pull request: the value says whether it is a draft.
const PULL_REQUEST_REVIEW: Review<bool> = Review {
    heading: "Proposed pull request",
    first_line: "title",
    noun: "pull request",
    what: "pull request title and description",
    edit: "edit the pull request in your editor",
    accept: &[
        Choice {
            word: "create",
            help: "push the branch if needed, then create the pull request",
            value: false,
        },
        Choice {
            word: "draft",
            help: "the same, as a draft pull request",
            value: true,
        },
    ],
    quit: "quit without creating it",
};

/// Accepting the plan of a split.
const PLAN_REVIEW: Review<()> = Review {
    heading: "Proposed commits",
    first_line: "first line",
    noun: "plan",
    what: "plan",
    edit: "edit the plan in your editor",
    accept: &[Choice {
        word: "accept",
        help: "make the commits one by one, reviewing each message",
        value: (),
    }],
    quit: "quit without committing anything",
};

/// Accepting one commit of a split: the value says whether to make it.
const SPLIT_COMMIT_REVIEW: Review<bool> = Review {
    heading: "Proposed commit message",
    first_line: "subject line",
    noun: "message",
    what: "commit message",
    edit: "edit the message in your editor",
    accept: &[
        Choice {
            word: "commit",
            help: "commit these changes with this message",
            value: true,
        },
        Choice {
            word: "skip",
            help: "leave these changes uncommitted and go on to the next commit",
            value: false,
        },
    ],
    quit: "stop here; the commits made so far stay, and the rest stays uncommitted",
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

/// Accepting a new message for a commit being tidied: the value says whether to use it.
const TIDY_MESSAGE_REVIEW: Review<bool> = Review {
    heading: "Proposed commit message",
    first_line: "subject line",
    noun: "message",
    what: "commit message",
    edit: "edit the message in your editor",
    accept: &[
        Choice {
            word: "use",
            help: "give the commit this message",
            value: true,
        },
        Choice {
            word: "keep",
            help: "keep the commit's old message",
            value: false,
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
    let cli = Cli::parse();
    let result = match cli.command {
        None => generate(&cli.generate, &cli.settings).await,
        Some(Command::Split(args)) => split_changes(&args, &cli.settings).await,
        Some(Command::Pr(args)) => pull_request(&args, &cli.settings).await,
        Some(Command::Why(args)) => explain_lines(&args, &cli.settings).await,
        Some(Command::Explain(args)) => explain_commits(&args, &cli.settings).await,
        Some(Command::Models) => list_models(&cli.settings).await,
        Some(Command::Config { command: None }) => menu::run(&cli.settings).await,
        Some(Command::Config {
            command: Some(ConfigCommand::Init { force }),
        }) => init_config(&cli.settings, force),
        Some(Command::Config {
            command: Some(ConfigCommand::Show),
        }) => show_config(&cli.settings),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            ui::error(&err);
            ExitCode::FAILURE
        }
    }
}

async fn generate(args: &GenerateArgs, settings: &SettingArgs) -> Result<()> {
    const NOT_PUSHED: &str = "the commit was made, but not pushed";

    let config = Config::load(settings)?.config;
    let repo = Repo::open(".")?;
    if args.all {
        repo.stage_all()?;
    }

    let Some((scope, mut changes)) = repo.changes(&config.exclude)? else {
        match repo.untracked_count()? {
            0 => ui::info("✨ No changes to commit."),
            n => ui::info(format!(
                "No changes to tracked files, but {n} untracked path(s). Use --all to include them."
            )),
        }
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
    if untracked > 0 {
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
    let (conversation, fit) = prompt::initial_messages(
        &config.system_prompt,
        &changes,
        args.hint.as_deref(),
        model.context_length,
        config.max_output_tokens,
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
        println!("{message}");
        return Ok(());
    }
    ui::show_proposal(&COMMIT_REVIEW, &message);
    if args.commit {
        commit(&repo, &message, scope, &args.git_args)?;
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

    let edit = |message: &str| ui::edit_message(&repo, message);
    let reviewed = review(
        &COMMIT_REVIEW,
        message,
        &server,
        &model,
        &conversation,
        edit,
    )
    .await?;
    let Some((and_push, message)) = reviewed else {
        ui::info("Nothing committed.");
        return Ok(());
    };
    commit(&repo, &message, scope, &args.git_args)?;
    if and_push || args.push {
        check_outgoing(&repo, &config).context(NOT_PUSHED)?;
        push(&repo, true).context(NOT_PUSHED)?;
    }
    Ok(())
}

async fn split_changes(args: &SplitArgs, settings: &SettingArgs) -> Result<()> {
    const NOT_PUSHED: &str = "the commits were made, but not pushed";

    let config = Config::load(settings)?.config;
    let repo = Repo::open(".")?;
    if let Some(operation) = repo.operation_in_progress()? {
        bail!("{operation} is in progress; finish or abort it before splitting changes");
    }
    if args.all {
        repo.stage_all()?;
    }
    let Some((scope, changes)) = repo.changes(&config.exclude)? else {
        ui::info("✨ No changes to split.");
        return Ok(());
    };
    let redacted = redact::diff(&changes.diff);
    if config.on_secrets == OnSecrets::Reject {
        reject_added(
            &redacted.findings,
            "the changes add what look like secrets, so git-agent didn't split them",
            "Take them out of the changes.",
        )?;
    }
    report_secrets(
        &redacted.findings,
        "If they are real, take them out of the files before you commit.",
    );

    let files = split::parse(
        &repo.patch(scope)?,
        &repo.included_paths(scope, &config.exclude)?,
    );
    let (server, model) = connect(&config).await?;
    let room = prompt::room(
        prompt::SPLIT_PROMPT,
        model.context_length,
        config.max_output_tokens,
    );
    let described = split::describe(&files, room)?;
    if described.units.len() < 2 {
        ui::info(
            "✨ The changes are a single hunk, so there is nothing to split; commit them with \
             `git agent`.",
        );
        return Ok(());
    }
    let summary = changes.stat.lines().last().unwrap_or_default().trim();
    let parts = described.units.len();
    match scope {
        Scope::Staged => ui::info(format!("🔍 Staged changes: {summary}, in {parts} parts")),
        Scope::Tracked => ui::info(format!(
            "🔍 Nothing staged; splitting all changes to tracked files: {summary}, in {parts} parts"
        )),
    }
    if described.by_file {
        ui::info(ui::dim(
            "   There are too many changes to show the model one by one, so it groups whole files.",
        ));
    }
    let text = redact::text(&described.text, "changes").text;
    let conversation = prompt::split_messages(&text, args.hint.as_deref());
    report_model(&model, conversation_tokens(&conversation));

    let splitter = Splitter {
        repo: &repo,
        scope,
        config: &config,
        args,
        files: &files,
        units: &described.units,
        server: &server,
        model: &model,
    };
    let mut plan = splitter.ask_for_plan(&conversation).await?;
    if args.commit {
        ui::show_framed(PLAN_REVIEW.heading, &splitter.show(&plan));
    } else if !io::stdin().is_terminal() {
        ui::show_framed(PLAN_REVIEW.heading, &splitter.show(&plan));
        ui::info(ui::dim(
            "Input is not a terminal, so not asking for review. \
             Use --commit to make these commits directly.",
        ));
        return Ok(());
    } else {
        match splitter.review_plan(plan, &conversation).await? {
            Some(reviewed) => plan = reviewed,
            None => {
                ui::info("Nothing committed.");
                return Ok(());
            }
        }
    }

    let outcome = splitter.make_commits(&plan).await.context(
        "the split stopped; the commits made so far stay, and the rest of the changes are as \
         they were",
    )?;
    if outcome.made > 0 {
        ui::info(ui::green(&format!("\n✅ Made {} commit(s):", outcome.made)));
        for line in repo.recent_commits(outcome.made)?.iter().rev() {
            ui::info(format!("   {line}"));
        }
    }
    let left = match scope {
        Scope::Staged => repo.has_staged_changes()?,
        Scope::Tracked => repo.has_uncommitted_changes()?,
    };
    if left {
        let place = match scope {
            Scope::Staged => "staged",
            Scope::Tracked => "uncommitted",
        };
        if outcome.skipped > 0 || outcome.stopped {
            ui::info(format!("The changes you didn't commit are still {place}."));
        } else {
            ui::warn(format!(
                "the commits don't hold all of the changes; the rest are still {place} \
                 (see `git status`)"
            ));
        }
    }
    if args.push && outcome.made > 0 {
        check_outgoing(&repo, &config).context(NOT_PUSHED)?;
        push(&repo, !args.commit).context(NOT_PUSHED)?;
    }
    Ok(())
}

/// What making the commits of a split needs.
struct Splitter<'a> {
    repo: &'a Repo,
    scope: Scope,
    config: &'a Config,
    args: &'a SplitArgs,
    files: &'a [FilePatch],
    units: &'a [Unit],
    server: &'a Server,
    model: &'a Model,
}

/// How a split ended.
#[derive(Default)]
struct Outcome {
    made: usize,
    skipped: usize,
    /// Whether the user stopped before the last commit.
    stopped: bool,
}

impl Splitter<'_> {
    fn show(&self, plan: &Plan) -> String {
        split::show(plan, self.files, self.units)
    }

    /// Asks the model for a plan, reminding it once of the form if its first
    /// reply can't be read.
    async fn ask_for_plan(&self, conversation: &[Message]) -> Result<Plan> {
        let reply = generate_message(self.server, self.model, conversation).await?;
        let (plan, notes) = match split::parse_plan(&reply, self.files, self.units) {
            Ok(parsed) => parsed,
            Err(err) => {
                let retry = prompt::retry_plan(conversation, &reply, &format!("{err:#}"));
                let reply = generate_message(self.server, self.model, &retry).await?;
                split::parse_plan(&reply, self.files, self.units)
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
    async fn review_plan(&self, mut plan: Plan, conversation: &[Message]) -> Result<Option<Plan>> {
        loop {
            ui::show_framed(PLAN_REVIEW.heading, &self.show(&plan));
            let revised = match ui::ask_action(&PLAN_REVIEW)? {
                Action::Accept(()) => return Ok(Some(plan)),
                Action::Quit => return Ok(None),
                Action::Edit => self.edit_plan(&plan),
                Action::Regenerate => self.ask_for_plan(conversation).await.map(Some),
                Action::Refine(feedback) => {
                    let refined = prompt::refine_plan(conversation, &plan.to_json(), &feedback);
                    self.ask_for_plan(&refined).await.map(Some)
                }
            };
            // A failed edit or generation keeps the current plan so the review can go on.
            match revised {
                Ok(Some(revised)) => plan = revised,
                Ok(None) => ui::warn("the edited plan has no commits; keeping the previous one"),
                Err(err) => ui::error(&err),
            }
        }
    }

    fn edit_plan(&self, plan: &Plan) -> Result<Option<Plan>> {
        let text = ui::edit_plan(self.repo, &split::to_text(plan, self.files, self.units))?;
        let (edited, notes) = split::from_text(&text, self.files, self.units);
        for note in notes {
            ui::warn(note);
        }
        Ok((!edited.commits.is_empty()).then_some(edited))
    }

    /// Makes the planned commits one by one, each built in a temporary index
    /// from HEAD and its own changes, so the real index is left alone.
    async fn make_commits(&self, plan: &Plan) -> Result<Outcome> {
        let path = self.repo.git_path("GIT_AGENT_SPLIT_INDEX")?;
        let index = self.repo.with_index(path.clone());
        let outcome = self.commit_each(plan, &index).await;
        let _ = fs::remove_file(&path);
        // Nothing was staged before the split, so the real index stays at HEAD.
        if self.scope == Scope::Tracked {
            self.repo.reset_index()?;
        }
        outcome
    }

    async fn commit_each(&self, plan: &Plan, index: &Repo) -> Result<Outcome> {
        let mut committed = split::Committed::default();
        let mut outcome = Outcome::default();
        let total = plan.commits.len();
        for (number, planned) in (1..).zip(&plan.commits) {
            ui::info(format!(
                "\n📦 Commit {number} of {total}: {}",
                planned.title
            ));
            let files = split::files_of(planned, self.files, self.units);
            ui::info(ui::dim(&format!("   {}", files.join(", "))));
            let patch = split::patch(self.files, self.units, &planned.units, &committed);
            index.read_tree("HEAD")?;
            index
                .apply_to_index(&patch.text)
                .with_context(|| format!("could not stage commit {number} of {total}"))?;
            let Some((_, mut changes)) = index.changes(&self.config.exclude)? else {
                ui::warn("these changes are already committed; skipping them");
                continue;
            };
            changes.diff = redact::diff(&changes.diff).text;
            let mut hint = format!(
                "This commit is one of several that split a larger change. It should: {}",
                planned.title
            );
            if let Some(extra) = &self.args.hint {
                hint = format!("{hint}\n{extra}");
            }
            let (conversation, fit) = prompt::initial_messages(
                &self.config.system_prompt,
                &changes,
                Some(&hint),
                self.model.context_length,
                self.config.max_output_tokens,
            );
            if !fit.is_complete() {
                report_fit(self.model, &fit, "message", None);
            }
            let message = generate_message(self.server, self.model, &conversation).await?;
            ui::show_proposal(&SPLIT_COMMIT_REVIEW, &message);
            let (make, message) = if self.args.commit {
                (true, message)
            } else {
                let edit = |message: &str| ui::edit_message(self.repo, message);
                let reviewed = review(
                    &SPLIT_COMMIT_REVIEW,
                    message,
                    self.server,
                    self.model,
                    &conversation,
                    edit,
                )
                .await?;
                match reviewed {
                    Some(reviewed) => reviewed,
                    None => {
                        outcome.stopped = true;
                        break;
                    }
                }
            };
            if !make {
                ui::info("Skipped; these changes stay uncommitted.");
                outcome.skipped += 1;
                continue;
            }
            commit(index, &message, Scope::Staged, &self.args.git_args)?;
            committed.record(&patch);
            outcome.made += 1;
            if self.scope == Scope::Tracked {
                self.repo.reset_index()?;
            }
        }
        Ok(outcome)
    }
}

async fn pull_request(args: &PrArgs, settings: &SettingArgs) -> Result<()> {
    let config = Config::load(settings)?.config;
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
    let (conversation, fit) = prompt::pull_request_messages(
        &messages,
        &changes,
        args.hint.as_deref(),
        model.context_length,
        config.max_output_tokens,
    );
    report_fit(&model, &fit, "pull request", None);
    let text = generate_message(&server, &model, &conversation).await?;

    if args.raw {
        println!("{text}");
        return Ok(());
    }
    ui::show_proposal(&PULL_REQUEST_REVIEW, &text);
    let (draft, text) = if args.create {
        (false, text)
    } else if !io::stdin().is_terminal() {
        ui::info(ui::dim(
            "Input is not a terminal, so not asking for review. \
             Use --create to create the pull request directly.",
        ));
        return Ok(());
    } else {
        let edit = |text: &str| ui::edit_pull_request(&repo, text);
        let reviewed = review(
            &PULL_REQUEST_REVIEW,
            text,
            &server,
            &model,
            &conversation,
            edit,
        )
        .await?;
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
        draft: draft || args.draft,
        extra_args: &args.gh_args,
    };
    create_pull_request(&repo, &config, &pull_request, !args.create)
}

async fn explain_lines(args: &WhyArgs, settings: &SettingArgs) -> Result<()> {
    let config = Config::load(settings)?.config;
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
            latest.short, latest.author, latest.date
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
    let room = prompt::room(
        prompt::WHY_PROMPT,
        model.context_length,
        config.max_output_tokens,
    )
    .saturating_sub(prompt::question_bytes(args.question.as_deref()));
    let intro = why::intro(&target, lines, &uncommitted, &excerpt.text, &blame.text);
    let described = why::describe(&intro, &history, complete, room);
    let conversation = prompt::why_messages(&described.text, args.question.as_deref());
    report_model(&model, conversation_tokens(&conversation));
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
        &config,
        &format!("Why {}", target.label()),
        conversation,
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

async fn explain_commits(args: &ExplainArgs, settings: &SettingArgs) -> Result<()> {
    let config = Config::load(settings)?.config;
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
        let title = message.lines().next().unwrap_or_default();
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
        model.context_length,
        config.max_output_tokens,
    );
    let smaller = (!subject.single).then_some("explain fewer commits at a time");
    report_fit(&model, &fit, "explanation", smaller);
    discuss(&server, &model, &config, &subject.heading, conversation).await
}

/// Shows the model's answer to `conversation` under `heading`, then answers
/// follow-up questions until the user is done or the context is full.
async fn discuss(
    server: &impl ModelServer,
    model: &Model,
    config: &Config,
    heading: &str,
    mut conversation: Vec<Message>,
) -> Result<()> {
    let mut answer = generate_message(server, model, &conversation).await?;
    ui::show_answer(heading, &answer);
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Ok(());
    }
    while let Some(question) = ui::ask_follow_up()? {
        let next = prompt::follow_up(&conversation, &answer, &question);
        if !prompt::has_room(&next, model.context_length, config.max_output_tokens) {
            ui::info(ui::dim(
                "The conversation no longer fits in the model's context; ask your question in a \
                 new run.",
            ));
            return Ok(());
        }
        // A failed reply keeps the conversation as it was, so the user can ask again.
        match generate_message(server, model, &next).await {
            Ok(reply) => {
                conversation = next;
                answer = reply;
                ui::show_answer(heading, &answer);
            }
            Err(err) => ui::error(&err),
        }
    }
    Ok(())
}

fn conversation_tokens(conversation: &[Message]) -> usize {
    conversation
        .iter()
        .map(|message| prompt::estimate_tokens(&message.content))
        .sum()
}

/// Connects to the configured server and picks the model to use.
async fn connect(config: &Config) -> Result<(Server, Model)> {
    let server = Server::connect(config)?;
    if config.temperature.is_some() && !server.takes_temperature() {
        ui::warn("Anthropic's API takes no temperature, so git-agent doesn't send the one you set");
    }
    let model = models::resolve(&server, config).await?;
    Ok((server, model))
}

/// Lets the user review `text` until they accept or quit it, revising it as
/// they ask. Returns the accept choice and the final text, or `None` on quit.
async fn review<T: Copy>(
    review: &Review<T>,
    mut text: String,
    server: &impl ModelServer,
    model: &Model,
    conversation: &[Message],
    edit: impl Fn(&str) -> Result<Option<String>>,
) -> Result<Option<(T, String)>> {
    loop {
        let revised = match ui::ask_action(review)? {
            Action::Accept(choice) => return Ok(Some((choice, text))),
            Action::Quit => return Ok(None),
            Action::Edit => edit(&text),
            Action::Regenerate => generate_message(server, model, conversation)
                .await
                .map(Some),
            Action::Refine(feedback) => {
                let refined = prompt::refine_messages(conversation, &text, &feedback, review.what);
                generate_message(server, model, &refined).await.map(Some)
            }
        };
        // A failed edit or generation keeps the current text so the review can go on.
        match revised {
            Ok(Some(revised)) => text = revised,
            Ok(None) => ui::warn(format!(
                "the edited {} was empty; keeping the previous one",
                review.noun
            )),
            Err(err) => ui::error(&err),
        }
        ui::show_proposal(review, &text);
    }
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
            shell_quote(pull_request.title),
            path.display()
        );
    }
    ui::info(ui::green("✅ Pull request created."));
    Ok(())
}

/// Lists the secrets that were replaced before sending, then gives `advice`.
fn report_secrets(findings: &[Finding], advice: &str) {
    if findings.is_empty() {
        return;
    }
    ui::warn(format!(
        "found {} possible secret(s); the model gets {} in their place:",
        findings.len(),
        redact::REDACTED
    ));
    ui::info(list_findings(&findings.iter().collect::<Vec<_>>()));
    ui::info(ui::dim(&format!("   {advice}")));
}

/// Fails if the change adds any of the secrets in `findings`, listing them
/// after `what` happened, then saying how to `fix` it.
fn reject_added(findings: &[Finding], what: &str, fix: &str) -> Result<()> {
    let added = added(findings);
    if added.is_empty() {
        return Ok(());
    }
    bail!(
        "{what}:\n{}\n{fix} If they aren't real secrets, add --on-secrets warn, or choose Warn \
         under Secrets in `git agent config`.",
        list_findings(&added)
    );
}

/// Checks the commits a push would send. If one adds what looks like a
/// secret, fails when `on_secrets` is reject and warns otherwise.
fn check_outgoing(repo: &Repo, config: &Config) -> Result<()> {
    let findings = scan_commits(&repo.outgoing_patches(&config.exclude)?);
    match config.on_secrets {
        OnSecrets::Reject => reject_added(
            &findings,
            "commits the push would send add what look like secrets",
            "Remove them from those commits, for example with `git rebase -i`.",
        ),
        OnSecrets::Warn => {
            let added = added(&findings);
            if !added.is_empty() {
                ui::warn(format!(
                    "pushing commits that add what look like secrets:\n{}",
                    list_findings(&added)
                ));
            }
            Ok(())
        }
    }
}

fn scan_commits(commits: &[CommitPatch]) -> Vec<Finding> {
    commits
        .iter()
        .flat_map(|commit| redact::commit(&commit.hash, &commit.message, &commit.patch))
        .collect()
}

/// The findings the change adds, rather than leaves alone or removes.
fn added(findings: &[Finding]) -> Vec<&Finding> {
    findings
        .iter()
        .filter(|finding| finding.change == Change::Added)
        .collect()
}

/// The first few findings, one per line.
fn list_findings(findings: &[&Finding]) -> String {
    const SHOWN: usize = 5;
    let mut lines: Vec<String> = findings
        .iter()
        .take(SHOWN)
        .map(|finding| {
            let place = match finding.line {
                Some(line) => format!("{}:{line}", finding.place),
                None => finding.place.clone(),
            };
            let change = match finding.change {
                Change::Added => "",
                Change::Unchanged => ", on an unchanged line",
                Change::Removed => ", on a removed line",
            };
            format!("   {place}  {}{change}", finding.kind)
        })
        .collect();
    if findings.len() > SHOWN {
        lines.push(format!("   … and {} more", findings.len() - SHOWN));
    }
    lines.join("\n")
}

/// Says which model gets the changes and, if they didn't fit its context, what
/// was left out. `noun` names what the model writes; `smaller` says how to
/// send less at a time, if there is a way.
fn report_fit(model: &Model, fit: &Fit, noun: &str, smaller: Option<&str>) {
    report_model(model, fit.prompt_tokens);
    if fit.is_complete() {
        if fit.context_trimmed {
            ui::info(ui::dim(
                "   To fit, git-agent left out the unchanged lines around the changes; \
                 every changed line was sent.",
            ));
        }
        return;
    }
    report_shortened(model, fit, noun, smaller);
}

/// Says which model gets the prompt, and how much of its context the prompt takes.
fn report_model(model: &Model, prompt_tokens: usize) {
    let chosen = match model.picked {
        Some(Pick::Loaded) => " (loaded on the server)",
        Some(Pick::Only) => " (the server's only model)",
        None => "",
    };
    ui::info(format!(
        "🤖 {}{chosen} {}",
        model.id,
        ui::dim(&format!(
            "· prompt about {} of {} context tokens",
            thousands(prompt_tokens),
            thousands(model.context_length)
        ))
    ));
    if model.context_source == ContextSource::Assumed {
        ui::info(ui::dim(&format!(
            "   The server didn't report the model's context length, so git-agent assumes {} \
             tokens.\n   If the model has more, set `context_length` in the config file or pass \
             --context-length.",
            thousands(model.context_length)
        )));
    }
}

/// Lists what was left out of changes that didn't fit, and how to send more.
fn report_shortened(model: &Model, fit: &Fit, noun: &str, smaller: Option<&str>) {
    ui::warn(format!(
        "the changes need about {} tokens, more than fit in the context, so git-agent sent \
         part of them:",
        thousands(fit.full_tokens)
    ));
    let whole = fit.files.iter().filter(|file| file.is_whole()).count();
    let cut: Vec<String> = fit
        .files
        .iter()
        .filter(|file| file.kept == Kept::Start)
        .map(|file| {
            format!(
                "{} ({} of {} changed lines)",
                file.path, file.kept_changed_lines, file.changed_lines
            )
        })
        .collect();
    let dropped: Vec<String> = fit
        .files
        .iter()
        .filter(|file| file.is_dropped())
        .map(|file| file.path.clone())
        .collect();
    ui::info(format!(
        "   {whole} of {} files in full, without the unchanged lines around the changes",
        fit.files.len()
    ));
    if !cut.is_empty() {
        ui::info(format!("   {} cut short: {}", cut.len(), first_few(&cut)));
    }
    if !dropped.is_empty() {
        ui::info(format!(
            "   {} by name only: {}",
            dropped.len(),
            first_few(&dropped)
        ));
    }
    if fit.list_lines_left_out > 0 {
        ui::info(format!(
            "   {} files left out of the file list too",
            fit.list_lines_left_out
        ));
    }
    if fit.commit_bodies_left_out {
        ui::info("   the commits by their subjects only, without the rest of their messages");
    }
    if fit.commits_left_out > 0 {
        ui::info(format!(
            "   {} commits left out of the commit list",
            fit.commits_left_out
        ));
    }
    let larger_context = match model.context_source {
        ContextSource::Server => "load the model with a larger context length",
        ContextSource::Config => "raise `context_length` if the model allows more",
        ContextSource::Assumed => "set `context_length` if the model has more",
    };
    let ways = match smaller {
        Some(smaller) => format!("{smaller}, or {larger_context}"),
        None => larger_context.to_owned(),
    };
    ui::info(ui::dim(&format!(
        "   The {noun} may miss details from these files. To send more, {ways}."
    )));
}

/// The first three items, then how many more there are.
fn first_few(items: &[String]) -> String {
    const SHOWN: usize = 3;
    let mut text = items[..items.len().min(SHOWN)].join(", ");
    if items.len() > SHOWN {
        text.push_str(&format!(", and {} more", items.len() - SHOWN));
    }
    text
}

async fn generate_message(
    server: &impl ModelServer,
    model: &Model,
    conversation: &[Message],
) -> Result<String> {
    let reply = {
        let _spinner = ui::Spinner::start(format!("Generating with {}", model.id));
        server.complete(&model.id, conversation).await?
    };
    let message = prompt::clean_response(&reply);
    if message.is_empty() {
        bail!("the model returned an empty message");
    }
    Ok(message)
}

fn commit(repo: &Repo, message: &str, scope: Scope, git_args: &[String]) -> Result<()> {
    repo.commit(message, scope, git_args)?;
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

async fn list_models(settings: &SettingArgs) -> Result<()> {
    let config = Config::load(settings)?.config;
    let models = Server::connect(&config)?.models().await?;
    let chosen = match &config.model {
        Some(model) => Some(model.clone()),
        None => models::choose(models.clone()).ok().map(|model| model.id),
    };
    let width = models.iter().map(|model| model.id.len()).max().unwrap_or(0);
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
            println!("{marker} {}", model.id);
        } else {
            let details = ui::dim(&details.join(", "));
            println!("{marker} {:width$}  {details}", model.id);
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

fn init_config(settings: &SettingArgs, force: bool) -> Result<()> {
    let path = config::resolve_path(settings)
        .context("could not find a config directory; set HOME or pass --config")?;
    if path.exists() && !force {
        bail!(
            "{} already exists; use --force to overwrite it",
            path.display()
        );
    }
    let mut config = Config::default();
    config.apply(settings);
    config.save(&path)?;
    ui::info(format!("Wrote {}", path.display()));
    Ok(())
}

fn show_config(settings: &SettingArgs) -> Result<()> {
    let loaded = Config::load(settings)?;
    let source = match (&loaded.path, loaded.file_found) {
        (Some(path), true) => format!("# Loaded from {}", path.display()),
        (Some(path), false) => format!("# No file at {}; using defaults", path.display()),
        (None, _) => "# No config location found; using defaults".to_owned(),
    };
    println!("{source}\n# Flags and GIT_AGENT_* environment variables are applied.");
    if loaded.config.model.is_none() {
        println!("# `model` is not set: git-agent uses the model loaded on the server.");
    }
    if loaded.config.context_length.is_none() {
        println!(
            "# `context_length` is not set: git-agent asks the server, or assumes {} tokens.",
            models::FALLBACK_CONTEXT_LENGTH
        );
    }
    let flag_key = settings
        .api_key
        .as_deref()
        .is_some_and(|key| !key.trim().is_empty());
    match (flag_key, loaded.config.api_key.is_some()) {
        (true, _) => println!("# API key: from --api-key or GIT_AGENT_API_KEY."),
        (false, true) => println!("# API key: saved in the macOS keychain."),
        (false, false) => println!("# API key: none."),
    }
    print!("\n{}", toml::to_string_pretty(&loaded.config)?);
    Ok(())
}
