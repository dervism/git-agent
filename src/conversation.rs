//! Talking to the model: choosing it and the context to fit into, fitting the changes in,
//! building the conversation, sending it, and cleaning up the reply.

mod exchange;
pub mod fit;
pub mod messages;
pub mod models;
mod reply;
mod report;

pub use exchange::{
    commit_message, connect, conversation_tokens, discuss, generate_message, separated,
};
pub use messages::{
    Fit, estimate_tokens, follow_up, has_room, initial_messages, notes_bytes, outgrew_context,
    question_bytes, refine_messages, room,
};
pub use reply::{clean_reply, separate_subject};
pub use report::{report_fit, report_model};
