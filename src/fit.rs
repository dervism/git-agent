//! Shortens a diff to a size limit while keeping as much of the changes as it can.
//!
//! Header lines the model doesn't need are always dropped. If the diff is still
//! too large, the unchanged lines around the changes go next. If even that isn't
//! enough, the files share the room: files with small diffs are kept whole and
//! the larger ones split what is left, so every file shows at least its first
//! changes. Cutting the diff as a whole would drop every file after the first
//! few large ones.

use std::fmt::Write;

/// Least room worth giving a file: its header and a few changed lines.
const MIN_FILE_BYTES: usize = 400;
/// Room for the note that replaces the end of a shortened file.
const NOTE_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kept {
    Whole,
    /// The file's first changes.
    Start,
    /// Nothing: the file is only named.
    Nothing,
}

/// How much of one file's diff was kept.
#[derive(Debug, Clone, PartialEq)]
pub struct FileFit {
    pub path: String,
    pub kept: Kept,
    pub changed_lines: usize,
    pub kept_changed_lines: usize,
}

impl FileFit {
    pub fn is_whole(&self) -> bool {
        self.kept == Kept::Whole
    }

    pub fn is_dropped(&self) -> bool {
        self.kept == Kept::Nothing
    }
}

pub struct Fitted {
    pub text: String,
    pub files: Vec<FileFit>,
    /// Whether the unchanged lines around the changes were left out.
    pub context_trimmed: bool,
}

/// Shortens `diff` to at most `limit` bytes. Shortened files end with a note,
/// and files with no room left are named at the end.
pub fn fit(diff: &str, limit: usize) -> Fitted {
    let sections = split_files(diff);
    let paths: Vec<String> = sections.iter().map(|file| path(file)).collect();
    let mut files: Vec<String> = sections.iter().map(|file| compact_header(file)).collect();
    let size = |files: &[String]| files.iter().map(String::len).sum::<usize>();

    let mut context_trimmed = false;
    for keep in [1, 0] {
        if size(&files) <= limit {
            break;
        }
        files = files.iter().map(|file| trim_context(file, keep)).collect();
        context_trimmed = true;
    }
    let sizes: Vec<usize> = files.iter().map(String::len).collect();
    let shares = if size(&files) <= limit {
        sizes
    } else {
        // Keep room to name every file, in case some get no room at all.
        let names: usize = paths.iter().map(|path| path.len() + 2).sum();
        share(&sizes, limit.saturating_sub(names + NOTE_BYTES))
    };

    let mut text = String::new();
    let mut fits = Vec::new();
    for ((path, file), share) in paths.into_iter().zip(&files).zip(shares) {
        let changed_lines = count_changes(file);
        let (kept, part) = if share >= file.len() {
            (Kept::Whole, file.as_str())
        } else {
            match head_lines(file, share.saturating_sub(NOTE_BYTES)) {
                "" => (Kept::Nothing, ""),
                part => (Kept::Start, part),
            }
        };
        let kept_changed_lines = count_changes(part);
        text.push_str(part);
        if kept == Kept::Start {
            let rest = changed_lines - kept_changed_lines;
            let _ = writeln!(text, "[… {rest} more changed lines of this file not shown]");
        }
        fits.push(FileFit {
            path,
            kept,
            changed_lines,
            kept_changed_lines,
        });
    }
    let dropped: Vec<&str> = fits
        .iter()
        .filter(|file| file.is_dropped())
        .map(|file| file.path.as_str())
        .collect();
    if !dropped.is_empty() {
        let _ = writeln!(
            text,
            "[Diffs left out to save space: {}]",
            dropped.join(", ")
        );
    }
    Fitted {
        text,
        files: fits,
        context_trimmed,
    }
}

/// The whole lines at the start of `text` that fit in `limit` bytes.
pub fn head_lines(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    match text.as_bytes()[..limit]
        .iter()
        .rposition(|&byte| byte == b'\n')
    {
        Some(newline) => &text[..=newline],
        None => "",
    }
}

/// Splits a diff into one part per file. Each part starts with "diff --git".
fn split_files(diff: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = diff
        .match_indices("diff --git ")
        .map(|(index, _)| index)
        .filter(|&index| index == 0 || diff.as_bytes()[index - 1] == b'\n')
        .collect();
    if starts.first() != Some(&0) {
        starts.insert(0, 0);
    }
    let ends = starts.iter().skip(1).copied().chain([diff.len()]);
    starts
        .iter()
        .zip(ends)
        .map(|(&start, end)| &diff[start..end])
        .filter(|file| !file.is_empty())
        .collect()
}

/// The file's path from its "diff --git a/<path> b/<path>" header.
fn path(file: &str) -> String {
    let header = file.lines().next().unwrap_or_default();
    let names = header.strip_prefix("diff --git ").unwrap_or(header);
    let path = names
        .rsplit_once(" b/")
        .or_else(|| names.rsplit_once(" \"b/"))
        .map_or(names, |(_, path)| path.trim_end_matches('"'));
    path.to_owned()
}

/// Drops header lines the model doesn't need: the blob hashes on the `index`
/// line, and the `---` and `+++` lines that repeat the paths of the first line.
/// Lines such as "new file mode" and "rename from" stay.
fn compact_header(file: &str) -> String {
    let hunks = file.find("\n@@").map_or(file.len(), |newline| newline + 1);
    let (header, hunks) = file.split_at(hunks);
    let mut out: String = header
        .split_inclusive('\n')
        .filter(|line| {
            !["index ", "--- ", "+++ "]
                .iter()
                .any(|start| line.starts_with(start))
        })
        .collect();
    out.push_str(hunks);
    out
}

/// Keeps at most `keep` unchanged lines before and after each change.
fn trim_context(file: &str, keep: usize) -> String {
    let mut out = String::with_capacity(file.len());
    let mut unchanged: Vec<&str> = Vec::new();
    let mut in_hunk = false;
    let mut after_change = false;
    for line in file.split_inclusive('\n') {
        if in_hunk && line.starts_with(' ') {
            unchanged.push(line);
            continue;
        }
        let is_change = in_hunk && !line.starts_with("@@");
        keep_ends(&mut out, &unchanged, keep, after_change, is_change);
        unchanged.clear();
        if line.starts_with("@@") {
            in_hunk = true;
            after_change = false;
        } else if is_change {
            after_change = true;
        }
        out.push_str(line);
    }
    keep_ends(&mut out, &unchanged, keep, after_change, false);
    out
}

/// Adds the first `keep` lines of a run of unchanged lines if it follows a
/// change, and its last `keep` lines if a change follows it.
fn keep_ends(out: &mut String, run: &[&str], keep: usize, after_change: bool, before_change: bool) {
    let head = if after_change { keep.min(run.len()) } else { 0 };
    let tail = if before_change {
        keep.min(run.len() - head)
    } else {
        0
    };
    for line in run[..head].iter().chain(&run[run.len() - tail..]) {
        out.push_str(line);
    }
}

/// Number of added and removed lines in a diff whose `---`/`+++` lines are gone.
fn count_changes(diff: &str) -> usize {
    diff.lines()
        .filter(|line| line.starts_with('+') || line.starts_with('-'))
        .count()
}

/// Splits `limit` bytes between parts of the given sizes. Parts smaller than an
/// even share get all they need and the others split what is left evenly. When
/// an even share would be too small to be useful, the smallest parts get
/// `MIN_FILE_BYTES` each until the room runs out, and the rest get nothing.
fn share(sizes: &[usize], limit: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by_key(|&index| sizes[index]);
    let mut shares = vec![0; sizes.len()];
    let mut left = limit;
    for (done, &index) in order.iter().enumerate() {
        let even = left / (sizes.len() - done);
        let want = sizes[index].min(even.max(MIN_FILE_BYTES));
        if want <= left {
            shares[index] = want;
            left -= want;
        }
    }
    shares
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file diff with `changes` added lines, each between two unchanged ones.
    fn file(path: &str, changes: usize) -> String {
        let mut diff = format!(
            "diff --git a/{path} b/{path}\nindex 1111111..2222222 100644\n\
             --- a/{path}\n+++ b/{path}\n@@ -1,{changes} +1,{changes} @@ fn main\n"
        );
        for line in 0..changes {
            let _ = writeln!(diff, " unchanged line before change {line}");
            let _ = writeln!(
                diff,
                "+line {line} of {path}, long enough to take some room"
            );
        }
        diff.push_str(" the last unchanged line\n");
        diff
    }

    #[test]
    fn keeps_a_diff_that_fits_without_its_hashes() {
        let diff = file("a.rs", 3) + &file("dir/b c.rs", 2);
        let fitted = fit(&diff, diff.len());
        assert!(!fitted.context_trimmed);
        assert_eq!(fitted.files.len(), 2);
        assert_eq!(fitted.files[1].path, "dir/b c.rs");
        assert_eq!(fitted.files[1].changed_lines, 2);
        assert!(fitted.files.iter().all(FileFit::is_whole));
        assert!(
            fitted
                .text
                .contains("diff --git a/a.rs b/a.rs\n@@ -1,3 +1,3 @@ fn main\n")
        );
        assert_eq!(fitted.text.lines().count(), diff.lines().count() - 6);
    }

    #[test]
    fn compacts_headers_but_keeps_what_happened_to_the_file() {
        let new_file = "diff --git a/n.rs b/n.rs\nnew file mode 100644\nindex 0000000..1111111\n\
                        --- /dev/null\n+++ b/n.rs\n@@ -0,0 +1 @@\n+fn n() {}\n--- a removed line\n";
        assert_eq!(
            compact_header(new_file),
            "diff --git a/n.rs b/n.rs\nnew file mode 100644\n@@ -0,0 +1 @@\n+fn n() {}\n\
             --- a removed line\n"
        );
    }

    #[test]
    fn trims_unchanged_lines() {
        let hunk = "@@ -1,9 +1,9 @@\n a\n b\n c\n-old\n+new\n d\n e\n f\n g\n-x\n h\n";
        assert_eq!(
            trim_context(hunk, 1),
            "@@ -1,9 +1,9 @@\n c\n-old\n+new\n d\n g\n-x\n h\n"
        );
        assert_eq!(trim_context(hunk, 0), "@@ -1,9 +1,9 @@\n-old\n+new\n-x\n");
    }

    #[test]
    fn leaves_out_unchanged_lines_before_cutting_files() {
        let diff = file("a.rs", 20);
        let without_context = trim_context(&compact_header(&diff), 0).len();
        let fitted = fit(&diff, without_context);
        assert!(fitted.context_trimmed);
        assert!(fitted.files[0].is_whole());
        assert!(!fitted.text.contains("unchanged"));
    }

    #[test]
    fn shares_the_room_between_files() {
        let diff = file("big.rs", 200) + &file("small.rs", 2) + &file("medium.rs", 50);
        let fitted = fit(&diff, 4000);
        assert!(fitted.text.len() <= 4000);
        let [big, small, medium] = fitted.files.as_slice() else {
            panic!("expected three files");
        };
        assert!(small.is_whole());
        // The two larger files are both shortened rather than one dropped.
        assert_eq!(big.kept, Kept::Start);
        assert_eq!(medium.kept, Kept::Start);
        assert!(big.kept_changed_lines > 0 && big.kept_changed_lines < big.changed_lines);
        let note = format!(
            "[… {} more changed lines of this file not shown]",
            big.changed_lines - big.kept_changed_lines
        );
        assert!(fitted.text.contains(&note));
        // Files stay in git's order.
        let order: Vec<_> = ["big.rs", "small.rs", "medium.rs"]
            .iter()
            .map(|path| fitted.text.find(&format!("a/{path} b/{path}")).unwrap())
            .collect();
        assert!(order.is_sorted());
    }

    #[test]
    fn names_files_that_get_no_room() {
        let diff: String = (0..20).map(|n| file(&format!("f{n}.rs"), 30)).collect();
        let fitted = fit(&diff, 2000);
        assert!(fitted.text.len() <= 2000);
        let dropped: Vec<_> = fitted.files.iter().filter(|f| f.is_dropped()).collect();
        assert!(!dropped.is_empty());
        let last = &dropped.last().unwrap().path;
        assert!(fitted.text.contains(&format!("{last}]")));
    }

    #[test]
    fn cuts_at_line_breaks() {
        assert_eq!(head_lines("aaa\nbbb\nccc\n", 9), "aaa\nbbb\n");
        assert_eq!(head_lines("aaa\nbbb\n", 100), "aaa\nbbb\n");
        assert_eq!(head_lines("ææææ\n", 3), "");
    }

    #[test]
    fn shares_never_exceed_the_limit() {
        for limit in [0, 100, 1000, 5000] {
            let shares = share(&[50, 3000, 800, 10, 2000], limit);
            assert!(shares.iter().sum::<usize>() <= limit);
        }
        assert_eq!(share(&[100, 5000, 5000], 2100), [100, 1000, 1000]);
    }
}
