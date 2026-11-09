//! Showing a proposal, and revising it until the user accepts it or quits.

use super::{Action, Person, Review, ask_action};
use crate::conversation::models::Model;
use crate::conversation::{generate_message, refine_messages};
use crate::server::{Message, ModelServer};
use crate::ui;
use anyhow::Result;

const SUBJECT_LIMIT: usize = 72;

pub fn show_proposal<T>(review: &Review<T>, text: &str) {
    ui::show_framed(review.heading, text);
    let first_len = text.lines().next().unwrap_or_default().chars().count();
    if first_len > SUBJECT_LIMIT {
        ui::warn(format!(
            "the {} is {first_len} characters; {SUBJECT_LIMIT} or fewer is conventional",
            review.first_line
        ));
    }
}

/// Where new versions of a proposal come from: the model, asked with `conversation`, whose
/// replies `clean` checks and tidies, and the person's own changes in `edit`.
pub struct Revisions<'a, S, E, C> {
    pub server: &'a S,
    pub model: &'a Model,
    pub conversation: &'a [Message],
    pub edit: E,
    pub clean: C,
}

impl<S, E, C> Revisions<'_, S, E, C>
where
    S: ModelServer,
    C: Fn(String) -> Result<String>,
{
    async fn regenerated(&self) -> Result<String> {
        generate_message(self.server, self.model, self.conversation)
            .await
            .and_then(&self.clean)
    }

    async fn refined(&self, text: &str, feedback: &str, what: &str) -> Result<String> {
        let refined = refine_messages(self.conversation, text, feedback, what);
        generate_message(self.server, self.model, &refined)
            .await
            .and_then(&self.clean)
    }
}

/// Lets `person` review `text` until they accept or quit it, revising it as they ask.
/// Returns the accept choice and the final text, or `None` on quit.
pub async fn revise<T, S, E, C>(
    review: &Review<T>,
    mut text: String,
    revisions: Revisions<'_, S, E, C>,
    person: &dyn Person,
) -> Result<Option<(T, String)>>
where
    T: Copy,
    S: ModelServer,
    E: Fn(&str) -> Result<Option<String>>,
    C: Fn(String) -> Result<String>,
{
    loop {
        let revised = match ask_action(review, person)? {
            Action::Accept(choice) => return Ok(Some((choice, text))),
            Action::Quit => return Ok(None),
            Action::Edit => (revisions.edit)(&text),
            Action::Regenerate => revisions.regenerated().await.map(Some),
            Action::Refine(feedback) => revisions
                .refined(&text, &feedback, review.what)
                .await
                .map(Some),
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
        show_proposal(review, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::REVIEW;
    use super::*;
    use crate::conversation::test_support::{ScriptedPerson, configured};
    use crate::server::Role;
    use crate::server::test_support::FakeServer;
    use anyhow::anyhow;
    use std::cell::Cell;

    /// Runs a review of "draft" that `person` types through, with a model whose replies are
    /// numbered versions, or fail if `failing`, and an editor that adds ", edited" or fails
    /// as `edits` says in turn.
    async fn reviewed(
        person: &ScriptedPerson,
        server: &FakeServer<'_>,
        edits: &[Option<bool>],
    ) -> Option<(u8, String)> {
        let edited = Cell::new(0);
        let revisions = Revisions {
            server,
            model: &configured(100_000),
            conversation: &[Message::user("write it")],
            edit: |text: &str| {
                let turn = edited.replace(edited.get() + 1);
                match edits[turn] {
                    Some(true) => Ok(Some(format!("{text}, edited"))),
                    Some(false) => Ok(None),
                    None => Err(anyhow!("the editor failed")),
                }
            },
            clean: |reply: String| Ok(format!("{reply}.")),
        };
        revise(&REVIEW, "draft".to_owned(), revisions, person)
            .await
            .unwrap()
    }

    fn versions() -> FakeServer<'static> {
        let asked = Cell::new(0);
        FakeServer::answering(move |_| {
            asked.set(asked.get() + 1);
            Ok(format!("version {}", asked.get()))
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn regenerates_refines_and_edits_until_the_person_accepts() {
        let person = ScriptedPerson::typing(&["r", "f shorter", "e", "d"]);
        let server = versions();

        let accepted = reviewed(&person, &server, &[Some(true)]).await;

        assert_eq!(accepted, Some((2, "version 2., edited".to_owned())));
        let sent = server.received.borrow();
        assert_eq!(sent.len(), 2);
        let refining = &sent[1];
        assert_eq!(
            (refining[1].role, refining[1].content.as_str()),
            (Role::Assistant, "version 1.")
        );
        assert!(refining[2].content.contains("feedback:\nshorter"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn keeps_the_text_when_an_edit_is_empty_or_fails() {
        let person = ScriptedPerson::typing(&["e", "e", "c"]);

        let accepted = reviewed(&person, &versions(), &[Some(false), None]).await;

        assert_eq!(accepted, Some((1, "draft".to_owned())));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn keeps_the_text_when_the_model_fails() {
        let person = ScriptedPerson::typing(&["r", "f longer", "y"]);
        let server = FakeServer::answering(|_| Err(anyhow!("the server went away")));

        let accepted = reviewed(&person, &server, &[]).await;

        assert_eq!(accepted, Some((1, "draft".to_owned())));
        assert_eq!(server.received.borrow().len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quits_when_the_person_quits_or_input_ends() {
        for lines in [&["q"][..], &["e", "n"], &[]] {
            let person = ScriptedPerson::typing(lines);
            assert_eq!(reviewed(&person, &versions(), &[Some(true)]).await, None);
        }
    }
}
