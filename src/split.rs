//! Splitting one large change into several commits (`git agent split`).
//!
//! The change is cut into units: one hunk of a modified text file, or a whole
//! file when it can't be committed in parts (new, deleted, renamed or binary
//! files, mode changes, and excluded files such as lockfiles). The model groups
//! the units into commits. Each commit is then built in a temporary index from
//! HEAD and the hunks of its units, so the real index keeps the changes that
//! are still to be committed.

use crate::fit;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;

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
    fit::split_files(patch)
        .into_iter()
        .map(|text| {
            let path = fit::path(text);
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
    header.lines().find_map(|line| {
        KINDS
            .iter()
            .find(|(start, _)| line.starts_with(start))
            .map(|(_, kind)| *kind)
    })
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
    name
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
    hunks
        .iter()
        .flat_map(|hunk| hunk.body.split_inclusive('\n'))
        .filter(|line| !line.starts_with('\\'))
        .collect()
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
    let value: Value = serde_json::from_str(reply.get(start..=end).unwrap_or_default())
        .context("the model's plan is not valid JSON")?;
    let commits = match &value {
        Value::Array(commits) => commits,
        Value::Object(plan) => plan
            .get("commits")
            .and_then(Value::as_array)
            .context("the model's plan has no \"commits\" list")?,
        _ => bail!("the model's plan is not a list of commits"),
    };
    let raw = commits
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
        .collect();
    Ok(settle(raw, files, units))
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
    text
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
    use crate::git::{Repo, Scope};
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    fn run(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// A file of `count` numbered lines.
    fn lines(count: usize) -> String {
        (1..=count).map(|n| format!("line {n}\n")).collect()
    }

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
            index.commit(message, Scope::Staged, &[]).unwrap();
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

    #[test]
    fn reads_hunk_headers() {
        let hunk = parse_hunk_header("@@ -10,3 +12 @@ fn main() {\n").unwrap();
        assert_eq!((hunk.old_start, hunk.old_len, hunk.new_len), (10, 3, 1));
        assert_eq!(hunk.section, " fn main() {");
        assert!(parse_hunk_header("@@ nonsense\n").is_none());
    }
}
