//! Text that can't change the terminal, or hide or reverse what people and models read.
//!
//! Text from a model, a repository, GitHub or a model server can hold control
//! characters and invisible ones. They are escaped the way git writes them where the
//! text is only shown, and removed from a model's reply, so that what the user reviews is
//! what gets committed. Letters of any script and emoji stay as they are.

/// The escape letters git uses, with the bytes they stand for.
pub const NAMED_ESCAPES: [(u8, u8); 7] = [
    (b'a', 0x07),
    (b'b', 0x08),
    (b't', b'\t'),
    (b'n', b'\n'),
    (b'v', 0x0b),
    (b'f', 0x0c),
    (b'r', b'\r'),
];

/// A model's text with its hidden characters taken out.
#[derive(Debug, Clone, PartialEq)]
pub struct Cleaned {
    pub text: String,
    /// How many characters were taken out.
    pub removed: usize,
}

/// `text` on one line, with everything unsafe escaped: for names and other values that
/// must not add a line. A presentation selector after a character stays, because file
/// names with emoji need it.
pub fn one_line(text: &str) -> String {
    rendered(text, Form::Name)
}

/// Prose with everything unsafe escaped, keeping its line breaks and tabs, and the
/// selectors and joiners that emoji and some scripts need.
pub fn escaped(text: &str) -> String {
    rendered(text, Form::Prose)
}

fn rendered(text: &str, form: Form) -> String {
    let mut out = String::with_capacity(text.len());
    for (character, safe) in judged(text, form) {
        if safe {
            out.push(character);
        } else {
            escape(character, &mut out);
        }
    }
    out
}

/// Prose without what [`escaped`] would escape.
pub fn without_hidden(text: &str) -> Cleaned {
    let mut cleaned = Cleaned {
        text: String::with_capacity(text.len()),
        removed: 0,
    };
    for (character, safe) in judged(text, Form::Prose) {
        if safe {
            cleaned.text.push(character);
        } else {
            cleaned.removed += 1;
        }
    }
    cleaned
}

/// Whether text may keep its lines and joiners, as prose does, or not, as a name doesn't.
#[derive(Clone, Copy, PartialEq)]
enum Form {
    Prose,
    Name,
}

/// What a selector or a joiner comes after, since a run of them can hide data.
#[derive(Clone, Copy, PartialEq)]
enum Before {
    Nothing,
    Visible,
    Selector,
    Joiner,
}

/// Each character of `text`, with whether it's safe where it stands.
fn judged(text: &str, form: Form) -> impl Iterator<Item = (char, bool)> + '_ {
    let mut characters = text.chars().peekable();
    let mut before = Before::Nothing;
    std::iter::from_fn(move || {
        let character = characters.next()?;
        let (safe, now) = judge(character, characters.peek().copied(), before, form);
        before = now;
        Some((character, safe))
    })
}

/// Whether `character` is safe after `before` and ahead of `next`, and what the
/// character after it comes after. Only the two selectors that pick text or emoji
/// presentation are allowed, one after a visible character, since a run of the others
/// is a known way to carry hidden data.
fn judge(character: char, next: Option<char>, before: Before, form: Form) -> (bool, Before) {
    let prose = form == Form::Prose;
    match character {
        '\n' | '\t' if prose => (true, Before::Nothing),
        '\r' if prose => (next == Some('\n'), Before::Nothing),
        '\u{fe0e}' | '\u{fe0f}' if before == Before::Visible => (true, Before::Selector),
        joiner
            if prose
                && is_joiner(joiner)
                && matches!(before, Before::Visible | Before::Selector) =>
        {
            (true, Before::Joiner)
        }
        other if is_variation_selector(other) || is_unsafe_to_show(other) => (false, before),
        space if space.is_whitespace() => (true, Before::Nothing),
        _ => (true, Before::Visible),
    }
}

fn is_variation_selector(character: char) -> bool {
    matches!(character, '\u{fe00}'..='\u{fe0f}')
}

fn is_joiner(character: char) -> bool {
    matches!(character, '\u{200c}' | '\u{200d}')
}

/// Control characters, and those that can hide text, reverse it or start a line
/// without showing that they do: separators, direction marks and overrides,
/// characters of no width, invisible fillers and tags.
fn is_unsafe_to_show(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{00ad}'
                | '\u{034f}'
                | '\u{061c}'
                | '\u{115f}'..='\u{1160}'
                | '\u{17b4}'..='\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{3164}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{fff0}'..='\u{fffb}'
                | '\u{13430}'..='\u{1343f}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0000}'..='\u{e0fff}'
        )
}

fn escape(character: char, out: &mut String) {
    for &byte in character.encode_utf8(&mut [0; 4]).as_bytes() {
        match NAMED_ESCAPES.iter().find(|(_, value)| *value == byte) {
            Some((code, _)) => {
                out.push('\\');
                out.push(char::from(*code));
            }
            None => out.push_str(&format!("\\{byte:03o}")),
        }
    }
}

/// Trims trailing whitespace from every line and blank lines around the text.
pub fn normalize(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    lines.join("\n").trim_matches('\n').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Characters and sequences that change a terminal or hide text.
    const UNSAFE: [(&str, &str); 19] = [
        ("csi\u{1b}[2Jx", "csi\\033[2Jx"),
        ("title\u{1b}]0;x\u{7}y", "title\\033]0;x\\ay"),
        (
            "link\u{1b}]8;;http://x\u{1b}\\y",
            "link\\033]8;;http://x\\033\\y",
        ),
        ("clip\u{1b}]52;c;eA==\u{7}", "clip\\033]52;c;eA==\\a"),
        ("bell\u{7}", "bell\\a"),
        ("back\u{8}x", "back\\bx"),
        ("over\rx", "over\\rx"),
        ("del\u{7f}", "del\\177"),
        ("c1\u{9b}2J", "c1\\302\\2332J"),
        ("nel\u{85}x", "nel\\302\\205x"),
        ("line\u{2028}x", "line\\342\\200\\250x"),
        ("rev\u{202e}x", "rev\\342\\200\\256x"),
        ("iso\u{2066}x", "iso\\342\\201\\246x"),
        ("mark\u{200f}x", "mark\\342\\200\\217x"),
        ("zero\u{200b}x", "zero\\342\\200\\213x"),
        ("soft\u{ad}x", "soft\\302\\255x"),
        ("bom\u{feff}x", "bom\\357\\273\\277x"),
        ("tag\u{e0041}x", "tag\\363\\240\\201\\201x"),
        ("fill\u{3164}x", "fill\\343\\205\\244x"),
    ];

    /// Text that stays as it is, in prose.
    const SAFE: [&str; 11] = [
        "æøåÆØÅ blåbærsyltetøy",
        "日本語のテキスト",
        "نص عربي",
        "line one\nline two\n",
        "tab\there",
        "windows\r\nline\r\n",
        "\u{1f469}\u{200d}\u{1f4bb} works",
        "\u{2764}\u{fe0f}\u{200d}\u{1f525} flame",
        "#\u{fe0f}\u{20e3} keycap",
        "\u{1f441}\u{fe0f}\u{200d}\u{1f5e8}\u{fe0f} eye",
        "می\u{200c}خواهم",
    ];

    #[test]
    fn escapes_everything_unsafe_in_prose() {
        for (text, wanted) in UNSAFE {
            assert_eq!(escaped(text), wanted, "{text:?}");
        }
    }

    #[test]
    fn removes_everything_unsafe_from_prose() {
        for (text, _) in UNSAFE {
            let cleaned = without_hidden(text);
            assert!(cleaned.removed > 0, "{text:?}");
            assert_eq!(without_hidden(&cleaned.text).removed, 0, "{text:?}");
            assert!(!cleaned.text.contains('\u{1b}'), "{text:?}");
        }
        assert_eq!(
            without_hidden("a\u{1b}[2Jb\u{202e}c"),
            Cleaned {
                text: "a[2Jbc".to_owned(),
                removed: 2
            }
        );
    }

    #[test]
    fn leaves_safe_prose_alone() {
        for text in SAFE {
            assert_eq!(escaped(text), text, "{text:?}");
            assert_eq!(
                without_hidden(text),
                Cleaned {
                    text: text.to_owned(),
                    removed: 0
                },
                "{text:?}"
            );
        }
    }

    #[test]
    fn doesnt_keep_runs_of_selectors_or_joiners() {
        for (text, kept) in [
            ("a\u{fe0f}\u{fe0f}\u{fe01}b", "a\u{fe0f}b"),
            ("\u{1f600}\u{fe00}\u{fe01}\u{fe02}", "\u{1f600}"),
            ("\u{1f600}\u{fe0e}", "\u{1f600}\u{fe0e}"),
            ("a\u{fe05}b", "ab"),
            ("a\u{200d}\u{200d}b", "a\u{200d}b"),
            ("a\u{200c}\u{200d}\u{200c}b", "a\u{200c}b"),
            ("\u{200d}a", "a"),
            ("\u{fe0f}a", "a"),
            ("a \u{200d}b", "a b"),
            ("a\n\u{fe0f}b", "a\nb"),
            ("a\u{200d}\u{fe0f}b", "a\u{200d}b"),
        ] {
            assert_eq!(without_hidden(text).text, kept, "{text:?}");
        }
        assert_eq!(escaped("a\u{fe0f}\u{fe01}"), "a\u{fe0f}\\357\\270\\201");
    }

    #[test]
    fn one_line_escapes_line_breaks_too() {
        for (text, wanted) in [
            ("a\nb", "a\\nb"),
            ("a\tb", "a\\tb"),
            ("a\r\nb", "a\\r\\nb"),
            ("a\u{1b}[31mb", "a\\033[31mb"),
            ("a\u{200d}b", "a\\342\\200\\215b"),
            ("Kjøre.txt", "Kjøre.txt"),
            ("\u{1f600}\u{fe0f}.txt", "\u{1f600}\u{fe0f}.txt"),
            ("a\u{fe0f}\u{fe0f}.txt", "a\u{fe0f}\\357\\270\\217.txt"),
            ("a\u{fe01}.txt", "a\\357\\270\\201.txt"),
            ("\u{fe0f}a", "\\357\\270\\217a"),
        ] {
            assert_eq!(one_line(text), wanted, "{text:?}");
        }
    }

    #[test]
    fn escapes_the_rarer_invisible_controls_too() {
        for character in [
            '\u{13430}',
            '\u{1343f}',
            '\u{fff0}',
            '\u{e0080}',
            '\u{e0fff}',
        ] {
            assert!(is_unsafe_to_show(character), "{character:?}");
        }
    }

    #[test]
    fn knows_what_can_hide_text_from_letters_and_emoji() {
        for unsafe_character in [
            '\u{00ad}',
            '\u{061c}',
            '\u{180e}',
            '\u{200b}',
            '\u{200f}',
            '\u{2028}',
            '\u{202e}',
            '\u{2060}',
            '\u{2066}',
            '\u{206f}',
            '\u{feff}',
            '\u{fff9}',
            '\u{fffb}',
            '\u{e0001}',
        ] {
            assert!(is_unsafe_to_show(unsafe_character), "{unsafe_character:?}");
        }
        for safe in [
            'æ',
            'ø',
            'å',
            'ß',
            '日',
            'ع',
            '\u{1f600}',
            '\u{fe0f}',
            '\u{2027}',
            '\u{202f}',
        ] {
            assert!(!is_unsafe_to_show(safe), "{safe:?}");
        }
    }

    #[test]
    fn normalizes_line_ends_and_the_blank_lines_around_the_text() {
        assert_eq!(normalize("\n\nfeat: x  \n\nbody\t\n\n"), "feat: x\n\nbody");
        assert_eq!(normalize("  kept indent\n"), "  kept indent");
        assert_eq!(normalize("a\r\nb"), "a\nb");
        assert_eq!(normalize(""), "");
    }
}
