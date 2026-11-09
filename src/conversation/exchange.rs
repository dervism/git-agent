//! Sending the conversation to the model and reading its reply: connecting, generating a
//! message, and a discussion with follow-up questions.

use super::models::{self, Model};
use super::{clean_reply, estimate_tokens, follow_up, has_room, outgrew_context, separate_subject};
use crate::config::Config;
use crate::server::{Message, ModelServer, Request, Server};
use crate::{safe_text, ui};
use anyhow::{Result, bail};
use std::io::{self, IsTerminal};

/// Connects to the configured server and picks the model to use.
pub async fn connect(config: &Config) -> Result<(Server, Model)> {
    let server = Server::connect(config)?;
    if config.temperature.is_some() && !server.takes_temperature() {
        ui::warn("Anthropic's API takes no temperature, so git-agent doesn't send the one you set");
    }
    let model = models::resolve(&server, config).await?;
    Ok((server, model))
}

/// Asks the model, with control and invisible characters escaped on the way out and
/// taken out of its reply, so that what the user reviews is what gets committed.
pub async fn generate_message(
    server: &impl ModelServer,
    model: &Model,
    conversation: &[Message],
) -> Result<String> {
    let request = Request::new(&model.id, conversation);
    if outgrew_context(
        conversation,
        request.messages(),
        model.context_length,
        model.max_output_tokens,
    ) {
        bail!(
            "the changes hold so many control or invisible characters that, escaped, they no \
             longer fit in the model's context"
        );
    }
    let reply = {
        let _spinner = ui::Spinner::start(format!(
            "Generating with {}",
            safe_text::one_line(&model.id)
        ));
        server.complete(&request).await?
    };
    let cleaned = clean_reply(&reply.text);
    if cleaned.removed > 0 {
        ui::warn(format!(
            "the model's reply held {} control or invisible character(s); git-agent removed them",
            cleaned.removed
        ));
    }
    if cleaned.text.is_empty() {
        bail!("the model returned an empty message");
    }
    Ok(cleaned.text)
}

/// A commit message from the model, with its subject apart from its body.
pub async fn commit_message(
    server: &impl ModelServer,
    model: &Model,
    conversation: &[Message],
) -> Result<String> {
    generate_message(server, model, conversation)
        .await
        .and_then(separated)
}

/// A review's way of tidying a new commit message (see [`commit_message`]).
pub fn separated(message: String) -> Result<String> {
    Ok(separate_subject(&message))
}

/// Shows the model's answer to `conversation` under `heading`, then answers
/// follow-up questions until the user is done or the context is full.
pub async fn discuss(
    server: &impl ModelServer,
    model: &Model,
    config: &Config,
    heading: &str,
    mut conversation: Vec<Message>,
) -> Result<()> {
    let mut answer = generate_message(server, model, &conversation).await?;
    ui::show_answer(heading, &answer);
    if !io::stdin().is_terminal() || !ui::stdout_is_terminal() {
        return Ok(());
    }
    while let Some(question) = ui::ask_follow_up()? {
        let next = follow_up(&conversation, &answer, &question);
        if !has_room(&next, model.context_length, config.max_output_tokens) {
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

pub fn conversation_tokens(conversation: &[Message]) -> usize {
    conversation
        .iter()
        .map(|message| estimate_tokens(&message.content))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::conversation::models::ContextSource;
    use crate::server::test_support::FakeServer;

    fn model(context_length: usize) -> Model {
        Model {
            id: "fake".to_owned(),
            picked: None,
            context_length,
            context_source: ContextSource::Config,
            max_output_tokens: config::DEFAULT_MAX_OUTPUT_TOKENS,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sends_the_model_escaped_text_with_letters_and_line_endings_intact() {
        let server = FakeServer::replying("feat: x");
        let conversation = [Message::user("+ blåbær\r\n+ \u{e0041}\u{1b}[2J")];

        generate_message(&server, &model(100_000), &conversation)
            .await
            .unwrap();

        let sent = server.received.borrow();
        assert_eq!(
            sent[0][0].content,
            "+ blåbær\r\n+ \\363\\240\\201\\201\\033[2J"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn refuses_a_reply_of_only_hidden_characters() {
        let server = FakeServer::replying("\u{1b}\u{202e}");
        let error = generate_message(&server, &model(100_000), &[Message::user("x")])
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "the model returned an empty message");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn says_why_when_escaping_overfills_the_context() {
        let server = FakeServer::replying("feat: x");
        let conversation = [Message::user("\u{1b}".repeat(4000))];
        let error = generate_message(&server, &model(3000), &conversation)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no longer fit"), "{error}");
        assert!(server.received.borrow().is_empty());
    }
}
