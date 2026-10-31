mod cli;
mod config;
mod git;
mod llm;
mod prompt;
mod ui;

use anyhow::{Context, Result, bail};
use clap::Parser;
use cli::{Cli, Command, ConfigCommand, GenerateArgs, SettingArgs};
use config::Config;
use git::{Repo, Scope};
use std::fs;
use std::io::{self, IsTerminal};
use std::process::ExitCode;
use ui::Action;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        None => generate(&cli.generate, &cli.settings).await,
        Some(Command::Models) => list_models(&cli.settings).await,
        Some(Command::Config(ConfigCommand::Init { force })) => init_config(&cli.settings, force),
        Some(Command::Config(ConfigCommand::Show)) => show_config(&cli.settings),
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

    let client = llm::Client::new(&config)?;
    let conversation = prompt::initial_messages(
        &config.system_prompt,
        &changes,
        args.hint.as_deref(),
        config.max_diff_chars,
    );
    let mut message = generate_message(&client, &conversation).await?;

    if args.raw {
        println!("{message}");
        return Ok(());
    }
    ui::show_message(&message);
    if args.commit {
        return commit(&repo, &message, changes.scope, &args.git_args);
    }
    if !io::stdin().is_terminal() {
        ui::info(ui::dim(
            "Input is not a terminal, so not asking for review. Use --commit to commit directly.",
        ));
        return Ok(());
    }

    loop {
        let revised = match ui::ask_action()? {
            Action::Commit => return commit(&repo, &message, changes.scope, &args.git_args),
            Action::Quit => {
                ui::info("Nothing committed.");
                return Ok(());
            }
            Action::Edit => ui::edit_message(&repo, &message),
            Action::Regenerate => generate_message(&client, &conversation).await.map(Some),
            Action::Refine(feedback) => {
                let refined = prompt::refine_messages(&conversation, &message, &feedback);
                generate_message(&client, &refined).await.map(Some)
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

async fn generate_message(client: &llm::Client, conversation: &[llm::Message]) -> Result<String> {
    let reply = {
        let _spinner = ui::Spinner::start(format!("Generating with {}", client.model()));
        client.complete(conversation).await?
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

async fn list_models(settings: &SettingArgs) -> Result<()> {
    let config = Config::load(settings)?.config;
    let models = llm::Client::new(&config)?.models().await?;
    for model in &models {
        let marker = if *model == config.model { "*" } else { " " };
        println!("{marker} {model}");
    }
    if !models.contains(&config.model) {
        ui::warn(format!(
            "the configured model {:?} is not in this list",
            config.model
        ));
    }
    ui::info(ui::dim(
        "Choose with --model, GIT_AGENT_MODEL, or `model` in the config file (git-agent config init).",
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
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }
    fs::write(&path, config::template(&config))
        .with_context(|| format!("failed to write {}", path.display()))?;
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
    println!("{source}\n# Flags and GIT_AGENT_* environment variables are applied.\n");
    print!("{}", toml::to_string_pretty(&loaded.config.redacted())?);
    Ok(())
}
