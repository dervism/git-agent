//! Checks the planning documents: the roadmap, the backlog, iterations and designs.
//!
//! Run it from anywhere in the repository:
//!
//! ```sh
//! rustc --edition 2024 -o tmp/check-planning scripts/check-planning.rs && tmp/check-planning
//! ```
//!
//! It only reads, and it fails if it finds a problem. These documents are private, so a
//! fresh clone has nothing to check.
//!
//! Frontmatter has one fixed form, a strict part of YAML, so this needs no YAML library.
//! Anything outside that form is a problem, including a date that isn't a real date, which
//! YAML parsers can roll over to another day without an error.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{env, fs};

const STATUSES: [&str; 5] = ["OPEN", "IN_PROGRESS", "DONE", "BLOCKED", "PARKED"];
const DESIGN_STATUSES: [&str; 4] = ["DRAFT", "IN_REVIEW", "APPROVED", "SUPERSEDED"];
const LABELS: [&str; 10] = [
    "accessibility",
    "bug",
    "documentation",
    "duplicate",
    "enhancement",
    "wontfix",
    "security",
    "refactor",
    "testing",
    "tooling",
];
/// Plain words that YAML can read as something other than text.
const AMBIGUOUS_WORDS: [&str; 9] = ["true", "false", "yes", "no", "on", "off", "null", "y", "n"];

const ROADMAP_INDEX: &str = "docs/planning/roadmap/ROADMAP.md";
const BACKLOG_INDEX: &str = "docs/planning/backlog/BACKLOG.md";

/// What a field must hold.
#[derive(Clone, Copy)]
enum Kind {
    /// Text of this many digits, such as `"0047"`.
    Number(usize),
    NumberOrNull(usize),
    Text,
    /// Lower-case words joined by hyphens, or null.
    GroupOrNull,
    /// `X.Y.Z`, or null.
    VersionOrNull,
    OneOf(&'static [&'static str]),
    DateOrNull,
    /// Relative paths, from the document's folder, that must exist.
    Paths,
    Labels,
}

/// The fields of one kind of document, in their order.
type Schema = &'static [(&'static str, Kind)];

const BACKLOG_ITEM: Schema = &[
    ("number", Kind::Number(4)),
    ("title", Kind::Text),
    ("iteration", Kind::NumberOrNull(3)),
    ("group", Kind::GroupOrNull),
    ("priority", Kind::Number(3)),
    ("date_started", Kind::DateOrNull),
    ("date_finished", Kind::DateOrNull),
    ("status", Kind::OneOf(&STATUSES)),
    ("target_release_version", Kind::VersionOrNull),
    ("roadmap_items", Kind::Paths),
    ("design_documents", Kind::Paths),
    ("related_items", Kind::Paths),
    ("labels", Kind::Labels),
];
const ROADMAP_ITEM: Schema = &[
    ("number", Kind::Number(4)),
    ("title", Kind::Text),
    ("category", Kind::Text),
    ("status", Kind::OneOf(&STATUSES)),
    ("roadmap_items", Kind::Paths),
];
const ITERATION: Schema = &[
    ("iteration", Kind::Number(3)),
    ("date_started", Kind::DateOrNull),
    ("date_finished", Kind::DateOrNull),
    ("target_release_version", Kind::VersionOrNull),
    ("roadmap_items", Kind::Paths),
    ("design_documents", Kind::Paths),
    ("related_items", Kind::Paths),
];
const DESIGN: Schema = &[
    ("title", Kind::Text),
    ("iteration", Kind::Number(3)),
    ("backlog_items", Kind::Paths),
    ("status", Kind::OneOf(&DESIGN_STATUSES)),
    ("date", Kind::DateOrNull),
    ("related_documents", Kind::Paths),
];

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Null,
    Text(String),
    Date(String),
    List(Vec<String>),
}

type Fields = BTreeMap<String, Value>;

fn split_frontmatter(text: &str) -> Result<(&str, &str), String> {
    let rest = text.strip_prefix("---\n").ok_or("it has no frontmatter")?;
    if let Some(body) = rest.strip_prefix("---\n") {
        return Ok(("", body));
    }
    Ok(rest
        .split_once("\n---\n")
        .ok_or("its frontmatter never ends")?)
}

fn parse_fields(front: &str) -> Result<Fields, String> {
    let mut fields = Fields::new();
    let mut opened = Vec::new();
    let mut last_key: Option<String> = None;
    for line in front
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
    {
        if let Some(text) = line.strip_prefix("  - ") {
            let list = last_key.as_ref().and_then(|key| fields.get_mut(key));
            let Some(Value::List(items)) = list else {
                return Err(format!("the list item {line:?} belongs to no list"));
            };
            items.push(list_item(text)?);
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .filter(|(key, _)| is_key(key))
            .ok_or_else(|| format!("can't read the line {line:?}"))?;
        let value = value.trim_start();
        if value.is_empty() || value.starts_with('#') {
            opened.push(key.to_owned());
        }
        let value = field_value(value).map_err(|error| format!("{key}: {error}"))?;
        if fields.insert(key.to_owned(), value).is_some() {
            return Err(format!("{key} is there twice"));
        }
        last_key = Some(key.to_owned());
    }
    match opened
        .iter()
        .find(|key| fields[*key] == Value::List(Vec::new()))
    {
        Some(key) => Err(format!("{key} has no value; write null or []")),
        None => Ok(fields),
    }
}

fn is_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|character| character.is_ascii_lowercase() || character == '_')
}

/// A field's value. Nothing after the colon means a list follows.
fn field_value(text: &str) -> Result<Value, String> {
    if text.starts_with('"') {
        return quoted(text).map(Value::Text);
    }
    match without_comment(text) {
        "" | "[]" => Ok(Value::List(Vec::new())),
        "null" => Ok(Value::Null),
        bare => date(bare).unwrap_or_else(|| plain(bare).map(Value::Text)),
    }
}

fn list_item(text: &str) -> Result<String, String> {
    if text.starts_with('"') {
        quoted(text)
    } else {
        plain(without_comment(text))
    }
}

fn without_comment(text: &str) -> &str {
    let bare = if text.starts_with('#') { "" } else { text };
    bare.split_once(" #")
        .map_or(bare, |(value, _)| value)
        .trim_end()
}

/// A double-quoted string, in which only `\"` and `\\` are escapes.
fn quoted(text: &str) -> Result<String, String> {
    let inner = text.strip_prefix('"').ok_or("no opening quote")?;
    let mut out = String::new();
    let mut characters = inner.char_indices();
    while let Some((at, character)) = characters.next() {
        match character {
            '"' => {
                let rest = inner[at + 1..].trim_start();
                return if rest.is_empty() || rest.starts_with('#') {
                    Ok(out)
                } else {
                    Err(format!("{text} has text after its closing quote"))
                };
            }
            '\\' => match characters.next() {
                Some((_, escaped @ ('"' | '\\'))) => out.push(escaped),
                _ => return Err(format!("{text} has an escape this form doesn't allow")),
            },
            other => out.push(other),
        }
    }
    Err(format!("{text} has no closing quote"))
}

/// Text without quotes: only what no YAML parser can read as anything else.
fn plain(text: &str) -> Result<String, String> {
    let safe = text
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_./".contains(character));
    let lower = text.to_lowercase();
    let ambiguous = AMBIGUOUS_WORDS.contains(&lower.as_str())
        || text == "-"
        || [".inf", "-.inf", "+.inf", ".nan"].contains(&lower.as_str())
        || text.parse::<f64>().is_ok()
        || lower.starts_with("0x")
        || lower.starts_with("0o");
    if safe && !ambiguous && !text.is_empty() {
        Ok(text.to_owned())
    } else {
        Err(format!("{text:?} needs double quotes"))
    }
}

/// `text` as a date if it looks like one: `None` if it doesn't, and an error if it
/// looks like one but isn't a real day.
fn date(text: &str) -> Option<Result<Value, String>> {
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 10
        && bytes.iter().enumerate().all(|(at, byte)| match at {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    if !shaped {
        return None;
    }
    let (year, month, day): (u32, u32, u32) = (
        text[..4].parse().ok()?,
        text[5..7].parse().ok()?,
        text[8..].parse().ok()?,
    );
    Some(
        if (1..=12).contains(&month) && (1..=days_in(year, month)).contains(&day) {
            Ok(Value::Date(text.to_owned()))
        } else {
            Err(format!("{text} is not a real date"))
        },
    )
}

fn days_in(year: u32, month: u32) -> u32 {
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// A planning document: where it is, its frontmatter and its first heading.
#[derive(Debug)]
struct Document {
    path: PathBuf,
    fields: Fields,
    heading: String,
}

impl Document {
    fn parse(path: PathBuf, text: &str) -> Result<Self, String> {
        let (front, body) = split_frontmatter(text)?;
        let heading = body
            .lines()
            .find(|line| !line.is_empty())
            .and_then(|line| line.strip_prefix("# "))
            .ok_or("its body doesn't start with a # heading")?
            .to_owned();
        Ok(Self {
            path,
            fields: parse_fields(front)?,
            heading,
        })
    }

    fn text(&self, key: &str) -> &str {
        match self.fields.get(key) {
            Some(Value::Text(text)) => text,
            _ => "",
        }
    }

    fn list(&self, key: &str) -> &[String] {
        match self.fields.get(key) {
            Some(Value::List(items)) => items,
            _ => &[],
        }
    }

    fn file_name(&self) -> &str {
        name_of(&self.path)
    }

    fn folder_name(&self) -> &str {
        self.path.parent().map_or("", name_of)
    }

    /// The heading's title, as Markdown, after its number.
    fn heading_title(&self) -> &str {
        self.heading
            .split_once(" - ")
            .map_or(self.heading.as_str(), |(_, title)| title)
    }

    /// The link to this item in an index next to its folder.
    fn index_link(&self) -> String {
        format!(
            "[{}]({}/{})",
            self.heading_title(),
            self.folder_name(),
            self.file_name()
        )
    }
}

fn name_of(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
}

fn digits(text: &str, count: usize) -> bool {
    text.len() == count && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_group(text: &str) -> bool {
    text.split('-').all(|word| {
        !word.is_empty()
            && word
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

fn is_version(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Whether `value` is what `kind` asks for. Paths and labels are checked apart.
fn fits(value: &Value, kind: Kind) -> bool {
    match (kind, value) {
        (Kind::NumberOrNull(_) | Kind::GroupOrNull | Kind::VersionOrNull, Value::Null) => true,
        (Kind::Number(count) | Kind::NumberOrNull(count), Value::Text(text)) => digits(text, count),
        (Kind::Text, Value::Text(text)) => !text.is_empty(),
        (Kind::GroupOrNull, Value::Text(text)) => is_group(text),
        (Kind::VersionOrNull, Value::Text(text)) => is_version(text),
        (Kind::OneOf(allowed), Value::Text(text)) => allowed.contains(&text.as_str()),
        (Kind::DateOrNull, Value::Date(_) | Value::Null) => true,
        (Kind::Paths | Kind::Labels, Value::List(_)) => true,
        _ => false,
    }
}

/// The problems of `document`'s fields against `schema`, paths included.
fn schema_problems(root: &Path, document: &Document, schema: Schema) -> Vec<String> {
    let known: BTreeSet<&str> = schema.iter().map(|(key, _)| *key).collect();
    let unknown = document
        .fields
        .keys()
        .filter(|key| !known.contains(key.as_str()))
        .map(|key| format!("{key} isn't a field here"));
    let wrong = schema
        .iter()
        .flat_map(|&(key, kind)| match document.fields.get(key) {
            None => vec![format!("{key} is missing")],
            Some(value) if !fits(value, kind) => vec![format!("{key} can't be {value:?}")],
            Some(_) => list_problems(root, document, key, kind),
        });
    unknown.chain(wrong).collect()
}

fn list_problems(root: &Path, document: &Document, key: &str, kind: Kind) -> Vec<String> {
    let folder = root.join(document.path.parent().unwrap_or(Path::new("")));
    let items = document.list(key);
    match kind {
        Kind::Paths => items
            .iter()
            .filter(|path| !folder.join(path).exists())
            .map(|path| format!("{key} points to {path}, which doesn't exist"))
            .collect(),
        Kind::Labels => items
            .iter()
            .filter(|label| !LABELS.contains(&label.as_str()))
            .map(|label| format!("{label} isn't one of the labels"))
            .collect(),
        _ => Vec::new(),
    }
}

/// What a backlog or roadmap item's number, heading, status and folder must agree on.
fn numbered_item_problems(document: &Document) -> Vec<String> {
    let number = document.text("number");
    let title = document.text("title");
    let status = document.text("status");
    let folder = if status == "DONE" {
        "finished"
    } else {
        "items"
    };
    let mut problems = Vec::new();
    if !document.file_name().starts_with(&format!("{number}-")) {
        problems.push(format!("the file name doesn't start with {number}"));
    }
    if document.heading.replace('`', "") != format!("{number} - {title}") {
        problems.push(format!(
            "the heading {:?} doesn't match number {number} and title {title:?}",
            document.heading
        ));
    }
    if document.folder_name() != folder {
        problems.push(format!("status {status} belongs in {folder}/"));
    }
    problems
}

fn backlog_item_problems(root: &Path, document: &Document) -> Vec<String> {
    let mut problems = schema_problems(root, document, BACKLOG_ITEM);
    problems.extend(numbered_item_problems(document));
    if document.text("status") == "DONE"
        && document.fields.get("date_finished") == Some(&Value::Null)
    {
        problems.push("it's DONE, so it needs date_finished".to_owned());
    }
    problems
}

fn roadmap_item_problems(root: &Path, document: &Document) -> Vec<String> {
    let mut problems = schema_problems(root, document, ROADMAP_ITEM);
    problems.extend(numbered_item_problems(document));
    problems
}

fn iteration_problems(root: &Path, document: &Document) -> Vec<String> {
    let mut problems = schema_problems(root, document, ITERATION);
    let iteration = document.text("iteration");
    if document.heading != format!("Iteration {iteration}")
        || document.file_name() != format!("iteration-{iteration}.md")
    {
        problems.push(format!(
            "its heading and file name should say iteration {iteration}"
        ));
    }
    problems
}

fn design_problems(root: &Path, document: &Document) -> Vec<String> {
    let mut problems = schema_problems(root, document, DESIGN);
    let iteration = document.text("iteration");
    if document.folder_name() != format!("iteration-{iteration}") {
        problems.push(format!("the folder doesn't match iteration {iteration}"));
    }
    if document.heading != document.text("title") {
        problems.push(format!(
            "the heading {:?} doesn't match the title",
            document.heading
        ));
    }
    problems
}

/// The rows of the first Markdown table in `text`, without its header.
fn table_rows(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .skip(2)
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_owned())
                .collect()
        })
        .collect()
}

/// Compares an index with the rows its items should have, by the number in `number_column`.
fn index_problems(
    rows: &[Vec<String>],
    expected: &[Vec<String>],
    number_column: usize,
) -> Vec<String> {
    let number = |row: &Vec<String>| row.get(number_column).cloned().unwrap_or_default();
    let mut by_number: BTreeMap<String, Vec<&Vec<String>>> = BTreeMap::new();
    for row in rows {
        by_number.entry(number(row)).or_default().push(row);
    }
    let known: BTreeSet<String> = expected.iter().map(number).collect();
    let wrong = expected.iter().filter_map(|wanted| {
        match by_number.get(&number(wanted)).map(Vec::as_slice) {
            None => Some(format!("it has no row for item {}", number(wanted))),
            Some([row]) if *row != wanted => Some(format!("the row {row:?} should be {wanted:?}")),
            Some([_]) => None,
            Some(_) => Some(format!(
                "it has more than one row for item {}",
                number(wanted)
            )),
        }
    });
    let unknown = rows
        .iter()
        .filter(|row| !known.contains(&number(row)))
        .map(|row| format!("the row {row:?} has no item"));
    wrong.chain(unknown).collect()
}

fn backlog_row(document: &Document) -> Vec<String> {
    let labels = document.list("labels").join(", ");
    let or_dash = |text: &str| {
        if text.is_empty() {
            "-".to_owned()
        } else {
            text.to_owned()
        }
    };
    vec![
        document.text("priority").to_owned(),
        document.text("number").to_owned(),
        document.index_link(),
        or_dash(document.text("group")),
        or_dash(&labels),
        document.text("status").to_owned(),
    ]
}

fn roadmap_row(document: &Document) -> Vec<String> {
    vec![
        document.text("number").to_owned(),
        document.index_link(),
        document.text("category").to_owned(),
        document.text("status").to_owned(),
    ]
}

/// Values that more than one document has under `key`.
fn duplicates(documents: &[Document], key: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    documents
        .iter()
        .map(|document| document.text(key))
        .filter(|value| !seen.insert(*value))
        .map(|value| format!("{key} {value} is used twice"))
        .collect()
}

/// The entries of `folder` that `keep` says yes to, sorted.
fn entries(folder: &Path, keep: fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| keep(path))
        .collect();
    found.sort();
    found
}

fn is_markdown(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "md")
}

/// The top of the repository that `start` is in, or `start` if it's in none.
fn repository_top(start: &Path) -> PathBuf {
    start
        .ancestors()
        .find(|folder| folder.join(".git").exists())
        .unwrap_or(start)
        .to_owned()
}

/// All the planning documents under `root`, and the problems of those it couldn't read.
#[derive(Default)]
struct Planning {
    backlog: Vec<Document>,
    roadmap: Vec<Document>,
    iterations: Vec<Document>,
    designs: Vec<Document>,
    unreadable: Vec<String>,
}

impl Planning {
    fn load(root: &Path) -> Self {
        let items_in = |base: &str| -> Vec<PathBuf> {
            ["items", "finished"]
                .iter()
                .flat_map(|folder| entries(&root.join(base).join(folder), is_markdown))
                .collect()
        };
        let in_subfolders = |base: &str| -> Vec<PathBuf> {
            entries(&root.join(base), Path::is_dir)
                .iter()
                .flat_map(|folder| entries(folder, is_markdown))
                .collect()
        };
        let mut planning = Self::default();
        planning.backlog = planning.read(root, items_in("docs/planning/backlog"));
        planning.roadmap = planning.read(root, items_in("docs/planning/roadmap"));
        planning.iterations = planning.read(root, in_subfolders("docs/iterations"));
        planning.designs = planning.read(root, in_subfolders("docs/architecture/design"));
        planning
    }

    fn read(&mut self, root: &Path, files: Vec<PathBuf>) -> Vec<Document> {
        files
            .into_iter()
            .filter_map(|file| {
                let path = file.strip_prefix(root).unwrap_or(&file).to_owned();
                let parsed = fs::read_to_string(&file)
                    .map_err(|error| error.to_string())
                    .and_then(|text| Document::parse(path.clone(), &text));
                parsed
                    .map_err(|error| self.unreadable.push(format!("{}: {error}", path.display())))
                    .ok()
            })
            .collect()
    }

    fn problems(&self, root: &Path) -> Vec<String> {
        let each = |documents: &[Document], check: fn(&Path, &Document) -> Vec<String>| {
            documents
                .iter()
                .flat_map(|document| {
                    check(root, document)
                        .into_iter()
                        .map(|problem| format!("{}: {problem}", document.path.display()))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let mut problems = self.unreadable.clone();
        problems.extend(each(&self.backlog, backlog_item_problems));
        problems.extend(each(&self.roadmap, roadmap_item_problems));
        problems.extend(each(&self.iterations, iteration_problems));
        problems.extend(each(&self.designs, design_problems));
        problems.extend(duplicates(&self.backlog, "number"));
        problems.extend(duplicates(&self.backlog, "priority"));
        problems.extend(duplicates(&self.roadmap, "number"));
        problems.extend(index(root, BACKLOG_INDEX, &self.backlog, backlog_row, 1));
        problems.extend(index(root, ROADMAP_INDEX, &self.roadmap, roadmap_row, 0));
        problems
    }

    fn summary(&self) -> String {
        format!(
            "{} backlog items, {} roadmap items, {} iterations and {} designs",
            self.backlog.len(),
            self.roadmap.len(),
            self.iterations.len(),
            self.designs.len()
        )
    }
}

fn index(
    root: &Path,
    path: &str,
    items: &[Document],
    row: fn(&Document) -> Vec<String>,
    number_column: usize,
) -> Vec<String> {
    let Ok(text) = fs::read_to_string(root.join(path)) else {
        return vec![format!("{path}: can't read it")];
    };
    let expected: Vec<Vec<String>> = items.iter().map(row).collect();
    index_problems(&table_rows(&text), &expected, number_column)
        .into_iter()
        .map(|problem| format!("{path}: {problem}"))
        .collect()
}

fn main() -> ExitCode {
    let root = repository_top(&env::current_dir().unwrap_or_default());
    if !root.join("docs/planning").is_dir() {
        println!("Nothing to check: this clone doesn't have the planning documents.");
        return ExitCode::SUCCESS;
    }
    let planning = Planning::load(&root);
    let problems = planning.problems(&root);
    if problems.is_empty() {
        println!("Everything is in order: {}.", planning.summary());
        return ExitCode::SUCCESS;
    }
    problems.iter().for_each(|problem| println!("{problem}"));
    println!("{} problem(s) in {}.", problems.len(), planning.summary());
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn text(value: &str) -> Value {
        Value::Text(value.to_owned())
    }

    fn fields(front: &str) -> Fields {
        parse_fields(front).unwrap()
    }

    #[test]
    fn reads_every_kind_of_value() {
        let read = fields(
            "number: \"0008\"\ntitle: \"a \\\"b\\\" \\\\ c: d\"\ngroup: null\nstatus: OPEN\n\
             date_started: 2026-11-05\nlabels: []\nrelated_items:\n  - ../a/0001-x.md\n  - \"b c.md\"",
        );
        assert_eq!(read["number"], text("0008"));
        assert_eq!(read["title"], text("a \"b\" \\ c: d"));
        assert_eq!(read["group"], Value::Null);
        assert_eq!(read["status"], text("OPEN"));
        assert_eq!(read["date_started"], Value::Date("2026-11-05".to_owned()));
        assert_eq!(read["labels"], Value::List(Vec::new()));
        assert_eq!(
            read["related_items"],
            Value::List(vec!["../a/0001-x.md".to_owned(), "b c.md".to_owned()])
        );
        assert_eq!(fields("title: \"Kjøre å\"")["title"], text("Kjøre å"));
        assert_eq!(fields("d: 05-11-2026")["d"], text("05-11-2026"));
    }

    #[test]
    fn ignores_comments() {
        let read = fields(
            "# a comment\nstatus: OPEN   # one of these\ntitle: \"x # y\"  # after\n\
             group: null # none\nlabels: [] # empty\nrelated_items: # paths\n  - a.md # first",
        );
        assert_eq!(read["status"], text("OPEN"));
        assert_eq!(read["title"], text("x # y"));
        assert_eq!(read["group"], Value::Null);
        assert_eq!(read["labels"], Value::List(Vec::new()));
        assert_eq!(read["related_items"], Value::List(vec!["a.md".to_owned()]));
    }

    #[test]
    fn refuses_what_is_outside_the_form() {
        for (front, part) in [
            ("title: a: b", "double quotes"),
            ("title: two words", "double quotes"),
            ("yes_no: yes", "double quotes"),
            ("up: True", "double quotes"),
            ("number: 011", "double quotes"),
            ("version: 0.11", "double quotes"),
            ("big: 1e3", "double quotes"),
            ("hex: 0x1F", "double quotes"),
            ("float: .inf", "double quotes"),
            ("group: -", "double quotes"),
            ("title: \"open", "no closing quote"),
            ("title: \"a\" b", "after its closing quote"),
            ("title: \"a\\nb\"", "escape"),
            ("labels:", "write null or []"),
            ("labels:\ntitle: x", "write null or []"),
            ("  - a.md", "belongs to no list"),
            ("status: OPEN\n  - a.md", "belongs to no list"),
            ("a: b\na: c", "twice"),
            ("no colon here", "can't read"),
            ("Key: x", "can't read"),
            ("\tkey: x", "can't read"),
        ] {
            let error = parse_fields(front).unwrap_err();
            assert!(error.contains(part), "{front:?} gave {error:?}");
        }
    }

    #[test]
    fn knows_a_real_date_from_a_rolled_over_one() {
        for good in ["2026-11-05", "2024-02-29", "2000-02-29", "2026-12-31"] {
            assert_eq!(
                fields(&format!("d: {good}"))["d"],
                Value::Date(good.to_owned())
            );
        }
        for bad in [
            "2026-13-05",
            "2026-00-05",
            "2026-02-30",
            "2026-02-29",
            "1900-02-29",
            "2026-11-31",
            "2026-11-00",
        ] {
            let error = parse_fields(&format!("d: {bad}")).unwrap_err();
            assert!(error.contains("not a real date"), "{bad} gave {error:?}");
        }
    }

    #[test]
    fn splits_frontmatter_from_the_body() {
        assert_eq!(
            split_frontmatter("---\na: b\n---\n\n# Title\n").unwrap(),
            ("a: b", "\n# Title\n")
        );
        assert_eq!(split_frontmatter("---\n---\nbody").unwrap(), ("", "body"));
        assert!(split_frontmatter("# Title\n").is_err());
        assert!(split_frontmatter("---\na: b\n").is_err());
    }

    #[test]
    fn reads_only_the_first_table() {
        let rows = table_rows(
            "# T\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\nText\n\n| c |\n| --- |\n| 3 |\n",
        );
        assert_eq!(rows, [["1", "2"]]);
    }

    /// A folder of planning documents that are all in order, removed when dropped.
    struct Tree(PathBuf);

    static NEXT_TREE: AtomicUsize = AtomicUsize::new(0);

    const BACKLOG: &str = "# Backlog\n\n## Items\n\n\
        | Item priority | Item number | Item title | Item group | Item labels | Item status |\n\
        | --- | --- | --- | --- | --- | --- |\n\
        | 001 | 0001 | [Done item](finished/0001-done-item.md) | - | bug, security | DONE |\n\
        | 002 | 0002 | [Open item](items/0002-open-item.md) | - | bug, security | OPEN |\n";

    const ROADMAP: &str = "# Roadmap\n\nIntro\n\n## Items\n\n\
        | Item number | Item title | Item category | Item status |\n\
        | --- | --- | --- | --- |\n\
        | 0001 | [`git agent idea`](items/0001-idea.md) | Ideas | OPEN |\n";

    const OPEN_ITEM: &str = "docs/planning/backlog/items/0002-open-item.md";
    const ITERATION_DOC: &str = "docs/iterations/001/iteration-001.md";
    const DESIGN_DOC: &str = "docs/architecture/design/iteration-001/a-design.md";

    fn backlog_item(
        number: &str,
        priority: &str,
        status: &str,
        finished: &str,
        title: &str,
    ) -> String {
        format!(
            "---\nnumber: \"{number}\"\ntitle: \"{title}\"\niteration: null\ngroup: null\n\
             priority: \"{priority}\"\ndate_started: 2026-11-05\ndate_finished: {finished}\n\
             status: {status}\ntarget_release_version: null\nroadmap_items:\n  \
             - ../../roadmap/items/0001-idea.md\ndesign_documents: []\nrelated_items: []\n\
             labels:\n  - bug\n  - security\n---\n\n# {number} - {title}\n\n## Description\n"
        )
    }

    impl Tree {
        fn new() -> Self {
            let name = format!(
                "check-planning-{}-{}",
                std::process::id(),
                NEXT_TREE.fetch_add(1, Ordering::Relaxed)
            );
            let tree = Self(env::temp_dir().join(name));
            tree.write(".git/HEAD", "ref: refs/heads/main\n");
            tree.write(BACKLOG_INDEX, BACKLOG);
            tree.write(ROADMAP_INDEX, ROADMAP);
            tree.write(
                OPEN_ITEM,
                &backlog_item("0002", "002", "OPEN", "null", "Open item"),
            );
            tree.write(
                "docs/planning/backlog/finished/0001-done-item.md",
                &backlog_item("0001", "001", "DONE", "2026-11-05", "Done item"),
            );
            tree.write(
                "docs/planning/roadmap/items/0001-idea.md",
                "---\nnumber: \"0001\"\ntitle: \"git agent idea\"\ncategory: \"Ideas\"\n\
                 status: OPEN\nroadmap_items: []\n---\n\n# 0001 - `git agent idea`\n",
            );
            tree.write(
                ITERATION_DOC,
                "---\niteration: \"001\"\ndate_started: 2026-11-05\ndate_finished: null\n\
                 target_release_version: \"0.1.0\"\nroadmap_items:\n  \
                 - ../../planning/roadmap/items/0001-idea.md\ndesign_documents: []\n\
                 related_items: []\n---\n\n# Iteration 001\n",
            );
            tree.write(
                DESIGN_DOC,
                "---\ntitle: \"A design\"\niteration: \"001\"\nbacklog_items:\n  \
                 - ../../../planning/backlog/items/0002-open-item.md\nstatus: APPROVED\n\
                 date: 2026-11-05\nrelated_documents: []\n---\n\n# A design\n",
            );
            tree
        }

        fn write(&self, path: &str, text: &str) {
            let file = self.0.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, text).unwrap();
        }

        fn replace(&self, path: &str, from: &str, to: &str) {
            let file = self.0.join(path);
            let text = fs::read_to_string(&file).unwrap();
            assert!(text.contains(from), "{from:?} isn't in {path}");
            fs::write(file, text.replacen(from, to, 1)).unwrap();
        }

        fn problems(&self) -> Vec<String> {
            Planning::load(&self.0).problems(&self.0)
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn finds_nothing_wrong_with_documents_in_order() {
        let tree = Tree::new();
        assert_eq!(tree.problems(), Vec::<String>::new());
        assert_eq!(
            Planning::load(&tree.0).summary(),
            "2 backlog items, 1 roadmap items, 1 iterations and 1 designs"
        );
    }

    #[test]
    fn finds_each_kind_of_problem() {
        let done_row =
            "| 002 | 0002 | [Open item](items/0002-open-item.md) | - | bug, security | OPEN |\n";
        for (path, from, to, problem) in [
            (
                OPEN_ITEM,
                "../../roadmap/items/0001-idea.md",
                "../../gone.md",
                "roadmap_items points to ../../gone.md",
            ),
            (
                OPEN_ITEM,
                "- security",
                "- nonsense",
                "nonsense isn't one of the labels",
            ),
            (OPEN_ITEM, "group: null", "team: null", "group is missing"),
            (
                OPEN_ITEM,
                "group: null",
                "team: null",
                "team isn't a field here",
            ),
            (
                OPEN_ITEM,
                "title: \"Open item\"",
                "title: \"Other\"",
                "doesn't match number 0002 and title",
            ),
            (
                OPEN_ITEM,
                "status: OPEN",
                "status: DONE",
                "status DONE belongs in finished/",
            ),
            (
                OPEN_ITEM,
                "status: OPEN",
                "status: DONE",
                "it's DONE, so it needs date_finished",
            ),
            (
                OPEN_ITEM,
                "status: OPEN",
                "status: WAITING",
                "status can't be",
            ),
            (
                OPEN_ITEM,
                "group: null",
                "group: Some_Group",
                "group can't be",
            ),
            (OPEN_ITEM, "group: null", "group: []", "group can't be"),
            (
                OPEN_ITEM,
                "iteration: null",
                "iteration: []",
                "iteration can't be",
            ),
            (
                OPEN_ITEM,
                "iteration: null",
                "iteration: \"2\"",
                "iteration can't be",
            ),
            (
                OPEN_ITEM,
                "target_release_version: null",
                "target_release_version: []",
                "target_release_version can't be",
            ),
            (
                OPEN_ITEM,
                "target_release_version: null",
                "target_release_version: \"1.2\"",
                "target_release_version can't be",
            ),
            (
                OPEN_ITEM,
                "priority: \"002\"",
                "priority: \"2\"",
                "priority can't be",
            ),
            (
                OPEN_ITEM,
                "date_started: 2026-11-05",
                "date_started: \"2026-11-05\"",
                "date_started can't be",
            ),
            (
                OPEN_ITEM,
                "related_items: []",
                "related_items: null",
                "related_items can't be",
            ),
            (BACKLOG_INDEX, "| OPEN |", "| DONE |", "the row"),
            (BACKLOG_INDEX, done_row, "", "no row for item 0002"),
            (
                BACKLOG_INDEX,
                done_row,
                &format!("{done_row}{done_row}"),
                "more than one row for item 0002",
            ),
            (ROADMAP_INDEX, "| 0001 |", "| 0009 |", "has no item"),
            (
                ITERATION_DOC,
                "# Iteration 001",
                "# Iteration 002",
                "should say iteration 001",
            ),
            (
                DESIGN_DOC,
                "status: APPROVED",
                "status: FINE",
                "status can't be",
            ),
            (DESIGN_DOC, "date: 2026-11-05", "date: []", "date can't be"),
            (
                DESIGN_DOC,
                "# A design",
                "# Another",
                "doesn't match the title",
            ),
        ] {
            let tree = Tree::new();
            tree.replace(path, from, to);
            let problems = tree.problems();
            assert!(
                problems.iter().any(|found| found.contains(problem)),
                "{problem:?} isn't among {problems:#?}"
            );
        }
    }

    #[test]
    fn finds_a_number_or_a_priority_used_twice() {
        let tree = Tree::new();
        tree.write(
            "docs/planning/backlog/items/0002-copy.md",
            &backlog_item("0002", "002", "OPEN", "null", "Copy"),
        );
        let problems = tree.problems();
        for problem in ["number 0002 is used twice", "priority 002 is used twice"] {
            assert!(
                problems.iter().any(|found| found.contains(problem)),
                "{problems:#?}"
            );
        }
    }

    #[test]
    fn allows_a_design_without_a_date_yet() {
        let tree = Tree::new();
        tree.replace(DESIGN_DOC, "date: 2026-11-05", "date: null");
        tree.replace(DESIGN_DOC, "status: APPROVED", "status: DRAFT");
        assert_eq!(tree.problems(), Vec::<String>::new());
    }

    #[test]
    fn reports_a_file_it_cant_read_and_goes_on() {
        let tree = Tree::new();
        tree.write(
            "docs/planning/backlog/items/0003-table.md",
            "# 0003 - Table\n",
        );
        tree.write(
            "docs/planning/backlog/items/0004-date.md",
            "---\nd: 2026-13-05\n---\n\n# x\n",
        );
        let problems = tree.problems();
        assert_eq!(problems.len(), 2, "{problems:#?}");
        assert!(problems[0].ends_with("0003-table.md: it has no frontmatter"));
        assert!(problems[1].ends_with("0004-date.md: d: 2026-13-05 is not a real date"));
    }

    #[test]
    fn finds_the_top_of_the_repository_from_a_subfolder() {
        let tree = Tree::new();
        assert_eq!(repository_top(&tree.0.join("docs/planning")), tree.0);
    }
}
