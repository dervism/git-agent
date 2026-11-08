//! Command-line interface.

use crate::config::{OnSecrets, Overrides};
use clap::error::ErrorKind;
use clap::parser::ValueSource;
use clap::{Arg, ArgMatches, Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use std::ffi::OsString;
use std::path::PathBuf;

/// The two ways to call git-agent. Settings may come before a subcommand, but
/// the default command's own options may not, so a single `[OPTIONS] <COMMAND>`
/// line would promise too much.
const USAGE: &str = "git-agent [OPTIONS] [-- <GIT_COMMIT_ARGS>...]\n       git-agent <COMMAND>";

/// Write git commit messages from your changes with a local LLM.
///
/// git-agent describes the staged changes (or, if nothing is staged, all
/// changes to tracked files), shows the proposed message, and lets you commit
/// it, edit it, regenerate it, or ask the model to revise it.
#[derive(Parser, Debug)]
#[command(name = "git-agent", version, override_usage = USAGE)]
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
    pub on_secrets: Option<SecretsAction>,
}

impl SettingArgs {
    /// The settings as the config takes them.
    pub fn overrides(&self) -> Overrides {
        Overrides {
            config: self.config.clone(),
            model: self.model.clone(),
            context_length: self.context_length,
            max_output_tokens: self.max_output_tokens,
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            temperature: self.temperature,
            on_secrets: self.on_secrets.map(OnSecrets::from),
        }
    }
}

/// `--on-secrets`, as the command line takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SecretsAction {
    /// Don't commit, push or create the pull request
    Reject,
    /// List them and carry on
    Warn,
}

impl From<SecretsAction> for OnSecrets {
    fn from(action: SecretsAction) -> Self {
        match action {
            SecretsAction::Reject => Self::Reject,
            SecretsAction::Warn => Self::Warn,
        }
    }
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

    /// Run this command on each planned commit in a clean checkout, and merge
    /// a commit that fails it into the next one, e.g. `cargo check`
    #[arg(long, value_name = "COMMAND", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub check: Option<String>,

    /// How long one run of the --check command may take
    #[arg(
        long,
        value_name = "SECONDS",
        default_value_t = 600,
        requires = "check",
        value_parser = clap::value_parser!(u64).range(1..),
    )]
    pub check_timeout: u64,

    /// Extra arguments for each `git commit`, e.g. `-- --signoff`
    #[arg(last = true, value_name = "GIT_COMMIT_ARGS")]
    pub git_args: Vec<String>,
}

/// Options for `git agent branch`.
#[derive(Args, Debug)]
pub struct BranchArgs {
    /// What the branch is for, or a GitHub issue number or URL
    /// [default: the uncommitted changes]
    #[arg(value_name = "DESCRIPTION")]
    pub description: Vec<String>,

    /// Where the branch starts, e.g. origin/main [default: HEAD]
    #[arg(long, value_name = "COMMIT")]
    pub from: Option<String>,

    /// Create the branch without reviewing its name
    #[arg(short, long, conflicts_with = "raw")]
    pub create: bool,

    /// Print only the name to stdout and exit (for scripts and pipes)
    #[arg(short, long)]
    pub raw: bool,
}

/// Options for `git agent tidy`.
#[derive(Args, Debug)]
pub struct TidyArgs {
    /// Tidy the commits after this one [default: the commits that aren't pushed]
    #[arg(long, value_name = "COMMIT")]
    pub base: Option<String>,

    /// Tidy the commits without reviewing the plan or the new messages
    #[arg(long)]
    pub apply: bool,

    /// Extra context for the model, e.g. what the commits are for
    #[arg(short = 'H', long, value_name = "TEXT")]
    pub hint: Option<String>,
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
    /// Name a new branch after a description, an issue or your changes, then create it
    Branch(BranchArgs),
    /// Reword unclear messages and fold fixes into their commits, before you push
    Tidy(TidyArgs),
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

/// Parses the command line, and exits the way clap does when it's wrong.
pub fn parse() -> Cli {
    try_parse_from(std::env::args_os()).unwrap_or_else(|error| error.exit())
}

/// Parses `args`, and refuses the default command's options when they come with a subcommand.
pub fn try_parse_from<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let mut matches = Cli::command().try_get_matches_from(args)?;
    refuse_misplaced_options(&matches)?;
    Cli::from_arg_matches_mut(&mut matches).map_err(|error| error.format(&mut Cli::command()))
}

/// `git agent --all split` would run `split` without `--all`, so the default
/// command's own options can't come with a subcommand. Settings are global and can.
fn refuse_misplaced_options(matches: &ArgMatches) -> Result<(), clap::Error> {
    let Some((name, _)) = matches.subcommand() else {
        return Ok(());
    };
    let mut command = Cli::command();
    match misplaced(&command, name, matches) {
        Some(message) => Err(command.error(ErrorKind::ArgumentConflict, message)),
        None => Ok(()),
    }
}

/// What's wrong with the default command's options that came with the subcommand `name`,
/// if any did.
fn misplaced(command: &clap::Command, name: &str, matches: &ArgMatches) -> Option<String> {
    let subcommand = command.find_subcommand(name)?;
    let (its_own, not_its): (Vec<&Arg>, Vec<&Arg>) = command
        .get_arguments()
        .filter(|arg| !arg.is_global_set() && given(matches, arg))
        .partition(|arg| takes(subcommand, arg));
    let parts: Vec<String> = [
        goes_after(name, &its_own),
        stays_alone(name, &not_its),
        same_short(name, subcommand, &not_its),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

fn given(matches: &ArgMatches, arg: &Arg) -> bool {
    matches.value_source(arg.get_id().as_str()) == Some(ValueSource::CommandLine)
}

fn takes(subcommand: &clap::Command, arg: &Arg) -> bool {
    subcommand
        .get_arguments()
        .any(|own| own.get_id() == arg.get_id())
}

fn long_name(arg: &Arg) -> String {
    arg.get_long()
        .map_or_else(|| arg.get_id().to_string(), |long| format!("--{long}"))
}

/// The option as it's typed, with a placeholder for a value it takes, such as
/// `--hint <TEXT>`.
fn as_typed(arg: &Arg) -> String {
    let name = long_name(arg);
    if !arg.get_action().takes_values() {
        return name;
    }
    let value = arg.get_value_names().and_then(<[_]>::first).map_or_else(
        || arg.get_id().to_string().to_uppercase(),
        ToString::to_string,
    );
    format!("{name} <{value}>")
}

/// Options the subcommand has too: they go after it.
fn goes_after(name: &str, args: &[&Arg]) -> Option<String> {
    let verb = match args {
        [] => return None,
        [_] => "goes",
        _ => "go",
    };
    let names: Vec<String> = args.iter().map(|arg| long_name(arg)).collect();
    let typed: Vec<String> = args.iter().map(|arg| as_typed(arg)).collect();
    Some(format!(
        "{} {verb} after the subcommand: 'git agent {name} {}'",
        quoted_list(&names),
        typed.join(" ")
    ))
}

/// Options only the default command has.
fn stays_alone(name: &str, args: &[&Arg]) -> Option<String> {
    let (verb, pronoun) = match args {
        [] => return None,
        [_] => ("is", "it"),
        _ => ("are", "they"),
    };
    let names: Vec<String> = args.iter().map(|arg| long_name(arg)).collect();
    Some(format!(
        "{} {verb} for 'git agent' alone, so {pronoun} can't come with '{name}'",
        quoted_list(&names)
    ))
}

/// The short flags among `args` that the subcommand gives an option of its own, as
/// `branch` gives `-c` to `--create`, which the user may have meant.
fn same_short(name: &str, subcommand: &clap::Command, args: &[&Arg]) -> Option<String> {
    let clashes: Vec<String> = args
        .iter()
        .filter_map(|arg| {
            let short = arg.get_short()?;
            let theirs = subcommand
                .get_arguments()
                .find(|own| own.get_short() == Some(short))?;
            Some(format!(
                "'-{short}' after '{name}' is '{}'",
                long_name(theirs)
            ))
        })
        .collect();
    (!clashes.is_empty()).then(|| clashes.join("; "))
}

/// `'a'`, `'a' and 'b'`, or `'a', 'b' and 'c'`.
fn quoted_list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|item| format!("'{item}'")).collect();
    match quoted.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => quoted.concat(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn passes_trailing_args_to_git_commit() {
        let cli = try_parse_from(["git-agent", "-c", "--", "--no-verify", "-s"]).unwrap();
        assert!(cli.generate.commit);
        assert_eq!(cli.generate.git_args, ["--no-verify", "-s"]);
    }

    #[test]
    fn push_combines_with_commit_but_not_raw() {
        let cli = try_parse_from(["git-agent", "-cp"]).unwrap();
        assert!(cli.generate.commit && cli.generate.push);
        assert!(try_parse_from(["git-agent", "--push", "--raw"]).is_err());
    }

    #[test]
    fn setting_flags_work_with_subcommands() {
        let cli = try_parse_from(["git-agent", "models", "--model", "qwen3"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Models)));
        assert_eq!(cli.settings.model.as_deref(), Some("qwen3"));
        let cli = try_parse_from(["git-agent", "--context-length", "8192"]).unwrap();
        assert_eq!(cli.settings.context_length, Some(8192));
        let cli = try_parse_from(["git-agent", "pr", "--on-secrets", "warn"]).unwrap();
        assert_eq!(cli.settings.on_secrets, Some(SecretsAction::Warn));
        assert!(try_parse_from(["git-agent", "--on-secrets", "ignore"]).is_err());
    }

    #[test]
    fn pr_takes_its_own_options_and_settings() {
        let cli = try_parse_from([
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
        assert!(try_parse_from(["git-agent", "pr", "--raw", "--create"]).is_err());
        // The commit options belong to plain `git agent`.
        assert!(try_parse_from(["git-agent", "--all", "pr"]).is_err());
    }

    #[test]
    fn split_takes_its_own_options() {
        let cli = try_parse_from(["git-agent", "split", "-a", "-c", "--", "--signoff"]).unwrap();
        let Some(Command::Split(args)) = cli.command else {
            panic!("expected the split command");
        };
        assert!(args.all && args.commit && !args.push);
        assert_eq!(args.git_args, ["--signoff"]);
    }

    #[test]
    fn branch_takes_a_description_without_quotes() {
        let cli = try_parse_from([
            "git-agent",
            "branch",
            "fix",
            "login",
            "timeout",
            "--from",
            "origin/main",
        ])
        .unwrap();
        let Some(Command::Branch(args)) = cli.command else {
            panic!("expected the branch command");
        };
        assert_eq!(args.description, ["fix", "login", "timeout"]);
        assert_eq!(args.from.as_deref(), Some("origin/main"));
        let cli = try_parse_from(["git-agent", "branch", "-c"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Branch(args)) if args.create && args.description.is_empty())
        );
        assert!(try_parse_from(["git-agent", "branch", "-c", "-r", "42"]).is_err());
    }

    #[test]
    fn tidy_takes_a_base_and_applies_without_review() {
        let cli = try_parse_from(["git-agent", "tidy", "--base", "HEAD~3", "--apply"]).unwrap();
        let Some(Command::Tidy(args)) = cli.command else {
            panic!("expected the tidy command");
        };
        assert_eq!(args.base.as_deref(), Some("HEAD~3"));
        assert!(args.apply && args.hint.is_none());
    }

    #[test]
    fn why_takes_lines_and_a_question() {
        let cli = try_parse_from(["git-agent", "why", "src/fit.rs:120", "why 400?"]).unwrap();
        let Some(Command::Why(args)) = cli.command else {
            panic!("expected the why command");
        };
        assert_eq!(args.target, "src/fit.rs:120");
        assert_eq!(args.question.as_deref(), Some("why 400?"));
        assert!(try_parse_from(["git-agent", "why"]).is_err());
    }

    #[test]
    fn explain_takes_a_commit_a_branch_or_the_last_pull() {
        let parse = |args: &[&str]| {
            let cli = try_parse_from([&["git-agent", "explain"], args].concat())?;
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
        let cli = try_parse_from(["git-agent", "config"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Config { command: None })
        ));
        let cli = try_parse_from(["git-agent", "config", "show"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                command: Some(ConfigCommand::Show)
            })
        ));
    }

    /// Each subcommand with the arguments it needs.
    const SUBCOMMANDS: [&[&str]; 8] = [
        &["split"],
        &["pr"],
        &["branch"],
        &["tidy"],
        &["why", "f:1"],
        &["explain"],
        &["models"],
        &["config"],
    ];

    /// Each setting with a value it takes.
    const SETTINGS: [&[&str]; 8] = [
        &["--config", "f.toml"],
        &["--model", "m"],
        &["--context-length", "8192"],
        &["--max-output-tokens", "100"],
        &["--base-url", "http://localhost:1/v1"],
        &["--api-key", "k"],
        &["--temperature", "0.5"],
        &["--on-secrets", "warn"],
    ];

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        try_parse_from(std::iter::once("git-agent").chain(args.iter().copied()))
    }

    #[test]
    fn the_tables_cover_every_subcommand_and_setting() {
        let command = Cli::command();
        let subcommands: Vec<&str> = command
            .get_subcommands()
            .map(clap::Command::get_name)
            .collect();
        assert_eq!(subcommands, SUBCOMMANDS.map(|call| call[0]));
        let settings: Vec<String> = command
            .get_arguments()
            .filter(|arg| arg.is_global_set())
            .filter_map(Arg::get_long)
            .map(|long| format!("--{long}"))
            .collect();
        assert_eq!(settings, SETTINGS.map(|setting| setting[0]));
    }

    #[test]
    fn a_setting_works_before_and_after_every_subcommand() {
        for call in SUBCOMMANDS {
            for setting in SETTINGS {
                let before = parse(&[setting, call].concat()).unwrap();
                let after = parse(&[call, setting].concat()).unwrap();
                assert_eq!(
                    format!("{before:?}"),
                    format!("{after:?}"),
                    "{setting:?} and {call:?}"
                );
                assert_ne!(
                    format!("{:?}", before.settings),
                    format!("{:?}", SettingArgs::default())
                );
            }
        }
    }

    #[test]
    fn the_default_commands_options_fail_before_every_subcommand() {
        let command = Cli::command();
        let options: Vec<&Arg> = command
            .get_arguments()
            .filter(|arg| !arg.is_global_set() && !arg.is_last_set())
            .collect();
        assert!(!options.is_empty());
        for arg in options {
            let option = format!("--{}", arg.get_long().unwrap());
            let given = if arg.get_action().takes_values() {
                vec![option.as_str(), "x"]
            } else {
                vec![option.as_str()]
            };
            for call in SUBCOMMANDS {
                let error = parse(&[given.as_slice(), call].concat()).unwrap_err();
                assert_eq!(
                    error.kind(),
                    ErrorKind::ArgumentConflict,
                    "{given:?} before {call:?}"
                );
            }
        }
    }

    #[test]
    fn the_default_command_takes_settings_and_its_own_options_in_any_order() {
        for args in [
            &["--model", "m", "-c", "--", "--signoff"][..],
            &["-c", "--model", "m", "--", "--signoff"],
        ] {
            let cli = parse(args).unwrap();
            assert!(cli.command.is_none() && cli.generate.commit);
            assert_eq!(cli.settings.model.as_deref(), Some("m"));
            assert_eq!(cli.generate.git_args, ["--signoff"]);
        }
        let cli = parse(&["--model", "m"]).unwrap();
        assert!(cli.command.is_none() && !cli.generate.commit);
    }

    #[test]
    fn a_word_after_an_option_or_the_dashes_is_no_subcommand() {
        let cli = parse(&["-H", "split"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.generate.hint.as_deref(), Some("split"));
        let cli = parse(&["--", "--signoff", "split"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.generate.git_args, ["--signoff", "split"]);
        let cli = parse(&["split", "--", "--signoff"]).unwrap();
        let Some(Command::Split(args)) = cli.command else {
            panic!("expected the split command");
        };
        assert_eq!(args.git_args, ["--signoff"]);
    }

    #[test]
    fn says_where_a_misplaced_option_goes() {
        let message = |args: &[&str]| parse(args).unwrap_err().to_string();
        let after = message(&["--all", "split"]);
        assert!(
            after.contains("'--all' goes after the subcommand: 'git agent split --all'"),
            "{after}"
        );
        let alone = message(&["--raw", "why", "f:1"]);
        assert!(
            alone.contains("'--raw' is for 'git agent' alone, so it can't come with 'why'"),
            "{alone}"
        );
        let both = message(&["-r", "-a", "-H", "x", "split"]);
        assert!(
            both.contains(
                "'--all' and '--hint' go after the subcommand: \
                 'git agent split --all --hint <TEXT>'; \
                 '--raw' is for 'git agent' alone, so it can't come with 'split'"
            ),
            "{both}"
        );
        let clash = message(&["-c", "branch"]);
        assert!(
            clash.contains(
                "'--commit' is for 'git agent' alone, so it can't come with 'branch'; \
                 '-c' after 'branch' is '--create'"
            ),
            "{clash}"
        );
        let value = message(&["-H", "x", "tidy"]);
        assert!(value.contains("'git agent tidy --hint <TEXT>'"), "{value}");
        let several = message(&["-c", "-p", "-a", "models"]);
        assert!(
            several.contains("'--commit', '--push' and '--all' are for 'git agent' alone, so they"),
            "{several}"
        );
    }

    #[test]
    fn settings_reach_nested_subcommands_and_the_last_one_wins() {
        for args in [
            &["--model", "x", "config", "init", "--force"][..],
            &["config", "--model", "x", "init", "--force"],
        ] {
            let cli = parse(args).unwrap();
            assert_eq!(cli.settings.model.as_deref(), Some("x"), "{args:?}");
            assert!(matches!(
                cli.command,
                Some(Command::Config {
                    command: Some(ConfigCommand::Init { force: true })
                })
            ));
        }
        let cli = parse(&["-m", "a", "split", "-m", "b"]).unwrap();
        assert_eq!(cli.settings.model.as_deref(), Some("b"));
    }

    #[test]
    fn help_and_version_win_over_a_misplaced_option() {
        for args in [&["--all", "help"][..], &["--all", "--help", "split"]] {
            let kind = parse(args).unwrap_err().kind();
            assert_eq!(kind, ErrorKind::DisplayHelp, "{args:?}");
        }
        let kind = parse(&["--all", "--version", "split"]).unwrap_err().kind();
        assert_eq!(kind, ErrorKind::DisplayVersion);
    }

    #[test]
    fn hands_every_setting_to_the_config_as_given() {
        let cli = parse(&[
            "--config",
            "c.toml",
            "-m",
            "m",
            "--context-length",
            "5000",
            "--max-output-tokens",
            "600",
            "--base-url",
            "http://b/v1",
            "--api-key",
            "k",
            "-t",
            "0.7",
            "--on-secrets",
            "warn",
        ])
        .unwrap();
        let overrides = cli.settings.overrides();
        assert_eq!(overrides.config, Some(PathBuf::from("c.toml")));
        assert_eq!(overrides.model.as_deref(), Some("m"));
        assert_eq!(overrides.context_length, Some(5000));
        assert_eq!(overrides.max_output_tokens, Some(600));
        assert_eq!(overrides.base_url.as_deref(), Some("http://b/v1"));
        assert_eq!(overrides.api_key.as_deref(), Some("k"));
        assert_eq!(overrides.temperature, Some(0.7));
        assert_eq!(overrides.on_secrets, Some(OnSecrets::Warn));
    }

    #[test]
    fn spells_each_secrets_action_as_the_config_file_does() {
        for action in SecretsAction::value_variants() {
            let flag = action.to_possible_value().unwrap();
            assert_eq!(flag.get_name(), OnSecrets::from(*action).as_str());
        }
        // A new setting doesn't compile here until the command line takes it too.
        for setting in [OnSecrets::Reject, OnSecrets::Warn] {
            let action = match setting {
                OnSecrets::Reject => SecretsAction::Reject,
                OnSecrets::Warn => SecretsAction::Warn,
            };
            assert_eq!(OnSecrets::from(action), setting);
        }
    }

    #[test]
    fn lists_with_commas_and_a_final_and() {
        let list = |items: &[&str]| {
            quoted_list(&items.iter().map(ToString::to_string).collect::<Vec<_>>())
        };
        assert_eq!(list(&[]), "");
        assert_eq!(list(&["a"]), "'a'");
        assert_eq!(list(&["a", "b"]), "'a' and 'b'");
        assert_eq!(list(&["a", "b", "c"]), "'a', 'b' and 'c'");
    }

    #[test]
    fn keeps_both_usage_lines() {
        let usage = Cli::command().render_usage().to_string();
        assert!(
            usage.contains(
                "git-agent [OPTIONS] [-- <GIT_COMMIT_ARGS>...]\n       git-agent <COMMAND>"
            ),
            "{usage}"
        );
    }
}
