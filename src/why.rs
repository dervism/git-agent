//! Explaining why lines of code are the way they are (`git agent why`).
//!
//! The model sees the lines with some code around them, the commit that last
//! changed each line (`git blame`), and the history of the lines (`git log -L`):
//! each commit that changed them, with its message and its diff of those lines.
//! The history follows the lines through renames.

use crate::fit;
use crate::git::Summary;
use anyhow::{Result, anyhow, bail};
use std::fmt::Write;

/// Most commits of the history to read.
pub const HISTORY_LIMIT: usize = 30;
/// Lines of code shown before and after the explained ones.
const AROUND: usize = 8;
/// Most of the explained lines to show; a long function is cut short.
const MOST_SHOWN: usize = 150;
/// Most runs of blamed lines to list.
const MOST_RUNS: usize = 40;
/// Most bytes of a commit message to show.
const MESSAGE_BYTES: usize = 2000;
/// Least room worth giving a commit's diff when deciding which commits fit.
const MIN_DIFF_BYTES: usize = 200;
/// Room for the notes about commits left out and history not read.
const NOTE_BYTES: usize = 120;
/// Room for the note that replaces the end of a shortened diff.
const DIFF_NOTE_BYTES: usize = 48;

const HISTORY_HEADING: &str = "History of the lines, newest first: each commit that changed \
                               them, with its message and its diff of the lines.\n";

/// The lines `git agent why` explains.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub path: String,
    pub lines: Lines,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lines {
    /// The first and last line, counting from 1.
    Range(usize, usize),
    /// A function, found the way `git log -L :<name>:<file>` finds it.
    Function(String),
}

impl Target {
    /// Reads `file:line`, `file:start-end`, `file:start,end`, `file:start,+count`
    /// or `file:function`. A path may contain ':' itself, so the split comes
    /// after the first part that `is_file` accepts.
    pub fn parse(text: &str, is_file: impl Fn(&str) -> bool) -> Result<Target> {
        let split = text
            .match_indices(':')
            .map(|(index, _)| (&text[..index], &text[index + 1..]))
            .find(|(path, lines)| !lines.is_empty() && is_file(path));
        let Some((path, lines)) = split else {
            if is_file(text) {
                bail!(
                    "say which lines of {text} to explain: {text}:120, {text}:120-140, or a \
                     function such as {text}:main"
                );
            }
            let path = text.rsplit_once(':').map_or(text, |(path, _)| path);
            bail!("{path} is not a file");
        };
        Ok(Target {
            path: path.to_owned(),
            lines: Lines::parse(lines)?,
        })
    }

    /// The lines as git's `-L` option takes them, `120,140` or `:name`, in
    /// the order to try them. A function's name is tried as a whole word
    /// first, so `diff` finds `fn diff` rather than an earlier `fn has_diff`.
    pub fn ranges(&self) -> Vec<String> {
        match &self.lines {
            Lines::Range(first, last) => vec![format!("{first},{last}")],
            Lines::Function(name) => {
                let mut ranges = Vec::new();
                if name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    ranges.push(format!(":[^A-Za-z0-9_]{name}[^A-Za-z0-9_]"));
                }
                ranges.push(format!(":{name}"));
                ranges
            }
        }
    }

    /// The lines as they were asked for, e.g. `src/fit.rs:120-140`.
    pub fn label(&self) -> String {
        match &self.lines {
            Lines::Range(first, last) if first == last => format!("{}:{first}", self.path),
            Lines::Range(first, last) => format!("{}:{first}-{last}", self.path),
            Lines::Function(name) => format!("{}:{name}", self.path),
        }
    }

    /// The target, ending at the file's last line if the range goes past it.
    pub fn within(self, line_count: usize) -> Result<Target> {
        match self.lines {
            Lines::Range(first, _) if first > line_count => {
                bail!("{} has only {line_count} lines", self.path)
            }
            Lines::Range(first, last) => Ok(Target {
                lines: Lines::Range(first, last.min(line_count)),
                ..self
            }),
            Lines::Function(_) => Ok(self),
        }
    }
}

impl Lines {
    fn parse(text: &str) -> Result<Lines> {
        if !text.starts_with(|c: char| c.is_ascii_digit()) {
            let name = text.trim_start_matches(':').trim();
            if name.is_empty() {
                bail!("{text:?} is neither lines nor a function");
            }
            return Ok(Lines::Function(name.to_owned()));
        }
        let number = |text: &str| text.trim().parse::<usize>().ok().filter(|&n| n > 0);
        let invalid =
            || anyhow!("{text:?} is not a line or a range of lines, such as 120 or 120-140");
        let (first, last) = match text.split_once(['-', ',']) {
            None => {
                let line = number(text).ok_or_else(invalid)?;
                (line, line)
            }
            Some((first, rest)) => {
                let first = number(first).ok_or_else(invalid)?;
                let last = match rest.trim().strip_prefix('+') {
                    Some(count) => first + number(count).ok_or_else(invalid)? - 1,
                    None => number(rest).ok_or_else(invalid)?,
                };
                (first, last)
            }
        };
        if last < first {
            bail!("the range {text} ends before it starts");
        }
        Ok(Lines::Range(first, last))
    }
}

/// The commit that last changed a line of the file in the work tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Blamed {
    pub line: usize,
    /// The full hash; all zeros if the line isn't committed.
    pub hash: String,
}

impl Blamed {
    pub fn is_committed(&self) -> bool {
        !self.hash.bytes().all(|byte| byte == b'0')
    }
}

/// The lines of `git blame --porcelain` output, in order.
pub fn parse_blame(porcelain: &str) -> Vec<Blamed> {
    porcelain
        .lines()
        .filter(|line| !line.starts_with('\t'))
        .filter_map(|line| {
            // Each line starts with "<hash> <original line> <final line>".
            let mut parts = line.split(' ');
            let hash = parts.next()?;
            parts.next()?.parse::<usize>().ok()?;
            let line = parts.next()?.parse().ok()?;
            let is_hash = hash.len() >= 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
            is_hash.then(|| Blamed {
                line,
                hash: hash.to_owned(),
            })
        })
        .collect()
}

/// One line per run of lines from the same commit, e.g.
/// "Lines 120-121: a1b2c3d by Ann, 2025-10-02: Add a cache".
pub fn blame_summary(blamed: &[Blamed], summaries: &[Summary]) -> String {
    let runs: Vec<&[Blamed]> = blamed
        .chunk_by(|a, b| a.hash == b.hash && b.line == a.line + 1)
        .collect();
    let mut out = String::new();
    for run in runs.iter().take(MOST_RUNS) {
        let (first, last) = (run[0].line, run[run.len() - 1].line);
        let lines = if first == last {
            format!("Line {first}")
        } else {
            format!("Lines {first}-{last}")
        };
        let commit = if !run[0].is_committed() {
            "not committed yet".to_owned()
        } else {
            match summaries.iter().find(|summary| summary.hash == run[0].hash) {
                Some(summary) => format!(
                    "{} by {}, {}: {}",
                    summary.short, summary.author, summary.date, summary.subject
                ),
                None => run[0].hash[..7].to_owned(),
            }
        };
        let _ = writeln!(out, "{lines}: {commit}");
    }
    if runs.len() > MOST_RUNS {
        let _ = writeln!(out, "[… {} more not listed]", runs.len() - MOST_RUNS);
    }
    out
}

/// A `git diff --unified=0` hunk: where lines were removed and added.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hunk {
    old_start: usize,
    old_len: usize,
    new_start: usize,
    new_len: usize,
}

/// The hunks of a `git diff --unified=0` of one file.
pub fn parse_hunks(diff: &str) -> Vec<Hunk> {
    diff.lines()
        .filter_map(|line| {
            let ranges = line.strip_prefix("@@ -")?.split(" @@").next()?;
            let (old, new) = ranges.split_once(" +")?;
            let (old_start, old_len) = start_and_len(old)?;
            let (new_start, new_len) = start_and_len(new)?;
            Some(Hunk {
                old_start,
                old_len,
                new_start,
                new_len,
            })
        })
        .collect()
}

/// "12,3" as (12, 3), and "12" as (12, 1).
fn start_and_len(range: &str) -> Option<(usize, usize)> {
    match range.split_once(',') {
        Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
        None => Some((range.parse().ok()?, 1)),
    }
}

/// HEAD's number for `line` of the file in the work tree, given the file's
/// uncommitted `hunks`; `None` if the line itself is uncommitted.
pub fn head_line(hunks: &[Hunk], line: usize) -> Option<usize> {
    let mut offset: isize = 0;
    for hunk in hunks {
        let new_end = hunk.new_start + hunk.new_len;
        if hunk.new_len > 0 && (hunk.new_start..new_end).contains(&line) {
            return None;
        }
        // A hunk that only removes lines sits after line `new_start`.
        let before = if hunk.new_len > 0 {
            new_end <= line
        } else {
            hunk.new_start < line
        };
        if !before {
            break;
        }
        offset += hunk.old_len as isize - hunk.new_len as isize;
    }
    line.checked_add_signed(offset)
}

/// Lines `first` to `last` of `source`, numbered and marked with ">", with a
/// few lines of code before and after them.
pub fn excerpt(source: &str, first: usize, last: usize) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let shown_last = last.min(first + MOST_SHOWN - 1).min(lines.len());
    let cut = last > shown_last;
    let start = first.saturating_sub(AROUND).max(1);
    let end = if cut {
        shown_last
    } else {
        (last + AROUND).min(lines.len())
    };
    let width = end.to_string().len();
    let mut out = String::new();
    for number in start..=end {
        let marker = if (first..=last).contains(&number) {
            '>'
        } else {
            ' '
        };
        let _ = writeln!(out, "{marker} {number:>width$} | {}", lines[number - 1]);
    }
    if cut {
        let _ = writeln!(
            out,
            "[… {} more of these lines not shown]",
            last - shown_last
        );
    }
    out
}

/// What the model is asked to explain: the lines, the code, and the blame.
/// `uncommitted` lists the lines that differ from HEAD.
pub fn intro(
    target: &Target,
    (first, last): (usize, usize),
    uncommitted: &[usize],
    excerpt: &str,
    blame: &str,
) -> String {
    let what = match &target.lines {
        Lines::Function(name) => format!(
            "the function {name} in {}, lines {first}-{last}",
            target.path
        ),
        _ if first == last => format!("line {first} of {}", target.path),
        _ => format!("lines {first}-{last} of {}", target.path),
    };
    let mut out = format!("Explain {what}, marked with \">\" in the code below.\n");
    if !uncommitted.is_empty() {
        let _ = writeln!(
            out,
            "Lines {} have changes that are not committed yet; the history is of the \
             committed version.",
            runs(uncommitted)
        );
    }
    let _ = write!(
        out,
        "\nCode:\n{excerpt}\nLast change to each line (git blame):\n{blame}\n"
    );
    out
}

/// Ascending numbers as runs, e.g. "3, 7-9".
fn runs(numbers: &[usize]) -> String {
    let runs: Vec<String> = numbers
        .chunk_by(|a, b| b == &(a + 1))
        .map(|run| match run {
            [only] => only.to_string(),
            _ => format!("{}-{}", run[0], run[run.len() - 1]),
        })
        .collect();
    runs.join(", ")
}

/// A commit in the history of the lines.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub short: String,
    pub author: String,
    pub date: String,
    pub message: String,
    /// The commit's diff of the lines.
    pub patch: String,
}

/// The commits in `git log -L` output in the format of `Repo::line_history`.
pub fn parse_history(log: &str) -> Vec<Change> {
    log.split('\x1e')
        .filter_map(|record| {
            let mut parts = record.splitn(5, '\x1f');
            let short = parts.next()?.trim();
            if short.is_empty() {
                return None;
            }
            let mut next = || parts.next().unwrap_or_default();
            Some(Change {
                short: short.to_owned(),
                author: next().to_owned(),
                date: next().to_owned(),
                message: next().trim().to_owned(),
                patch: next().trim_start_matches('\n').to_owned(),
            })
        })
        .collect()
}

pub struct Described {
    pub text: String,
    /// Commits of the history left out to save space.
    pub left_out: usize,
    /// Commits whose diffs were cut short.
    pub cut: usize,
}

/// `intro` followed by the history of the lines in about `room` bytes. When
/// the history doesn't fit, its diffs are cut short, and when even its
/// messages don't fit, commits are left out from the second oldest back: the
/// newest commits say how the lines got their current form, and the oldest
/// one often says why they exist. `complete` says whether `history` goes back
/// to the commit that added the lines.
pub fn describe(intro: &str, history: &[Change], complete: bool, room: usize) -> Described {
    let mut out = intro.to_owned();
    if history.is_empty() {
        out.push_str("History: git has no commits that changed these lines.\n");
        return Described {
            text: out,
            left_out: 0,
            cut: 0,
        };
    }
    let heads: Vec<String> = history.iter().map(head).collect();
    let diffs: Vec<String> = history
        .iter()
        .map(|change| fit::compact_header(&change.patch))
        .collect();
    let budget = room.saturating_sub(intro.len() + HISTORY_HEADING.len() + NOTE_BYTES);

    let mut kept: Vec<usize> = (0..history.len()).collect();
    let heads_size = |kept: &[usize]| kept.iter().map(|&index| heads[index].len()).sum::<usize>();
    while kept.len() > 2 && heads_size(&kept) + MIN_DIFF_BYTES * kept.len() > budget {
        kept.remove(kept.len() - 2);
    }
    let left_out = history.len() - kept.len();
    let sizes: Vec<usize> = kept.iter().map(|&index| diffs[index].len()).collect();
    let room_for_diffs = budget.saturating_sub(heads_size(&kept));
    let shares = if sizes.iter().sum::<usize>() <= room_for_diffs {
        sizes
    } else {
        fit::share(
            &sizes,
            room_for_diffs.saturating_sub(DIFF_NOTE_BYTES * kept.len()),
        )
    };

    out.push_str(HISTORY_HEADING);
    let mut cut = 0;
    for (position, (&index, share)) in kept.iter().zip(shares).enumerate() {
        if left_out > 0 && position == kept.len() - 1 {
            let _ = writeln!(
                out,
                "\n[… {left_out} older commits that changed the lines left out to save space]"
            );
        }
        out.push_str(&heads[index]);
        let diff = &diffs[index];
        if share >= diff.len() {
            out.push_str(diff);
        } else {
            let part = fit::head_lines(diff, share);
            let rest = diff[part.len()..].lines().count();
            out.push_str(part);
            let _ = writeln!(out, "[… {rest} more lines of this diff not shown]");
            cut += 1;
        }
    }
    if !complete {
        let _ = writeln!(
            out,
            "\n[The history goes back further than these {} commits.]",
            history.len()
        );
    }
    Described {
        text: out,
        left_out,
        cut,
    }
}

/// The commit's line, then its message indented.
fn head(change: &Change) -> String {
    let message = format!("{}\n", change.message);
    let mut message = fit::head_lines(&message, MESSAGE_BYTES).to_owned();
    if message.len() < change.message.len() {
        message.push_str("[… rest of the message not shown]\n");
    }
    let mut out = format!(
        "\ncommit {} by {}, {}\n",
        change.short, change.author, change.date
    );
    for line in message.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            let _ = writeln!(out, "    {line}");
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(text: &str) -> Result<Target> {
        Target::parse(text, |path| ["src/fit.rs", "odd:name.txt"].contains(&path))
    }

    #[test]
    fn reads_lines_ranges_and_functions() {
        let lines = |text: &str| target(text).map(|target| target.lines);
        assert_eq!(lines("src/fit.rs:120").unwrap(), Lines::Range(120, 120));
        assert_eq!(lines("src/fit.rs:120-140").unwrap(), Lines::Range(120, 140));
        assert_eq!(lines("src/fit.rs:120,140").unwrap(), Lines::Range(120, 140));
        assert_eq!(lines("src/fit.rs:120,+3").unwrap(), Lines::Range(120, 122));
        assert_eq!(
            lines("src/fit.rs:shorten").unwrap(),
            Lines::Function("shorten".into())
        );
        assert_eq!(
            lines("src/fit.rs::Repo::open").unwrap(),
            Lines::Function("Repo::open".into())
        );
        let odd = target("odd:name.txt:7").unwrap();
        assert_eq!(odd.path, "odd:name.txt");
        assert_eq!(odd.label(), "odd:name.txt:7");
        assert_eq!(odd.ranges(), ["7,7"]);
        let function = target("src/fit.rs:diff").unwrap();
        assert_eq!(
            function.ranges(),
            [":[^A-Za-z0-9_]diff[^A-Za-z0-9_]", ":diff"]
        );
        let pattern = target("src/fit.rs:get.*").unwrap();
        assert_eq!(pattern.ranges(), [":get.*"]);
    }

    #[test]
    fn explains_what_is_wrong_with_a_target() {
        let error = |text: &str| target(text).unwrap_err().to_string();
        assert!(error("src/fit.rs").contains("src/fit.rs:120-140"));
        assert_eq!(error("src/nope.rs:12"), "src/nope.rs is not a file");
        assert!(error("src/fit.rs:0").contains("not a line"));
        assert!(error("src/fit.rs:12-x").contains("not a line"));
        assert!(error("src/fit.rs:20-10").contains("ends before it starts"));
    }

    #[test]
    fn keeps_a_range_inside_the_file() {
        let range = target("src/fit.rs:360-400").unwrap().within(371).unwrap();
        assert_eq!(range.lines, Lines::Range(360, 371));
        assert_eq!(range.ranges(), ["360,371"]);
        let past = target("src/fit.rs:380").unwrap().within(371).unwrap_err();
        assert_eq!(past.to_string(), "src/fit.rs has only 371 lines");
    }

    #[test]
    fn reads_blame_porcelain() {
        let committed = "a".repeat(40);
        let zeros = "0".repeat(40);
        let porcelain = format!(
            "{committed} 10 120 2\nauthor Ann\nsummary 1111111111111111111111111111111111111111 1 2\n\
             filename src/fit.rs\n\tlet a = 1;\n{committed} 11 121\n\tlet b = 2;\n\
             {zeros} 122 122 1\nauthor Not Committed Yet\n\tlet c = 3;\n"
        );
        let blamed = parse_blame(&porcelain);
        let lines: Vec<(usize, bool)> = blamed
            .iter()
            .map(|line| (line.line, line.is_committed()))
            .collect();
        assert_eq!(lines, [(120, true), (121, true), (122, false)]);

        let summaries = [Summary {
            hash: committed.clone(),
            short: "aaaaaaa".into(),
            author: "Ann".into(),
            date: "2025-10-02".into(),
            subject: "Add a cache".into(),
        }];
        assert_eq!(
            blame_summary(&blamed, &summaries),
            "Lines 120-121: aaaaaaa by Ann, 2025-10-02: Add a cache\nLine 122: not committed yet\n"
        );
    }

    #[test]
    fn maps_work_tree_lines_to_head() {
        // Line 2 is new, HEAD's line 5 was removed, and line 8 replaces HEAD's 7 and 8.
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,0 +2 @@\n+new\n\
                    @@ -5 +5,0 @@\n-gone\n@@ -7,2 +7 @@ fn x\n-a\n-b\n+c\n";
        let hunks = parse_hunks(diff);
        assert_eq!(hunks.len(), 3);
        let mapped: Vec<Option<usize>> = (1..=9).map(|line| head_line(&hunks, line)).collect();
        assert_eq!(
            mapped,
            [
                Some(1),
                None,
                Some(2),
                Some(3),
                Some(4),
                Some(6),
                None,
                Some(9),
                Some(10)
            ]
        );
        assert_eq!(head_line(&[], 4), Some(4));
    }

    #[test]
    fn shows_the_lines_with_code_around_them() {
        let source: String = (1..=200).map(|n| format!("code {n}\n")).collect();
        let shown = excerpt(&source, 12, 13);
        assert!(shown.starts_with("   4 | code 4\n"));
        assert!(shown.contains("> 12 | code 12\n> 13 | code 13\n  14 | code 14\n"));
        assert!(shown.ends_with("  21 | code 21\n"));
        assert!(excerpt(&source, 1, 1).starts_with("> 1 | code 1\n  2 |"));

        let long = excerpt(&source, 10, 200);
        assert!(long.contains("> 159 | code 159\n"));
        assert!(!long.contains("code 160"));
        assert!(long.ends_with("[… 41 more of these lines not shown]\n"));
    }

    #[test]
    fn introduces_the_lines_and_their_uncommitted_changes() {
        let target = target("src/fit.rs:120-125").unwrap();
        let intro = intro(
            &target,
            (120, 125),
            &[121, 123, 124],
            "> 120 | x\n",
            "Line 120: a\n",
        );
        assert!(intro.starts_with("Explain lines 120-125 of src/fit.rs, marked with \">\""));
        assert!(intro.contains("Lines 121, 123-124 have changes that are not committed yet"));
        assert!(intro.ends_with(
            "Code:\n> 120 | x\n\nLast change to each line (git blame):\nLine 120: a\n\n"
        ));
    }

    fn change(number: usize, diff_lines: usize) -> Change {
        let mut patch = format!(
            "diff --git a/src/fit.rs b/src/fit.rs\n--- a/src/fit.rs\n+++ b/src/fit.rs\n\
             @@ -{number},2 +{number},2 @@ fn fit\n"
        );
        for line in 0..diff_lines {
            let _ = writeln!(patch, "+changed line {line} in commit {number}");
        }
        Change {
            short: format!("c{number:06}"),
            author: "Ann".into(),
            date: "2025-10-02".into(),
            message: format!("Change {number}\n\nBecause of reason {number}."),
            patch,
        }
    }

    #[test]
    fn reads_the_history_of_lines() {
        let log = "\x1ea1b2c3d\x1fAnn\x1f2025-10-02\x1fFix it\n\nBecause.\n\x1f\n\n\
                   diff --git a/g.txt b/g.txt\n--- a/g.txt\n+++ b/g.txt\n@@ -2 +2 @@\n-b\n+B\n\
                   \x1e5d3fc96\x1fBo\x1f2025-01-01\x1fAdd it\n\x1f\n\ndiff --git a/f.txt b/f.txt\n";
        let history = parse_history(log);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].short, "a1b2c3d");
        assert_eq!(history[0].message, "Fix it\n\nBecause.");
        assert!(history[0].patch.starts_with("diff --git a/g.txt"));
        assert_eq!(history[1].author, "Bo");
    }

    #[test]
    fn sends_the_whole_history_when_it_fits() {
        let history = [change(3, 2), change(2, 2), change(1, 2)];
        let described = describe("intro\n", &history, true, 100_000);
        assert_eq!((described.left_out, described.cut), (0, 0));
        let text = &described.text;
        assert!(text.starts_with("intro\nHistory of the lines, newest first"));
        assert!(text.contains(
            "\ncommit c000003 by Ann, 2025-10-02\n    Change 3\n\n    Because of reason 3.\n\n\
             diff --git a/src/fit.rs b/src/fit.rs\n@@ -3,2 +3,2 @@ fn fit\n"
        ));
        assert!(text.find("c000003").unwrap() < text.find("c000001").unwrap());
        assert!(!text.contains("goes back further"));

        let none = describe("intro\n", &[], true, 1000);
        assert!(
            none.text
                .ends_with("git has no commits that changed these lines.\n")
        );
    }

    #[test]
    fn keeps_the_newest_and_oldest_commits_when_the_history_is_long() {
        let history: Vec<Change> = (1..=30).rev().map(|n| change(n, 40)).collect();
        let described = describe("intro\n", &history, false, 4000);
        let text = &described.text;
        assert!(text.len() <= 4000 + 200, "{} bytes", text.len());
        assert!(described.left_out > 0 && described.cut > 0);
        assert!(text.contains("commit c000030") && text.contains("commit c000001"));
        assert!(!text.contains("commit c000002"));
        let note = format!(
            "[… {} older commits that changed the lines left out to save space]\n\ncommit c000001",
            described.left_out
        );
        assert!(text.contains(&note));
        assert!(text.contains("more lines of this diff not shown]"));
        assert!(text.ends_with("[The history goes back further than these 30 commits.]\n"));
    }
}
