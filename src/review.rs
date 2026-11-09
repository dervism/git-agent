//! The review of a proposal: what it shows, the choices that accept it and the keys that pick
//! them, editing it, and the loop that revises it with the model until the user accepts it or
//! quits.

mod editor;
mod revise;

use crate::ui;
use anyhow::Result;

pub use editor::{edit_message, edit_plan, edit_pull_request};
pub use revise::{revise, show_proposal};

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

#[derive(Debug, PartialEq)]
pub enum Action<T> {
    /// One of the review's accept choices.
    Accept(T),
    Edit,
    Regenerate,
    Refine(String),
    Quit,
}

/// Asks for help, like [`Key::Help`].
const HELP_ALIAS: &str = "?";

/// The keys every review reads besides its accept choices, typed as their first letter or
/// in full. An accept choice may not start with the letter of one of them.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Key {
    /// Picks the review's first accept choice.
    Yes,
    /// Quits, like [`Key::Quit`].
    No,
    Edit,
    Regenerate,
    Feedback,
    Quit,
    Help,
}

impl Key {
    const ALL: [Key; 7] = [
        Key::Yes,
        Key::No,
        Key::Edit,
        Key::Regenerate,
        Key::Feedback,
        Key::Quit,
        Key::Help,
    ];

    /// The keys the menu shows after the accept choices.
    const SHOWN: [Key; 4] = [Key::Edit, Key::Regenerate, Key::Feedback, Key::Quit];

    fn word(self) -> &'static str {
        match self {
            Key::Yes => "yes",
            Key::No => "no",
            Key::Edit => "edit",
            Key::Regenerate => "regenerate",
            Key::Feedback => "feedback",
            Key::Quit => "quit",
            Key::Help => "help",
        }
    }

    fn letter(self) -> &'static str {
        &self.word()[..1]
    }

    /// The key typed as `word`, in lower case, or as [`HELP_ALIAS`].
    fn typed(word: &str) -> Option<Key> {
        if word == HELP_ALIAS {
            return Some(Key::Help);
        }
        Key::ALL
            .into_iter()
            .find(|key| word == key.word() || word == key.letter())
    }
}

/// What a line typed at a review asks for.
#[derive(Debug, PartialEq)]
pub enum Typed<T> {
    Action(Action<T>),
    /// Feedback, without what should change: that comes on a line of its own.
    FeedbackToFollow,
    Help,
    /// An empty line.
    Nothing,
    /// A word that is no choice or key, in lower case.
    Unknown(String),
}

/// What `line` asks for at `review`. The first word picks the choice or the key, in any
/// case, and the rest of the line is the feedback, as in `f shorter`. `y` picks the first
/// accept choice, so a review without one doesn't know it.
pub fn parse<T: Copy>(review: &Review<T>, line: &str) -> Typed<T> {
    let line = line.trim();
    let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
    let word = word.to_lowercase();
    if word.is_empty() {
        return Typed::Nothing;
    }
    if let Some(value) = accepted(review, &word) {
        return Typed::Action(Action::Accept(value));
    }
    match (Key::typed(&word), rest.trim()) {
        (Some(Key::Edit), _) => Typed::Action(Action::Edit),
        (Some(Key::Regenerate), _) => Typed::Action(Action::Regenerate),
        (Some(Key::Feedback), "") => Typed::FeedbackToFollow,
        (Some(Key::Feedback), feedback) => Typed::Action(Action::Refine(feedback.to_owned())),
        (Some(Key::Quit | Key::No), _) => Typed::Action(Action::Quit),
        (Some(Key::Help), _) => Typed::Help,
        (Some(Key::Yes) | None, _) => Typed::Unknown(word),
    }
}

/// Asks what to do with the proposal. End of input counts as quitting.
pub fn ask_action<T: Copy>(review: &Review<T>) -> Result<Action<T>> {
    let menu = menu(review);
    loop {
        let Some(line) = ui::read_line(&menu)? else {
            return Ok(Action::Quit);
        };
        match parse(review, &line) {
            Typed::Action(action) => return Ok(action),
            Typed::FeedbackToFollow => {
                let feedback = ui::read_line("What should change? ›")?.unwrap_or_default();
                if !feedback.is_empty() {
                    return Ok(Action::Refine(feedback));
                }
            }
            Typed::Help => ui::info(help(review)),
            Typed::Nothing => {}
            Typed::Unknown(word) => ui::info(format!(
                "Unknown choice {word:?}. Type {HELP_ALIAS} for help."
            )),
        }
    }
}

/// The value of the accept choice typed as `choice`, in lower case. `y` and `yes` pick the
/// first (see [`Review::accept`]).
pub fn accepted<T: Copy>(review: &Review<T>, choice: &str) -> Option<T> {
    if Key::typed(choice) == Some(Key::Yes) {
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

/// The choices and keys a review offers, each with its letter in bold.
fn menu<T>(review: &Review<T>) -> String {
    let words = review.accept.iter().map(|choice| choice.word);
    let mut items: Vec<String> = words
        .chain(Key::SHOWN.map(Key::word))
        .map(|word| format!("{}{}", ui::bold(&format!("[{}]", &word[..1])), &word[1..]))
        .collect();
    items.push(ui::dim(&format!("[{HELP_ALIAS}]")));
    format!("{} ›", items.join("  "))
}

fn help<T>(review: &Review<T>) -> String {
    let mut lines: Vec<String> = review
        .accept
        .iter()
        .map(|choice| format!("  {}  {}", &choice.word[..1], choice.help))
        .collect();
    let feedback = Key::Feedback.letter();
    lines.push(format!("  {}  {}", Key::Edit.letter(), review.edit));
    lines.push(format!(
        "  {}  generate a new {}",
        Key::Regenerate.letter(),
        review.noun
    ));
    lines.push(format!(
        "  {feedback}  tell the model what to change, e.g. `{feedback} mention the API change`"
    ));
    lines.push(format!("  {}  {}", Key::Quit.letter(), review.quit));
    lines.join("\n")
}

/// What the tests of the reviews share.
#[cfg(test)]
pub mod test_support {
    use super::{Key, Review};

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
        let taken: Vec<char> = Key::ALL
            .iter()
            .flat_map(|key| key.letter().chars())
            .collect();
        for (choice, letter) in review.accept.iter().zip(&letters) {
            let word = choice.word;
            assert!(
                letter.is_ascii_lowercase(),
                "{word:?} starts with {letter:?}"
            );
            assert!(!taken.contains(letter), "{word:?} takes the key {letter:?}");
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

    #[test]
    fn reads_each_key_by_its_letter_or_its_word_in_any_case() {
        for (typed, action) in [
            ("e", Action::Edit),
            ("EDIT", Action::Edit),
            ("e and more words", Action::Edit),
            (" e ", Action::Edit),
            ("\tdraft", Action::Accept(2)),
            ("r", Action::Regenerate),
            ("Regenerate", Action::Regenerate),
            ("q", Action::Quit),
            ("quit", Action::Quit),
            ("n", Action::Quit),
            ("No", Action::Quit),
            ("c", Action::Accept(1)),
            ("Draft", Action::Accept(2)),
            ("y", Action::Accept(1)),
            ("YES", Action::Accept(1)),
        ] {
            assert_eq!(parse(&REVIEW, typed), Typed::Action(action), "{typed:?}");
        }
        for typed in ["h", "Help", "?"] {
            assert_eq!(parse(&REVIEW, typed), Typed::Help, "{typed:?}");
        }
    }

    #[test]
    fn takes_feedback_from_the_rest_of_the_line_or_from_the_next() {
        let refine = |feedback: &str| Typed::Action(Action::Refine(feedback.to_owned()));
        assert_eq!(
            parse(&REVIEW, "f Shorter, please"),
            refine("Shorter, please")
        );
        assert_eq!(
            parse(&REVIEW, "feedback  name the flag "),
            refine("name the flag")
        );
        assert_eq!(parse(&REVIEW, "f"), Typed::FeedbackToFollow);
        assert_eq!(parse(&REVIEW, "F   "), Typed::FeedbackToFollow);
    }

    #[test]
    fn ignores_an_empty_line_and_names_what_it_doesnt_know() {
        assert_eq!(parse(&REVIEW, ""), Typed::Nothing);
        assert_eq!(parse(&REVIEW, " \t "), Typed::Nothing);
        assert_eq!(parse(&REVIEW, "Zap it"), Typed::Unknown("zap".to_owned()));
        assert_eq!(parse(&REVIEW, "x"), Typed::Unknown("x".to_owned()));
        // With no choice to accept, `y` picks nothing.
        assert_eq!(parse(&review_of(&[]), "y"), Typed::Unknown("y".to_owned()));
        assert_eq!(
            parse(&review_of(&[]), "yes"),
            Typed::Unknown("yes".to_owned())
        );
    }

    #[test]
    fn accepts_a_choice_of_several_words_by_its_first() {
        const SEVERAL: &[Choice<u8>] = &[choice("commit"), choice("all commits")];
        for typed in ["a", "all", "all commits", "ALL Commits"] {
            assert_eq!(
                parse(&review_of(SEVERAL), typed),
                Typed::Action(Action::Accept(0)),
                "{typed:?}"
            );
        }
    }

    #[test]
    fn gives_each_key_a_letter_of_its_own() {
        for key in Key::ALL {
            assert_eq!(Key::typed(key.word()), Some(key));
            assert_eq!(Key::typed(key.letter()), Some(key), "{key:?}");
        }
        assert!(Key::SHOWN.iter().all(|key| Key::ALL.contains(key)));
    }

    #[test]
    fn helps_with_each_choice_and_key() {
        assert_eq!(
            help(&REVIEW),
            "  c  create it\n  d  create a draft\n  e  edit the thing in your editor\n  \
             r  generate a new thing\n  f  tell the model what to change, e.g. `f mention the \
             API change`\n  q  quit without creating it"
        );
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
