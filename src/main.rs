mod cli;
mod config;
mod fit;
mod git;
mod menu;
mod models;
mod prompt;
mod providers;
mod secrets;
mod server;
mod ui;

use anyhow::{Context, Result, bail};
use clap::Parser;
use cli::{Cli, Command, ConfigCommand, GenerateArgs, SettingArgs};
use config::Config;
use fit::Kept;
use git::{Repo, Scope};
use models::{ContextSource, Model, Pick};
use prompt::Fit;
use server::{Message, ModelServer, Server};
use std::io::{self, IsTerminal};
use std::process::ExitCode;
use ui::{Action, thousands};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        None => generate(&cli.generate, &cli.settings).await,
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
    let config = Config::load(settings)?.config;
    let repo = Repo::open(".")?;
    if args.all {
        repo.stage_all()?;
    }

    let Some(changes) = repo.changes(&config.exclude)? else {
        match repo.untracked_count()? {
            0 => ui::info("✨ No changes to commit."),
            n => ui::info(format!(
                "No changes to tracked files, but {n} untracked path(s). Use --all to include them."
            )),
        }
        return Ok(());
    };

    let summary = changes.stat.lines().last().unwrap_or_default().trim();
    match changes.scope {
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

    let server = Server::connect(&config)?;
    if config.temperature.is_some() && !server.takes_temperature() {
        ui::warn("Anthropic's API takes no temperature, so git-agent doesn't send the one you set");
    }
    let model = models::resolve(&server, &config).await?;
    let (conversation, fit) = prompt::initial_messages(
        &config.system_prompt,
        &changes,
        args.hint.as_deref(),
        model.context_length,
        config.max_output_tokens,
    );
    report_fit(&model, &fit);
    let mut message = generate_message(&server, &model, &conversation).await?;

    if args.raw {
        println!("{message}");
        return Ok(());
    }
    ui::show_message(&message);
    if args.commit {
        commit(&repo, &message, changes.scope, &args.git_args)?;
        return if args.push {
            push(&repo, false)
        } else {
            Ok(())
        };
    }
    if !io::stdin().is_terminal() {
        ui::info(ui::dim(
            "Input is not a terminal, so not asking for review. \
             Use --commit to commit directly (add --push to also push).",
        ));
        return Ok(());
    }

    loop {
        let revised = match ui::ask_action()? {
            Action::Commit { push: and_push } => {
                commit(&repo, &message, changes.scope, &args.git_args)?;
                return if and_push || args.push {
                    push(&repo, true)
                } else {
                    Ok(())
                };
            }
            Action::Quit => {
                ui::info("Nothing committed.");
                return Ok(());
            }
            Action::Edit => ui::edit_message(&repo, &message),
            Action::Regenerate => generate_message(&server, &model, &conversation)
                .await
                .map(Some),
            Action::Refine(feedback) => {
                let refined = prompt::refine_messages(&conversation, &message, &feedback);
                generate_message(&server, &model, &refined).await.map(Some)
            }
        };
        // A failed edit or generation keeps the current message so the review can go on.
        match revised {
            Ok(Some(revised)) => message = revised,
            Ok(None) => ui::warn("the edited message was empty; keeping the previous one"),
            Err(err) => ui::error(&err),
        }
        ui::show_message(&message);
    }
}

/// Says which model gets the changes and, if they didn't fit its context, what was left out.
fn report_fit(model: &Model, fit: &Fit) {
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
    let larger_context = match model.context_source {
        ContextSource::Server => "load the model with a larger context length",
        ContextSource::Config => "raise `context_length` if the model allows more",
        ContextSource::Assumed => "set `context_length` if the model has more",
    };
    ui::info(ui::dim(&format!(
        "   The message may miss details from these files. To send more, commit in smaller \
         parts\n   (stage some files, then run git agent), or {larger_context}."
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
fn push(repo: &Repo, ask: bool) -> Result<()> {
    const NOT_PUSHED: &str = "the commit was made, but not pushed";
    const SHOWN: usize = 5;

    let incoming = repo.incoming_commits().context(NOT_PUSHED)?;
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
        if ask && !ui::confirm("Rebase your commit on top of them and push?")? {
            ui::info("Not pushed. When you're ready, run `git pull --rebase` and `git push`.");
            return Ok(());
        }
        repo.rebase_onto_upstream().context(NOT_PUSHED)?;
        ui::info("🔀 Rebased onto the remote commits.");
    }
    repo.push().context(NOT_PUSHED)?;
    ui::info(ui::green("✅ Pushed."));
    Ok(())
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
