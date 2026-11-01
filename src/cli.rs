//! Command-line interface.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// Write git commit messages from your changes with a local LLM.
///
/// git-agent describes the staged changes (or, if nothing is staged, all
/// changes to tracked files), shows the proposed message, and lets you commit
/// it, edit it, regenerate it, or ask the model to revise it.
#[derive(Parser, Debug)]
#[command(name = "git-agent", version, args_conflicts_with_subcommands = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub generate: GenerateArgs,

    #[command(flatten)]
    pub settings: SettingArgs,
}

#[derive(Args, Debug)]
pub struct GenerateArgs {
    /// Commit with the generated message without reviewing it
    #[arg(short, long, conflicts_with = "raw")]
    pub commit: bool,

    /// Push after committing; a branch's first push also sets its upstream
    #[arg(short, long, conflicts_with = "raw")]
    pub push: bool,

    /// Print only the message to stdout and exit (for scripts and pipes)
    #[arg(short, long)]
    pub raw: bool,

    /// Stage all changes, including untracked files, before generating
    #[arg(short, long)]
    pub all: bool,

    /// Extra context for the model, e.g. why the change was made
    #[arg(short = 'H', long, value_name = "TEXT")]
    pub hint: Option<String>,

    /// Extra arguments for `git commit`, e.g. `-- --no-verify --signoff`
    #[arg(last = true, value_name = "GIT_COMMIT_ARGS")]
    pub git_args: Vec<String>,
}

/// Overrides for the config file. Each can also be set through its environment variable.
#[derive(Args, Debug, Default)]
pub struct SettingArgs {
    /// Config file [default: ~/.config/git-agent/config.toml]
    #[arg(long, global = true, env = "GIT_AGENT_CONFIG", value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Model ID (see `git-agent models`) [default: the model loaded on the server]
    #[arg(short, long, global = true, env = "GIT_AGENT_MODEL")]
    pub model: Option<String>,

    /// The model's context window in tokens [default: as reported by the server]
    #[arg(
        long,
        global = true,
        env = "GIT_AGENT_CONTEXT_LENGTH",
        value_name = "TOKENS"
    )]
    pub context_length: Option<usize>,

    /// Most tokens the model may write in one reply, thinking included (sent to Anthropic)
    #[arg(
        long,
        global = true,
        env = "GIT_AGENT_MAX_OUTPUT_TOKENS",
        value_name = "TOKENS"
    )]
    pub max_output_tokens: Option<usize>,

    /// Base URL of an OpenAI-compatible API, e.g. http://localhost:11434/v1
    #[arg(long, global = true, env = "GIT_AGENT_BASE_URL", value_name = "URL")]
    pub base_url: Option<String>,

    /// API key sent as a Bearer token (local servers usually need none)
    #[arg(long, global = true, env = "GIT_AGENT_API_KEY", hide_env_values = true)]
    pub api_key: Option<String>,

    /// Sampling temperature, 0.0 to 2.0
    #[arg(short, long, global = true)]
    pub temperature: Option<f32>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List the models available on the configured server
    Models,
    /// Change the settings in an interactive menu, or create or inspect the config file
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommand>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Write a config file with the current settings and explanatory comments
    Init {
        /// Overwrite an existing file
        #[arg(long)]
        force: bool,
    },
    /// Print the effective settings and where they came from
    Show,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn passes_trailing_args_to_git_commit() {
        let cli = Cli::try_parse_from(["git-agent", "-c", "--", "--no-verify", "-s"]).unwrap();
        assert!(cli.generate.commit);
        assert_eq!(cli.generate.git_args, ["--no-verify", "-s"]);
    }

    #[test]
    fn push_combines_with_commit_but_not_raw() {
        let cli = Cli::try_parse_from(["git-agent", "-cp"]).unwrap();
        assert!(cli.generate.commit && cli.generate.push);
        assert!(Cli::try_parse_from(["git-agent", "--push", "--raw"]).is_err());
    }

    #[test]
    fn setting_flags_work_with_subcommands() {
        let cli = Cli::try_parse_from(["git-agent", "models", "--model", "qwen3"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Models)));
        assert_eq!(cli.settings.model.as_deref(), Some("qwen3"));
        let cli = Cli::try_parse_from(["git-agent", "--context-length", "8192"]).unwrap();
        assert_eq!(cli.settings.context_length, Some(8192));
    }

    #[test]
    fn config_opens_the_menu_without_a_subcommand() {
        let cli = Cli::try_parse_from(["git-agent", "config"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Config { command: None })
        ));
        let cli = Cli::try_parse_from(["git-agent", "config", "show"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                command: Some(ConfigCommand::Show)
            })
        ));
    }
}
