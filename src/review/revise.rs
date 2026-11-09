//! Showing a proposal, and revising it until the user accepts it or quits.

use super::{Action, Review, ask_action};
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

/// Lets the user review `text` until they accept or quit it, revising it as
/// they ask; `clean` checks and tidies each new reply from the model. Returns
/// the accept choice and the final text, or `None` on quit.
pub async fn review<T: Copy>(
    review: &Review<T>,
    mut text: String,
    server: &impl ModelServer,
    model: &Model,
    conversation: &[Message],
    edit: impl Fn(&str) -> Result<Option<String>>,
    clean: impl Fn(String) -> Result<String>,
) -> Result<Option<(T, String)>> {
    loop {
        let revised = match ask_action(review)? {
            Action::Accept(choice) => return Ok(Some((choice, text))),
            Action::Quit => return Ok(None),
            Action::Edit => edit(&text),
            Action::Regenerate => generate_message(server, model, conversation)
                .await
                .and_then(&clean)
                .map(Some),
            Action::Refine(feedback) => {
                let refined = refine_messages(conversation, &text, &feedback, review.what);
                generate_message(server, model, &refined)
                    .await
                    .and_then(&clean)
                    .map(Some)
            }
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
