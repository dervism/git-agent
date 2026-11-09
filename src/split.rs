//! Splitting one large change into several commits (`git agent split`).
//!
//! The change is cut into units: one hunk of a modified text file, or a whole
//! file when it can't be committed in parts (new, deleted, renamed or binary
//! files, mode changes, and excluded files such as lockfiles). The model groups
//! the units into commits. Each commit is then built in a temporary index from
//! HEAD and the hunks of its units, so the real index keeps the changes that
//! are still to be committed.

pub mod check;
pub mod command;
pub mod dry_run;
pub mod flow;
pub mod kinds;
pub mod signals;

use crate::conversation::fit;
use crate::git::{diff, lossless, names};
use crate::safe_text::{escaped, without_hidden};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::ops::Range;

/// Room for the note that replaces the end of a shortened unit.
const NOTE_BYTES: usize = 40;
/// Longest part of a hunk's section (usually the enclosing function) to show.
const SECTION_CHARS: usize = 60;

/// Help at the top of the plan when the user edits it.
const EDIT_HELP: &str = "\
# Edit the plan. Each \"commit:\" line starts a commit, and the numbered lines
# under it are the changes it includes. Move a line to move that change to
# another commit, and move a whole block to change the order of the commits.
# Lines starting with '#' are ignored. A plan without commits keeps the previous one.
";

/// One file's part of a patch.
#[derive(Debug, Clone)]
pub struct FilePatch {
    pub path: String,
    /// Everything before the first hunk: the "diff --git" line, mode and
    /// rename lines, and the "---" and "+++" lines.
    header: String,
    hunks: Vec<Hunk>,
    /// What happens to the file, such as "new file"; `None` for a plain change.
    status: Option<&'static str>,
    /// Whether the model is shown only the file's name (see `exclude`).
    excluded: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct Hunk {
    old_start: usize,
    old_len: usize,
    new_len: usize,
    /// The hunk's "@@" line as git wrote it.
    header: String,
    /// What follows the second "@@", usually the enclosing function.
    section: String,
    /// The lines after the "@@" line.
    body: String,
}

impl FilePatch {
    /// Whether the file's hunks can go into different commits.
    fn splittable(&self) -> bool {
        self.status.is_none() && !self.excluded && self.hunks.len() > 1
    }
}

/// A part of the change that goes into one commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unit {
    pub file: usize,
    /// A hunk of the file, or `None` for all of it.
    hunk: Option<usize>,
}

/// Reads a patch from [`Repo::patch`](crate::git::Repo::patch). Files not in
/// `included` are shown to the model by name only.
pub fn parse(patch: &str, included: &HashSet<String>) -> Vec<FilePatch> {
    diff::split_files(patch)
        .into_iter()
        .map(|text| {
            let path = names::of_diff(text);
            let hunks_start = text.find("\n@@").map_or(text.len(), |newline| newline + 1);
            let (header, hunks) = text.split_at(hunks_start);
            FilePatch {
                excluded: !included.contains(&path),
                path,
                header: header.to_owned(),
                hunks: parse_hunks(hunks),
                status: status(header),
            }
        })
        .collect()
}

fn status(header: &str) -> Option<&'static str> {
    const KINDS: [(&str, &str); 7] = [
        ("new file mode", "new file"),
        ("deleted file mode", "deleted"),
        ("rename from", "renamed"),
        ("copy from", "copied"),
        ("old mode", "mode changed"),
        ("GIT binary patch", "binary"),
        ("Binary files", "binary"),
    ];
    // By the order of the list, because a renamed file can also change its mode.
    KINDS
        .iter()
        .find(|(start, _)| header.lines().any(|line| line.starts_with(start)))
        .map(|(_, kind)| *kind)
}

fn parse_hunks(text: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    for line in text.split_inclusive('\n') {
        if let Some(hunk) = line
            .starts_with("@@")
            .then(|| parse_hunk_header(line))
            .flatten()
        {
            hunks.push(hunk);
        } else if let Some(hunk) = hunks.last_mut() {
            hunk.body.push_str(line);
        }
    }
    hunks
}

/// A hunk without its body, from a "@@ -a,b +c,d @@ section" line.
fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let (ranges, section) = line.strip_prefix("@@ -")?.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |range: &str| -> Option<(usize, usize)> {
        match range.split_once(',') {
            Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
            None => Some((range.parse().ok()?, 1)),
        }
    };
    let ((old_start, old_len), (_, new_len)) = (range(old)?, range(new)?);
    Some(Hunk {
        old_start,
        old_len,
        new_len,
        header: line.to_owned(),
        section: section.trim_end_matches(['\n', '\r']).to_owned(),
        body: String::new(),
    })
}

/// The units in file order: hunk by hunk where a file can be split, or whole files.
fn units(files: &[FilePatch], by_hunk: bool) -> Vec<Unit> {
    let mut units = Vec::new();
    for (file, patch) in files.iter().enumerate() {
        if by_hunk && patch.splittable() {
            units.extend((0..patch.hunks.len()).map(|hunk| Unit {
                file,
                hunk: Some(hunk),
            }));
        } else {
            units.push(Unit { file, hunk: None });
        }
    }
    units
}

/// The units and how the model sees them.
pub struct Described {
    pub units: Vec<Unit>,
    /// The units numbered from 1, each with its changed lines, perhaps cut short.
    pub text: String,
    /// Whether whole files stand in for their hunks, because there were too
    /// many hunks to show.
    pub by_file: bool,
}

/// Describes the change for the model in about `room` bytes: hunk by hunk if
/// their names take at most half the room, else file by file.
pub fn describe(files: &[FilePatch], room: usize) -> Result<Described> {
    for by_hunk in [true, false] {
        let units = units(files, by_hunk);
        let names: Vec<String> = units
            .iter()
            .enumerate()
            .map(|(index, &unit)| format!("Unit {}: {}\n", index + 1, name(files, unit)))
            .collect();
        let names_len: usize = names.iter().map(String::len).sum();
        if by_hunk && names_len > room / 2 {
            continue;
        }
        if names_len > room {
            bail!(
                "{} changed files are too many to show the model in its context; stage a part \
                 of them and split those first, or use a model with a larger context",
                files.len()
            );
        }
        let previews: Vec<String> = units.iter().map(|&unit| lines(files, unit)).collect();
        let sizes: Vec<usize> = previews.iter().map(String::len).collect();
        let shares = fit::share(&sizes, room - names_len);
        let mut text = String::new();
        for ((name, preview), share) in names.iter().zip(&previews).zip(shares) {
            text.push_str(name);
            text.push_str(&cut(preview, share));
        }
        return Ok(Described {
            units,
            text,
            by_file: !by_hunk,
        });
    }
    unreachable!("the file-by-file pass returns or fails")
}

/// What a unit is, e.g. "src/main.rs, change 2 of 3, in `fn main() {` (+4 -1)".
fn name(files: &[FilePatch], unit: Unit) -> String {
    let file = &files[unit.file];
    let mut name = file.path.clone();
    let body: String = match unit.hunk {
        Some(index) => {
            let hunk = &file.hunks[index];
            let _ = write!(name, ", change {} of {}", index + 1, file.hunks.len());
            let section: String = hunk.section.trim().chars().take(SECTION_CHARS).collect();
            if !section.is_empty() {
                let _ = write!(name, ", near `{section}`");
            }
            hunk.body.clone()
        }
        None => {
            if let Some(status) = file.status {
                let _ = write!(name, ", {status}");
            }
            if file.excluded {
                name.push_str(", diff not shown");
            }
            file.hunks.iter().map(|hunk| hunk.body.as_str()).collect()
        }
    };
    let added = body.lines().filter(|line| line.starts_with('+')).count();
    let removed = body.lines().filter(|line| line.starts_with('-')).count();
    match (added, removed) {
        (0, 0) => {}
        (added, 0) => {
            let _ = write!(name, " (+{added})");
        }
        (0, removed) => {
            let _ = write!(name, " (-{removed})");
        }
        (added, removed) => {
            let _ = write!(name, " (+{added} -{removed})");
        }
    }
    lossless::readable(&name)
}

/// The lines of a unit's hunks the model sees, with the unchanged lines around
/// the changes, so it can tell where they are. Excluded, binary and deleted
/// files show none.
fn lines(files: &[FilePatch], unit: Unit) -> String {
    let file = &files[unit.file];
    if file.excluded || matches!(file.status, Some("binary" | "deleted")) {
        return String::new();
    }
    let hunks = match unit.hunk {
        Some(index) => &file.hunks[index..=index],
        None => &file.hunks[..],
    };
    let shown: String = hunks
        .iter()
        .flat_map(|hunk| hunk.body.split_inclusive('\n'))
        .filter(|line| !line.starts_with('\\'))
        .collect();
    lossless::readable(&shown)
}

/// `text` in at most about `room` bytes, ending with a note if it was cut.
fn cut(text: &str, room: usize) -> String {
    if text.len() <= room {
        return text.to_owned();
    }
    let kept = fit::head_lines(text, room.saturating_sub(NOTE_BYTES));
    let left = text[kept.len()..].lines().count();
    format!("{kept}[… {left} more lines]\n")
}

/// Commits to make, in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub commits: Vec<Planned>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub title: String,
    /// Indexes into the units.
    pub units: Vec<usize>,
}

impl Plan {
    /// The plan in the form the model writes it, for continuing the conversation.
    pub fn to_json(&self) -> String {
        let commits: Vec<Value> = self
            .commits
            .iter()
            .map(|commit| {
                let numbers: Vec<usize> = commit.units.iter().map(|unit| unit + 1).collect();
                json!({ "title": commit.title, "units": numbers })
            })
            .collect();
        json!({ "commits": commits }).to_string()
    }
}

/// A plan made valid by [`settle`], and what had to be fixed.
#[derive(Debug, Clone, PartialEq)]
pub struct Settled {
    pub plan: Plan,
    /// What was fixed, for the user.
    pub notes: Vec<String>,
    /// The 1-based numbers of the units the plan left out.
    pub missing: Vec<usize>,
    /// The 1-based numbers of the units the plan put in more than one commit.
    pub repeated: Vec<usize>,
}

/// Reads the model's plan, then makes it complete (see [`settle`]).
pub fn parse_plan(reply: &str, files: &[FilePatch], units: &[Unit]) -> Result<Settled> {
    let start = reply
        .find(['{', '['])
        .context("the model's reply has no plan in it")?;
    let end = reply
        .rfind(['}', ']'])
        .context("the model's plan is cut off")?;
    let after_plan = reply.get(end + 1..).unwrap_or_default();
    let (value, repaired) = read_json(reply.get(start..=end).unwrap_or_default(), after_plan)?;
    let (raw, hidden) = without_hidden_titles(raw_commits(&value)?);
    let mut settled = settle(raw, files, units);
    if hidden > 0 {
        settled.notes.insert(
            0,
            format!("the plan's titles held {hidden} control or invisible character(s); git-agent removed them"),
        );
    }
    if repaired {
        settled.notes.insert(
            0,
            "the plan's JSON had its closing brackets wrong, so git-agent fixed them".to_owned(),
        );
    }
    Ok(settled)
}

/// The plan's commits with hidden characters taken out of their titles, and how many
/// were. JSON can spell them as `\\u001b`, so cleaning the reply as text can't catch them.
fn without_hidden_titles(raw: Vec<(String, Vec<usize>)>) -> (Vec<(String, Vec<usize>)>, usize) {
    let mut hidden = 0;
    let cleaned = raw
        .into_iter()
        .map(|(title, numbers)| {
            let title = without_hidden(&title);
            hidden += title.removed;
            (title.text.trim().to_owned(), numbers)
        })
        .collect();
    (cleaned, hidden)
}

/// Reads the JSON of a plan, and says if it had to fix closing brackets the
/// model got wrong. A reply that goes on after the last bracket was cut off, so
/// what's there can't be trusted, and one that names no units is no plan.
fn read_json(text: &str, after_plan: &str) -> Result<(Value, bool)> {
    let error = match serde_json::from_str(text) {
        Ok(value) => return Ok((value, false)),
        Err(error) => error,
    };
    let is_cut_off = !after_plan.trim().trim_matches('`').trim().is_empty();
    (!is_cut_off)
        .then(|| closed_brackets(text))
        .flatten()
        .and_then(|closed| serde_json::from_str::<Value>(&closed).ok())
        .filter(|value| {
            raw_commits(value).is_ok_and(|raw| raw.iter().any(|(_, units)| !units.is_empty()))
        })
        .map(|value| (value, true))
        .ok_or(error)
        .context("the model's plan is not valid JSON")
}

/// `text` with its brackets closed, if that's all it lacks: those left open at
/// the end, and those a closing bracket passes over, as in `[{"a":[1]]`. Not if
/// it ends inside a string, or a closing bracket matches nothing that's open.
fn closed_brackets(text: &str) -> Option<String> {
    let mut open: Vec<char> = Vec::new();
    let mut closed = String::with_capacity(text.len());
    let (mut in_string, mut escaped) = (false, false);
    for character in text.chars() {
        match character {
            _ if escaped => escaped = false,
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            _ if in_string => {}
            '{' => open.push('}'),
            '[' => open.push(']'),
            '}' | ']' => {
                let matching = open.iter().rposition(|&closer| closer == character)?;
                closed.extend(open.drain(matching + 1..).rev());
                open.pop();
            }
            _ => {}
        }
        closed.push(character);
    }
    if in_string {
        return None;
    }
    closed.extend(open.iter().rev());
    (closed != text).then_some(closed)
}

/// The titles and 1-based unit numbers of a plan's commits, in any of the forms
/// models write them.
fn raw_commits(value: &Value) -> Result<Vec<(String, Vec<usize>)>> {
    let commits = match value {
        Value::Array(commits) => commits,
        Value::Object(plan) => plan
            .get("commits")
            .and_then(Value::as_array)
            .context("the model's plan has no \"commits\" list")?,
        _ => bail!("the model's plan is not a list of commits"),
    };
    Ok(commits
        .iter()
        .map(|commit| {
            let title = ["title", "message", "summary", "subject"]
                .iter()
                .find_map(|key| commit.get(key)?.as_str())
                .and_then(|title| title.lines().next())
                .unwrap_or_default()
                .trim()
                .to_owned();
            let numbers = ["units", "changes", "hunks"]
                .iter()
                .find_map(|key| commit.get(key)?.as_array())
                .map(|numbers| {
                    let mut found = Vec::new();
                    numbers
                        .iter()
                        .for_each(|number| unit_numbers(number, &mut found));
                    found
                })
                .unwrap_or_default();
            (title, numbers)
        })
        .collect())
}

/// Adds the unit numbers in `value` to `found`. Besides plain numbers, models
/// write them as strings ("3", "Unit 3"), lists ([3]) or objects ({"unit": 3}).
fn unit_numbers(value: &Value, found: &mut Vec<usize>) {
    match value {
        Value::Number(number) => found.extend(number.as_u64().map(|number| number as usize)),
        Value::String(text) => found.extend(
            text.trim_matches(|c: char| !c.is_ascii_digit())
                .parse::<usize>()
                .ok(),
        ),
        Value::Array(values) => values.iter().for_each(|value| unit_numbers(value, found)),
        Value::Object(fields) => ["unit", "id", "number"]
            .iter()
            .filter_map(|key| fields.get(*key))
            .take(1)
            .for_each(|value| unit_numbers(value, found)),
        _ => {}
    }
}

/// Makes a plan of titles and 1-based unit numbers valid: unknown and
/// repeated numbers are dropped, and units the plan left out join the commit
/// with another part of their file, or a last commit of their own.
fn settle(raw: Vec<(String, Vec<usize>)>, files: &[FilePatch], units: &[Unit]) -> Settled {
    let mut placed = vec![false; units.len()];
    let (mut unknown, mut repeated) = (Vec::new(), Vec::new());
    let mut commits: Vec<Planned> = Vec::new();
    for (title, numbers) in raw {
        let mut chosen = Vec::new();
        for number in numbers {
            match number.checked_sub(1).filter(|&index| index < units.len()) {
                None => unknown.push(number),
                Some(index) if placed[index] => repeated.push(number),
                Some(index) => {
                    placed[index] = true;
                    chosen.push(index);
                }
            }
        }
        if !chosen.is_empty() {
            let title = if title.is_empty() {
                format!("Change {}", files[units[chosen[0]].file].path)
            } else {
                title
            };
            commits.push(Planned {
                title,
                units: chosen,
            });
        }
    }

    let mut notes = Vec::new();
    if !unknown.is_empty() {
        notes.push(format!(
            "the plan named changes that don't exist: {}",
            join(&unknown)
        ));
    }
    if !repeated.is_empty() {
        notes.push(format!(
            "the plan put changes in more than one commit: {}; each stays in the first",
            join(&repeated)
        ));
    }
    let missing: Vec<usize> = (0..units.len()).filter(|&index| !placed[index]).collect();
    let missing_numbers: Vec<usize> = missing.iter().map(|index| index + 1).collect();
    let mut leftover = Vec::new();
    for &index in &missing {
        let file = units[index].file;
        match commits
            .iter_mut()
            .find(|commit| commit.units.iter().any(|&unit| units[unit].file == file))
        {
            Some(commit) => commit.units.push(index),
            None => leftover.push(index),
        }
    }
    if !missing.is_empty() {
        notes.push(format!(
            "the plan left out changes {}; they joined the commit with the rest of their file, \
             or a last commit",
            join(&missing_numbers)
        ));
    }
    if !leftover.is_empty() {
        commits.push(Planned {
            title: "Commit the remaining changes".to_owned(),
            units: leftover,
        });
    }
    Settled {
        plan: Plan { commits },
        notes,
        missing: missing_numbers,
        repeated,
    }
}

fn join(numbers: &[usize]) -> String {
    let numbers: Vec<String> = numbers.iter().map(usize::to_string).collect();
    numbers.join(", ")
}

/// The files a commit changes, e.g. "src/cache.rs (new file)" or "src/main.rs
/// (2 of 3 changes)".
pub fn files_of(commit: &Planned, files: &[FilePatch], units: &[Unit]) -> Vec<String> {
    let mut by_file: BTreeMap<usize, usize> = BTreeMap::new();
    for &unit in &commit.units {
        *by_file.entry(units[unit].file).or_default() += 1;
    }
    by_file
        .into_iter()
        .map(|(index, count)| {
            let file = &files[index];
            match file.status {
                Some(status) => format!("{} ({status})", file.path),
                None if file.splittable() && count < file.hunks.len() => {
                    format!("{} ({count} of {} changes)", file.path, file.hunks.len())
                }
                None => file.path.clone(),
            }
        })
        .collect()
}

/// The plan as shown for review, ending with how many parts go into how many commits.
pub fn show(plan: &Plan, files: &[FilePatch], units: &[Unit]) -> String {
    let mut text = String::new();
    for (index, commit) in plan.commits.iter().enumerate() {
        let _ = writeln!(text, "{}. {}", index + 1, commit.title);
        let _ = writeln!(text, "   {}", files_of(commit, files, units).join(", "));
    }
    let commits = plan.commits.len();
    let _ = write!(
        text,
        "\n{} parts in {commits} commit{}",
        units.len(),
        if commits == 1 { "" } else { "s" }
    );
    text
}

/// The plan as the user edits it.
pub fn to_text(plan: &Plan, files: &[FilePatch], units: &[Unit]) -> String {
    let mut text = format!("{EDIT_HELP}\n");
    for commit in &plan.commits {
        let _ = writeln!(text, "commit: {}", commit.title);
        for &unit in &commit.units {
            let _ = writeln!(text, "  {}  {}", unit + 1, name(files, units[unit]));
        }
        text.push('\n');
    }
    escaped(&text)
}

/// Reads a plan the user edited (see [`to_text`]). A plan without commits
/// stays empty, so the caller can keep the previous one.
pub fn from_text(text: &str, files: &[FilePatch], units: &[Unit]) -> (Plan, Vec<String>) {
    let mut raw: Vec<(String, Vec<usize>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(title) = line.strip_prefix("commit:") {
            raw.push((title.trim().to_owned(), Vec::new()));
        } else if let Some(number) = line.split_whitespace().next().and_then(|n| n.parse().ok()) {
            match raw.last_mut() {
                Some((_, numbers)) => numbers.push(number),
                None => raw.push((String::new(), vec![number])),
            }
        }
    }
    if raw.is_empty() {
        return (Plan::default(), Vec::new());
    }
    let settled = settle(raw, files, units);
    (settled.plan, settled.notes)
}

/// A commit that may hold several of a plan's commits.
#[derive(Debug, Clone, PartialEq)]
pub struct Merged {
    pub commit: Planned,
    /// Which of the plan's commits it holds. The last one's tree is its tree.
    pub from: Range<usize>,
}

/// Merges each commit whose result didn't pass into the next one. `passed` says
/// for each commit whether the tree it leaves is good. The last commit always
/// ends a merge, because the caller knows the whole change is what it is, and
/// a commit `passed` says nothing about counts as failed.
pub fn merge_failed(plan: &Plan, passed: &[bool]) -> Vec<Merged> {
    let mut merged = Vec::new();
    let mut start = 0;
    for end in 1..=plan.commits.len() {
        let is_last = end == plan.commits.len();
        if is_last || passed.get(end - 1).copied().unwrap_or(false) {
            merged.push(merge(&plan.commits, start..end));
            start = end;
        }
    }
    merged
}

fn merge(commits: &[Planned], from: Range<usize>) -> Merged {
    let parts = &commits[from.clone()];
    let titles: Vec<&str> = parts.iter().map(|commit| commit.title.as_str()).collect();
    Merged {
        commit: Planned {
            title: titles.join("; "),
            units: parts
                .iter()
                .flat_map(|commit| commit.units.iter().copied())
                .collect(),
        },
        from,
    }
}

/// The commits of `merged` as a plan.
pub fn merged_plan(merged: &[Merged]) -> Plan {
    Plan {
        commits: merged.iter().map(|merge| merge.commit.clone()).collect(),
    }
}

/// What merging did to `plan`, for the user. Commits go by their titles, because
/// their numbers change as they merge.
pub fn merge_notes(plan: &Plan, merged: &[Merged]) -> Vec<String> {
    merged
        .iter()
        .filter(|merge| merge.from.len() > 1)
        .map(|merge| {
            let titles: Vec<String> = plan.commits[merge.from.clone()]
                .iter()
                .map(|commit| format!("\"{}\"", commit.title))
                .collect();
            let (last, others) = titles.split_last().expect("a merge holds commits");
            format!("{} and {last} became one commit", others.join(", "))
        })
        .collect()
}

/// A patch that adds a commit's units to HEAD.
pub struct Patch {
    pub text: String,
    /// The line count changes of its hunks, by file and original start line.
    shifts: Vec<(usize, usize, isize)>,
}

/// The line count changes of hunks already committed, by file, so the next
/// hunks of the same file get the right line numbers.
#[derive(Default)]
pub struct Committed {
    shifts: BTreeMap<usize, Vec<(usize, isize)>>,
}

impl Committed {
    pub fn record(&mut self, patch: &Patch) {
        for &(file, start, delta) in &patch.shifts {
            self.shifts.entry(file).or_default().push((start, delta));
        }
    }
}

/// The patch for the units `chosen`, against HEAD once the `committed` hunks
/// are in it.
pub fn patch(
    files: &[FilePatch],
    units: &[Unit],
    chosen: &[usize],
    committed: &Committed,
) -> Patch {
    let mut by_file: BTreeMap<usize, Vec<Option<usize>>> = BTreeMap::new();
    for &unit in chosen {
        by_file
            .entry(units[unit].file)
            .or_default()
            .push(units[unit].hunk);
    }
    let mut text = String::new();
    let mut shifts = Vec::new();
    for (index, hunks) in by_file {
        let file = &files[index];
        if hunks.contains(&None) {
            text.push_str(&file.header);
            for hunk in &file.hunks {
                text.push_str(&hunk.header);
                text.push_str(&hunk.body);
            }
            continue;
        }
        // Without the "index" line, whose blob names fit only the whole change.
        let header = file.header.split_inclusive('\n');
        text.extend(header.filter(|line| !line.starts_with("index ")));
        let mut hunks: Vec<usize> = hunks.into_iter().flatten().collect();
        hunks.sort_unstable();
        let earlier = committed.shifts.get(&index).map_or(&[][..], Vec::as_slice);
        // How far this patch's own hunks have moved the lines after them.
        let mut moved = 0;
        for hunk in hunks.iter().map(|&hunk| &file.hunks[hunk]) {
            let above: isize = earlier
                .iter()
                .filter(|(start, _)| *start < hunk.old_start)
                .map(|(_, delta)| delta)
                .sum();
            let old_start = hunk.old_start as isize + above;
            let _ = writeln!(
                text,
                "@@ -{old_start},{} +{},{} @@{}",
                hunk.old_len,
                old_start + moved,
                hunk.new_len,
                hunk.section
            );
            text.push_str(&hunk.body);
            let delta = hunk.new_len as isize - hunk.old_len as isize;
            moved += delta;
            shifts.push((index, hunk.old_start, delta));
        }
    }
    Patch { text, shifts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support::{numbered_lines as lines, run};
    use crate::git::{Repo, Scope};
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    /// A repository with lib.txt (40 lines), notes.txt and Cargo.lock committed.
    fn setup() -> (TempDir, Repo) {
        let dir = TempDir::new().unwrap();
        let path = dir.path();
        run(path, &["init", "-q", "--initial-branch", "main"]);
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "no-hooks"),
        ] {
            run(path, &["config", key, value]);
        }
        fs::write(path.join("lib.txt"), lines(40)).unwrap();
        fs::write(path.join("notes.txt"), "a\n").unwrap();
        fs::write(path.join("Cargo.lock"), "lock 1\n").unwrap();
        run(path, &["add", "--all"]);
        run(path, &["commit", "-q", "-m", "init"]);
        let repo = Repo::open(path).unwrap();
        (dir, repo)
    }

    /// Changes lib.txt in three separate places (adding a line in the first),
    /// adds new.txt, deletes notes.txt and changes Cargo.lock, all staged.
    fn change(dir: &Path) {
        let mut lib: Vec<String> = lines(40).lines().map(str::to_owned).collect();
        lib[1] = "line 2 changed".into();
        lib.insert(2, "a new line 2b".into());
        lib[20] = "line 20 changed".into();
        lib[37] = "line 37 changed".into();
        fs::write(dir.join("lib.txt"), lib.join("\n") + "\n").unwrap();
        fs::write(dir.join("new.txt"), "brand new\n").unwrap();
        fs::remove_file(dir.join("notes.txt")).unwrap();
        fs::write(dir.join("Cargo.lock"), "lock 2\n").unwrap();
        run(dir, &["add", "--all"]);
    }

    fn files(repo: &Repo) -> Vec<FilePatch> {
        let exclude = ["Cargo.lock".to_owned()];
        let patch = repo.patch(Scope::Staged).unwrap();
        parse(
            &patch,
            &repo.included_paths(Scope::Staged, &exclude).unwrap(),
        )
    }

    #[test]
    fn cuts_the_change_into_hunks_and_whole_files() {
        let (dir, repo) = setup();
        change(dir.path());
        let files = files(&repo);
        let summary: Vec<_> = files
            .iter()
            .map(|file| {
                (
                    file.path.as_str(),
                    file.hunks.len(),
                    file.status,
                    file.excluded,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("Cargo.lock", 1, None, true),
                ("lib.txt", 3, None, false),
                ("new.txt", 1, Some("new file"), false),
                ("notes.txt", 1, Some("deleted"), false),
            ]
        );
        let described = describe(&files, 100_000).unwrap();
        assert!(!described.by_file);
        assert_eq!(described.units.len(), 6);
        assert!(described.text.starts_with(
            "Unit 1: Cargo.lock, diff not shown (+1 -1)\nUnit 2: lib.txt, change 1 of 3"
        ));
        assert!(described.text.contains(
            "Unit 3: lib.txt, change 2 of 3, near `line 16` (+1 -1)\n line 17\n line 18\n \
             line 19\n-line 20\n+line 20 changed\n line 21\n"
        ));
        assert!(
            described
                .text
                .contains("Unit 5: new.txt, new file (+1)\n+brand new\n")
        );
        assert!(described.text.contains("Unit 6: notes.txt, deleted (-1)\n"));
        assert!(!described.text.contains("lock 2"));
    }

    #[test]
    fn falls_back_to_whole_files_and_shortens_previews() {
        let (dir, repo) = setup();
        change(dir.path());
        let files = files(&repo);
        // Room for the names of the files, but not twice the names of all the hunks.
        let described = describe(&files, 200).unwrap();
        assert!(described.by_file);
        assert_eq!(described.units.len(), 4);
        assert!(describe(&files, 20).is_err());

        assert_eq!(cut("a\nb\nc\nd\n", 100), "a\nb\nc\nd\n");
        assert_eq!(cut(&"x\n".repeat(100), 44), "x\nx\n[… 98 more lines]\n");
    }

    #[test]
    fn commits_parts_of_a_file_in_any_order_and_ends_with_the_whole_change() {
        let (dir, repo) = setup();
        change(dir.path());
        let files = files(&repo);
        let units = describe(&files, 100_000).unwrap().units;
        let original = run(dir.path(), &["write-tree"]);
        let index = repo.with_index(dir.path().join(".git/split-test-index"));
        let mut committed = Committed::default();
        // The last change of lib.txt first, then the first one with the new file,
        // then the rest; the first change adds a line, so later ones move down.
        for (chosen, message) in [
            (vec![3], "third change"),
            (vec![1, 4], "first change and new file"),
            (vec![2, 0, 5], "the rest"),
        ] {
            let patch = patch(&files, &units, &chosen, &committed);
            index.read_tree("HEAD").unwrap();
            index.apply_to_index(&patch.text).unwrap();
            index.commit(message, Scope::Staged, &[], &[]).unwrap();
            committed.record(&patch);
        }
        assert_eq!(run(dir.path(), &["rev-parse", "HEAD^{tree}"]), original);
        let first = run(dir.path(), &["show", "--format=", "--stat", "HEAD~2"]);
        assert!(first.contains("lib.txt | 2 +-") && first.contains("1 file changed"));
        let second = run(dir.path(), &["show", "HEAD~1:lib.txt"]);
        assert!(
            second.contains("line 2 changed\na new line 2b\n")
                && second.contains("line 37 changed")
        );
        assert!(!second.contains("line 20 changed"));
        // The real index was never touched: it still holds the whole change, which
        // HEAD now has too.
        assert!(!repo.has_staged_changes().unwrap());
    }

    #[test]
    fn settles_a_plan_the_model_got_wrong() {
        let (dir, repo) = setup();
        change(dir.path());
        let files = files(&repo);
        let units = describe(&files, 100_000).unwrap().units;
        let reply = "Sure:\n```json\n{\"commits\": [{\"title\": \"Change lib\", \"units\": [[2], \"Unit 3\", 9]}, \
                     {\"title\": \"Add new\", \"units\": [{\"unit\": 5}, 2]}, {\"title\": \"\", \"units\": [6]}]}\n```";
        let Settled {
            plan,
            notes,
            missing,
            repeated,
        } = parse_plan(reply, &files, &units).unwrap();
        // Units 1 and 4 were left out, and unit 2 was in two commits.
        assert_eq!((missing, repeated), (vec![1, 4], vec![2]));
        let commits: Vec<_> = plan
            .commits
            .iter()
            .map(|commit| (commit.title.as_str(), commit.units.clone()))
            .collect();
        assert_eq!(
            commits,
            [
                ("Change lib", vec![1, 2, 3]),
                ("Add new", vec![4]),
                ("Change notes.txt", vec![5]),
                ("Commit the remaining changes", vec![0]),
            ]
        );
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(parse_plan("no plan here", &files, &units).is_err());
        assert!(parse_plan("{\"commits\": [", &files, &units).is_err());

        let shown = show(&plan, &files, &units);
        assert!(shown.starts_with("1. Change lib\n   lib.txt\n2. Add new\n   new.txt (new file)"));
        assert!(shown.ends_with("\n\n6 parts in 4 commits"), "{shown}");
        let one = Plan {
            commits: vec![plan.commits[0].clone()],
        };
        assert!(show(&one, &files, &units).ends_with("6 parts in 1 commit"));
    }

    #[test]
    fn closes_the_brackets_a_model_left_off_its_plan() {
        let (dir, repo) = setup();
        change(dir.path());
        let files = files(&repo);
        let units = describe(&files, 100_000).unwrap().units;
        let reply = "{\"commits\":[{\"title\":\"Brackets ] } [ { in a \\\"title\\\"\",\"units\":[1,2,3]},\
                     {\"title\":\"The rest\",\"units\":[4,5,6]}]";
        let settled = parse_plan(reply, &files, &units).unwrap();
        assert_eq!(settled.plan.commits.len(), 2);
        assert_eq!(
            settled.plan.commits[0].title,
            "Brackets ] } [ { in a \"title\""
        );
        assert!(settled.missing.is_empty());
        assert!(
            settled.notes[0].contains("closing brackets"),
            "{:?}",
            settled.notes
        );

        // A list of commits without the object around it, missing its own end.
        let bare = "[{\"title\":\"All\",\"units\":[1,2,3,4,5,6]}";
        let settled = parse_plan(bare, &files, &units).unwrap();
        assert_eq!(settled.plan.commits.len(), 1);

        // A closing bracket that passes over the object it should have ended.
        let passed_over = "{\"commits\":[{\"title\":\"All\",\"units\":[1,2,3,4,5,6]]}";
        let settled = parse_plan(passed_over, &files, &units).unwrap();
        assert_eq!(settled.plan.commits[0].units.len(), 6);

        // A code fence after the plan doesn't count as more of it.
        let fenced = "```json\n{\"commits\":[{\"title\":\"All\",\"units\":[1,2,3,4,5,6]}]\n```";
        assert_eq!(
            parse_plan(fenced, &files, &units)
                .unwrap()
                .plan
                .commits
                .len(),
            1
        );
    }

    #[test]
    fn doesnt_add_a_note_to_a_plan_that_needed_no_repair() {
        let (dir, repo) = setup();
        change(dir.path());
        let files = files(&repo);
        let units = describe(&files, 100_000).unwrap().units;
        let reply = "{\"commits\":[{\"title\":\"All\",\"units\":[1,2,3,4,5,6]}]}";
        assert!(parse_plan(reply, &files, &units).unwrap().notes.is_empty());
    }

    #[test]
    fn wont_repair_a_reply_that_is_no_plan() {
        let (dir, repo) = setup();
        change(dir.path());
        let files = files(&repo);
        let units = describe(&files, 100_000).unwrap().units;
        for reply in [
            "{\"commits\": [",
            "{\"commits\": []",
            "{\"commits\": [{\"title\": \"No units\"}",
            "{\"commits\": [{\"title\": \"Empty\", \"units\": []}",
            "{\"commits\": [{\"title\": \"Cut in a string",
            "[{\"title\": \"A closer with nothing to close\", \"units\": [1]}]}]",
            "{\"commits\":[{\"title\":\"Cut\",\"units\":[1]},{\"title\":\"B\",\"units\":[3",
            "{\"plan\": [{\"title\": \"Other key\", \"units\": [1]}]",
            "[3",
            "no json at all",
        ] {
            assert!(parse_plan(reply, &files, &units).is_err(), "{reply}");
        }
    }

    #[test]
    fn closes_only_what_is_open() {
        assert_eq!(
            closed_brackets("{\"a\":[1]").as_deref(),
            Some("{\"a\":[1]}")
        );
        assert_eq!(closed_brackets("[[1]").as_deref(), Some("[[1]]"));
        assert_eq!(
            closed_brackets("{\"a\":\"]\"").as_deref(),
            Some("{\"a\":\"]\"}")
        );
        assert_eq!(closed_brackets("{\"a\":1}"), None);
        assert_eq!(closed_brackets("{\"a\":\"open"), None);
        assert_eq!(closed_brackets("[1}"), None);
        assert_eq!(
            closed_brackets("{\"a\":[1}").as_deref(),
            Some("{\"a\":[1]}")
        );
        assert_eq!(
            closed_brackets("{\"c\":[{\"u\":[1]]}").as_deref(),
            Some("{\"c\":[{\"u\":[1]}]}")
        );
        assert_eq!(closed_brackets(""), None);
    }

    #[test]
    fn reads_back_a_plan_the_user_edited() {
        let (dir, repo) = setup();
        change(dir.path());
        let files = files(&repo);
        let units = describe(&files, 100_000).unwrap().units;
        let plan = Plan {
            commits: vec![
                Planned {
                    title: "Lib".into(),
                    units: vec![1, 2, 3],
                },
                Planned {
                    title: "Rest".into(),
                    units: vec![0, 4, 5],
                },
            ],
        };
        let text = to_text(&plan, &files, &units);
        assert_eq!(from_text(&text, &files, &units), (plan.clone(), Vec::new()));
        // Moving change 3 into its own commit, first.
        let edited = text.replacen("commit: Lib", "commit: Middle change\n  3\ncommit: Lib", 1);
        let edited = edited.replacen("  3  lib.txt", "# moved", 1);
        let (edited, notes) = from_text(&edited, &files, &units);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            edited.commits[0],
            Planned {
                title: "Middle change".into(),
                units: vec![2]
            }
        );
        assert_eq!(edited.commits[1].units, [1, 3]);
        assert_eq!(
            serde_json::from_str::<Value>(&plan.to_json()).unwrap()["commits"][0]["units"],
            json!([2, 3, 4])
        );
    }

    /// Commits `name` with 40 lines, then stages a change in two separate places.
    fn change_two_places(dir: &Path, name: &str) {
        if let Some(folder) = Path::new(name).parent() {
            fs::create_dir_all(dir.join(folder)).unwrap();
        }
        fs::write(dir.join(name), lines(40)).unwrap();
        run(dir, &["add", "--all"]);
        run(dir, &["commit", "-q", "-m", "add the file"]);
        let changed = lines(40)
            .replace("line 2\n", "line 2 changed\n")
            .replace("line 37\n", "line 37 changed\n");
        fs::write(dir.join(name), changed).unwrap();
        run(dir, &["add", "--all"]);
    }

    /// Applies every unit in order, as commits would, to a temporary index,
    /// and returns the tree it ends with.
    fn tree_of_all_units(dir: &Path, repo: &Repo, files: &[FilePatch], units: &[Unit]) -> String {
        let index = repo.with_index(dir.join(".git/split-test-index"));
        index.read_tree("HEAD").unwrap();
        let chosen: Vec<usize> = (0..units.len()).collect();
        let patch = patch(files, units, &chosen, &Committed::default());
        index.apply_to_index(&patch.text).unwrap();
        index.write_tree().unwrap()
    }

    #[test]
    fn shows_and_splits_files_with_norwegian_names_whatever_the_setting_is() {
        for setting in [None, Some("true"), Some("false")] {
            let (dir, repo) = setup();
            if let Some(setting) = setting {
                run(dir.path(), &["config", "core.quotePath", setting]);
            }
            change_two_places(dir.path(), "kjøring/Mønster æøå.txt");
            let files = files(&repo);
            assert_eq!(files[0].path, "kjøring/Mønster æøå.txt", "{setting:?}");
            assert!(!files[0].excluded, "{setting:?}");
            assert_eq!(files[0].hunks.len(), 2);
            let described = describe(&files, 100_000).unwrap();
            assert_eq!(described.units.len(), 2);
            assert!(
                described
                    .text
                    .starts_with("Unit 1: kjøring/Mønster æøå.txt, change 1 of 2"),
                "{}",
                described.text
            );
            assert!(described.text.contains("+line 37 changed"));
            assert_eq!(
                tree_of_all_units(dir.path(), &repo, &files, &described.units),
                repo.write_tree().unwrap()
            );
        }
    }

    #[test]
    fn keeps_excluding_a_lockfile_with_a_norwegian_name() {
        let (dir, repo) = setup();
        change_two_places(dir.path(), "låsefil.lock");
        let patch = repo.patch(Scope::Staged).unwrap();
        let exclude = ["*.lock".to_owned()];
        let files = parse(
            &patch,
            &repo.included_paths(Scope::Staged, &exclude).unwrap(),
        );
        assert_eq!(files[0].path, "låsefil.lock");
        assert!(files[0].excluded);
    }

    #[test]
    fn keeps_control_characters_out_of_what_the_model_and_the_user_see() {
        for (name, shown) in [
            ("tab\there.txt", "tab\\there.txt"),
            ("quo\"te.txt", "quo\"te.txt"),
            ("back\\slash.txt", "back\\slash.txt"),
            ("ny\nUnit 9: linje.txt", "ny\\nUnit 9: linje.txt"),
            ("farge\u{1b}[31m.txt", "farge\\033[31m.txt"),
        ] {
            let (dir, repo) = setup();
            change_two_places(dir.path(), name);
            let files = files(&repo);
            assert_eq!(files[0].path, shown);
            assert!(!files[0].excluded, "{name:?} counted as excluded");
            let described = describe(&files, 100_000).unwrap();
            assert!(!described.text.contains('\u{1b}'));
            assert!(!described.text.contains("\nUnit 9"), "{}", described.text);
            assert_eq!(described.units.len(), 2, "{name:?}");
            assert_eq!(
                tree_of_all_units(dir.path(), &repo, &files, &described.units),
                repo.write_tree().unwrap(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn replays_a_name_that_isnt_utf8_to_the_same_tree() {
        for setting in [None, Some("true"), Some("false")] {
            let (dir, repo) = setup();
            if let Some(setting) = setting {
                run(dir.path(), &["config", "core.quotePath", setting]);
            }
            // Only in the index, because not every file system accepts such a name.
            let blob = |text: &str| {
                let mut child = Command::new("git")
                    .current_dir(dir.path())
                    .args(["hash-object", "-w", "--stdin"])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                std::io::Write::write_all(child.stdin.as_mut().unwrap(), text.as_bytes()).unwrap();
                let output = child.wait_with_output().unwrap();
                String::from_utf8(output.stdout).unwrap().trim().to_owned()
            };
            let put = |text: &str| {
                let mut child = Command::new("git")
                    .current_dir(dir.path())
                    .args(["update-index", "--index-info"])
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                let mut line = format!("100644 blob {}\t", blob(text)).into_bytes();
                line.extend_from_slice(b"Kj\xf8re.txt\n");
                std::io::Write::write_all(child.stdin.as_mut().unwrap(), &line).unwrap();
                assert!(child.wait().unwrap().success());
            };
            put("one\n");
            run(dir.path(), &["commit", "-q", "-m", "add the file"]);
            put("two\n");
            let files = files(&repo);
            assert_eq!(files[0].path, "Kj\u{fffd}re.txt", "{setting:?}");
            assert!(!files[0].excluded, "{setting:?}");
            let units = describe(&files, 100_000).unwrap().units;
            assert_eq!(
                tree_of_all_units(dir.path(), &repo, &files, &units),
                repo.write_tree().unwrap(),
                "{setting:?}"
            );
        }
    }

    #[test]
    fn builds_a_commit_from_a_subfolder_with_every_file_in_it() {
        let (dir, _) = setup();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/in.txt"), "a\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "add the folder"]);
        fs::write(dir.path().join("sub/in.txt"), "b\n").unwrap();
        fs::write(dir.path().join("notes.txt"), "b\n").unwrap();
        run(dir.path(), &["add", "--all"]);

        let repo = Repo::open_at_top(dir.path().join("sub")).unwrap();
        let files = files(&repo);
        let units = describe(&files, 100_000).unwrap().units;
        assert_eq!(units.len(), 2);
        let index = repo.with_index(repo.git_path("split-test-index").unwrap());
        index.read_tree("HEAD").unwrap();
        let everything = patch(&files, &units, &[0, 1], &Committed::default());
        index.apply_to_index(&everything.text).unwrap();
        assert_eq!(index.write_tree().unwrap(), repo.write_tree().unwrap());
    }

    /// `text` in Latin-1: each character is one byte, so it isn't valid UTF-8.
    fn latin1(text: &str) -> Vec<u8> {
        text.chars().map(|character| character as u8).collect()
    }

    #[test]
    fn splits_a_file_that_isnt_utf8_and_keeps_its_bytes() {
        let (dir, repo) = setup();
        let before: String = (1..=40).map(|n| format!("nøkkel{n}=blå\n")).collect();
        let after = before
            .replace("nøkkel2=blå\n", "nøkkel2=grønn\n")
            .replace("nøkkel37=blå\n", "nøkkel37=rød\nnøkkel37b=rød\n");
        fs::write(dir.path().join("tekst.properties"), latin1(&before)).unwrap();
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "add the file"]);
        fs::write(dir.path().join("tekst.properties"), latin1(&after)).unwrap();
        run(dir.path(), &["add", "--all"]);

        let files = files(&repo);
        let described = describe(&files, 100_000).unwrap();
        assert_eq!(described.units.len(), 2);
        assert!(
            described.text.contains("+n\u{fffd}kkel2=gr\u{fffd}nn"),
            "{}",
            described.text
        );
        let plan = Plan {
            commits: vec![Planned {
                title: "Change".into(),
                units: vec![0, 1],
            }],
        };
        let edited = to_text(&plan, &files, &described.units);
        assert!(
            edited
                .chars()
                .all(|c| !('\u{10ef80}'..='\u{10efff}').contains(&c))
        );

        let original = run(dir.path(), &["write-tree"]);
        let index = repo.with_index(dir.path().join(".git/split-test-index"));
        let mut committed = Committed::default();
        for (unit, message) in [(1, "the last change"), (0, "the first change")] {
            let patch = patch(&files, &described.units, &[unit], &committed);
            index.read_tree("HEAD").unwrap();
            index.apply_to_index(&patch.text).unwrap();
            index.commit(message, Scope::Staged, &[], &[]).unwrap();
            committed.record(&patch);
        }
        assert_eq!(run(dir.path(), &["rev-parse", "HEAD^{tree}"]), original);
    }

    #[test]
    fn keeps_utf8_text_that_uses_the_private_characters() {
        let (dir, repo) = setup();
        let private = "\u{10ef80}\u{10efff}";
        let before: String = (1..=40)
            .map(|n| format!("line {n} {private} æ\n"))
            .collect();
        let after = before
            .replace("line 2 ", "line two ")
            .replace("line 37 ", "line thirty seven ");
        fs::write(dir.path().join("private.txt"), before).unwrap();
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "add the file"]);
        fs::write(dir.path().join("private.txt"), after).unwrap();
        run(dir.path(), &["add", "--all"]);

        let files = files(&repo);
        let units = describe(&files, 100_000).unwrap().units;
        assert_eq!(units.len(), 2);
        assert_eq!(
            tree_of_all_units(dir.path(), &repo, &files, &units),
            repo.write_tree().unwrap()
        );
    }

    /// A plan of `count` commits, each with one unit named after its number.
    fn numbered_plan(count: usize) -> Plan {
        Plan {
            commits: (0..count)
                .map(|index| Planned {
                    title: format!("c{}", index + 1),
                    units: vec![index],
                })
                .collect(),
        }
    }

    /// Where each commit that comes of merging starts and ends in the plan.
    fn merges(count: usize, passed: &[bool]) -> Vec<(usize, usize)> {
        merge_failed(&numbered_plan(count), passed)
            .into_iter()
            .map(|merge| (merge.from.start, merge.from.end))
            .collect()
    }

    #[test]
    fn merges_each_failed_commit_into_the_next() {
        // All pass: nothing changes.
        assert_eq!(merges(3, &[true, true, true]), [(0, 1), (1, 2), (2, 3)]);
        // All fail: one commit, because the last always ends a merge.
        assert_eq!(merges(3, &[false, false, false]), [(0, 3)]);
        // The first fails.
        assert_eq!(merges(3, &[false, true, true]), [(0, 2), (2, 3)]);
        // The one before last fails.
        assert_eq!(merges(3, &[true, false, true]), [(0, 1), (1, 3)]);
        // Every second one fails.
        assert_eq!(merges(4, &[false, true, false, true]), [(0, 2), (2, 4)]);
        // One commit, passed or not.
        assert_eq!(merges(1, &[true]), [(0, 1)]);
        assert_eq!(merges(1, &[false]), [(0, 1)]);
        // No commits at all.
        assert!(merges(0, &[]).is_empty());
        // A commit it says nothing about counts as failed.
        assert_eq!(merges(3, &[true]), [(0, 1), (1, 3)]);
        assert_eq!(merges(2, &[true, true, true, true]), [(0, 1), (1, 2)]);
    }

    #[test]
    fn says_which_commits_became_one_by_their_titles() {
        let plan = numbered_plan(6);
        let merged = merge_failed(&plan, &[false, true, true, false, false, true]);
        assert_eq!(
            merge_notes(&plan, &merged),
            [
                "\"c1\" and \"c2\" became one commit",
                "\"c4\", \"c5\" and \"c6\" became one commit"
            ]
        );
        assert!(merge_notes(&plan, &merge_failed(&plan, &[true; 6])).is_empty());
    }

    #[test]
    fn keeps_every_unit_and_title_in_order_when_merging() {
        let merged = merge_failed(&numbered_plan(4), &[false, false, true, true]);
        let plan = merged_plan(&merged);
        assert_eq!(plan.commits[0].units, [0, 1, 2]);
        assert_eq!(plan.commits[0].title, "c1; c2; c3");
        assert_eq!(plan.commits[1].units, [3]);
        assert_eq!(plan.commits[1].title, "c4");
    }

    #[test]
    fn tells_a_rename_with_a_mode_change_as_a_rename() {
        let header = "diff --git a/tool.sh b/guide.md\nold mode 100644\nnew mode 100755\n\
                      similarity index 100%\nrename from tool.sh\nrename to guide.md\n";
        assert_eq!(status(header), Some("renamed"));
        assert_eq!(
            status("diff --git a/x b/x\nold mode 100644\n"),
            Some("mode changed")
        );
        assert_eq!(status("diff --git a/x b/x\nindex 1..2\n"), None);
    }

    #[test]
    fn reads_hunk_headers() {
        let hunk = parse_hunk_header("@@ -10,3 +12 @@ fn main() {\n").unwrap();
        assert_eq!((hunk.old_start, hunk.old_len, hunk.new_len), (10, 3, 1));
        assert_eq!(hunk.section, " fn main() {");
        assert!(parse_hunk_header("@@ nonsense\n").is_none());
    }

    #[test]
    fn takes_hidden_characters_out_of_titles_that_json_spelled() {
        let (cleaned, hidden) = without_hidden_titles(vec![
            ("a\u{1b}[2Jb\u{202e}".to_owned(), vec![1]),
            ("  plain æøå ".to_owned(), vec![2]),
            ("\u{1b}".to_owned(), vec![3]),
        ]);
        assert_eq!(hidden, 3);
        assert_eq!(cleaned[0].0, "a[2Jb");
        assert_eq!(cleaned[1].0, "plain æøå");
        assert_eq!(cleaned[2].0, "");
    }
}
