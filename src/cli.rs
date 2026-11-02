//! Command-line interface.

use crate::config::OnSecrets;
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

    /// What to do when changes add what look like secrets [default: reject]
    #[arg(
        long,
        global = true,
        env = "GIT_AGENT_ON_SECRETS",
        value_name = "ACTION",
        value_enum
    )]
    pub on_secrets: Option<OnSecrets>,
}

/// Options for `git agent pr`.
#[derive(Args, Debug)]
pub struct PrArgs {
    /// Branch the pull request merges into [default: the repository's default branch]
    #[arg(short, long, value_name = "BRANCH")]
    pub base: Option<String>,

    /// Create the pull request as a draft
    #[arg(short, long, conflicts_with = "raw")]
    pub draft: bool,

    /// Create the pull request without reviewing it
    #[arg(short, long, conflicts_with = "raw")]
    pub create: bool,

    /// Print only the title and description to stdout and exit (for scripts and pipes)
    #[arg(short, long)]
    pub raw: bool,

    /// Extra context for the model, e.g. which issue the branch fixes
    #[arg(short = 'H', long, value_name = "TEXT")]
    pub hint: Option<String>,

    /// Extra arguments for `gh pr create`, e.g. `-- --label bug`
    #[arg(last = true, value_name = "GH_ARGS")]
    pub gh_args: Vec<String>,
}

/// Options for `git agent split`.
#[derive(Args, Debug)]
pub struct SplitArgs {
    /// Stage all changes, including untracked files, before splitting them
    #[arg(short, long)]
    pub all: bool,

    /// Make the planned commits without reviewing the plan or the messages
    #[arg(short, long)]
    pub commit: bool,

    /// Push after the last commit
    #[arg(short, long)]
    pub push: bool,

    /// Extra context for the model, e.g. what the changes are for
    #[arg(short = 'H', long, value_name = "TEXT")]
    pub hint: Option<String>,

    /// Extra arguments for each `git commit`, e.g. `-- --signoff`
    #[arg(last = true, value_name = "GIT_COMMIT_ARGS")]
    pub git_args: Vec<String>,
}

/// Options for `git agent why`.
#[derive(Args, Debug)]
pub struct WhyArgs {
    /// The lines to explain: FILE:LINE, FILE:START-END or FILE:FUNCTION
    #[arg(value_name = "FILE:LINE")]
    pub target: String,

    /// A question about the lines, e.g. "why 400 bytes?"
    #[arg(value_name = "QUESTION")]
    pub question: Option<String>,
}

/// Options for `git agent explain`.
#[derive(Args, Debug)]
pub struct ExplainArgs {
    /// A commit (e.g. HEAD~2) or a range (e.g. v1.0..v1.1); with --branch, a branch
    /// [default: HEAD, or with --branch the current branch]
    #[arg(value_name = "COMMIT")]
    pub target: Option<String>,

    /// Explain a branch: its commits since it left the default branch
    #[arg(short, long)]
    pub branch: bool,

    /// The branch --branch compares with [default: the remote's default branch]
    #[arg(long, value_name = "BRANCH", requires = "branch")]
    pub base: Option<String>,

    /// Explain what the last `git pull` brought into the current branch
    #[arg(long, conflicts_with_all = ["target", "branch"])]
    pub pull: bool,

    /// A question about the commits, e.g. "does anything break?"
    #[arg(short, long, value_name = "TEXT")]
    pub question: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Split the changes into several commits, grouped by the model
    Split(SplitArgs),
    /// Write a pull request for the current branch, then create it on GitHub with gh
    Pr(PrArgs),
    /// Explain why lines of code are the way they are, from their history
    Why(WhyArgs),
    /// Explain a commit, a range of commits, a branch or the last pull
    Explain(ExplainArgs),
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
        let cli = Cli::try_parse_from(["git-agent", "pr", "--on-secrets", "warn"]).unwrap();
        assert_eq!(cli.settings.on_secrets, Some(OnSecrets::Warn));
        assert!(Cli::try_parse_from(["git-agent", "--on-secrets", "ignore"]).is_err());
    }

    #[test]
    fn pr_takes_its_own_options_and_settings() {
        let cli = Cli::try_parse_from([
            "git-agent",
            "pr",
            "--base",
            "develop",
            "-d",
            "-H",
            "fixes #3",
            "-m",
            "qwen3",
            "--",
            "--label",
            "bug",
        ])
        .unwrap();
        let Some(Command::Pr(args)) = cli.command else {
            panic!("expected the pr command");
        };
        assert_eq!(args.base.as_deref(), Some("develop"));
        assert!(args.draft && !args.create);
        assert_eq!(args.hint.as_deref(), Some("fixes #3"));
        assert_eq!(args.gh_args, ["--label", "bug"]);
        assert_eq!(cli.settings.model.as_deref(), Some("qwen3"));
        assert!(Cli::try_parse_from(["git-agent", "pr", "--raw", "--create"]).is_err());
        // The commit options belong to plain `git agent`.
        assert!(Cli::try_parse_from(["git-agent", "--all", "pr"]).is_err());
    }

    #[test]
    fn split_takes_its_own_options() {
        let cli =
            Cli::try_parse_from(["git-agent", "split", "-a", "-c", "--", "--signoff"]).unwrap();
        let Some(Command::Split(args)) = cli.command else {
            panic!("expected the split command");
        };
        assert!(args.all && args.commit && !args.push);
        assert_eq!(args.git_args, ["--signoff"]);
    }

    #[test]
    fn why_takes_lines_and_a_question() {
        let cli = Cli::try_parse_from(["git-agent", "why", "src/fit.rs:120", "why 400?"]).unwrap();
        let Some(Command::Why(args)) = cli.command else {
            panic!("expected the why command");
        };
        assert_eq!(args.target, "src/fit.rs:120");
        assert_eq!(args.question.as_deref(), Some("why 400?"));
        assert!(Cli::try_parse_from(["git-agent", "why"]).is_err());
    }

    #[test]
    fn explain_takes_a_commit_a_branch_or_the_last_pull() {
        let parse = |args: &[&str]| {
            let cli = Cli::try_parse_from([&["git-agent", "explain"], args].concat())?;
            match cli.command {
                Some(Command::Explain(args)) => Ok::<_, clap::Error>(args),
                _ => panic!("expected the explain command"),
            }
        };
        let args = parse(&[]).unwrap();
        assert!(args.target.is_none() && !args.branch && !args.pull);
        let args = parse(&["v1.0..v1.1", "-q", "breaking?"]).unwrap();
        assert_eq!(args.target.as_deref(), Some("v1.0..v1.1"));
        assert_eq!(args.question.as_deref(), Some("breaking?"));
        let args = parse(&["--branch", "feature", "--base", "develop"]).unwrap();
        assert!(args.branch);
        assert_eq!(args.base.as_deref(), Some("develop"));
        assert!(parse(&["--pull"]).unwrap().pull);
        assert!(parse(&["--pull", "HEAD~2"]).is_err());
        assert!(parse(&["--pull", "--branch"]).is_err());
        assert!(parse(&["--base", "main"]).is_err());
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
