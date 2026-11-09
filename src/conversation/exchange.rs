//! Sending the conversation to the model and reading its reply: connecting, generating a
//! message, and a discussion with follow-up questions with the person it's for.

use super::messages::follow_up;
use super::models::{self, Model};
use super::reply::{clean_reply, separated};
use crate::config::Config;
use crate::server::{Message, ModelServer, Request, Server};
use crate::{safe_text, ui};
use anyhow::{Result, bail};

/// The person a discussion with the model is for.
pub trait Discussant {
    /// Whether the person can ask follow-up questions: they type at a terminal and read the
    /// answers on one.
    fn is_interactive(&self) -> bool;

    /// Shows the person the model's answer, under `heading`, or fails if they can't be shown
    /// it, as when the program that reads the output has closed it.
    fn show_answer(&self, heading: &str, answer: &str) -> Result<()>;

    /// The follow-up question the person types, or `None` at end of input. An empty one
    /// ends the discussion too.
    fn ask_follow_up(&self) -> Result<Option<String>>;
}

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
    let request = Request::escaped(&model.id, conversation);
    if model
        .budget()
        .outgrown_by_escaping(conversation, request.messages())
    {
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

/// Shows `discussant` the model's answer to `conversation` under `heading`, then answers
/// their follow-up questions until they are done or the context is full.
pub async fn discuss(
    server: &impl ModelServer,
    model: &Model,
    heading: &str,
    mut conversation: Vec<Message>,
    discussant: &dyn Discussant,
) -> Result<()> {
    let mut answer = generate_message(server, model, &conversation).await?;
    discussant.show_answer(heading, &answer)?;
    if !discussant.is_interactive() {
        return Ok(());
    }
    while let Some(question) = discussant
        .ask_follow_up()?
        .filter(|question| !question.is_empty())
    {
        let next = follow_up(&conversation, &answer, &question);
        if !model.budget().has_room_for_a_reply(&next) {
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
                discussant.show_answer(heading, &answer)?;
            }
            Err(err) => ui::error(&err),
        }
    }
    Ok(())
}

/// A person for tests.
#[cfg(test)]
pub mod test_support {
    use super::Discussant;
    use crate::ui::OutputClosed;
    use anyhow::Result;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// A person who types the given lines in turn, trimmed as the terminal trims them, and
    /// keeps every answer they were shown.
    pub struct ScriptedPerson {
        interactive: bool,
        closes_after: Option<usize>,
        lines: RefCell<VecDeque<String>>,
        pub shown: RefCell<Vec<String>>,
    }

    impl ScriptedPerson {
        /// At a terminal, typing `lines`, and then nothing more.
        pub fn typing(lines: &[&str]) -> Self {
            Self {
                interactive: true,
                closes_after: None,
                lines: RefCell::new(lines.iter().map(|&line| line.to_owned()).collect()),
                shown: RefCell::new(Vec::new()),
            }
        }

        /// Whose output closes after `answers` answers were shown to them: the program reading
        /// it has gone away.
        pub fn with_the_output_closed_after(self, answers: usize) -> Self {
            Self {
                closes_after: Some(answers),
                ..self
            }
        }

        /// Not at a terminal, with input from a pipe that holds `lines`.
        pub fn piping(lines: &[&str]) -> Self {
            Self {
                interactive: false,
                ..Self::typing(lines)
            }
        }

        /// Not at a terminal, and with nothing to read.
        pub fn away() -> Self {
            Self::piping(&[])
        }

        /// The next line the person types, or `None` once the lines run out.
        pub fn next_line(&self) -> Option<String> {
            let line = self.lines.borrow_mut().pop_front();
            line.map(|line| line.trim().to_owned())
        }

        /// The lines not typed yet.
        pub fn lines_left(&self) -> usize {
            self.lines.borrow().len()
        }
    }

    impl Discussant for ScriptedPerson {
        fn is_interactive(&self) -> bool {
            self.interactive
        }

        fn show_answer(&self, _heading: &str, answer: &str) -> Result<()> {
            if self.closes_after == Some(self.shown.borrow().len()) {
                return Err(OutputClosed.into());
            }
            self.shown.borrow_mut().push(answer.to_owned());
            Ok(())
        }

        fn ask_follow_up(&self) -> Result<Option<String>> {
            Ok(self.next_line())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::ScriptedPerson;
    use super::*;
    use crate::conversation::test_support::configured;
    use crate::server::Role;
    use crate::server::test_support::FakeServer;
    use crate::ui::OutputClosed;
    use anyhow::anyhow;
    use std::cell::Cell;

    /// A model that answers each question with its number, and fails at the `failing`th.
    fn numbering(failing: usize) -> impl Fn(&[Message]) -> Result<String> {
        let asked = Cell::new(0);
        move |_| {
            asked.set(asked.get() + 1);
            match asked.get() {
                n if n == failing => Err(anyhow!("the server went away")),
                n => Ok(format!("answer {n}")),
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn answers_follow_up_questions_until_an_empty_one() {
        let server = FakeServer::answering(numbering(0));
        let person = ScriptedPerson::typing(&["Why?", "  And then?  ", "", "never asked"]);
        let conversation = vec![Message::user("explain")];

        discuss(&server, &configured(100_000), "H", conversation, &person)
            .await
            .unwrap();

        assert_eq!(*person.shown.borrow(), ["answer 1", "answer 2", "answer 3"]);
        assert_eq!(person.lines_left(), 1);
        let last = server.received.borrow()[2].clone();
        let turns: Vec<(Role, &str)> = last
            .iter()
            .map(|message| (message.role, message.content.as_str()))
            .collect();
        assert_eq!(
            turns,
            [
                (Role::User, "explain"),
                (Role::Assistant, "answer 1"),
                (Role::User, "Why?"),
                (Role::Assistant, "answer 2"),
                (Role::User, "And then?"),
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn asks_nothing_of_a_person_away_from_the_terminal() {
        let server = FakeServer::replying("the answer");
        let person = ScriptedPerson::piping(&["never asked"]);

        discuss(&server, &configured(100_000), "H", vec![], &person)
            .await
            .unwrap();

        assert_eq!(*person.shown.borrow(), ["the answer"]);
        assert_eq!(server.received.borrow().len(), 1);
        assert_eq!(person.lines_left(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn forgets_a_question_whose_answer_failed_and_takes_the_next() {
        let server = FakeServer::answering(numbering(2));
        let person = ScriptedPerson::typing(&["lost", "kept"]);

        discuss(&server, &configured(100_000), "H", vec![], &person)
            .await
            .unwrap();

        assert_eq!(*person.shown.borrow(), ["answer 1", "answer 3"]);
        let sent = server.received.borrow();
        let last_question = |turn: usize| sent[turn].last().unwrap().content.clone();
        assert_eq!(last_question(1), "lost");
        assert_eq!(last_question(2), "kept");
        assert!(sent[2].iter().all(|message| message.content != "lost"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stops_when_the_next_question_no_longer_fits() {
        let server = FakeServer::replying("the answer");
        let long = "word ".repeat(3000);
        let person = ScriptedPerson::typing(&[&long, "never asked"]);

        discuss(&server, &configured(4096), "H", vec![], &person)
            .await
            .unwrap();

        assert_eq!(*person.shown.borrow(), ["the answer"]);
        assert_eq!(server.received.borrow().len(), 1);
        assert_eq!(person.lines_left(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fails_when_the_first_answer_fails() {
        let server = FakeServer::answering(numbering(1));
        let person = ScriptedPerson::typing(&["never asked"]);

        let error = discuss(&server, &configured(100_000), "H", vec![], &person)
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "the server went away");
        assert!(person.shown.borrow().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stops_when_the_first_answer_cant_be_shown_and_asks_nothing() {
        let server = FakeServer::replying("the answer");
        let person = ScriptedPerson::typing(&["never asked"]).with_the_output_closed_after(0);

        let error = discuss(&server, &configured(100_000), "H", vec![], &person)
            .await
            .unwrap_err();

        assert!(error.is::<OutputClosed>());
        assert_eq!(person.lines_left(), 1);
        assert_eq!(server.received.borrow().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stops_when_an_answer_to_a_follow_up_cant_be_shown() {
        let server = FakeServer::answering(numbering(0));
        let person =
            ScriptedPerson::typing(&["Why?", "never asked"]).with_the_output_closed_after(1);

        let error = discuss(&server, &configured(100_000), "H", vec![], &person)
            .await
            .unwrap_err();

        assert!(error.is::<OutputClosed>());
        assert_eq!(*person.shown.borrow(), ["answer 1"]);
        assert_eq!(person.lines_left(), 1);
        assert_eq!(server.received.borrow().len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sends_the_model_escaped_text_with_letters_and_line_endings_intact() {
        let server = FakeServer::replying("feat: x");
        let conversation = [Message::user("+ blåbær\r\n+ \u{e0041}\u{1b}[2J")];

        generate_message(&server, &configured(100_000), &conversation)
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
        let error = generate_message(&server, &configured(100_000), &[Message::user("x")])
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "the model returned an empty message");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn says_why_when_escaping_overfills_the_context() {
        let server = FakeServer::replying("feat: x");
        let conversation = [Message::user("\u{1b}".repeat(4000))];
        let error = generate_message(&server, &configured(3000), &conversation)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no longer fit"), "{error}");
        assert!(server.received.borrow().is_empty());
    }
}
