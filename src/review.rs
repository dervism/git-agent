//! The review of a proposal: what it shows, the choices that accept it and the keys that pick
//! them, editing it, and the loop that revises it with the model until the user accepts it or
//! quits.

mod editor;
mod revise;

use crate::ui;
use anyhow::Result;

pub use editor::{edit_message, edit_plan, edit_pull_request};
pub use revise::{review, show_proposal};

/// What a review shows, and the choices that accept the proposal besides
/// editing, regenerating, feedback and quitting.
pub struct Review<T: 'static> {
    /// Title of the frame around the proposal, e.g. "Proposed commit message".
    pub heading: &'static str,
    /// What the proposal's first line is called, e.g. "subject line".
    pub first_line: &'static str,
    /// What the help calls the proposal, e.g. "message".
    pub noun: &'static str,
    /// What the model is asked to revise, e.g. "commit message".
    pub what: &'static str,
    /// Help for editing, e.g. "edit the message in your editor".
    pub edit: &'static str,
    /// Typed as their first letter or in full. `y` and `yes` pick the first, so put the
    /// plain acceptance first: it skips no later review and does nothing more, such as
    /// pushing.
    pub accept: &'static [Choice<T>],
    /// Help for quitting, e.g. "quit without committing".
    pub quit: &'static str,
}

pub struct Choice<T> {
    pub word: &'static str,
    pub help: &'static str,
    pub value: T,
}

pub enum Action<T> {
    /// One of the review's accept choices.
    Accept(T),
    Edit,
    Regenerate,
    Refine(String),
    Quit,
}

/// Asks what to do with the proposal. End of input counts as quitting.
pub fn ask_action<T: Copy>(review: &Review<T>) -> Result<Action<T>> {
    let words = review.accept.iter().map(|choice| choice.word);
    let mut items: Vec<String> = words
        .chain(["edit", "regenerate", "feedback", "quit"])
        .map(|word| format!("{}{}", ui::bold(&format!("[{}]", &word[..1])), &word[1..]))
        .collect();
    items.push(ui::dim("[?]"));
    let menu = format!("{} ›", items.join("  "));
    loop {
        let Some(input) = ui::read_line(&menu)? else {
            return Ok(Action::Quit);
        };
        let (choice, rest) = input.split_once(' ').unwrap_or((&input, ""));
        let choice = choice.to_lowercase();
        if let Some(value) = accepted(review, &choice) {
            return Ok(Action::Accept(value));
        }
        match choice.as_str() {
            "e" | "edit" => return Ok(Action::Edit),
            "r" | "regenerate" => return Ok(Action::Regenerate),
            "f" | "feedback" => {
                let feedback = match rest.trim() {
                    "" => ui::read_line("What should change? ›")?.unwrap_or_default(),
                    rest => rest.to_owned(),
                };
                if !feedback.is_empty() {
                    return Ok(Action::Refine(feedback));
                }
            }
            "q" | "quit" | "n" | "no" => return Ok(Action::Quit),
            "" => {}
            "?" | "h" | "help" => ui::info(help(review)),
            other => ui::info(format!("Unknown choice {other:?}. Type ? for help.")),
        }
    }
}

/// The value of the accept choice typed as `choice`, in lower case. `y` and `yes` pick the
/// first (see [`Review::accept`]).
pub fn accepted<T: Copy>(review: &Review<T>, choice: &str) -> Option<T> {
    if matches!(choice, "y" | "yes") {
        return review.accept.first().map(|first| first.value);
    }
    // A choice of several words, such as "all commits", also takes its first word.
    review
        .accept
        .iter()
        .find(|accept| {
            choice == accept.word
                || choice == &accept.word[..1]
                || accept.word.split(' ').next() == Some(choice)
        })
        .map(|accept| accept.value)
}

fn help<T>(review: &Review<T>) -> String {
    let mut lines: Vec<String> = review
        .accept
        .iter()
        .map(|choice| format!("  {}  {}", &choice.word[..1], choice.help))
        .collect();
    lines.push(format!("  e  {}", review.edit));
    lines.push(format!("  r  generate a new {}", review.noun));
    lines.push("  f  tell the model what to change, e.g. `f mention the API change`".to_owned());
    lines.push(format!("  q  {}", review.quit));
    lines.join("\n")
}

/// What the tests of the reviews share.
#[cfg(test)]
pub mod test_support {
    use super::Review;

    /// The keys [`super::ask_action`] reads besides the accept choices: yes, no, edit,
    /// regenerate, feedback, quit and help.
    const TAKEN: [char; 7] = ['y', 'n', 'e', 'r', 'f', 'q', 'h'];

    /// Checks that `review` has a choice for `y`, and that each accept choice can be typed
    /// as its first letter: a lower-case ASCII letter that no other choice and no other key
    /// uses.
    pub fn assert_typable<T>(review: &Review<T>) {
        assert!(!review.accept.is_empty(), "a review needs a choice for `y`");
        let letters: Vec<char> = review
            .accept
            .iter()
            .map(|choice| choice.word.chars().next().expect("a choice has a word"))
            .collect();
        for (choice, letter) in review.accept.iter().zip(&letters) {
            let word = choice.word;
            assert!(
                letter.is_ascii_lowercase(),
                "{word:?} starts with {letter:?}"
            );
            assert!(!TAKEN.contains(letter), "{word:?} takes the key {letter:?}");
            let sharing = letters.iter().filter(|other| *other == letter).count();
            assert_eq!(sharing, 1, "{word:?} shares the key {letter:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVIEW: Review<u8> = Review {
        heading: "Proposed thing",
        first_line: "title",
        noun: "thing",
        what: "thing",
        edit: "edit the thing in your editor",
        accept: &[
            Choice {
                word: "create",
                help: "create it",
                value: 1,
            },
            Choice {
                word: "draft",
                help: "create a draft",
                value: 2,
            },
        ],
        quit: "quit without creating it",
    };

    #[test]
    fn accepts_choices_by_letter_or_word() {
        assert_eq!(accepted(&REVIEW, "c"), Some(1));
        assert_eq!(accepted(&REVIEW, "draft"), Some(2));
        assert_eq!(accepted(&REVIEW, "y"), Some(1));
        assert_eq!(accepted(&REVIEW, "e"), None);
        let several = Review {
            accept: &[Choice {
                word: "all commits",
                help: "make them all",
                value: 3,
            }],
            ..REVIEW
        };
        assert_eq!(accepted(&several, "a"), Some(3));
        assert_eq!(accepted(&several, "all"), Some(3));
        assert_eq!(accepted(&several, "commits"), None);
        let help = help(&REVIEW);
        assert!(help.starts_with("  c  create it\n  d  create a draft\n  e  edit the thing"));
        assert!(help.ends_with("  q  quit without creating it"));
    }

    /// [`REVIEW`] with `choices` to accept it.
    fn review_of(choices: &'static [Choice<u8>]) -> Review<u8> {
        Review {
            accept: choices,
            ..REVIEW
        }
    }

    /// An accept choice that only has a word.
    const fn choice(word: &'static str) -> Choice<u8> {
        Choice {
            word,
            help: "",
            value: 0,
        }
    }

    #[test]
    fn a_review_with_its_own_letters_is_typable() {
        const SPLIT_PLAN: &[Choice<u8>] = &[choice("commit"), choice("all commits")];
        test_support::assert_typable(&REVIEW);
        test_support::assert_typable(&review_of(SPLIT_PLAN));
    }

    #[test]
    #[should_panic(expected = "takes the key 'e'")]
    fn a_choice_cant_take_the_key_of_edit() {
        const CHOICES: &[Choice<u8>] = &[choice("export")];
        test_support::assert_typable(&review_of(CHOICES));
    }

    #[test]
    #[should_panic(expected = "takes the key 'y'")]
    fn a_choice_cant_take_the_key_of_yes() {
        const CHOICES: &[Choice<u8>] = &[choice("commit"), choice("yank")];
        test_support::assert_typable(&review_of(CHOICES));
    }

    #[test]
    #[should_panic(expected = "shares the key 'c'")]
    fn two_choices_cant_share_a_key() {
        const CHOICES: &[Choice<u8>] = &[choice("commit"), choice("create")];
        test_support::assert_typable(&review_of(CHOICES));
    }

    #[test]
    #[should_panic(expected = "starts with 'ä'")]
    fn a_choice_starts_with_an_ascii_letter() {
        const CHOICES: &[Choice<u8>] = &[choice("ändern")];
        test_support::assert_typable(&review_of(CHOICES));
    }

    #[test]
    #[should_panic(expected = "starts with 'C'")]
    fn a_choice_starts_in_lower_case() {
        const CHOICES: &[Choice<u8>] = &[choice("Commit")];
        test_support::assert_typable(&review_of(CHOICES));
    }

    #[test]
    #[should_panic(expected = "needs a choice for `y`")]
    fn a_review_has_a_choice_for_yes() {
        test_support::assert_typable(&review_of(&[]));
    }
}
