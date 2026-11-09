//! The person at the terminal, behind the ports that reviews and discussions ask through.

use super::Person;
use crate::conversation::Discussant;
use crate::ui;
use anyhow::Result;
use std::io::{self, IsTerminal};

/// The person at the terminal: what they type comes from stdin, and what they read goes out
/// through `ui`.
pub struct Terminal;

impl Discussant for Terminal {
    fn is_interactive(&self) -> bool {
        io::stdin().is_terminal() && ui::stdout_is_terminal()
    }

    fn show_answer(&self, heading: &str, answer: &str) -> Result<()> {
        ui::show_answer(heading, answer)
    }

    fn ask_follow_up(&self) -> Result<Option<String>> {
        ui::ask_follow_up()
    }
}

impl Person for Terminal {
    fn typed_at(&self, prompt: &str) -> Result<Option<String>> {
        ui::read_line(prompt)
    }
}
