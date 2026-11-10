//! git's diff text: cutting it into one part per file, and taking the lines out of a file's
//! header that only repeat what its first line says or name blobs.

/// Whether `line` starts a file's part of a diff, a merge's combined diff included.
pub fn is_file_header(line: &str) -> bool {
    ["diff --git ", "diff --cc ", "diff --combined "]
        .iter()
        .any(|start| line.starts_with(start))
}

/// Splits a diff into one part per file. Each part starts with "diff --git".
pub fn split_files(diff: &str) -> Vec<&str> {
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

/// Takes out the `index` line, with its blob hashes, and the `---` and `+++` lines, which
/// repeat the paths of the first line.
/// Lines such as "new file mode" and "rename from" stay.
pub fn compact_header(file: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn splits_a_diff_at_each_file() {
        let first = "diff --git a/a b/a\n@@ -1 +1 @@\n-a\n+b\n";
        let second = "diff --git a/c d/b/c d\nnew file mode 100644\n";
        assert_eq!(split_files(&format!("{first}{second}")), [first, second]);
        assert_eq!(split_files(first), [first]);
        assert!(split_files("").is_empty());
    }

    #[test]
    fn splits_only_where_a_line_starts_a_file() {
        let file = "diff --git a/a b/a\n@@ -1 +1 @@\n+text with diff --git a/x b/x in it\n";
        assert_eq!(split_files(file), [file]);
    }

    #[test]
    fn keeps_what_comes_before_the_first_file() {
        let diff = "a note\ndiff --git a/a b/a\n+x\n";
        assert_eq!(split_files(diff), ["a note\n", "diff --git a/a b/a\n+x\n"]);
    }

    #[test]
    fn leaves_a_header_without_hunks_as_it_is_but_for_its_hashes() {
        let renamed = "diff --git a/a b/b\nsimilarity index 100%\nrename from a\nrename to b\n";
        assert_eq!(compact_header(renamed), renamed);
        let mode = "diff --git a/a b/a\nold mode 100644\nnew mode 100755\nindex 1..2\n";
        assert_eq!(
            compact_header(mode),
            "diff --git a/a b/a\nold mode 100644\nnew mode 100755\n"
        );
    }
}
