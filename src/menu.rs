//! The interactive settings menu, `git agent config`.
//!
//! Each change is saved to the config file as soon as it is made. API keys go
//! to the macOS keychain instead (see `api_keys.rs`).

use crate::config::{
    self, Config, DEFAULT_EXCLUDE, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_SYSTEM_PROMPT,
    LOCAL_TEMPERATURE, MIN_CONTEXT_LENGTH, MIN_OUTPUT_TOKENS, OnSecrets, Overrides,
};
use crate::providers::{self, PROVIDERS};
use crate::safe_text::one_line;
use crate::server::{ModelInfo, ModelServer, Server};
use crate::temp::PrivateDir;
use crate::ui::{self, thousands};
use crate::{api_keys, git};
use anyhow::{Context, Result, bail};
use inquire::validator::Validation;
use inquire::{
    Confirm, CustomType, CustomUserError, InquireError, Password, PasswordDisplayMode, Select, Text,
};
use std::fmt;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process;

/// Width of the setting names in the menu.
const NAME_WIDTH: usize = 16;
/// Models shown at once; type to filter the rest.
const MODEL_PAGE_SIZE: usize = 15;

#[derive(Clone, Copy, PartialEq)]
enum Item {
    Provider,
    Model,
    ApiKey,
    ContextLength,
    OutputLimit,
    Temperature,
    Timeout,
    Secrets,
    Exclude,
    SystemPrompt,
    Done,
}

const ITEMS: [Item; 11] = [
    Item::Provider,
    Item::Model,
    Item::ApiKey,
    Item::ContextLength,
    Item::OutputLimit,
    Item::Temperature,
    Item::Timeout,
    Item::Secrets,
    Item::Exclude,
    Item::SystemPrompt,
    Item::Done,
];

impl Item {
    fn name(self) -> &'static str {
        match self {
            Item::Provider => "Provider",
            Item::Model => "Model",
            Item::ApiKey => "API key",
            Item::ContextLength => "Context length",
            Item::OutputLimit => "Output limit",
            Item::Temperature => "Temperature",
            Item::Timeout => "Timeout",
            Item::Secrets => "Secrets",
            Item::Exclude => "Excluded files",
            Item::SystemPrompt => "System prompt",
            Item::Done => "Done",
        }
    }
}

/// Where the API key for the configured server comes from.
#[derive(Clone, Copy, PartialEq)]
enum Key {
    Flag,
    Keychain,
    Missing,
}

struct Menu<'a> {
    overrides: &'a Overrides,
    path: PathBuf,
    config: Config,
    key: Key,
}

pub async fn run(overrides: &Overrides) -> Result<()> {
    if !io::stdin().is_terminal() || !ui::stderr_is_terminal() {
        bail!(
            "the settings menu needs a terminal; edit the config file instead \
             (`git agent config init` creates one)"
        );
    }
    let path = config::resolve_path(overrides)
        .context("could not find a config directory; set HOME or pass --config")?;
    let config = Config::read_file(&path)?.unwrap_or_default();
    let mut menu = Menu {
        overrides,
        path,
        config,
        key: Key::Missing,
    };
    menu.check_key();

    ui::info(format!(
        "{} {}",
        ui::bold("git-agent settings"),
        ui::dim(&format!("· {}", menu.path.display()))
    ));
    warn_about_overrides(overrides);
    let mut cursor = 0;
    loop {
        let labels: Vec<String> = ITEMS.iter().map(|&item| menu.label(item)).collect();
        let prompt = Select::new("Setting:", labels)
            .with_starting_cursor(cursor)
            .with_page_size(ITEMS.len())
            .with_help_message("↑↓ to move, enter to change, esc to quit");
        let Some(choice) = answer(prompt.raw_prompt_skippable())? else {
            break;
        };
        cursor = choice.index;
        let item = ITEMS[choice.index];
        if item == Item::Done {
            break;
        }
        // A failed step (no network, a keychain error) shouldn't close the menu.
        match menu.change(item).await {
            Ok(true) => match menu.config.save(&menu.path) {
                Ok(()) => ui::info(ui::dim("   Saved.")),
                Err(err) => ui::error(&err),
            },
            Ok(false) => {}
            Err(err) => ui::error(&err),
        }
    }
    Ok(())
}

/// The answer to a prompt, or `None` if the user pressed Esc. Ctrl-C quits at
/// once, which loses nothing because every change is saved as it is made.
fn answer<T>(result: Result<Option<T>, InquireError>) -> Result<Option<T>> {
    match result {
        Err(InquireError::OperationInterrupted) => process::exit(130),
        result => Ok(result?),
    }
}

/// Asks the user to pick one of `options`, starting at `start`. Returns the
/// index of the one they picked, or `None` if they went back.
fn pick(message: &str, options: &[&str], start: usize) -> Result<Option<usize>> {
    let prompt = Select::new(message, options.to_vec()).with_starting_cursor(start);
    Ok(answer(prompt.raw_prompt_skippable())?.map(|choice| choice.index))
}

/// A prompt validator that accepts the values for which `valid` holds and
/// shows `error` for the others.
fn accept<T: ?Sized>(
    valid: fn(&T) -> bool,
    error: impl Into<String>,
) -> impl Fn(&T) -> Result<Validation, CustomUserError> + Clone {
    let error = error.into();
    move |value| {
        Ok(if valid(value) {
            Validation::Valid
        } else {
            Validation::Invalid(error.clone().into())
        })
    }
}

fn valid_context(tokens: &usize) -> bool {
    *tokens >= MIN_CONTEXT_LENGTH
}

fn context_error() -> String {
    format!("Use at least {MIN_CONTEXT_LENGTH} tokens")
}

/// Replaces `slot` with a new value, if there is one. Returns whether it changed.
fn update<T: PartialEq>(slot: &mut T, value: Option<T>) -> bool {
    match value {
        Some(value) if *slot != value => {
            *slot = value;
            true
        }
        _ => false,
    }
}

impl Menu<'_> {
    fn label(&self, item: Item) -> String {
        match item {
            Item::Done => item.name().to_owned(),
            _ => format!("{:NAME_WIDTH$}{}", item.name(), self.value(item)),
        }
    }

    fn value(&self, item: Item) -> String {
        let config = &self.config;
        let local = providers::is_local(&config.base_url);
        match item {
            Item::Provider => provider_label(&config.base_url),
            Item::Model => match (&config.model, local) {
                (Some(model), _) => model.clone(),
                (None, true) => "automatic: the model loaded on the server".to_owned(),
                (None, false) => "not set".to_owned(),
            },
            Item::ApiKey => match (self.key, local) {
                (Key::Flag, _) => "from --api-key or GIT_AGENT_API_KEY".to_owned(),
                (Key::Keychain, _) => "saved in the macOS keychain".to_owned(),
                (Key::Missing, true) => "none (local servers usually need none)".to_owned(),
                (Key::Missing, false) => "not set".to_owned(),
            },
            Item::ContextLength => match config.context_length {
                Some(tokens) => format!("{} tokens", thousands(tokens)),
                None => "automatic: asked from the server".to_owned(),
            },
            Item::OutputLimit => {
                let tokens = thousands(config.max_output_tokens);
                if providers::is_anthropic(&config.base_url) {
                    format!("{tokens} tokens")
                } else {
                    format!("{tokens} tokens (sent only to Anthropic)")
                }
            }
            Item::Temperature => match config.effective_temperature() {
                _ if providers::is_anthropic(&config.base_url) => {
                    "not used: Anthropic's API takes none".to_owned()
                }
                Some(temperature) if config.temperature.is_none() => {
                    format!("automatic: {temperature} on local servers")
                }
                Some(temperature) => temperature.to_string(),
                None => "automatic: the provider's default".to_owned(),
            },
            Item::Timeout => format!("{} seconds", config.timeout_secs),
            Item::Secrets => match config.on_secrets {
                OnSecrets::Reject => "reject: don't commit or push them".to_owned(),
                OnSecrets::Warn => "warn: list them and carry on".to_owned(),
            },
            Item::Exclude => match config.exclude.len() {
                _ if config.exclude == default_exclude() => "common lockfiles".to_owned(),
                0 => "none".to_owned(),
                1 => "1 pattern".to_owned(),
                count => format!("{count} patterns"),
            },
            Item::SystemPrompt if config.system_prompt.trim() == DEFAULT_SYSTEM_PROMPT.trim() => {
                "default: Conventional Commits".to_owned()
            }
            Item::SystemPrompt => "custom".to_owned(),
            Item::Done => String::new(),
        }
    }

    /// Asks for a new value of `item`. Returns whether the config changed.
    async fn change(&mut self, item: Item) -> Result<bool> {
        let changed = match item {
            Item::Provider => {
                let value = self.choose_provider().await?;
                let changed = update(&mut self.config, value);
                self.check_key();
                changed
            }
            Item::Model => {
                let config = &self.config;
                let api_key = flag_key(self.overrides)
                    .or_else(|| api_keys::get(&config.base_url).ok().flatten());
                let local = providers::is_local(&config.base_url);
                match choose_model(config, api_key, local).await? {
                    Some(choice) => {
                        let model = update(&mut self.config.model, Some(choice.model));
                        let context_length = Some(choice.context_length);
                        update(&mut self.config.context_length, context_length) || model
                    }
                    None => false,
                }
            }
            Item::ApiKey => {
                self.change_api_key()?;
                false
            }
            Item::ContextLength => {
                let value = context_length_setting(self.config.context_length)?;
                update(&mut self.config.context_length, value)
            }
            Item::OutputLimit => {
                let value = output_limit_setting(self.config.max_output_tokens)?;
                update(&mut self.config.max_output_tokens, value)
            }
            Item::Temperature => {
                let value = temperature_setting(self.config.temperature)?;
                update(&mut self.config.temperature, value)
            }
            Item::Timeout => {
                let value = timeout_setting(self.config.timeout_secs)?;
                update(&mut self.config.timeout_secs, value)
            }
            Item::Secrets => {
                let value = on_secrets_setting(self.config.on_secrets)?;
                update(&mut self.config.on_secrets, value)
            }
            Item::Exclude => {
                let value = exclude_setting(&self.config.exclude)?;
                update(&mut self.config.exclude, value)
            }
            Item::SystemPrompt => {
                let value = system_prompt_setting(&self.config.system_prompt)?;
                update(&mut self.config.system_prompt, value)
            }
            Item::Done => false,
        };
        Ok(changed)
    }

    fn check_key(&mut self) {
        self.key = if flag_key(self.overrides).is_some() {
            Key::Flag
        } else {
            match api_keys::get(&self.config.base_url) {
                Ok(Some(_)) => Key::Keychain,
                Ok(None) => Key::Missing,
                Err(err) => {
                    ui::error(&err);
                    Key::Missing
                }
            }
        };
    }

    /// Asks for the provider, its API key if it needs one, and the model.
    /// Returns the changed config, or `None` if the user went back.
    async fn choose_provider(&self) -> Result<Option<Config>> {
        let current = providers::find(&self.config.base_url);
        let mut labels: Vec<String> = PROVIDERS
            .iter()
            .map(|provider| format!("{:14}{}", provider.name, provider.base_url))
            .collect();
        labels.push(format!(
            "{:14}{}",
            "Other", "a server with an OpenAI-compatible API"
        ));
        let start = current
            .and_then(|current| {
                PROVIDERS
                    .iter()
                    .position(|p| p.base_url == current.base_url)
            })
            .unwrap_or(PROVIDERS.len());
        let page_size = labels.len();
        let prompt = Select::new("Provider:", labels)
            .with_starting_cursor(start)
            .with_page_size(page_size)
            .with_help_message("↑↓ to move, type to filter, enter to choose, esc to go back");
        let Some(choice) = answer(prompt.raw_prompt_skippable())? else {
            return Ok(None);
        };

        let provider = PROVIDERS.get(choice.index);
        let base_url = match provider {
            Some(provider) if !provider.local => Some(provider.base_url.to_owned()),
            // A local server may run on another port or another machine.
            Some(provider) => {
                let initial = if current.is_some_and(|c| c.base_url == provider.base_url) {
                    &self.config.base_url
                } else {
                    provider.base_url
                };
                ask_url(&format!("{} address:", provider.name), initial)?
            }
            None => {
                let initial = if current.is_none() {
                    self.config.base_url.as_str()
                } else {
                    ""
                };
                ask_url("Base URL, including /v1:", initial)?
            }
        };
        let Some(base_url) = base_url else {
            return Ok(None);
        };

        let mut config = self.config.clone();
        if config.base_url != base_url {
            config.base_url = base_url;
            config.model = None;
            config.context_length = None;
        }
        let local = providers::is_local(&config.base_url);
        let mut api_key =
            flag_key(self.overrides).or_else(|| api_keys::get(&config.base_url).ok().flatten());
        if api_key.is_none() && provider.is_some_and(|provider| !provider.local) {
            api_key = ask_and_save_api_key(&config.base_url)?;
        }
        let Some(choice) = choose_model(&config, api_key, local).await? else {
            return Ok(None);
        };
        config.model = choice.model;
        config.context_length = choice.context_length;

        if let Some(temperature) = config.temperature
            && !local
        {
            let question = format!(
                "Many cloud models accept only their default temperature. \
                 Stop sending temperature {temperature}?"
            );
            let prompt = Confirm::new(&question).with_default(true);
            if answer(prompt.prompt_skippable())?.unwrap_or(false) {
                config.temperature = None;
            }
        }
        Ok(Some(config))
    }

    fn change_api_key(&mut self) -> Result<()> {
        if !api_keys::SUPPORTED {
            ui::info(
                "   git-agent can save API keys only in the macOS keychain. \
                 Set the GIT_AGENT_API_KEY environment variable instead.",
            );
            return Ok(());
        }
        let base_url = self.config.base_url.clone();
        if self.key == Key::Keychain {
            let options = ["Replace the saved key", "Remove the saved key"];
            let Some(choice) = pick("API key:", &options, 0)? else {
                return Ok(());
            };
            if choice == 1 {
                api_keys::delete(&base_url)?;
                ui::info(ui::dim("   Removed the key from the macOS keychain."));
                self.check_key();
                return Ok(());
            }
        }
        if ask_and_save_api_key(&base_url)?.is_some() && self.key == Key::Flag {
            ui::warn("--api-key or GIT_AGENT_API_KEY is set, so git-agent uses that key instead");
        }
        self.check_key();
        Ok(())
    }
}

/// The key from `--api-key` or `GIT_AGENT_API_KEY`, which wins over the keychain.
fn flag_key(overrides: &Overrides) -> Option<String> {
    overrides
        .api_key
        .clone()
        .filter(|key| !key.trim().is_empty())
}

fn warn_about_overrides(overrides: &Overrides) {
    let overridden: Vec<&str> = [
        (overrides.base_url.is_some(), "base URL"),
        (overrides.model.is_some(), "model"),
        (overrides.context_length.is_some(), "context length"),
        (overrides.max_output_tokens.is_some(), "output limit"),
        (overrides.temperature.is_some(), "temperature"),
        (overrides.on_secrets.is_some(), "secrets setting"),
    ]
    .into_iter()
    .filter_map(|(set, name)| set.then_some(name))
    .collect();
    if !overridden.is_empty() {
        ui::warn(format!(
            "flags or GIT_AGENT_* environment variables set the {}; while they are set, \
             the values saved here aren't used",
            overridden.join(", ")
        ));
    }
}

fn provider_label(base_url: &str) -> String {
    match providers::find(base_url) {
        Some(provider) => format!("{} · {base_url}", provider.name),
        None => format!("other · {base_url}"),
    }
}

fn default_exclude() -> Vec<String> {
    DEFAULT_EXCLUDE
        .iter()
        .map(|pattern| pattern.to_string())
        .collect()
}

fn ask_url(message: &str, initial: &str) -> Result<Option<String>> {
    let prompt = Text::new(message)
        .with_initial_value(initial)
        .with_validator(accept::<str>(
            |url| url.trim().starts_with("http://") || url.trim().starts_with("https://"),
            "Start the address with http:// or https://",
        ));
    let url = answer(prompt.prompt_skippable())?;
    Ok(url.map(|url| url.trim().trim_end_matches('/').to_owned()))
}

/// Asks for the API key of the server at `base_url` and saves it in the keychain.
fn ask_and_save_api_key(base_url: &str) -> Result<Option<String>> {
    let name = match providers::find(base_url) {
        Some(provider) => provider.name.to_owned(),
        None => api_keys::account(base_url).unwrap_or_else(|| base_url.to_owned()),
    };
    if !api_keys::SUPPORTED {
        ui::info(format!(
            "   {name} needs an API key. Set it in the GIT_AGENT_API_KEY environment variable."
        ));
        return Ok(None);
    }
    let message = format!("API key for {name}:");
    let prompt = Password::new(&message)
        .without_confirmation()
        .with_display_mode(PasswordDisplayMode::Masked)
        .with_help_message("saved in the macOS keychain, never in the config file; esc to skip");
    let Some(key) = answer(prompt.prompt_skippable())? else {
        return Ok(None);
    };
    let key = key.trim();
    if key.is_empty() {
        return Ok(None);
    }
    api_keys::set(base_url, key)?;
    ui::info(ui::dim("   Saved the key in the macOS keychain."));
    Ok(Some(key.to_owned()))
}

/// A model picked in the menu, with the context length to save for it.
struct ModelChoice {
    model: Option<String>,
    context_length: Option<usize>,
}

enum ModelOption {
    Automatic,
    Listed(ModelInfo),
    Typed,
}

impl fmt::Display for ModelOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelOption::Automatic => {
                f.write_str("Automatic: whichever model is loaded on the server")
            }
            ModelOption::Listed(model) => {
                let mut details = Vec::new();
                if model.loaded == Some(true) {
                    details.push("loaded".to_owned());
                }
                if let Some(tokens) = model.context_length {
                    details.push(format!("{} tokens", thousands(tokens)));
                }
                let id = one_line(&model.id);
                if details.is_empty() {
                    f.write_str(&id)
                } else {
                    write!(f, "{id}  ({})", details.join(", "))
                }
            }
            ModelOption::Typed => f.write_str("Type a model ID…"),
        }
    }
}

/// Lists the server's models to choose from, or asks for a model ID if it can't.
async fn choose_model(
    config: &Config,
    api_key: Option<String>,
    local: bool,
) -> Result<Option<ModelChoice>> {
    let server = Server::connect(&Config {
        api_key,
        ..config.clone()
    })?;
    let listed = {
        let _spinner = ui::Spinner::start(format!("Getting the models from {}", config.base_url));
        server.models().await
    };
    let models = match listed {
        Ok(models) if !models.is_empty() => models,
        Ok(_) => {
            ui::warn("the server lists no models");
            return type_model(local);
        }
        Err(err) => {
            ui::warn(format!("could not get the server's models: {err:#}"));
            return type_model(local);
        }
    };

    // Servers that say which model is loaded can pick it on every run.
    let automatic = local || models.iter().any(|model| model.loaded.is_some());
    let mut options = Vec::new();
    if automatic {
        options.push(ModelOption::Automatic);
    }
    let start = config
        .model
        .as_ref()
        .and_then(|current| models.iter().position(|model| model.id == *current))
        .map_or(0, |index| index + options.len());
    options.extend(models.into_iter().map(ModelOption::Listed));
    options.push(ModelOption::Typed);
    let prompt = Select::new("Model:", options)
        .with_starting_cursor(start)
        .with_page_size(MODEL_PAGE_SIZE)
        .with_help_message("↑↓ to move, type to filter, enter to choose, esc to go back");
    let Some(choice) = answer(prompt.prompt_skippable())? else {
        return Ok(None);
    };
    match choice {
        ModelOption::Automatic => Ok(Some(ModelChoice {
            model: None,
            context_length: None,
        })),
        ModelOption::Listed(model) => {
            // git-agent reads a context length the server reports on every run.
            let context_length = if model.context_length.is_some() || local {
                None
            } else {
                ask_context_length(&model.id)?
            };
            Ok(Some(ModelChoice {
                model: Some(model.id),
                context_length,
            }))
        }
        ModelOption::Typed => type_model(local),
    }
}

fn type_model(local: bool) -> Result<Option<ModelChoice>> {
    let prompt = Text::new("Model ID:").with_validator(accept::<str>(
        |id| !id.trim().is_empty(),
        "Type the model's ID",
    ));
    let Some(model) = answer(prompt.prompt_skippable())? else {
        return Ok(None);
    };
    let model = model.trim().to_owned();
    let context_length = if local {
        None
    } else {
        ask_context_length(&model)?
    };
    Ok(Some(ModelChoice {
        model: Some(model),
        context_length,
    }))
}

/// Asks for the context window of a model whose server doesn't report it.
fn ask_context_length(model: &str) -> Result<Option<usize>> {
    let message = format!("Context window of {model}, in tokens:");
    let help = format!(
        "see the provider's model page; esc to skip, and git-agent assumes {} tokens",
        thousands(config::FALLBACK_CONTEXT_LENGTH)
    );
    let prompt = CustomType::<usize>::new(&message)
        .with_help_message(&help)
        .with_error_message("Type a whole number of tokens, e.g. 128000")
        .with_validator(accept(valid_context, context_error()));
    answer(prompt.prompt_skippable())
}

/// Asks whether to leave a setting automatic or to set a value, and asks for
/// the value with `ask`. Returns `Some(None)` for automatic, `None` if the user
/// went back.
fn automatic_or<T>(
    message: &str,
    automatic: &str,
    current: Option<T>,
    ask: impl FnOnce(Option<T>) -> Result<Option<T>>,
) -> Result<Option<Option<T>>> {
    let options = [automatic, "Set a value…"];
    match pick(message, &options, usize::from(current.is_some()))? {
        None => Ok(None),
        Some(0) => Ok(Some(None)),
        Some(_) => Ok(ask(current)?.map(Some)),
    }
}

fn context_length_setting(current: Option<usize>) -> Result<Option<Option<usize>>> {
    let automatic = format!(
        "Automatic: ask the server, or assume {} tokens",
        thousands(config::FALLBACK_CONTEXT_LENGTH)
    );
    automatic_or("Context length:", &automatic, current, |current| {
        let mut prompt = CustomType::<usize>::new("Context window, in tokens:")
            .with_error_message("Type a whole number of tokens, e.g. 32768")
            .with_validator(accept(valid_context, context_error()));
        if let Some(current) = current {
            prompt = prompt.with_default(current);
        }
        answer(prompt.prompt_skippable())
    })
}

fn temperature_setting(current: Option<f32>) -> Result<Option<Option<f32>>> {
    let automatic = format!(
        "Automatic: {LOCAL_TEMPERATURE} on local servers, the provider's default in the cloud"
    );
    automatic_or("Temperature:", &automatic, current, |current| {
        let prompt = CustomType::<f32>::new("Temperature, from 0.0 to 2.0:")
            .with_default(current.unwrap_or(LOCAL_TEMPERATURE))
            .with_help_message("lower values give more predictable messages")
            .with_error_message("Type a number such as 0.4")
            .with_validator(accept(
                |temperature| (0.0..=2.0).contains(temperature),
                "Use a value from 0.0 to 2.0",
            ));
        answer(prompt.prompt_skippable())
    })
}

fn output_limit_setting(current: usize) -> Result<Option<usize>> {
    let error = format!("Use at least {MIN_OUTPUT_TOKENS} tokens");
    let help = format!(
        "default {}; much higher values can take longer than the timeout",
        thousands(DEFAULT_MAX_OUTPUT_TOKENS)
    );
    let prompt = CustomType::<usize>::new("Most tokens per reply, thinking included:")
        .with_default(current)
        .with_help_message(&help)
        .with_error_message("Type a whole number of tokens, e.g. 16000")
        .with_validator(accept(|tokens| *tokens >= MIN_OUTPUT_TOKENS, error));
    answer(prompt.prompt_skippable())
}

fn timeout_setting(current: u64) -> Result<Option<u64>> {
    let prompt = CustomType::<u64>::new("Seconds to wait for the model:")
        .with_default(current)
        .with_error_message("Type a whole number of seconds")
        .with_validator(accept(|seconds| *seconds > 0, "Wait at least 1 second"));
    answer(prompt.prompt_skippable())
}

/// Explains the pattern file opened in the editor. Lines starting with '#' are ignored.
const EXCLUDE_HELP: &str = "\
# Files whose diffs git-agent leaves out, one pattern per line. They still
# appear in the list of changed files. Patterns without a \"/\" match the file
# name in any directory; \"*\" wildcards are allowed, e.g. *.min.js or dist/**.
# Lines starting with '#' are ignored.
";

/// Asks what to do when changes add what look like secrets.
fn on_secrets_setting(current: OnSecrets) -> Result<Option<OnSecrets>> {
    ui::info(ui::dim(
        "   git-agent looks for API keys, private keys and passwords in what it commits and\n   \
         pushes. Either way, the model gets [REDACTED] in their place.",
    ));
    let options = [
        "Reject: don't commit, push or create the pull request",
        "Warn: list them and carry on",
    ];
    let start = match current {
        OnSecrets::Reject => 0,
        OnSecrets::Warn => 1,
    };
    Ok(match pick("When changes add secrets:", &options, start)? {
        Some(0) => Some(OnSecrets::Reject),
        Some(_) => Some(OnSecrets::Warn),
        None => None,
    })
}

fn exclude_setting(current: &[String]) -> Result<Option<Vec<String>>> {
    let options = [
        "Edit the patterns in your editor",
        "Use the defaults (common lockfiles)",
    ];
    match pick("Excluded files:", &options, 0)? {
        None => return Ok(None),
        Some(1) => return Ok(Some(default_exclude())),
        Some(_) => {}
    }
    let text = format!("{EXCLUDE_HELP}\n{}\n", current.join("\n"));
    Ok(Some(parse_patterns(&edit_text("excluded-files", &text)?)))
}

/// One pattern per line, without blank lines and '#' comments.
fn parse_patterns(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty() && !pattern.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

fn system_prompt_setting(current: &str) -> Result<Option<String>> {
    let options = [
        "Edit it in your editor",
        "Use the default (Conventional Commits)",
    ];
    match pick("System prompt:", &options, 0)? {
        None => return Ok(None),
        Some(1) => return Ok(Some(DEFAULT_SYSTEM_PROMPT.to_owned())),
        Some(_) => {}
    }
    let edited = edit_text("system-prompt", &format!("{}\n", current.trim()))?;
    let edited = edited.trim();
    if edited.is_empty() {
        ui::warn("the system prompt was empty; keeping the previous one");
        return Ok(None);
    }
    Ok(Some(edited.to_owned()))
}

/// Opens `text` in the user's git editor and returns what they saved.
fn edit_text(name: &str, text: &str) -> Result<String> {
    edit_text_with(name, text, git::edit_file)
}

/// Writes `text` to a file in a private folder, runs `open` on the file, and
/// returns what it holds then.
fn edit_text_with(
    name: &str,
    text: &str,
    open: impl FnOnce(&Path) -> Result<()>,
) -> Result<String> {
    let folder = PrivateDir::create("edit")?;
    let path = folder.path().join(format!("{name}.txt"));
    fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))?;
    open(&path)?;
    fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_what_the_editor_saved_from_a_private_folder() {
        let saved = edit_text_with("notes", "before", |path| {
            assert_eq!(fs::read_to_string(path).unwrap(), "before");
            assert_eq!(path.file_name().unwrap(), "notes.txt");
            let folder = path.parent().unwrap();
            let mode = std::os::unix::fs::PermissionsExt::mode(
                &fs::metadata(folder).unwrap().permissions(),
            );
            assert_eq!(mode & 0o777, 0o700);
            fs::write(path, "after").map_err(Into::into)
        });
        assert_eq!(saved.unwrap(), "after");
    }

    #[test]
    fn returns_what_an_editor_saved_by_replacing_the_file() {
        let saved = edit_text_with("notes", "before", |path| {
            let replacement = path.with_extension("new");
            fs::write(&replacement, "replaced")?;
            fs::rename(&replacement, path).map_err(Into::into)
        });
        assert_eq!(saved.unwrap(), "replaced");
    }

    #[test]
    fn removes_the_folder_after_the_edit_and_after_a_failed_editor() {
        let mut seen = None;
        edit_text_with("notes", "text", |path| {
            seen = Some(path.parent().unwrap().to_owned());
            Ok(())
        })
        .unwrap();
        assert!(!seen.as_ref().unwrap().exists());

        let mut seen = None;
        let failed = edit_text_with("notes", "text", |path| {
            seen = Some(path.parent().unwrap().to_owned());
            bail!("the editor failed")
        });
        assert!(failed.is_err());
        assert!(!seen.as_ref().unwrap().exists());
    }

    #[test]
    fn fails_when_the_editor_leaves_no_file() {
        let edited = edit_text_with("notes", "text", |path| {
            fs::remove_file(path).map_err(Into::into)
        });
        assert!(edited.is_err());
    }

    #[test]
    fn parses_one_pattern_per_line() {
        let text = format!("{EXCLUDE_HELP}\n Cargo.lock\n\n*.min.js \n# dist/**\ndocs/**\n");
        assert_eq!(parse_patterns(&text), ["Cargo.lock", "*.min.js", "docs/**"]);
        assert!(parse_patterns(EXCLUDE_HELP).is_empty());
    }

    #[test]
    fn labels_show_provider_names() {
        assert_eq!(
            provider_label("https://api.openai.com/v1"),
            "OpenAI · https://api.openai.com/v1"
        );
        assert_eq!(
            provider_label("http://10.0.0.5:1234/v1"),
            "other · http://10.0.0.5:1234/v1"
        );
    }

    #[test]
    fn update_reports_changes() {
        let mut value = 1;
        assert!(!update(&mut value, None));
        assert!(!update(&mut value, Some(1)));
        assert!(update(&mut value, Some(2)));
        assert_eq!(value, 2);
    }

    #[test]
    fn model_options_show_what_the_server_reports() {
        let model = ModelInfo {
            id: "gemma".into(),
            loaded: Some(true),
            context_length: Some(131072),
        };
        assert_eq!(
            ModelOption::Listed(model).to_string(),
            "gemma  (loaded, 131,072 tokens)"
        );
    }
}
