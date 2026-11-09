//! Talking to the model: choosing it and the context to fit into, fitting the changes in,
//! building the conversation, sending it, and cleaning up the reply.

mod budget;
mod exchange;
pub mod fit;
mod messages;
pub mod models;
mod reply;
mod report;

pub use budget::{room, tokens_in};
pub use exchange::{commit_message, connect, discuss, generate_message};
#[cfg(test)]
pub use messages::test_support;
pub use messages::{
    Fit, asks, describe_changes, initial_messages, notes, notes_bytes, question_bytes,
    refine_messages, with_commits,
};
pub use reply::separated;
pub use report::{report_fit, report_model};
