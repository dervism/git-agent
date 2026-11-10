//! The review of a proposal: what it shows, the choices that accept it and the keys that pick
//! them, the person who reviews it, editing it, and the loop that revises it with the model
//! until the person accepts it or quits.

mod editor;
mod revise;
mod terminal;

use crate::conversation::Discussant;
use crate::ui;
use anyhow::Result;

pub use editor::{edit_message, edit_plan, edit_pull_request};
pub use revise::{Revisions, revise, show_proposal};
pub use terminal::Terminal;

/// The person who reviews a proposal. As a [`Discussant`], they also read the model's answers
/// and ask follow-up questions.
pub trait Person: Discussant {
    /// The line the person types at `prompt`, or `None` at end of input.
    fn typed_at(&self, prompt: &str) -> Result<Option<String>>;
}

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
    /// What `n` does: turn down this proposal where the review has a choice for that, and
    /// otherwise quit. A choice that isn't one of `accept` gets past the compiler;
    /// `test_support::assert_typable` catches it, in the test of each review.
    pub decline: Decline<T>,
    /// Help for quitting, e.g. "quit without committing".
    pub quit: &'static str,
}

/// What typing `n` does at a review.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decline<T> {
    /// Quits, like `q`: the review has no way to turn down a proposal but to stop.
    Quit,
    /// Picks this accept choice of the review, which turns down the proposal and goes on.
    Choose(T),
}

impl<T: Copy> Decline<T> {
    /// What typing `n` asks for.
    fn action(self) -> Action<T> {
        match self {
            Decline::Quit => Action::Quit,
            Decline::Choose(value) => Action::Accept(value),
        }
    }
}

impl<T: PartialEq> Review<T> {
    /// The accept choice whose value is `value`, if the review has one.
    fn choice_of(&self, value: &T) -> Option<&Choice<T>> {
        self.accept.iter().find(|choice| choice.value == *value)
    }
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
    /// Does what the review's [`Decline`] says.
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
        first_letter(self.word())
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

/// The letter a choice or key is typed as: the first of its word.
fn first_letter(word: &str) -> &str {
    let length = word.chars().next().map_or(0, char::len_utf8);
    &word[..length]
}

/// What `line` asks for at `review`. The first word picks the choice or the key, in any
/// case, and the rest of the line is the feedback, as in `f shorter`. `y` picks the first
/// accept choice, so a review without one doesn't know it. `n` does what the review's
/// [`Decline`] says.
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
        (Some(Key::No), _) => Typed::Action(review.decline.action()),
        (Some(Key::Quit), _) => Typed::Action(Action::Quit),
        (Some(Key::Help), _) => Typed::Help,
        (Some(Key::Yes) | None, _) => Typed::Unknown(word),
    }
}

/// Asks `person` what to do with the proposal. End of input counts as quitting.
pub fn ask_action<T: Copy + PartialEq>(
    review: &Review<T>,
    person: &dyn Person,
) -> Result<Action<T>> {
    let menu = menu(review);
    loop {
        let Some(line) = person.typed_at(&menu)? else {
            return Ok(Action::Quit);
        };
        match parse(review, &line) {
            Typed::Action(action) => return Ok(action),
            Typed::FeedbackToFollow => {
                let feedback = person
                    .typed_at("What should change? ›")?
                    .unwrap_or_default();
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
                || choice == first_letter(accept.word)
                || accept.word.split(' ').next() == Some(choice)
        })
        .map(|accept| accept.value)
}

/// The choices and keys a review offers, each with its letter in bold.
fn menu<T>(review: &Review<T>) -> String {
    let mut items: Vec<String> = menu_words(review)
        .into_iter()
        .map(|word| {
            let letter = first_letter(word);
            format!(
                "{}{}",
                ui::bold(&format!("[{letter}]")),
                &word[letter.len()..]
            )
        })
        .collect();
    items.push(ui::dim(&format!("[{HELP_ALIAS}]")));
    format!("{} ›", items.join("  "))
}

/// The words of the menu: the review's accept choices, then the keys it shows.
fn menu_words<T>(review: &Review<T>) -> Vec<&'static str> {
    let choices = review.accept.iter().map(|choice| choice.word);
    choices.chain(Key::SHOWN.map(Key::word)).collect()
}

fn help<T: PartialEq>(review: &Review<T>) -> String {
    let mut lines: Vec<String> = review
        .accept
        .iter()
        .map(|choice| format!("  {}  {}", first_letter(choice.word), choice.help))
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
    lines.extend(yes_help(review));
    lines.extend(no_help(review));
    lines.join("\n")
}

/// The line of the help for `y`, if the review has a choice for it.
fn yes_help<T>(review: &Review<T>) -> Option<String> {
    let first = review.accept.first()?;
    Some(same_as(Key::Yes, first.word))
}

/// The line of the help for `n`, unless it picks a choice the review lacks.
fn no_help<T: PartialEq>(review: &Review<T>) -> Option<String> {
    let word = match &review.decline {
        Decline::Quit => Key::Quit.word(),
        Decline::Choose(value) => review.choice_of(value)?.word,
    };
    Some(same_as(Key::No, word))
}

/// A line of the help saying `key` is the same as the key typed for `word`, as in "c (commit)".
fn same_as(key: Key, word: &str) -> String {
    format!(
        "  {}  the same as {} ({word})",
        key.letter(),
        first_letter(word)
    )
}

/// What the tests of the reviews share.
#[cfg(test)]
pub mod test_support {
    use super::{Action, Choice, Decline, Key, Person, Review, ask_action};
    use anyhow::Result;

    pub use crate::conversation::test_support::ScriptedPerson;

    impl Person for ScriptedPerson {
        fn typed_at(&self, _prompt: &str) -> Result<Option<String>> {
            Ok(self.next_line())
        }
    }

    /// What a person who types `line` at `review` asks for, through the port the terminal
    /// reads from. A `q` follows, so a line the review doesn't take as an action fails here,
    /// and doesn't pass as a quit: end of input is a quit too.
    pub fn action_for<T: Copy + PartialEq>(review: &Review<T>, line: &str) -> Action<T> {
        let person = ScriptedPerson::typing(&[line, "q"]);
        let action = ask_action(review, &person).expect("a scripted person can always type");
        assert_eq!(person.lines_left(), 1, "{line:?} wasn't taken as an action");
        action
    }

    /// The last `count` lines of the help of `review`, which `?` shows.
    pub fn last_lines_of_help<T: PartialEq>(review: &Review<T>, count: usize) -> Vec<String> {
        let help = super::help(review);
        let lines: Vec<&str> = help.lines().collect();
        lines[lines.len() - count..]
            .iter()
            .map(|line| (*line).to_owned())
            .collect()
    }

    /// A review to test with: `c` creates (1), and `d` makes a draft (2).
    pub const REVIEW: Review<u8> = Review {
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
        decline: Decline::Quit,
        quit: "quit without creating it",
    };

    /// [`REVIEW`] where `n` picks `value`.
    pub fn declining_with(value: u8) -> Review<u8> {
        Review {
            decline: Decline::Choose(value),
            ..REVIEW
        }
    }

    /// Checks that `review` has a choice for `y`, that the choice `n` picks, if any, is one
    /// of its accept choices other than the one `y` picks, and that each accept choice can be
    /// typed as its first letter: a lower-case ASCII letter that no other choice and no other
    /// key uses.
    pub fn assert_typable<T: PartialEq>(review: &Review<T>) {
        assert!(!review.accept.is_empty(), "a review needs a choice for `y`");
        if let Decline::Choose(value) = &review.decline {
            let picked = review.choice_of(value);
            assert!(
                picked.is_some(),
                "`n` picks a choice the review doesn't have"
            );
            assert!(
                review.accept[0].value != *value,
                "`n` picks the choice `y` picks"
            );
        }
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
    use super::test_support::{REVIEW, declining_with};
    use super::*;
    use crate::conversation::test_support::ScriptedPerson;

    fn asked(lines: &[&str]) -> (Action<u8>, usize) {
        let person = ScriptedPerson::typing(lines);
        let action = ask_action(&REVIEW, &person).unwrap();
        (action, person.lines_left())
    }

    #[test]
    fn keeps_asking_after_an_empty_line_help_or_an_unknown_word() {
        assert_eq!(asked(&["", "?", "zap", "d", "c"]), (Action::Accept(2), 1));
    }

    #[test]
    fn asks_for_feedback_on_the_next_line_and_goes_back_on_an_empty_one() {
        let refined = Action::Refine("shorter".to_owned());
        assert_eq!(asked(&["f", "", "f", " shorter "]), (refined, 0));
    }

    #[test]
    fn quits_at_the_end_of_input() {
        assert_eq!(asked(&[]), (Action::Quit, 0));
        assert_eq!(asked(&["f"]), (Action::Quit, 0));
    }

    #[test]
    fn offers_the_choices_then_the_keys_and_help() {
        assert_eq!(
            menu_words(&REVIEW),
            ["create", "draft", "edit", "regenerate", "feedback", "quit"]
        );
        let menu = menu(&REVIEW);
        for item in ["[c]", "reate", "[d]", "raft", "[q]", "uit", "[?]"] {
            assert!(menu.contains(item), "{item:?} in {menu:?}");
        }
        assert!(menu.ends_with(" ›"), "{menu:?}");
    }

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
    fn helps_with_each_choice_and_key_and_ends_with_y_and_n() {
        assert_eq!(
            help(&REVIEW),
            "  c  create it\n  d  create a draft\n  e  edit the thing in your editor\n  \
             r  generate a new thing\n  f  tell the model what to change, e.g. `f mention the \
             API change`\n  q  quit without creating it\n  \
             y  the same as c (create)\n  n  the same as q (quit)"
        );
    }

    #[test]
    fn says_in_the_help_what_n_picks_when_it_picks_a_choice() {
        assert_eq!(
            test_support::last_lines_of_help(&declining_with(2), 2),
            ["  y  the same as c (create)", "  n  the same as d (draft)"]
        );
    }

    #[test]
    fn leaves_out_of_the_help_the_key_that_picks_nothing() {
        let without_yes = help(&review_of(&[]));
        assert!(without_yes.ends_with("  q  quit without creating it\n  n  the same as q (quit)"));
        assert!(!without_yes.contains("  y  "), "{without_yes:?}");
        let without_no = help(&declining_with(9));
        assert!(without_no.ends_with("  q  quit without creating it\n  y  the same as c (create)"));
        assert!(!without_no.contains("  n  "), "{without_no:?}");
    }

    #[test]
    fn names_a_choice_of_several_words_by_its_first_letter_in_the_help() {
        const SEVERAL: &[Choice<u8>] = &[
            Choice {
                word: "commit",
                help: "",
                value: 1,
            },
            Choice {
                word: "all commits",
                help: "",
                value: 2,
            },
        ];
        let review = Review {
            accept: SEVERAL,
            ..declining_with(2)
        };
        assert_eq!(
            test_support::last_lines_of_help(&review, 2),
            [
                "  y  the same as c (commit)",
                "  n  the same as a (all commits)"
            ]
        );
    }

    #[test]
    fn finds_a_choice_by_its_value() {
        assert_eq!(
            REVIEW.choice_of(&2).map(|choice| choice.word),
            Some("draft")
        );
        assert!(REVIEW.choice_of(&9).is_none());
    }

    #[test]
    fn declining_asks_for_a_quit_or_for_the_choice() {
        assert_eq!(Decline::<u8>::Quit.action(), Action::Quit);
        assert_eq!(Decline::Choose(7).action(), Action::Accept(7));
    }

    #[test]
    fn takes_the_first_letter_of_a_word_whatever_its_size() {
        assert_eq!(first_letter("all commits"), "a");
        assert_eq!(first_letter("ändern"), "ä");
        assert_eq!(first_letter(""), "");
    }

    #[test]
    fn n_and_no_do_what_the_review_says_in_any_case() {
        for typed in ["n", "no", "N", "No", "NO", " n ", "n and more words"] {
            assert_eq!(
                parse(&REVIEW, typed),
                Typed::Action(Action::Quit),
                "quits: {typed:?}"
            );
            assert_eq!(
                parse(&declining_with(2), typed),
                Typed::Action(Action::Accept(2)),
                "picks: {typed:?}"
            );
        }
    }

    #[test]
    fn asking_takes_n_as_the_choice_it_picks_or_as_a_quit() {
        for typed in ["n", "no"] {
            let person = ScriptedPerson::typing(&["zap", typed, "c"]);
            assert_eq!(
                ask_action(&declining_with(2), &person).unwrap(),
                Action::Accept(2)
            );
            assert_eq!(person.lines_left(), 1, "{typed:?}");
            assert_eq!(test_support::action_for(&REVIEW, typed), Action::Quit);
        }
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
    fn a_review_where_n_picks_one_of_its_choices_is_typable() {
        test_support::assert_typable(&declining_with(2));
    }

    #[test]
    #[should_panic(expected = "`n` picks the choice `y` picks")]
    fn n_cant_pick_what_y_picks() {
        test_support::assert_typable(&declining_with(1));
    }

    #[test]
    #[should_panic(expected = "`n` picks a choice the review doesn't have")]
    fn n_cant_pick_a_choice_the_review_lacks() {
        test_support::assert_typable(&declining_with(9));
    }

    #[test]
    #[should_panic(expected = "needs a choice for `y`")]
    fn a_review_has_a_choice_for_yes() {
        test_support::assert_typable(&review_of(&[]));
    }
}
