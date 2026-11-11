//! Reading Rust source for the modules it declares and refers to, line by line.
//!
//! A module is referred to by its name in a path position: in a `use` statement (all of it,
//! over several lines), as the first name of a path such as `safe_text::escaped`, and after
//! `crate::`, `self::` and `super::`. The paths of `std`, `core` and `alloc` are left out.
//! The names of other crates still count, since telling a crate from a module takes the
//! `Cargo.toml`. This doesn't resolve paths, so a name that's the same as a module's but
//! isn't one counts too.

use crate::split::Line;
use std::borrow::Cow;
use std::iter::Peekable;
use std::path::Path;
use std::str::CharIndices;

/// Files that stand for their folder: `staging/mod.rs` is the module `staging`, and
/// `crates/x/src/lib.rs` the crate `x`.
const FOLDER_FILES: [&str; 3] = ["mod.rs", "lib.rs", "main.rs"];
/// What a path can start with that isn't a module's name.
const PATH_ROOTS: [&str; 3] = ["crate", "self", "super"];
/// The crates that come with Rust, whose paths hold no module of the project.
const STANDARD_CRATES: [&str; 3] = ["std", "core", "alloc"];
/// The words of a `use` statement that aren't names of modules.
const USE_KEYWORDS: [&str; 6] = ["as", "self", "super", "crate", "pub", "in"];

/// The modules that the added `mod X;` lines declare, with any visibility.
pub(super) fn declared_modules<'a>(
    lines: impl Iterator<Item = Line<'a>>,
) -> impl Iterator<Item = &'a str> {
    lines.filter_map(|line| match line {
        Line::Added(text) => declared_module(without_comment(text)),
        Line::Kept(_) => None,
    })
}

/// The names of modules the added lines refer to. A `use` statement counts as a whole, with
/// the lines of it that were there before, until its semicolon.
pub(super) fn references<'a>(lines: impl Iterator<Item = Line<'a>>) -> Vec<&'a str> {
    let mut statement = Statement::Closed;
    let mut names = Vec::new();
    for line in lines {
        let (text, added) = match line {
            Line::Added(text) => (text, true),
            Line::Kept(text) => (text, false),
        };
        let text = without_comment(text);
        if declared_module(text).is_some() {
            continue;
        }
        let found = names_in_line(text, &mut statement);
        if added {
            names.extend(found);
        }
    }
    names
}

/// Where the lines read so far leave a `use` statement.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Statement {
    Closed,
    /// Open after a line without its semicolon, in the paths of another crate if `foreign`.
    Open {
        foreign: bool,
    },
}

fn names_in_line<'a>(text: &'a str, statement: &mut Statement) -> Vec<&'a str> {
    let (rest, foreign) = match *statement {
        Statement::Open { foreign } => (Some(text), foreign),
        Statement::Closed => (after_keyword(text, "use"), false),
    };
    let Some(rest) = rest else {
        return first_names_of_paths(text);
    };
    let (body, ended) = split_at_semicolon(rest);
    if !looks_like_paths(body) {
        *statement = Statement::Closed;
        return Vec::new();
    }
    let foreign = foreign || starts_in_a_standard_crate(body, *statement);
    *statement = if ended {
        Statement::Closed
    } else {
        Statement::Open { foreign }
    };
    if foreign {
        return Vec::new();
    }
    identifiers(body)
        .filter(|name| !USE_KEYWORDS.contains(name))
        .collect()
}

/// `text` up to its semicolon, and whether it has one.
fn split_at_semicolon(text: &str) -> (&str, bool) {
    match text.split_once(';') {
        Some((body, _)) => (body, true),
        None => (text, false),
    }
}

/// `line` without a `//` comment at its end. A `//` in a string or a character literal is
/// no comment.
fn without_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut chars = line.char_indices().peekable();
    while let Some((at, character)) = chars.next() {
        match character {
            '\\' if in_string => {
                chars.next();
            }
            '"' => in_string = !in_string,
            '\'' if !in_string => skip_character_literal(&mut chars),
            '/' if !in_string && line[at..].starts_with("//") => return &line[..at],
            _ => {}
        }
    }
    line
}

/// Moves past the character literal that starts after a `'`, if it is one and not a lifetime.
fn skip_character_literal(chars: &mut Peekable<CharIndices<'_>>) {
    let mut ahead = chars.clone();
    match (ahead.next(), ahead.next()) {
        (Some((_, '\\')), _) => {
            chars.nth(1);
            chars.by_ref().find(|&(_, character)| character == '\'');
        }
        (Some(_), Some((_, '\''))) => {
            chars.nth(1);
        }
        _ => {}
    }
}

/// Whether the first word of the paths in a statement that just opened is a standard crate.
fn starts_in_a_standard_crate(body: &str, before: Statement) -> bool {
    before == Statement::Closed
        && identifiers(body)
            .next()
            .is_some_and(|word| STANDARD_CRATES.contains(&word))
}

/// Whether `text` is made of what a `use` statement is: names, `::`, braces, commas, `*`, and
/// `as` between a name and another. Anything else is a line of prose or of another language.
fn looks_like_paths(text: &str) -> bool {
    text.split([':', '{', '}', ',', '*']).all(|part| {
        let words: Vec<&str> = part.split_whitespace().collect();
        let names = words
            .iter()
            .all(|word| word.chars().all(is_identifier_char));
        names && (words.len() <= 1 || (words.len() == 3 && words[1] == "as"))
    })
}

/// For each path in `line` with more than one name, its first name, or for a path that starts
/// with `crate`, `self` or `super`, the name after them.
fn first_names_of_paths(line: &str) -> Vec<&str> {
    line.match_indices("::")
        .filter_map(|(at, _)| first_name_of_path(line, at))
        .collect()
}

fn first_name_of_path(line: &str, colons_at: usize) -> Option<&str> {
    let before = &line[..colons_at];
    let name = trailing_identifier(before)?;
    if before[..before.len() - name.len()].ends_with(':') || STANDARD_CRATES.contains(&name) {
        return None;
    }
    if PATH_ROOTS.contains(&name) {
        return name_after_roots(&line[colons_at + 2..]);
    }
    Some(name)
}

fn name_after_roots(mut path: &str) -> Option<&str> {
    loop {
        let name = leading_identifier(path)?;
        let after_root = path[name.len()..]
            .strip_prefix("::")
            .filter(|_| PATH_ROOTS.contains(&name));
        match after_root {
            Some(after) => path = after,
            None => return Some(name).filter(|name| !PATH_ROOTS.contains(name)),
        }
    }
}

fn is_identifier_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn starts_an_identifier(word: &str) -> bool {
    word.chars().next().is_some_and(|c| !c.is_ascii_digit())
}

fn identifiers(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !is_identifier_char(c))
        .filter(|word| starts_an_identifier(word))
}

fn leading_identifier(text: &str) -> Option<&str> {
    let end = text
        .find(|c: char| !is_identifier_char(c))
        .unwrap_or(text.len());
    Some(&text[..end]).filter(|word| starts_an_identifier(word))
}

fn trailing_identifier(text: &str) -> Option<&str> {
    let start = text
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_identifier_char(c))
        .last()
        .map_or(text.len(), |(index, _)| index);
    Some(&text[start..]).filter(|word| starts_an_identifier(word))
}

/// What follows `keyword` in a line that starts with it, after any `pub` and its restriction.
fn after_keyword<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let line = skip_attributes(line);
    let line = match line.strip_prefix("pub") {
        Some(rest) if rest.starts_with(|c: char| c.is_whitespace() || c == '(') => {
            without_restriction(rest.trim_start())
        }
        _ => line,
    };
    let rest = line.strip_prefix(keyword)?;
    rest.starts_with(char::is_whitespace)
        .then(|| rest.trim_start())
}

/// `line` without the `#[...]` attributes it starts with, which can come before an item on
/// the same line.
fn skip_attributes(line: &str) -> &str {
    let mut line = line.trim_start();
    while let Some(inside) = line.strip_prefix("#[") {
        match closing_bracket(inside) {
            Some(end) => line = inside[end + 1..].trim_start(),
            None => break,
        }
    }
    line
}

/// Where the `]` is that closes the `[` before `text`.
fn closing_bracket(text: &str) -> Option<usize> {
    let mut depth = 1;
    for (at, character) in text.char_indices() {
        depth += i32::from(character == '[') - i32::from(character == ']');
        if depth == 0 {
            return Some(at);
        }
    }
    None
}

/// `text` without the `(crate)` or `(in path)` that follows `pub`.
fn without_restriction(text: &str) -> &str {
    match text
        .strip_prefix('(')
        .and_then(|inner| inner.split_once(')'))
    {
        Some((_, after)) => after.trim_start(),
        None => text,
    }
}

/// The module a `mod X;` line declares, with any visibility.
fn declared_module(line: &str) -> Option<&str> {
    let rest = after_keyword(line, "mod")?;
    let name = leading_identifier(rest)?;
    rest[name.len()..]
        .trim_start()
        .starts_with(';')
        .then_some(name)
}

// ---- Files -----------------------------------------------------------------------------

pub(super) fn is_rust(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension == "rs")
}

/// What Rust code calls the module in the file at `path`: its name without the extension,
/// or its folder's name for a file that stands for the folder.
pub(super) fn module_name(path: &str) -> Option<Cow<'_, str>> {
    let path = Path::new(path);
    if path.extension()? != "rs" {
        return None;
    }
    if FOLDER_FILES.contains(&path.file_name()?.to_str()?) {
        return folder_name(path).map(crate_name);
    }
    path.file_stem()?.to_str().map(Cow::Borrowed)
}

/// The folder a file stands for, which for a crate's `src` folder is the crate's.
fn folder_name(path: &Path) -> Option<&str> {
    let folder = path.parent()?;
    let folder = match folder.file_name() {
        Some(name) if name == "src" => folder.parent()?,
        _ => folder,
    };
    folder.file_name()?.to_str()
}

/// A crate's name in code, which has `_` where its folder has `-`.
fn crate_name(folder: &str) -> Cow<'_, str> {
    if folder.contains('-') {
        Cow::Owned(folder.replace('-', "_"))
    } else {
        Cow::Borrowed(folder)
    }
}

/// Whether Cargo compiles the file at `path` without a declaration, by its layout: a crate's
/// `src/main.rs` and `src/lib.rs`, `src/bin/*.rs` and `src/bin/*/main.rs`, a build script
/// next to its `Cargo.toml`, and the `*.rs` and `*/main.rs` of `tests`, `benches` and
/// `examples` outside `src`. A crate can be in a folder, as in a workspace.
pub(super) fn is_crate_root(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    let Some((&name, folders)) = parts.split_last() else {
        return false;
    };
    let outside_src = !folders.contains(&"src");
    match folders {
        [.., "src"] => matches!(name, "main.rs" | "lib.rs"),
        [.., "src", "bin"] => name.ends_with(".rs"),
        [.., "src", "bin", _] => name == "main.rs",
        [.., "tests" | "benches" | "examples"] if outside_src => name.ends_with(".rs"),
        [.., "tests" | "benches" | "examples", _] if outside_src => name == "main.rs",
        _ => name == "build.rs" && outside_src,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn added<'a>(lines: &[&'a str]) -> Vec<Line<'a>> {
        lines.iter().map(|line| Line::Added(line)).collect()
    }

    /// The names the lines refer to, each once, in order of name.
    fn names_of(lines: Vec<Line<'_>>) -> Vec<&str> {
        let mut names = references(lines.into_iter());
        names.sort_unstable();
        names.dedup();
        names
    }

    fn refers_to_safe_text(line: &str) -> bool {
        names_of(added(&[line])).contains(&"safe_text")
    }

    #[test]
    fn refers_to_a_module_in_a_path_position() {
        for line in [
            "use crate::safe_text;",
            "use crate::safe_text::escaped;",
            "use safe_text::escaped;",
            "use crate::{config, safe_text};",
            "use crate::{a, b::{c, safe_text::d}};",
            "use super::safe_text;",
            "use self::safe_text::x;",
            "use crate::a::safe_text::x;",
            "use crate::a::safe_text as escapes;",
            "pub use crate::safe_text::*;",
            "pub(crate) use crate::safe_text;",
            "    use super::super::safe_text::f;",
            "let escaped = safe_text::escaped(text);",
            "x = crate::safe_text::escaped(y)",
            "super::super::safe_text::f()",
            "self::safe_text::f()",
            "let v = Vec::<safe_text::Kind>::new();",
            "safe_text::<u8>::f()",
            "use serde_json::{Value, safe_text};",
        ] {
            assert!(refers_to_safe_text(line), "{line}");
        }
    }

    #[test]
    fn doesnt_refer_to_a_module_by_a_name_that_isnt_in_a_path_position() {
        for line in [
            "let safe_text = 1;",
            "fn safe_text() {}",
            "x.safe_text();",
            "safe_text(1);",
            "let s = \"safe_text\";",
            "use crate::unsafe_text;",
            "use crate::safe_text2;",
            "use crate::safe_texts;",
            "let safe_textual = 1;",
            "let my_safe_text = 1;",
            "let safeText = 1;",
            "mod safe_text {",
            "mod safe_text2;",
            "usage(safe_text)",
            "a::safe_text::x();",
            "<T as a::Trait>::safe_text()",
        ] {
            assert!(!refers_to_safe_text(line), "{line}");
        }
    }

    #[test]
    fn names_each_module_of_a_use_statement_once_and_leaves_out_its_keywords() {
        let line = "pub(crate) use crate::{config, a::safe_text as other, self, super::x::*};";
        assert_eq!(
            names_of(added(&[line])),
            ["a", "config", "other", "safe_text", "x"]
        );
        assert_eq!(names_of(added(&["use super::super::x::y;"])), ["x", "y"]);
    }

    #[test]
    fn reads_a_use_statement_over_several_lines_as_a_whole() {
        let rustfmt = [
            "use crate::{",
            "    config,",
            "    safe_text,",
            "    Other,",
            "};",
        ];
        assert_eq!(names_of(added(&rustfmt)), ["Other", "config", "safe_text"]);
        let nested = [
            "pub use crate::{",
            "    a::{",
            "        b,",
            "        safe_text::Kind,",
            "    },",
            "};",
        ];
        assert_eq!(names_of(added(&nested)), ["Kind", "a", "b", "safe_text"]);
    }

    #[test]
    fn ends_a_use_statement_at_its_semicolon() {
        let after = [
            "use crate::{",
            "    config,",
            "};",
            "let safe_text = 1;",
            "    safe_text,",
        ];
        assert_eq!(names_of(added(&after)), ["config"]);
        let same_line = ["use crate::config; safe_text", "    safe_text,"];
        assert_eq!(names_of(added(&same_line)), ["config"]);
    }

    #[test]
    fn takes_names_from_added_lines_only_and_the_statement_from_all_the_lines() {
        let lines = [
            Line::Kept("use crate::{"),
            Line::Kept("    config,"),
            Line::Added("    safe_text,"),
            Line::Kept("};"),
            Line::Kept("    other,"),
        ];
        assert_eq!(names_of(lines.to_vec()), ["safe_text"]);
        let nothing_added = [Line::Kept("use crate::{"), Line::Kept("    config,")];
        assert!(names_of(nothing_added.to_vec()).is_empty());
        let closed_before = [
            Line::Kept("use crate::config;"),
            Line::Added("    safe_text,"),
        ];
        assert!(names_of(closed_before.to_vec()).is_empty());
    }

    #[test]
    fn stops_a_use_statement_at_a_line_that_isnt_code() {
        let prose = [
            "use short subjects and mention the config file",
            "safe_text, then",
            "mention the config file;",
        ];
        assert!(names_of(added(&prose)).is_empty());
        let after_a_statement = [
            "use crate::{",
            "    config,",
            "use short subjects",
            "    safe_text,",
        ];
        assert_eq!(names_of(added(&after_a_statement)), ["config"]);
        for text in ["use \"safe_text\";", "use safe_text.rs", "use a-b::c;"] {
            assert!(names_of(added(&[text])).is_empty(), "{text}");
        }
    }

    #[test]
    fn skips_comment_lines() {
        for line in [
            "// use crate::safe_text;",
            "/// Uses safe_text::escaped.",
            "//! See crate::safe_text.",
            "    // safe_text::f()",
        ] {
            assert!(!refers_to_safe_text(line), "{line}");
        }
        let inside = [
            "use crate::{",
            "    // safe_text,",
            "    config, // safe_text",
            "};",
        ];
        assert_eq!(names_of(added(&inside)), ["config"]);
    }

    #[test]
    fn cuts_a_comment_at_the_end_of_any_line_and_not_a_slash_pair_in_a_literal() {
        for line in [
            "let x = 1; // moved from crate::safe_text",
            "foo(); // safe_text::escaped()",
            "let url = \"https://x\"; // safe_text::f()",
            "let q = '\"'; // safe_text::f()",
            "let q = '\\\\'; // safe_text::f()",
            "let a = \"//\"; // safe_text::f()",
            "use crate::config; // safe_text",
            "mod config; // see safe_text::x",
        ] {
            assert!(!refers_to_safe_text(line), "{line}");
        }
        for line in [
            "let url = \"https://x\"; safe_text::f();",
            "let q = '\"'; safe_text::f() // c",
            "let s = \"a\\\"//b\"; safe_text::f();",
            "fn f<'a>(x: &'a str) { safe_text::f() } // c",
            "let q = '/'; safe_text::f();",
            "let q = '\\n'; safe_text::f();",
        ] {
            assert!(refers_to_safe_text(line), "{line}");
        }
    }

    #[test]
    fn reads_an_item_after_attributes_on_the_same_line() {
        for line in [
            "#[cfg(test)] use crate::{config, safe_text};",
            "#[cfg(test)] #[allow(unused)] pub use crate::safe_text;",
            "  #[cfg(any(a, b))]  use safe_text::f;",
            "#[cfg_attr(x, y([1, 2]))] use crate::{config, safe_text};",
        ] {
            assert!(refers_to_safe_text(line), "{line}");
        }
        fn declared(line: &str) -> Option<&str> {
            declared_modules(added(&[line]).into_iter()).next()
        }
        assert_eq!(declared("#[cfg(test)] mod safe_text;"), Some("safe_text"));
        assert_eq!(
            declared("#[cfg(test)] #[doc(hidden)] pub(crate) mod safe_text;"),
            Some("safe_text")
        );
        assert_eq!(declared("#[path = \"gen/x.rs\"] mod other;"), Some("other"));
        assert_eq!(declared("#[cfg(test mod safe_text;"), None);
        assert_eq!(declared("#[cfg(test)]"), None);
        assert!(names_of(added(&["#[cfg(test)]"])).is_empty());
    }

    #[test]
    fn leaves_out_the_paths_of_the_standard_crates() {
        for line in [
            "use std::io;",
            "use std::error::Error;",
            "use std::{env, fs};",
            "use core::fmt;",
            "use alloc::vec::Vec;",
            "pub use std::io::Write as W;",
            "let line = std::io::stdin();",
            "std::env::var(\"a\")",
        ] {
            assert!(names_of(added(&[line])).is_empty(), "{line}");
        }
        let several_lines = [
            "use std::{",
            "    io::Write,",
            "    fs,",
            "};",
            "use crate::io;",
        ];
        assert_eq!(names_of(added(&several_lines)), ["io"]);
    }

    #[test]
    fn counts_what_it_cant_tell_from_a_module_and_pins_what_it_gets_wrong() {
        // Known gap: a path that starts with a name that an earlier `use` brought in, such as
        // `io` after `use std::io;`, counts as a module's name, and so does another crate's.
        assert_eq!(names_of(added(&["let line = io::stdin();"])), ["io"]);
        assert_eq!(
            names_of(added(&["use serde_json::{Value, json};"])),
            ["Value", "json", "serde_json"]
        );
        // Known gap: prose that starts with `use` and one word opens a statement, so that word
        // counts, and so do the names of the lines that follow until a semicolon.
        assert_eq!(names_of(added(&["use config"])), ["config"]);
        assert_eq!(
            names_of(added(&["use config", "    safe_text,"])),
            ["config", "safe_text"]
        );
    }

    #[test]
    fn declares_a_module_with_any_visibility() {
        fn declared(line: &str) -> Option<&str> {
            declared_modules(added(&[line]).into_iter()).next()
        }
        for line in [
            "mod safe_text;",
            "pub mod safe_text;",
            "pub(crate) mod safe_text;",
            "pub(super) mod safe_text;",
            "pub(in crate::a) mod safe_text;",
            "    mod safe_text; // the escapes",
            "pub  mod  safe_text ;",
        ] {
            assert_eq!(declared(line), Some("safe_text"), "{line}");
        }
        for line in [
            "modsafe_text;",
            "pub modx safe_text;",
            "mod safe_text {",
            "xmod safe_text;",
            "// mod safe_text;",
            "let m = \"mod safe_text;\";",
        ] {
            assert_eq!(declared(line), None, "{line}");
        }
        assert_eq!(
            declared_modules([Line::Kept("mod a;")].into_iter()).next(),
            None
        );
    }

    #[test]
    fn doesnt_count_a_declaration_as_a_reference() {
        assert!(names_of(added(&["pub mod safe_text;"])).is_empty());
    }

    #[test]
    fn names_a_file_by_its_stem_or_its_folder() {
        for (path, name) in [
            ("src/safe_text.rs", Some("safe_text")),
            ("safe_text.rs", Some("safe_text")),
            ("src/staging/mod.rs", Some("staging")),
            ("mod.rs", None),
            ("crates/x/src/lib.rs", Some("x")),
            ("crates/x/src/main.rs", Some("x")),
            ("crates/safe-text/src/lib.rs", Some("safe_text")),
            ("safe-text/src/main.rs", Some("safe_text")),
            ("src/bin/tool/main.rs", Some("tool")),
            ("src/lib.rs", None),
            ("src/main.rs", None),
            ("lib.rs", None),
            ("tool/lib.rs", Some("tool")),
            ("src/mod.rs.bak", None),
            ("src/modx.rs", Some("modx")),
            ("src/library.rs", Some("library")),
            ("src/café.rs", Some("café")),
            ("web/index.ts", None),
            ("src/safe_text.txt", None),
            ("", None),
        ] {
            assert_eq!(module_name(path).as_deref(), name, "{path}");
        }
    }

    #[test]
    fn knows_the_files_cargo_compiles_without_a_declaration() {
        for path in [
            "src/main.rs",
            "src/lib.rs",
            "crates/x/src/lib.rs",
            "crates/x/src/main.rs",
            "build.rs",
            "crates/x/build.rs",
            "tests/cli.rs",
            "tests/cli/main.rs",
            "crates/x/tests/cli.rs",
            "benches/speed.rs",
            "examples/demo.rs",
            "examples/demo/main.rs",
            "src/bin/tool.rs",
            "src/bin/tool/main.rs",
            "crates/x/src/bin/tool.rs",
        ] {
            assert!(is_crate_root(path), "{path}");
        }
    }

    #[test]
    fn doesnt_take_a_file_for_one_that_cargo_compiles_by_a_name_alone() {
        for path in [
            "src/util.rs",
            "src/util/mod.rs",
            "src/tests/helpers.rs",
            "src/tests.rs",
            "src/build.rs",
            "src/foo/build.rs",
            "crates/x/src/util/build.rs",
            "src/foo/main.rs",
            "src/foo/lib.rs",
            "src/bin/tool/helper.rs",
            "src/bin/tool/sub/main.rs",
            "tests/common/mod.rs",
            "tests/cli/helper.rs",
            "tests/cli/sub/main.rs",
            "benches/a/b/main.rs",
            "src/benches/speed.rs",
            "crates/x/src/examples/demo.rs",
            "src/bins.rs",
            "src/bin.rs",
            "main.rs",
            "lib.rs",
            "main.txt",
            "crates/x/build.rs.bak",
            "",
        ] {
            assert!(!is_crate_root(path), "{path}");
        }
    }
}
