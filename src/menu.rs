//! The interactive settings menu, `git agent config`.
//!
//! Each change is saved to the config file as soon as it is made, and only the setting that
//! changed is written: the others stay as the file has them, set or unset. API keys go
//! to the macOS keychain instead (see `api_keys.rs`).

mod answer;

use crate::config::{
    self, Config, ConfigFile, DEFAULT_EXCLUDE, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_SYSTEM_PROMPT,
    DEFAULT_TIMEOUT_SECS, LOCAL_TEMPERATURE, MIN_CONTEXT_LENGTH, MIN_OUTPUT_TOKENS, OnSecrets,
    Overrides,
};
use crate::providers::{self, PROVIDERS};
use crate::safe_text::one_line;
use crate::server::{ModelInfo, ModelServer, Server};
use crate::temp::PrivateDir;
use crate::ui::{self, thousands};
use crate::{api_keys, git};
use answer::{Answer, ModelChoice, ProviderChoice};
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
    /// What the file sets, which is all the menu writes back.
    file: ConfigFile,
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
    let file = ConfigFile::read(&path)?.unwrap_or_default();
    let mut menu = Menu {
        overrides,
        path,
        file,
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
        let config = menu.config();
        let labels: Vec<String> = ITEMS
            .iter()
            .map(|&item| menu.label(item, &config))
            .collect();
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
            Ok(true) => match menu.file.save(&menu.path) {
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

impl Menu<'_> {
    fn label(&self, item: Item, config: &Config) -> String {
        match item {
            Item::Done => item.name().to_owned(),
            _ => format!("{:NAME_WIDTH$}{}", item.name(), self.value(item, config)),
        }
    }

    fn config(&self) -> Config {
        self.file.clone().resolve()
    }

    /// The setting as the menu shows it. A setting the file doesn't set says it is the default.
    fn value(&self, item: Item, config: &Config) -> String {
        let file = &self.file;
        match item {
            Item::Provider => {
                noted_default(provider_label(&config.base_url), file.base_url.is_none())
            }
            Item::Model => model_label(config),
            Item::ApiKey => api_key_label(self.key, providers::is_local(&config.base_url)),
            Item::ContextLength => context_length_label(config),
            Item::OutputLimit => output_limit_label(config, file.max_output_tokens.is_none()),
            Item::Temperature => temperature_label(config),
            Item::Timeout => noted_default(
                format!("{} seconds", config.timeout_secs),
                file.timeout_secs.is_none(),
            ),
            Item::Secrets => {
                noted_default(secrets_label(config.on_secrets), file.on_secrets.is_none())
            }
            Item::Exclude => noted_default(exclude_label(&config.exclude), file.exclude.is_none()),
            Item::SystemPrompt => system_prompt_label(&config.system_prompt),
            Item::Done => String::new(),
        }
    }

    /// Asks for a new value of `item` and writes it into the file in memory. Returns whether
    /// the file changed.
    async fn change(&mut self, item: Item) -> Result<bool> {
        let changed = match self.ask(item).await? {
            Some(answer) => answer.write_into(&mut self.file),
            None => false,
        };
        if item == Item::Provider {
            self.check_key();
        }
        Ok(changed)
    }

    /// Asks for a new value of `item`, or returns `None` if the user went back.
    async fn ask(&mut self, item: Item) -> Result<Option<Answer>> {
        let config = self.config();
        let answer = match item {
            Item::Provider => self.choose_provider().await?.map(Answer::Provider),
            Item::Model => {
                let api_key = flag_key(self.overrides)
                    .or_else(|| api_keys::get(&config.base_url).ok().flatten());
                let local = providers::is_local(&config.base_url);
                choose_model(&config, api_key, local)
                    .await?
                    .map(Answer::Model)
            }
            Item::ApiKey => {
                self.change_api_key()?;
                None
            }
            Item::ContextLength => {
                context_length_setting(self.file.context_length)?.map(Answer::ContextLength)
            }
            Item::OutputLimit => {
                output_limit_setting(self.file.max_output_tokens)?.map(Answer::OutputLimit)
            }
            Item::Temperature => {
                temperature_setting(self.file.temperature)?.map(Answer::Temperature)
            }
            Item::Timeout => timeout_setting(self.file.timeout_secs)?.map(Answer::Timeout),
            Item::Secrets => on_secrets_setting(self.file.on_secrets)?.map(Answer::Secrets),
            Item::Exclude => exclude_setting(&config.exclude)?.map(Answer::Exclude),
            Item::SystemPrompt => {
                system_prompt_setting(&config.system_prompt)?.map(Answer::SystemPrompt)
            }
            Item::Done => None,
        };
        Ok(answer)
    }

    fn check_key(&mut self) {
        self.key = if flag_key(self.overrides).is_some() {
            Key::Flag
        } else {
            match api_keys::get(&self.config().base_url) {
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
    /// Returns `None` if the user went back.
    async fn choose_provider(&self) -> Result<Option<ProviderChoice>> {
        let mut config = self.config();
        let current = providers::find(&config.base_url);
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
                    &config.base_url
                } else {
                    provider.base_url
                };
                ask_url(&format!("{} address:", provider.name), initial)?
            }
            None => {
                let initial = if current.is_none() {
                    config.base_url.as_str()
                } else {
                    ""
                };
                ask_url("Base URL, including /v1:", initial)?
            }
        };
        let Some(base_url) = base_url else {
            return Ok(None);
        };

        if config.base_url != base_url {
            config.base_url = base_url.clone();
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
        let drops_temperature = match config.temperature {
            Some(temperature) if !local => stop_sending_temperature(temperature)?,
            _ => false,
        };
        Ok(Some(ProviderChoice {
            base_url,
            model: choice.model,
            context_length: choice.context_length,
            drops_temperature,
        }))
    }

    fn change_api_key(&mut self) -> Result<()> {
        if !api_keys::SUPPORTED {
            ui::info(
                "   git-agent can save API keys only in the macOS keychain. \
                 Set the GIT_AGENT_API_KEY environment variable instead.",
            );
            return Ok(());
        }
        let base_url = self.config().base_url;
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

fn stop_sending_temperature(temperature: f32) -> Result<bool> {
    let question = format!(
        "Many cloud models accept only their default temperature. \
         Stop sending temperature {temperature}?"
    );
    let prompt = Confirm::new(&question).with_default(true);
    Ok(answer(prompt.prompt_skippable())?.unwrap_or(false))
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
    let base_url = one_line(base_url);
    match providers::find(&base_url) {
        Some(provider) => format!("{} · {base_url}", provider.name),
        None => format!("other · {base_url}"),
    }
}

/// `label`, saying that it is the default when the file doesn't set the setting.
fn noted_default(label: String, unset: bool) -> String {
    if unset {
        format!("{label} (default)")
    } else {
        label
    }
}

fn model_label(config: &Config) -> String {
    match (&config.model, providers::is_local(&config.base_url)) {
        (Some(model), _) => one_line(model),
        (None, true) => "automatic: the model loaded on the server".to_owned(),
        (None, false) => "not set".to_owned(),
    }
}

fn api_key_label(key: Key, local: bool) -> String {
    match (key, local) {
        (Key::Flag, _) => "from --api-key or GIT_AGENT_API_KEY".to_owned(),
        (Key::Keychain, _) => "saved in the macOS keychain".to_owned(),
        (Key::Missing, true) => "none (local servers usually need none)".to_owned(),
        (Key::Missing, false) => "not set".to_owned(),
    }
}

fn context_length_label(config: &Config) -> String {
    match config.context_length {
        Some(tokens) => format!("{} tokens", thousands(tokens)),
        None => "automatic: asked from the server".to_owned(),
    }
}

fn output_limit_label(config: &Config, unset: bool) -> String {
    let tokens = thousands(config.max_output_tokens);
    let notes: Vec<&str> = [
        unset.then_some("default"),
        (!providers::is_anthropic(&config.base_url)).then_some("sent only to Anthropic"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if notes.is_empty() {
        format!("{tokens} tokens")
    } else {
        format!("{tokens} tokens ({})", notes.join(", "))
    }
}

fn temperature_label(config: &Config) -> String {
    match config.effective_temperature() {
        _ if providers::is_anthropic(&config.base_url) => {
            "not used: Anthropic's API takes none".to_owned()
        }
        Some(temperature) if config.temperature.is_none() => {
            format!("automatic: {temperature} on local servers")
        }
        Some(temperature) => temperature.to_string(),
        None => "automatic: the provider's default".to_owned(),
    }
}

fn secrets_label(on_secrets: OnSecrets) -> String {
    match on_secrets {
        OnSecrets::Reject => "reject: don't commit or push them".to_owned(),
        OnSecrets::Warn => "warn: list them and carry on".to_owned(),
    }
}

fn exclude_label(exclude: &[String]) -> String {
    match exclude.len() {
        _ if exclude == default_exclude() => "common lockfiles".to_owned(),
        0 => "none".to_owned(),
        1 => "1 pattern".to_owned(),
        count => format!("{count} patterns"),
    }
}

fn system_prompt_label(prompt: &str) -> String {
    if prompt.trim() == DEFAULT_SYSTEM_PROMPT.trim() {
        "default: Conventional Commits".to_owned()
    } else {
        "custom".to_owned()
    }
}

fn default_exclude() -> Vec<String> {
    DEFAULT_EXCLUDE
        .iter()
        .map(|pattern| pattern.to_string())
        .collect()
}

fn ask_url(message: &str, initial: &str) -> Result<Option<String>> {
    let initial = one_line(initial);
    let prompt = Text::new(message)
        .with_initial_value(&initial)
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
    let message = format!("API key for {}:", one_line(&name));
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
    let message = format!("Context window of {}, in tokens:", one_line(model));
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

/// Asks whether to leave a setting unset, which gives it its default or an automatic value,
/// or to set a value, and asks for the value with `ask`. `unset_option` is the text of the
/// first option. Returns `Some(None)` for unset, `None` if the user went back. The cursor
/// starts on the option that matches `current`, the value the file sets.
fn default_or<T>(
    message: &str,
    unset_option: &str,
    current: Option<T>,
    ask: impl FnOnce(Option<T>) -> Result<Option<T>>,
) -> Result<Option<Option<T>>> {
    let options = [unset_option, "Set a value…"];
    let picked = pick(message, &options, option_for(current.as_ref()))?;
    decide(picked, current, ask)
}

/// The option the cursor starts on: the first, the default, unless the setting has a value.
fn option_for<T>(current: Option<&T>) -> usize {
    usize::from(current.is_some())
}

/// What the picked option means: `None` if the user went back, `Some(None)` for the default,
/// and otherwise the value `ask` gets from them, which starts from `current`.
fn decide<T>(
    picked: Option<usize>,
    current: Option<T>,
    ask: impl FnOnce(Option<T>) -> Result<Option<T>>,
) -> Result<Option<Option<T>>> {
    match picked {
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
    default_or("Context length:", &automatic, current, |current| {
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
    default_or("Temperature:", &automatic, current, |current| {
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

fn output_limit_setting(current: Option<usize>) -> Result<Option<Option<usize>>> {
    let default = thousands(DEFAULT_MAX_OUTPUT_TOKENS);
    let automatic = format!("Default ({default} tokens)");
    default_or("Output limit:", &automatic, current, |current| {
        let error = format!("Use at least {MIN_OUTPUT_TOKENS} tokens");
        let help = "much higher values can take longer than the timeout";
        let prompt = CustomType::<usize>::new("Most tokens per reply, thinking included:")
            .with_default(current.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS))
            .with_help_message(help)
            .with_error_message("Type a whole number of tokens, e.g. 16000")
            .with_validator(accept(|tokens| *tokens >= MIN_OUTPUT_TOKENS, error));
        answer(prompt.prompt_skippable())
    })
}

fn timeout_setting(current: Option<u64>) -> Result<Option<Option<u64>>> {
    let automatic = format!("Default ({DEFAULT_TIMEOUT_SECS} seconds)");
    default_or("Timeout:", &automatic, current, |current| {
        let prompt = CustomType::<u64>::new("Seconds to wait for the model:")
            .with_default(current.unwrap_or(DEFAULT_TIMEOUT_SECS))
            .with_error_message("Type a whole number of seconds")
            .with_validator(accept(|seconds| *seconds > 0, "Wait at least 1 second"));
        answer(prompt.prompt_skippable())
    })
}

/// Explains the pattern file opened in the editor. Lines starting with '#' are ignored.
const EXCLUDE_HELP: &str = "\
# Files whose diffs git-agent leaves out, one pattern per line. They still
# appear in the list of changed files. Patterns without a \"/\" match the file
# name in any directory; \"*\" wildcards are allowed, e.g. *.min.js or dist/**.
# Lines starting with '#' are ignored.
";

/// Asks what to do when changes add what look like secrets.
fn on_secrets_setting(current: Option<OnSecrets>) -> Result<Option<Option<OnSecrets>>> {
    let default = format!("Default ({})", OnSecrets::default().as_str());
    default_or("When changes add secrets:", &default, current, |current| {
        ui::info(ui::dim(
            "   git-agent looks for API keys, private keys and passwords in what it commits and\n   \
             pushes. Either way, the model gets [REDACTED] in their place.",
        ));
        let options = [
            "Reject: don't commit, push or create the pull request",
            "Warn: list them and carry on",
        ];
        let start = secrets_option(current.unwrap_or_default());
        let picked = pick("When changes add secrets:", &options, start)?;
        Ok(picked.map(secrets_for_option))
    })
}

/// The position of `on_secrets` among the options of the secrets prompt.
fn secrets_option(on_secrets: OnSecrets) -> usize {
    match on_secrets {
        OnSecrets::Reject => 0,
        OnSecrets::Warn => 1,
    }
}

/// The setting for the option at `option` of the secrets prompt.
fn secrets_for_option(option: usize) -> OnSecrets {
    match option {
        0 => OnSecrets::Reject,
        _ => OnSecrets::Warn,
    }
}

/// Asks how to change the excluded files. Returns the new setting, which is `None` for the
/// defaults, or `None` if the user went back or left the patterns as they were.
fn exclude_setting(current: &[String]) -> Result<Option<Option<Vec<String>>>> {
    let options = [
        "Edit the patterns in your editor",
        "Use the defaults (common lockfiles)",
    ];
    match pick("Excluded files:", &options, 0)? {
        None => return Ok(None),
        Some(1) => return Ok(Some(None)),
        Some(_) => {}
    }
    let text = format!("{EXCLUDE_HELP}\n{}\n", current.join("\n"));
    let edited = parse_patterns(&edit_text("excluded-files", &text)?);
    Ok(edited_value(current, edited.as_slice()))
}

/// The setting to write after the user edited `current` in the editor: nothing if they left
/// it as it was, since closing the editor is not a choice of the value.
fn edited_value<T: PartialEq + ToOwned + ?Sized>(
    current: &T,
    edited: &T,
) -> Option<Option<T::Owned>> {
    (edited != current).then(|| Some(edited.to_owned()))
}

/// One pattern per line, without blank lines and '#' comments.
fn parse_patterns(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty() && !pattern.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// Asks how to change the system prompt. Returns the new setting, which is `None` for the
/// default, or `None` if the user went back or left the prompt as it was.
fn system_prompt_setting(current: &str) -> Result<Option<Option<String>>> {
    let options = [
        "Edit it in your editor",
        "Use the default (Conventional Commits)",
    ];
    match pick("System prompt:", &options, 0)? {
        None => return Ok(None),
        Some(1) => return Ok(Some(None)),
        Some(_) => {}
    }
    let edited = edit_text("system-prompt", &format!("{}\n", current.trim()))?;
    let edited = edited.trim();
    if edited.is_empty() {
        ui::warn("the system prompt was empty; keeping the previous one");
        return Ok(None);
    }
    Ok(edited_value(current.trim(), edited))
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
    fn the_cursor_starts_on_the_default_while_a_setting_is_unset() {
        assert_eq!(option_for::<u64>(None), 0);
        assert_eq!(option_for(Some(&300_u64)), 1);
        assert_eq!(option_for(Some(&OnSecrets::Reject)), 1);
    }

    fn never_asked<T>(_: Option<T>) -> Result<Option<T>> {
        panic!("the user was asked for a value");
    }

    #[test]
    fn the_secrets_options_and_the_settings_match() {
        for on_secrets in [OnSecrets::Reject, OnSecrets::Warn] {
            assert_eq!(secrets_for_option(secrets_option(on_secrets)), on_secrets);
        }
        assert_eq!(secrets_option(OnSecrets::default()), 0);
        assert_eq!(secrets_for_option(0), OnSecrets::Reject);
        assert_eq!(secrets_for_option(1), OnSecrets::Warn);
    }

    #[test]
    fn going_back_decides_nothing() {
        assert_eq!(decide(None, None::<u64>, never_asked).unwrap(), None);
        assert_eq!(decide(None, Some(5_u64), never_asked).unwrap(), None);
    }

    #[test]
    fn the_default_option_unsets_a_setting_without_asking() {
        assert_eq!(
            decide(Some(0), None::<u64>, never_asked).unwrap(),
            Some(None)
        );
        assert_eq!(
            decide(Some(0), Some(5_u64), never_asked).unwrap(),
            Some(None)
        );
    }

    #[test]
    fn setting_a_value_asks_and_starts_from_what_the_file_holds() {
        let asked_with = std::cell::Cell::new(None);
        let ask = |current: Option<u64>| {
            asked_with.set(Some(current));
            Ok(Some(7))
        };
        assert_eq!(decide(Some(1), None, ask).unwrap(), Some(Some(7)));
        assert_eq!(asked_with.take(), Some(None));
        let ask = |current: Option<u64>| {
            asked_with.set(Some(current));
            Ok(Some(8))
        };
        assert_eq!(decide(Some(1), Some(5), ask).unwrap(), Some(Some(8)));
        assert_eq!(asked_with.take(), Some(Some(5)));
    }

    #[test]
    fn going_back_from_the_value_prompt_decides_nothing() {
        let back = |_: Option<u64>| Ok(None);
        assert_eq!(decide(Some(1), Some(5), back).unwrap(), None);
    }

    #[test]
    fn a_failure_while_asking_is_passed_on() {
        let broken = |_: Option<u64>| -> Result<Option<u64>> { bail!("no terminal") };
        assert!(decide(Some(1), None, broken).is_err());
    }

    fn menu_for<'a>(overrides: &'a Overrides, file: ConfigFile) -> Menu<'a> {
        Menu {
            overrides,
            path: PathBuf::from("config.toml"),
            file,
            key: Key::Missing,
        }
    }

    fn labels_of(file: ConfigFile) -> Vec<String> {
        let overrides = Overrides::default();
        let menu = menu_for(&overrides, file);
        let config = menu.config();
        ITEMS
            .iter()
            .map(|&item| menu.label(item, &config))
            .collect()
    }

    fn marked_default(labels: &[String]) -> Vec<&str> {
        ITEMS
            .iter()
            .zip(labels)
            .filter(|(_, label)| label.contains("(default"))
            .map(|(item, _)| item.name())
            .collect()
    }

    #[test]
    fn unset_settings_say_that_they_are_the_default() {
        let labels = labels_of(ConfigFile::default());
        assert_eq!(
            marked_default(&labels),
            [
                "Provider",
                "Output limit",
                "Timeout",
                "Secrets",
                "Excluded files"
            ]
        );
        let timeout = labels
            .iter()
            .find(|label| label.starts_with("Timeout"))
            .unwrap();
        assert!(timeout.ends_with("300 seconds (default)"), "{timeout}");
    }

    #[test]
    fn set_settings_do_not_say_so() {
        let file = ConfigFile {
            base_url: Some(config::DEFAULT_BASE_URL.into()),
            max_output_tokens: Some(DEFAULT_MAX_OUTPUT_TOKENS),
            timeout_secs: Some(DEFAULT_TIMEOUT_SECS),
            on_secrets: Some(OnSecrets::Reject),
            exclude: Some(vec!["*.min.js".into()]),
            ..ConfigFile::default()
        };
        let labels = labels_of(file);
        assert_eq!(marked_default(&labels), Vec::<&str>::new());
    }

    #[test]
    fn a_label_built_from_the_file_holds_no_escape_sequence() {
        let file = ConfigFile {
            model: Some("evil\u{1b}[2Jmodel".into()),
            base_url: Some("http://host\u{1b}]0;title\u{7}/v1".into()),
            ..ConfigFile::default()
        };
        for label in labels_of(file) {
            assert!(!label.contains('\u{1b}'), "{label:?}");
            assert!(!label.contains('\u{7}'), "{label:?}");
        }
        assert!(!provider_label("http://a\u{1b}[31m/v1").contains('\u{1b}'));
    }

    #[test]
    fn leaving_the_editor_without_a_change_chooses_nothing() {
        let current = ["Cargo.lock".to_owned()];
        assert_eq!(edited_value(current.as_slice(), current.as_slice()), None);
        let edited = ["Cargo.lock".to_owned(), "*.min.js".to_owned()];
        assert_eq!(
            edited_value(current.as_slice(), edited.as_slice()),
            Some(Some(edited.to_vec()))
        );
        assert_eq!(edited_value("same", "same"), None);
        assert_eq!(
            edited_value("before", "after"),
            Some(Some("after".to_owned()))
        );
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
