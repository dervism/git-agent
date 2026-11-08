//! Reads Rust source well enough to tell which module uses which. It takes the comments out,
//! empties the strings, finds the `crate::` paths, and leaves out what's marked
//! `#[cfg(test)]`. What it can't follow, it refuses or reports, so that a check never passes
//! because it missed something.

use std::collections::BTreeSet;

/// `source` with every comment taken out and every string and character literal emptied,
/// so that only code is left to search.
pub fn code_only(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut code = String::with_capacity(source.len());
    let mut at = 0;
    while at < chars.len() {
        let next = chars.get(at + 1).copied();
        at = match chars[at] {
            '/' if next == Some('/') => line_end(&chars, at),
            '/' if next == Some('*') => block_comment_end(&chars, at),
            '"' => {
                code.push_str("\"\"");
                string_end(&chars, at)
            }
            'r' if starts_raw_string(&chars, at) => {
                code.push_str("\"\"");
                raw_string_end(&chars, at)
            }
            '\'' => match char_literal_end(&chars, at) {
                Some(end) => {
                    code.push_str("' '");
                    end
                }
                None => {
                    code.push('\'');
                    at + 1
                }
            },
            other => {
                code.push(other);
                at + 1
            }
        };
    }
    code
}

fn line_end(chars: &[char], at: usize) -> usize {
    chars[at..]
        .iter()
        .position(|&character| character == '\n')
        .map_or(chars.len(), |offset| at + offset)
}

/// Where a block comment ends, counting the comments nested in it.
fn block_comment_end(chars: &[char], start: usize) -> usize {
    let mut depth = 0;
    let mut at = start;
    while at + 1 < chars.len() {
        match (chars[at], chars[at + 1]) {
            ('/', '*') => {
                depth += 1;
                at += 2;
            }
            ('*', '/') => {
                depth -= 1;
                at += 2;
                if depth == 0 {
                    return at;
                }
            }
            _ => at += 1,
        }
    }
    chars.len()
}

/// Where a string that opens at `start` ends, after its closing quote.
fn string_end(chars: &[char], start: usize) -> usize {
    let mut at = start + 1;
    while at < chars.len() {
        match chars[at] {
            '\\' => at += 2,
            '"' => return at + 1,
            _ => at += 1,
        }
    }
    chars.len()
}

fn in_a_name(character: Option<char>) -> bool {
    character.is_some_and(|character| character.is_alphanumeric() || character == '_')
}

/// Whether `r` at `at` opens a raw string, such as `r"…"` or `br#"…"#`, rather than being
/// part of a name or a raw name such as `r#type`.
fn starts_raw_string(chars: &[char], at: usize) -> bool {
    let before = |back: usize| at.checked_sub(back).map(|index| chars[index]);
    let prefixed = match before(1) {
        Some('b' | 'c') => !in_a_name(before(2)),
        other => !in_a_name(other),
    };
    let hashes = chars[at + 1..]
        .iter()
        .take_while(|&&character| character == '#')
        .count();
    prefixed && chars.get(at + 1 + hashes) == Some(&'"')
}

fn raw_string_end(chars: &[char], start: usize) -> usize {
    let hashes = chars[start + 1..]
        .iter()
        .take_while(|&&character| character == '#')
        .count();
    let closing: Vec<char> = std::iter::once('"')
        .chain(std::iter::repeat_n('#', hashes))
        .collect();
    let body = start + 2 + hashes;
    chars[body..]
        .windows(closing.len())
        .position(|window| window == closing.as_slice())
        .map_or(chars.len(), |offset| body + offset + closing.len())
}

/// Where a character literal that opens at `start` ends, or `None` for a lifetime or a label.
fn char_literal_end(chars: &[char], start: usize) -> Option<usize> {
    match chars.get(start + 1)? {
        '\\' => chars
            .get(start + 3..)?
            .iter()
            .position(|&character| character == '\'')
            .map(|offset| start + 3 + offset + 1),
        _ if chars.get(start + 2) == Some(&'\'') => Some(start + 3),
        _ => None,
    }
}

/// The modules that `crate::` paths in `code` name, such as `git` for `crate::git::Repo`
/// and `ui` and `git` for `crate::{ui, git::names}`.
pub fn crate_paths(code: &str) -> BTreeSet<String> {
    crate_path_starts(code)
        .flat_map(|rest| match rest.strip_prefix('{') {
            Some(group) => group_items(group)
                .iter()
                .filter_map(|item| first_name(item))
                .collect(),
            None => first_name(rest).into_iter().collect::<Vec<_>>(),
        })
        .filter(|name| name != "self")
        .collect()
}

/// What follows each `crate::` in `code`. Inside this crate's macros, `$crate::` is the same.
fn crate_path_starts(code: &str) -> impl Iterator<Item = &str> {
    code.match_indices("crate::").filter_map(|(index, path)| {
        let before = code[..index].chars().next_back();
        (!in_a_name(before)).then(|| code[index + path.len()..].trim_start())
    })
}

/// The items of a `{…}` group of paths, from just after its opening brace, each trimmed.
fn group_items(group: &str) -> Vec<String> {
    let mut depth = 0;
    let mut items = vec![String::new()];
    for character in group.chars() {
        match character {
            '}' if depth == 0 => break,
            ',' if depth == 0 => {
                items.push(String::new());
                continue;
            }
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
        if let Some(item) = items.last_mut() {
            item.push(character);
        }
    }
    items
        .iter()
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

fn first_name(text: &str) -> Option<String> {
    let name: String = text
        .trim_start()
        .chars()
        .take_while(|&character| in_a_name(Some(character)))
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Code with the items marked `#[cfg(test)]` left out, and the names of the test modules it
/// declares in files of their own, as `#[cfg(test)] mod layers;` does.
pub struct WithoutTests {
    pub code: String,
    pub test_modules: Vec<String>,
}

/// The item kinds `#[cfg(test)]` may mark here. Anything else, such as a field or a match
/// arm, isn't left out but refused, since the end of it can't be told safely.
const ITEMS: [&str; 10] = [
    "mod", "fn", "impl", "struct", "enum", "use", "const", "static", "type", "trait",
];

/// `code` without the items marked `#[cfg(test)]`. Panics on one it can't follow.
pub fn without_tests(code: &str) -> WithoutTests {
    let mut kept = String::with_capacity(code.len());
    let mut test_modules = Vec::new();
    let mut rest = code;
    while let Some(index) = rest.find("#[cfg(test)]") {
        kept.push_str(&rest[..index]);
        let item = after_attributes(&rest[index..]);
        let (end, module) = item_end(item).unwrap_or_else(|| {
            let shown: String = item.chars().take(60).collect();
            panic!("can't tell where the item marked #[cfg(test)] ends: {shown:?}")
        });
        test_modules.extend(module);
        rest = &item[end..];
    }
    kept.push_str(rest);
    WithoutTests {
        code: kept,
        test_modules,
    }
}

/// `text` from the item after the attributes it starts with.
fn after_attributes(text: &str) -> &str {
    let mut rest = text.trim_start();
    while rest.starts_with('#') {
        let Some(open) = rest.find('[') else {
            return rest;
        };
        rest = rest[closing(rest, open, '[', ']')..].trim_start();
    }
    rest
}

/// Where the bracket that opens at `open` closes, after it.
fn closing(text: &str, open: usize, opens: char, closes: char) -> usize {
    let mut depth = 0;
    for (index, character) in text[open..].char_indices() {
        if character == opens {
            depth += 1;
        } else if character == closes {
            depth -= 1;
            if depth == 0 {
                return open + index + character.len_utf8();
            }
        }
    }
    text.len()
}

/// Where an item ends, after its block or its semicolon, and the module's name if it's a
/// `mod` declared without a body. `None` if it isn't an item, or its end can't be found.
fn item_end(item: &str) -> Option<(usize, Option<String>)> {
    let start = item.len() - without_qualifiers(item)?.len();
    let words = &item[start..];
    let keyword = first_name(words)?;
    if !ITEMS.contains(&keyword.as_str()) {
        return None;
    }
    let body = start + keyword.len();
    let end = end_at_depth_zero(item, body)?;
    let module = (keyword == "mod" && item[..end].ends_with(';'))
        .then(|| first_name(&item[body..]))
        .flatten();
    Some((end, module))
}

/// `item` after the words that may come before its keyword, such as `pub(crate) async`.
fn without_qualifiers(item: &str) -> Option<&str> {
    let mut rest = item.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("pub(") {
            rest = after[after.find(')')? + 1..].trim_start();
        } else if let Some(word) =
            first_name(rest).filter(|word| ["pub", "async", "unsafe"].contains(&word.as_str()))
        {
            rest = rest[word.len()..].trim_start();
        } else {
            return Some(rest);
        }
    }
}

/// From `from`, where the item ends: after the first block outside parentheses, brackets and
/// angle brackets, or after the first semicolon outside them. `None` when a semicolon comes
/// inside angle brackets, as in `const A: bool = 1 < 2;` where `<` can't be told from a
/// generic, or when a bracket closes that didn't open.
fn end_at_depth_zero(item: &str, from: usize) -> Option<usize> {
    let (mut brackets, mut angles) = (0_i32, 0_i32);
    let mut previous = ' ';
    let mut at = from;
    while let Some(character) = item[at..].chars().next() {
        match character {
            '(' | '[' => brackets += 1,
            ')' | ']' => brackets -= 1,
            '<' => angles += 1,
            '>' if !matches!(previous, '-' | '=') => angles -= 1,
            '{' if brackets == 0 && angles == 0 => return Some(closing(item, at, '{', '}')),
            '{' => {
                at = closing(item, at, '{', '}');
                previous = '}';
                continue;
            }
            ';' if angles > 0 => return None,
            ';' if brackets == 0 => return Some(at + 1),
            _ => {}
        }
        if brackets < 0 || angles < 0 {
            return None;
        }
        previous = character;
        at += character.len_utf8();
    }
    None
}

/// The forms of path in `code` that [`crate_paths`] doesn't follow, which would let a use go
/// unseen: glob imports, renamed roots, `extern crate`, and paths in strings, which only
/// `raw`, the text before its strings were emptied, still holds.
pub fn unfollowed_paths(raw: &str, code: &str) -> Vec<String> {
    let groups = crate_path_starts(code).flat_map(|rest| match rest.strip_prefix('{') {
        Some(group) => group_items(group),
        None => vec![
            rest.chars()
                .take_while(|&character| {
                    in_a_name(Some(character)) || matches!(character, '*' | ':')
                })
                .collect(),
        ],
    });
    let mut found: Vec<String> = groups
        .filter(|item| item.starts_with('*') || item.starts_with("self as"))
        .map(|item| format!("crate::{item}"))
        .collect();
    for (text, report) in [("crate as ", "crate as"), ("extern crate", "extern crate")] {
        if code
            .match_indices(text)
            .any(|(index, _)| !in_a_name(code[..index].chars().next_back()))
        {
            found.push(report.to_owned());
        }
    }
    if raw.contains("\"crate::") {
        found.push("\"crate::\" in a string".to_owned());
    }
    found
}

/// The longest run of `super::` in `code`, such as 2 for `super::super::fit` and for
/// `super::{super::fit}`.
pub fn super_run(code: &str) -> usize {
    code.match_indices("super::")
        .map(|(index, _)| {
            let mut rest = &code[index..];
            let mut run = 0;
            while let Some(after) = rest.strip_prefix("super::") {
                run += 1;
                rest = after.trim_start_matches(|character: char| {
                    character == '{' || character.is_whitespace()
                });
            }
            run
        })
        .max()
        .unwrap_or(0)
}

/// An attribute: whether it's an inner one, `#![…]`, which applies to the module it's in,
/// and the text inside its brackets.
pub struct Attribute<'a> {
    pub inner: bool,
    pub text: &'a str,
}

/// Every attribute in `code`, `#[…]` and `#![…]`.
pub fn attributes(code: &str) -> Vec<Attribute<'_>> {
    code.match_indices('#')
        .filter_map(|(index, _)| {
            let inner = code[index + 1..].starts_with('!');
            let open = index + 1 + usize::from(inner);
            code[open..].starts_with('[').then(|| Attribute {
                inner,
                text: &code[open + 1..closing(code, open, '[', ']') - 1],
            })
        })
        .collect()
}

/// The lint names an attribute gives to `level`, such as `allow`, wherever in it: in
/// `allow(clippy::a, b)` or in `cfg_attr(test, allow(c))`.
pub fn lint_names(attribute: &str, level: &str) -> Vec<String> {
    let call = format!("{level}(");
    attribute
        .match_indices(&call)
        .filter(|(index, _)| !in_a_name(attribute[..*index].chars().next_back()))
        .flat_map(|(index, _)| {
            let open = index + level.len();
            attribute[open + 1..closing(attribute, open, '(', ')') - 1]
                .split(',')
                .map(|name| name.trim().to_owned())
                .collect::<Vec<_>>()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn leaves_only_code_to_search() {
        let source = "use crate::a; // crate::b\n/* crate::c /* crate::d */ crate::e */\n\
                      let s = \"crate::f \\\" crate::g\"; let r = r#\"crate::h\"#;\n\
                      let b = br##\"crate::i\"##; let c = '\"'; let d = '\\''; let e = '\\u{27}';\n\
                      fn f<'a>(x: &'a str) { 'outer: loop {} } let r#type = 1;\n";
        let code = code_only(source);
        assert_eq!(crate_paths(&code), set(&["a"]));
        assert!(
            code.contains("fn f<'a>(x: &'a str) { 'outer: loop {} }"),
            "{code}"
        );
        assert!(code.contains("let r#type = 1;"), "{code}");
    }

    #[test]
    fn reads_groups_of_paths_over_several_lines() {
        let code = "use crate::{\n    git::{self, Repo},\n    ui as shown,\n    self,\n};\n$crate::fit::x\n";
        assert_eq!(crate_paths(code), set(&["fit", "git", "ui"]));
    }

    #[test]
    fn leaves_out_test_items_and_names_test_modules_in_files_of_their_own() {
        let code = "fn a() { x.push() }\n#[cfg(test)]\nmod tests { fn b() { y.complete() } }\n\
                    #[cfg(test)]\nuse z;\n#[cfg(test)]\n#[allow(dead_code)]\npub(crate) fn h(x: [u8; 2]) -> u8 { q() }\n\
                    #[cfg(test)]\nmod layers;\nfn c() {}\n";
        let kept = without_tests(code);
        assert!(kept.code.contains("x.push()") && kept.code.contains("fn c()"));
        for gone in ["complete", "use z", "q()", "allow", "layers"] {
            assert!(
                !kept.code.contains(gone),
                "{gone} is still in {:?}",
                kept.code
            );
        }
        assert_eq!(kept.test_modules, ["layers"]);
    }

    #[test]
    #[should_panic(expected = "can't tell where the item marked #[cfg(test)] ends")]
    fn refuses_a_test_only_field() {
        without_tests(
            "struct S {\n    #[cfg(test)]\n    calls: Vec<u8>,\n}\nfn send() { s.complete() }\n",
        );
    }

    #[test]
    #[should_panic(expected = "can't tell where the item marked #[cfg(test)] ends")]
    fn refuses_a_test_only_match_arm() {
        without_tests("match x {\n    #[cfg(test)]\n    1 => {}\n    _ => {}\n}\n");
    }

    #[test]
    fn reports_the_paths_it_cant_follow() {
        let follow = |raw: &str| unfollowed_paths(raw, &code_only(raw));
        assert!(follow("use crate::{git, ui::Shown};\nuse super::x;\n").is_empty());
        assert_eq!(follow("use crate::*;\n"), ["crate::*"]);
        assert_eq!(follow("use crate::{git, *};\n"), ["crate::*"]);
        assert_eq!(
            follow("use crate::{self as root};\n"),
            ["crate::self as root"]
        );
        assert_eq!(follow("use crate as root;\n"), ["crate as"]);
        assert_eq!(follow("extern crate self as root;\n"), ["extern crate"]);
        assert_eq!(
            follow("#[serde(with = \"crate::git::x\")]\n"),
            ["\"crate::\" in a string"]
        );
    }

    #[test]
    fn counts_how_far_super_climbs() {
        assert_eq!(super_run("use super::x;\n"), 1);
        assert_eq!(super_run("use super::super::fit;\n"), 2);
        assert_eq!(super_run("use super::{super::split::parse};\n"), 2);
        assert_eq!(super_run("use super::{\n    super::a,\n};\n"), 2);
        assert_eq!(super_run("fn f() {}\n"), 0);
    }

    #[test]
    #[should_panic(expected = "can't tell where the item marked #[cfg(test)] ends")]
    fn refuses_a_less_than_in_a_test_constant() {
        without_tests(
            "#[cfg(test)]\nconst SMALL: bool = 1 < 2;\nfn send() { if a > b { s.complete(x); } }\n",
        );
    }

    #[test]
    #[should_panic(expected = "can't tell where the item marked #[cfg(test)] ends")]
    fn refuses_a_shift_in_a_test_constant() {
        without_tests(
            "#[cfg(test)]\nconst BIG: usize = 1 << 20;\nfn f() { if a > b && c > d { r.push(); } }\n",
        );
    }

    #[test]
    fn ends_items_with_generics_closures_and_const_arguments() {
        for item in [
            "#[cfg(test)]\nimpl<T: Into<String>> From<T> for X { fn from(t: T) -> Self { X } }\nfn kept() {}",
            "#[cfg(test)]\nfn f<F: Fn(u8) -> u8>(f: F) -> Vec<u8> { vec![] }\nfn kept() {}",
            "#[cfg(test)]\ntype Uses = BTreeSet<(String, String)>;\nfn kept() {}",
            "#[cfg(test)]\nstruct A(Foo<{ 1 }>);\nfn kept() {}",
            "#[cfg(test)]\nconst X: u8 = match y { 1 => 2, _ => 3 };\nfn kept() {}",
        ] {
            let kept = without_tests(item).code;
            assert!(
                kept.trim_start_matches(';').trim() == "fn kept() {}",
                "{item:?} kept {kept:?}"
            );
        }
    }

    #[test]
    fn finds_lint_names_in_every_form_of_attribute() {
        let code = "#![allow(clippy::disallowed_methods)]\n#[allow(clippy::print_stdout, clippy::all)]\n\
                    #[cfg_attr(test, expect(dead_code))]\nfn f() { ui::warn(x) }\n";
        let found = attributes(code);
        let names = |level: &str| -> Vec<String> {
            found
                .iter()
                .flat_map(|attribute| lint_names(attribute.text, level))
                .collect()
        };
        assert_eq!(
            names("allow"),
            [
                "clippy::disallowed_methods",
                "clippy::print_stdout",
                "clippy::all"
            ]
        );
        assert_eq!(names("expect"), ["dead_code"]);
        assert!(names("warn").is_empty());
        let inner: Vec<bool> = found.iter().map(|attribute| attribute.inner).collect();
        assert_eq!(inner, [true, false, false]);
    }
}
