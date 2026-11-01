mod cli;
mod config;
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
mod ui;

use anyhow::{Context, Result, bail};
use clap::Parser;
use cli::{Cli, Command, ConfigCommand, GenerateArgs, PrArgs, SettingArgs};
use config::{Config, OnSecrets};
use fit::Kept;
use git::{CommitPatch, Repo, Scope};
use github::PullRequest;
use models::{ContextSource, Model, Pick};
use prompt::Fit;
use redact::{Change, Finding};
use server::{Message, ModelServer, Server};
use std::fs;
use std::io::{self, IsTerminal};
use std::process::ExitCode;
use ui::{Action, Choice, Review, thousands};

/// Accepting a commit message: the value says whether to push afterwards.
const COMMIT_REVIEW: Review<bool> = Review {
    heading: "Proposed commit message",
    first_line: "subject line",
    noun: "message",
    what: "commit message",
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        None => generate(&cli.generate, &cli.settings).await,
        Some(Command::Pr(args)) => pull_request(&args, &cli.settings).await,
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
        Some("commit in smaller parts\n   (stage some files, then run git agent)"),
    );
    let message = generate_message(&server, &model, &conversation).await?;

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

/// `text` quoted for a POSIX shell.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
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
            thousands(fit.prompt_tokens),
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
    if fit.is_complete() {
        if fit.context_trimmed {
            ui::info(ui::dim(
                "   To fit, git-agent left out the unchanged lines around the changes; \
                 every changed line was sent.",
            ));
        }
        return;
    }

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
