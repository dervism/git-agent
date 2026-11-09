//! How git writes file names, and the form git-agent shows them in.
//!
//! git puts a name with unusual characters in C-style quotes, such as
//! `"Kj\303\270re.txt"`. People and models get a readable form instead, with
//! letters as they are and control characters still escaped, so a file name
//! can't change the terminal or add lines to a prompt.

use super::lossless;
use crate::safe_text::{NAMED_ESCAPES, one_line};

/// The readable name of the file a part of a diff is about: its new name if it
/// was renamed or copied. `file` starts with the "diff --git" line, or a merge's
/// "diff --cc" line, which is all it needs to hold.
pub fn of_diff(file: &str) -> String {
    let mut lines = file.lines();
    let header = lines.next().unwrap_or_default();
    let new_name = lines
        .take_while(|line| !line.starts_with("@@"))
        .find_map(|line| {
            line.strip_prefix("rename to ")
                .or_else(|| line.strip_prefix("copy to "))
        });
    let name = new_name.map_or_else(|| from_header(header), unquoted);
    one_line(&name)
}

/// `name`, as git-agent reads it from git, in the form to show people.
pub fn shown(name: &str) -> String {
    one_line(&lossless::readable(name))
}

/// The name in a "diff --git" line, whose two names may each be quoted, or the one
/// name of a merge's "diff --cc" line.
fn from_header(header: &str) -> String {
    if let Some(name) = header
        .strip_prefix("diff --cc ")
        .or_else(|| header.strip_prefix("diff --combined "))
    {
        return unquoted(name);
    }
    let names = header.strip_prefix("diff --git ").unwrap_or(header);
    if !names.contains('"') {
        return target_of_plain_names(names).to_owned();
    }
    let (source, target) = split_quoted_names(names);
    let (source, target) = (unquoted(source), unquoted(target));
    if source == target {
        target
    } else {
        without_prefix(&target).to_owned()
    }
}

/// The second name of two unquoted ones. Both are the same apart from their
/// prefixes unless the file was renamed, so their middle is the only place
/// they can part, whatever spaces and " b/" the name holds.
fn target_of_plain_names(names: &str) -> &str {
    let middle = names.len() / 2;
    let sides = names
        .get(..middle)
        .zip(names.get(middle + 1..))
        .filter(|_| names.as_bytes().get(middle) == Some(&b' '));
    match sides {
        Some((source, target)) if source == target => target,
        Some((source, target)) if without_prefix(source) == without_prefix(target) => {
            without_prefix(target)
        }
        _ => names.rsplit_once(" b/").map_or(names, |(_, target)| target),
    }
}

/// Splits two names of which at least one is quoted. Inside quotes, every `"`
/// follows a backslash, so an unescaped one or a space before one is a border.
fn split_quoted_names(names: &str) -> (&str, &str) {
    let border = if names.starts_with('"') {
        closing_quote(names).map(|end| end + 1)
    } else {
        names.find(" \"")
    };
    border.map_or((names, names), |border| {
        let (source, target) = names.split_at(border);
        (source, target.trim_start())
    })
}

/// Where the quoted name at the start of `names` ends.
fn closing_quote(names: &str) -> Option<usize> {
    let mut escaped = false;
    for (index, byte) in names.bytes().enumerate().skip(1) {
        match byte {
            _ if escaped => escaped = false,
            b'\\' => escaped = true,
            b'"' => return Some(index),
            _ => {}
        }
    }
    None
}

/// `name` without its one-letter prefix, such as `b/`, if it has one.
fn without_prefix(name: &str) -> &str {
    match name.as_bytes() {
        [_, b'/', ..] => &name[2..],
        _ => name,
    }
}

/// What a name means that git may have put in quotes. Bytes that aren't valid
/// UTF-8 become U+FFFD, as they do everywhere git's output is read as text.
fn unquoted(name: &str) -> String {
    match name
        .strip_prefix('"')
        .and_then(|name| name.strip_suffix('"'))
    {
        Some(inner) => String::from_utf8_lossy(&decode_escapes(inner)).into_owned(),
        None => name.to_owned(),
    }
}

fn decode_escapes(text: &str) -> Vec<u8> {
    let mut bytes = text.bytes();
    let mut out = Vec::with_capacity(text.len());
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        match bytes.next() {
            Some(first @ b'0'..=b'3') => {
                let digit = |digit: Option<u8>| digit.map_or(0, |digit| digit.wrapping_sub(b'0'));
                let (second, third) = (digit(bytes.next()), digit(bytes.next()));
                out.push((first - b'0') << 6 | second << 3 | third);
            }
            Some(code) => out.push(
                NAMED_ESCAPES
                    .iter()
                    .find(|(letter, _)| *letter == code)
                    .map_or(code, |(_, value)| *value),
            ),
            None => out.push(b'\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(names: &str) -> String {
        of_diff(&format!("diff --git {names}\nindex 1..2 100644\n"))
    }

    #[test]
    fn reads_plain_names() {
        assert_eq!(header("a/src/main.rs b/src/main.rs"), "src/main.rs");
        assert_eq!(header("a/dir/b c.rs b/dir/b c.rs"), "dir/b c.rs");
        assert_eq!(header("a/Kjøre.txt b/Kjøre.txt"), "Kjøre.txt");
        assert_eq!(header("a/æ b/æ"), "æ");
    }

    #[test]
    fn isnt_fooled_by_a_name_that_looks_like_two() {
        assert_eq!(header("a/one b/two b/one b/two"), "one b/two");
        assert_eq!(header("a/b/ b/ b/b/ b/"), "b/ b/");
    }

    #[test]
    fn reads_names_with_other_prefixes() {
        assert_eq!(header("c/src/main.rs i/src/main.rs"), "src/main.rs");
        assert_eq!(header("src/main.rs src/main.rs"), "src/main.rs");
        assert_eq!(header("\"tab\\there\" \"tab\\there\""), "tab\\there");
    }

    #[test]
    fn decodes_quoted_names() {
        let quoted = "\"a/Kj\\303\\270re.txt\" \"b/Kj\\303\\270re.txt\"";
        assert_eq!(header(quoted), "Kjøre.txt");
        let backslash = "\"a/back\\\\slash.txt\" \"b/back\\\\slash.txt\"";
        assert_eq!(header(backslash), "back\\slash.txt");
        let quote = "\"a/quo\\\"te.txt\" \"b/quo\\\"te.txt\"";
        assert_eq!(header(quote), "quo\"te.txt");
        let spaces = "\"a/x y b/z\\tq\" \"b/x y b/z\\tq\"";
        assert_eq!(header(spaces), "x y b/z\\tq");
    }

    #[test]
    fn keeps_control_characters_escaped() {
        for (quoted, shown) in [
            ("t\\tb", "t\\tb"),
            ("n\\nUnit 9: x", "n\\nUnit 9: x"),
            ("e\\033[31m", "e\\033[31m"),
            ("d\\177", "d\\177"),
            ("c\\302\\233", "c\\302\\233"),
            ("r\\342\\200\\256", "r\\342\\200\\256"),
            ("l\\342\\200\\250Unit 9", "l\\342\\200\\250Unit 9"),
            ("z\\342\\200\\213", "z\\342\\200\\213"),
            ("m\\342\\200\\216", "m\\342\\200\\216"),
            ("t\\363\\240\\200\\201", "t\\363\\240\\200\\201"),
        ] {
            let names = format!("\"a/{quoted}\" \"b/{quoted}\"");
            assert_eq!(header(&names), shown);
        }
    }

    #[test]
    fn shows_a_name_that_isnt_utf8_on_one_line() {
        assert_eq!(shown("Kj\u{10efe8}re.txt"), "Kj\u{fffd}re.txt");
        assert_eq!(shown("a\nb\u{1b}c"), "a\\nb\\033c");
        assert_eq!(shown("Kjøre.txt"), "Kjøre.txt");
    }

    #[test]
    fn replaces_bytes_that_arent_utf8() {
        assert_eq!(
            header("\"a/Kj\\370re.txt\" \"b/Kj\\370re.txt\""),
            "Kj\u{fffd}re.txt"
        );
    }

    #[test]
    fn gives_the_same_form_for_quoted_names_and_raw_ones() {
        for (raw, quoted) in [
            ("Kjøre.txt", "Kj\\303\\270re.txt"),
            ("tab\there.txt", "tab\\there.txt"),
            ("back\\slash.txt", "back\\\\slash.txt"),
            ("quo\"te.txt", "quo\\\"te.txt"),
            ("ny\nlinje", "ny\\nlinje"),
            ("farge\u{1b}[31m", "farge\\033[31m"),
            ("c\u{9b}", "c\\302\\233"),
        ] {
            assert_eq!(
                one_line(raw),
                header(&format!("\"a/{quoted}\" \"b/{quoted}\""))
            );
        }
    }

    #[test]
    fn finds_the_new_name_of_a_renamed_file() {
        let renamed = "diff --git a/old.txt b/ny æ.txt\nsimilarity index 90%\n\
                       rename from old.txt\nrename to ny æ.txt\n";
        assert_eq!(of_diff(renamed), "ny æ.txt");
        let copied = "diff --git a/old.txt b/new.txt\ncopy from old.txt\ncopy to new.txt\n";
        assert_eq!(of_diff(copied), "new.txt");
        let quoted = "diff --git a/old.txt \"b/ny\\303\\246\\t.txt\"\n\
                      rename from old.txt\nrename to \"ny\\303\\246\\t.txt\"\n";
        assert_eq!(of_diff(quoted), "nyæ\\t.txt");
    }

    #[test]
    fn reads_the_one_name_of_a_combined_diff() {
        assert_eq!(
            of_diff("diff --cc src/a b.rs\nindex 1,2..3\n"),
            "src/a b.rs"
        );
        assert_eq!(of_diff("diff --combined x.txt\n"), "x.txt");
        assert_eq!(of_diff("diff --cc \"ny\\303\\246\\t.txt\"\n"), "nyæ\\t.txt");
    }

    #[test]
    fn reads_a_rename_from_its_first_line_alone() {
        assert_eq!(header("a/old.txt b/new.txt"), "new.txt");
        assert_eq!(header("a/old.txt \"b/n\\ty.txt\""), "n\\ty.txt");
        assert_eq!(header("\"a/o\\tld.txt\" b/new.txt"), "new.txt");
    }

    #[test]
    fn doesnt_read_a_rename_from_the_changed_lines() {
        let diff = "diff --git a/x.txt b/x.txt\n@@ -1 +1 @@\nrename to y.txt\n+rename to z.txt\n";
        assert_eq!(of_diff(diff), "x.txt");
    }

    #[test]
    fn survives_odd_input() {
        assert_eq!(of_diff(""), "");
        assert_eq!(header(""), "");
        assert_eq!(header("single"), "single");
        assert_eq!(header("\"unterminated"), "\"unterminated");
        assert_eq!(unquoted("\"ends with \\\""), "ends with \\");
        assert_eq!(decode_escapes("trailing\\"), b"trailing\\");
        assert_eq!(
            decode_escapes("short\\3"),
            [b"short".as_slice(), &[0o300]].concat()
        );
    }
}
