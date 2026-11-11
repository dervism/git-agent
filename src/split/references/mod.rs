//! The order of a new plan's commits by the new files they refer to.
//!
//! A model groups changes by what they are for, and can't see that the commit using
//! `crate::safe_text` can't build before the commit that adds `src/safe_text.rs`. The diff
//! shows both. A commit comes after the commit that adds a file it refers to, in one of
//! two ways:
//!
//! - In Rust files, by the name of a module in a path position of an added line (see
//!   [`rust`]). An added `mod safe_text;` refers to the module's file too, and it declares
//!   the module when the plan adds the file, so a use of the module comes after the
//!   declaration, and the declaration after the file.
//! - In any file, by the full name of a new file, with its extension, as a whole word of an
//!   added line, such as `include_str!("guide.md")`. A name that another file has already,
//!   in HEAD or among the plan's new files, says nothing about which file is meant.
//!
//! A new Rust file isn't compiled before a commit declares its module, so its own
//! references count for that commit, when a commit of the plan declares it. Otherwise they
//! count for the commit that adds the file, as do those of the files Cargo compiles
//! without a declaration, such as `src/main.rs` and `tests/*.rs`.
//!
//! Documentation and excluded files aren't read. Only whole commits move, so the model's
//! commits and their titles stay, apart from commits that refer to each other's new files,
//! which become one. This runs after [`kinds::docs_last`], and moves as little as it can,
//! so documentation stays last unless code refers to a new documentation file.

mod order;
mod rust;

use super::{FilePatch, Hunk, Line, Plan, Planned, Unit, became_one, combined, kinds};
use order::in_order;
use rust::{declared_modules, is_crate_root, is_rust, module_name, references};
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

/// A plan in the order its commits need, and what had to be merged to get there.
#[derive(Debug, PartialEq)]
pub struct OrderedPlan {
    pub plan: Plan,
    /// The commits that became one, for the user.
    pub notes: Vec<String>,
}

/// The names, with extension, of the new files among `files`: the names that a line of the
/// plan can mention to refer to a file.
pub fn candidate_file_names(files: &[FilePatch]) -> HashSet<String> {
    files
        .iter()
        .filter_map(|file| arrived(file, name_with_extension))
        .map(Cow::into_owned)
        .collect()
}

/// The plan with each commit after the commits that add the new files it refers to. A
/// commit moves only as far as it must: the commit it needs comes right before it, and the
/// rest keep the model's order. Commits that need each other's files become one commit.
/// `names_in_head` are the names of the files HEAD has that are among
/// [`candidate_file_names`].
pub fn new_files_first(
    plan: Plan,
    files: &[FilePatch],
    units: &[Unit],
    names_in_head: &HashSet<String>,
) -> OrderedPlan {
    let commits: Vec<CommitFiles> = plan
        .commits
        .iter()
        .map(|commit| CommitFiles::of(commit, files, units))
        .collect();
    let groups = in_order(&must_follow(&commits, names_in_head));
    let planned = |group: &[usize]| -> Vec<&Planned> {
        group.iter().map(|&number| &plan.commits[number]).collect()
    };
    OrderedPlan {
        notes: groups
            .iter()
            .filter(|group| group.len() > 1)
            .map(|group| became_one(&planned(group)))
            .collect(),
        plan: Plan {
            commits: groups
                .iter()
                .map(|group| combined(&planned(group)))
                .collect(),
        },
    }
}

/// The files a commit adds, and the lines it adds that can refer to files.
struct CommitFiles<'a> {
    /// The Rust modules whose files it adds.
    modules: HashSet<Cow<'a, str>>,
    /// The modules its added `mod X;` lines declare.
    declared: HashSet<&'a str>,
    /// The names, with extension, of the files it adds.
    file_names: Vec<Cow<'a, str>>,
    /// Its parts of files that aren't documentation or excluded.
    read: Vec<(&'a FilePatch, &'a [Hunk])>,
}

impl<'a> CommitFiles<'a> {
    fn of(commit: &Planned, files: &'a [FilePatch], units: &[Unit]) -> Self {
        let parts: Vec<(&FilePatch, &[Hunk])> = commit
            .units
            .iter()
            .map(|&unit| (&files[units[unit].file], units[unit]))
            .map(|(file, unit)| (file, file.hunks_of(unit)))
            .collect();
        let read: Vec<(&FilePatch, &[Hunk])> = parts
            .iter()
            .filter(|(file, _)| !file.excluded && !kinds::is_documentation(file))
            .copied()
            .collect();
        Self {
            modules: parts
                .iter()
                .filter_map(|(file, _)| arrived(file, module_name))
                .collect(),
            declared: rust_hunks(&read)
                .flat_map(|hunk| declared_modules(hunk.lines()))
                .collect(),
            file_names: parts
                .iter()
                .filter_map(|(file, _)| arrived(file, name_with_extension))
                .collect(),
            read,
        }
    }

    /// The names of modules its added Rust lines refer to, apart from those of new files
    /// that wait for a declaration.
    fn referred_modules(&self, declarations: &Index<'_>) -> HashSet<&'a str> {
        self.read
            .iter()
            .filter(|(file, _)| is_rust(&file.path) && !waits_for_declaration(file, declarations))
            .flat_map(|(_, hunks)| hunks.iter())
            .flat_map(hunk_references)
            .collect()
    }

    /// The new files that wait for a declaration, each with its module's name and what it
    /// refers to.
    fn waiting_references(&self, declarations: &Index<'_>) -> Vec<(Cow<'a, str>, Vec<&'a str>)> {
        self.read
            .iter()
            .filter(|(file, _)| is_rust(&file.path) && waits_for_declaration(file, declarations))
            .filter_map(|(file, hunks)| {
                let references = hunks.iter().flat_map(hunk_references).collect();
                Some((module_name(&file.path)?, references))
            })
            .collect()
    }

    /// The names in `known` that its added lines mention as a whole word.
    fn mentioned_file_names(&self, known: &Index<'_>) -> HashSet<&'a str> {
        self.read
            .iter()
            .flat_map(|(_, hunks)| hunks.iter())
            .flat_map(added_lines)
            .flat_map(words_of_a_file_name)
            .filter(|word| known.contains_key(*word))
            .collect()
    }
}

fn hunk_references(hunk: &Hunk) -> Vec<&str> {
    references(hunk.lines())
}

/// What the new files that wait for a declaration refer to, by their module's name.
type WaitingReferences<'a> = HashMap<Cow<'a, str>, HashSet<&'a str>>;

/// Whether `file` is a new Rust file that isn't compiled before a commit of the plan declares
/// its module.
fn waits_for_declaration(file: &FilePatch, declarations: &Index<'_>) -> bool {
    file.status == Some("new file")
        && !is_crate_root(&file.path)
        && module_name(&file.path).is_some_and(|module| declarations.contains_key(&*module))
}

/// The hunks of the Rust files among `parts`.
fn rust_hunks<'a, 'p>(
    parts: &'p [(&'a FilePatch, &'a [Hunk])],
) -> impl Iterator<Item = &'a Hunk> + 'p {
    parts
        .iter()
        .filter(|(file, _)| is_rust(&file.path))
        .flat_map(|(_, hunks)| hunks.iter())
}

/// What `file` gives its commit to add under `name_of`: the name of a new file, or of a
/// renamed or copied one when its source had another name.
fn arrived(file: &FilePatch, name_of: fn(&str) -> Option<Cow<'_, str>>) -> Option<Cow<'_, str>> {
    let name = name_of(&file.path)?;
    match file.status {
        Some("new file") => Some(name),
        Some("renamed" | "copied") => {
            let source = file.source();
            let known_before = source.as_deref().and_then(name_of).as_ref() == Some(&name);
            (!known_before).then_some(name)
        }
        _ => None,
    }
}

/// The name of the file at `path`, if it has an extension.
fn name_with_extension(path: &str) -> Option<Cow<'_, str>> {
    let name = Path::new(path).file_name()?.to_str()?;
    name.chars()
        .skip(1)
        .any(|c| c == '.')
        .then_some(Cow::Borrowed(name))
}

fn added_lines(hunk: &Hunk) -> impl Iterator<Item = &str> {
    hunk.lines().filter_map(|line| match line {
        Line::Added(text) => Some(text),
        Line::Kept(_) => None,
    })
}

/// The words of a line that a file name can be, such as `guide.md` in `include_str!("guide.md")`.
fn words_of_a_file_name(line: &str) -> impl Iterator<Item = &str> {
    line.split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .map(|word| word.trim_end_matches('.'))
        .filter(|word| !word.is_empty())
}

// ---- Who needs whom --------------------------------------------------------------------

/// For each name, the commits that add it, by number.
type Index<'a> = HashMap<Cow<'a, str>, Vec<usize>>;

/// Which commits add the names that lines can refer to.
#[derive(Default)]
struct Providers<'a> {
    module_files: Index<'a>,
    /// The modules the plan adds the file of, and the commits that declare them.
    declarations: Index<'a>,
    file_names: Index<'a>,
}

impl<'a> Providers<'a> {
    fn of(commits: &[CommitFiles<'a>], names_in_head: &HashSet<String>) -> Self {
        let mut providers = Self::default();
        for (number, commit) in commits.iter().enumerate() {
            add_to(
                &mut providers.module_files,
                commit.modules.iter().cloned(),
                number,
            );
            add_to(
                &mut providers.declarations,
                commit.declared.iter().map(|&name| Cow::Borrowed(name)),
                number,
            );
        }
        let Self {
            module_files,
            declarations,
            ..
        } = &mut providers;
        declarations.retain(|module, _| module_files.contains_key(module));
        providers.file_names = unique_file_names(commits, names_in_head);
        providers
    }

    /// The commits that `using`, the commit number `user`, has to come after.
    fn commits_before(
        &self,
        user: usize,
        using: &CommitFiles<'a>,
        waiting: &WaitingReferences<'a>,
    ) -> BTreeSet<usize> {
        let mut referred = using.referred_modules(&self.declarations);
        for module in &using.declared {
            referred.extend(waiting.get(*module).into_iter().flatten());
        }
        let module_files = referred
            .iter()
            .chain(&using.declared)
            .filter(|name| !using.modules.contains(**name))
            .flat_map(|name| adders(&self.module_files, name));
        let declarations = referred
            .difference(&using.declared)
            .flat_map(|name| adders(&self.declarations, name));
        let file_names = using
            .mentioned_file_names(&self.file_names)
            .into_iter()
            .flat_map(|name| adders(&self.file_names, name));
        module_files
            .chain(declarations)
            .chain(file_names)
            .filter(|&adder| adder != user)
            .collect()
    }
}

fn add_to<'a>(index: &mut Index<'a>, names: impl Iterator<Item = Cow<'a, str>>, commit: usize) {
    for name in names {
        index.entry(name).or_default().push(commit);
    }
}

/// The file names that one new file of the plan has, and no file in HEAD has: the names that
/// mean one file when a line says them.
fn unique_file_names<'a>(
    commits: &[CommitFiles<'a>],
    names_in_head: &HashSet<String>,
) -> Index<'a> {
    let mut index = Index::new();
    for (number, commit) in commits.iter().enumerate() {
        add_to(&mut index, commit.file_names.iter().cloned(), number);
    }
    index.retain(|name, adders| adders.len() == 1 && !names_in_head.contains(name.as_ref()));
    index
}

fn adders<'i>(index: &'i Index<'_>, name: &str) -> impl Iterator<Item = usize> + 'i {
    index.get(name).into_iter().flatten().copied()
}

/// For each commit, the commits it has to come after.
fn must_follow(commits: &[CommitFiles], names_in_head: &HashSet<String>) -> Vec<BTreeSet<usize>> {
    let providers = Providers::of(commits, names_in_head);
    let mut waiting = WaitingReferences::new();
    let waiting_files = commits
        .iter()
        .flat_map(|commit| commit.waiting_references(&providers.declarations));
    for (module, references) in waiting_files {
        waiting.entry(module).or_default().extend(references);
    }
    commits
        .iter()
        .enumerate()
        .map(|(user, using)| providers.commits_before(user, using, &waiting))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_file(path: &str, lines: &[&str]) -> String {
        let added: String = lines.iter().map(|line| format!("+{line}\n")).collect();
        format!(
            "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n\
             @@ -0,0 +1,{} @@\n{added}",
            lines.len()
        )
    }

    fn binary_file(path: &str) -> String {
        format!(
            "diff --git a/{path} b/{path}\nnew file mode 100644\nindex 0000000..e69de29\n\
             Binary files /dev/null and b/{path} differ\n"
        )
    }

    /// A change to a file that has the line `kept`, which removes and adds lines.
    fn edited(path: &str, removed: &[&str], added: &[&str]) -> String {
        let removed: String = removed.iter().map(|line| format!("-{line}\n")).collect();
        let added: String = added.iter().map(|line| format!("+{line}\n")).collect();
        format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,9 +1,9 @@\n kept\n\
             {removed}{added}"
        )
    }

    /// A file moved or copied, as git writes it, with `names` for its two names in the
    /// "diff --git" line and `from` and `to` for the lines that say them.
    fn moved(names: &str, word: &str, (from, to): (&str, &str), lines: &[&str]) -> String {
        let added: String = lines.iter().map(|line| format!("+{line}\n")).collect();
        format!(
            "diff --git {names}\nsimilarity index 80%\n{word} from {from}\n{word} to {to}\n\
             --- a/x\n+++ b/y\n@@ -1 +1 @@\n-old\n{added}"
        )
    }

    fn renamed(from: &str, to: &str, word: &str, lines: &[&str]) -> String {
        let names = format!("a/{from} b/{to}");
        moved(&names, word, (from, to), lines)
    }

    /// The files of `patches`, all shown to the model.
    fn files_of(patches: &[String]) -> Vec<FilePatch> {
        let text = patches.concat();
        let paths = crate::split::parse(&text, &HashSet::new())
            .into_iter()
            .map(|file| file.path)
            .collect();
        crate::split::parse(&text, &paths)
    }

    fn each_file_a_unit(files: &[FilePatch]) -> Vec<Unit> {
        crate::split::units(files, false)
    }

    fn plan(commits: &[(&str, &[usize])]) -> Plan {
        Plan {
            commits: commits
                .iter()
                .map(|(title, units)| Planned {
                    title: (*title).to_owned(),
                    units: units.to_vec(),
                })
                .collect(),
        }
    }

    fn ordered(patches: &[String], plan: Plan) -> OrderedPlan {
        let files = files_of(patches);
        new_files_first(plan, &files, &each_file_a_unit(&files), &HashSet::new())
    }

    /// The titles of the commits of `plan` in the order they would be made.
    fn order_of(patches: &[String], plan: Plan) -> Vec<String> {
        let ordered = ordered(patches, plan);
        ordered
            .plan
            .titles()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    /// The pattern a split of 42 files had: a commit using a module, one declaring it, and
    /// the commits adding its file, in that order.
    fn rust_change() -> Vec<String> {
        vec![
            edited("src/split.rs", &[], &["use crate::safe_text::escaped;"]),
            edited("src/main.rs", &[], &["mod safe_text;", "mod staging;"]),
            new_file("src/safe_text.rs", &["pub fn escaped() {}"]),
            new_file("src/staging.rs", &["pub fn stage() {}"]),
        ]
    }

    #[test]
    fn puts_the_file_first_then_the_declaration_and_then_the_use() {
        let patches = vec![
            edited("src/report.rs", &[], &["use crate::safe_text::escaped;"]),
            edited("src/main.rs", &[], &["mod safe_text;"]),
            new_file("src/safe_text.rs", &["pub fn escaped() {}"]),
        ];
        let model = plan(&[("Use", &[0]), ("Declare", &[1]), ("Add file", &[2])]);
        assert_eq!(order_of(&patches, model), ["Add file", "Declare", "Use"]);
    }

    #[test]
    fn orders_the_failing_pattern_of_a_real_split() {
        let model = plan(&[
            ("Use the escapes", &[0]),
            ("Declare the modules", &[1]),
            ("Add safe_text", &[2]),
            ("Add staging", &[3]),
        ]);
        let ordered = ordered(&rust_change(), model);
        assert_eq!(
            ordered.plan.titles(),
            [
                "Add safe_text",
                "Add staging",
                "Declare the modules",
                "Use the escapes"
            ]
        );
        assert!(ordered.notes.is_empty());
    }

    #[test]
    fn needs_no_declaration_when_the_commit_declares_the_module_itself() {
        let patches = vec![
            edited(
                "src/main.rs",
                &[],
                &["mod safe_text;", "use crate::safe_text::escaped;"],
            ),
            new_file("src/safe_text.rs", &["pub fn escaped() {}"]),
            edited("src/other.rs", &[], &["mod safe_text;"]),
        ];
        let model = plan(&[("Both", &[0]), ("Other", &[2]), ("Add", &[1])]);
        assert_eq!(order_of(&patches, model), ["Add", "Both", "Other"]);
    }

    #[test]
    fn moves_a_new_file_only_to_just_before_the_first_commit_that_needs_it() {
        let wrong = plan(&[
            ("one", &[4]),
            ("Declare the module", &[1]),
            ("two", &[5]),
            ("Add safe_text", &[2]),
            ("three", &[6]),
        ]);
        let mut patches = rust_change();
        patches.extend([
            new_file("one.txt", &["1"]),
            new_file("two.txt", &["2"]),
            new_file("three.txt", &["3"]),
        ]);
        assert_eq!(
            order_of(&patches, wrong),
            ["one", "Add safe_text", "Declare the module", "two", "three"]
        );
    }

    #[test]
    fn keeps_every_unit_once_and_leaves_no_commit_empty() {
        let wrong = plan(&[
            ("Use the escapes", &[0]),
            ("Declare the modules", &[1]),
            ("Add both", &[2, 3]),
        ]);
        let ordered = ordered(&rust_change(), wrong);
        let mut units: Vec<usize> = ordered
            .plan
            .commits
            .iter()
            .flat_map(|commit| commit.units.clone())
            .collect();
        units.sort_unstable();
        assert_eq!(units, [0, 1, 2, 3]);
        assert!(
            ordered
                .plan
                .commits
                .iter()
                .all(|commit| !commit.units.is_empty())
        );
        assert_eq!(
            ordered.plan.titles(),
            ["Add both", "Declare the modules", "Use the escapes"]
        );
    }

    #[test]
    fn leaves_a_plan_alone_when_no_commit_refers_to_a_new_file() {
        let patches = vec![
            edited("src/main.rs", &[], &["fn main() {}"]),
            new_file("src/lonely.rs", &["pub fn alone() {}"]),
            edited("src/other.rs", &[], &["let x = 1;"]),
        ];
        let unchanged = plan(&[("Other", &[2]), ("Lonely", &[1]), ("Main", &[0])]);
        let ordered = ordered(&patches, unchanged.clone());
        assert_eq!(ordered.plan, unchanged);
        assert!(ordered.notes.is_empty());
    }

    #[test]
    fn keeps_the_models_order_of_commits_that_depend_on_nothing() {
        let patches: Vec<String> = (0..6)
            .map(|number| new_file(&format!("file{number}.txt"), &["text"]))
            .collect();
        let shuffled = plan(&[
            ("c", &[2]),
            ("a", &[0]),
            ("f", &[5]),
            ("b", &[1]),
            ("e", &[4]),
            ("d", &[3]),
        ]);
        assert_eq!(order_of(&patches, shuffled), ["c", "a", "f", "b", "e", "d"]);
    }

    #[test]
    fn doesnt_join_a_use_statement_over_two_hunks() {
        let two_hunks = "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n\
                         +++ b/src/main.rs\n@@ -1 +1 @@\n-a\n+use crate::{\n\
                         @@ -20 +20 @@\n-b\n+    safe_text,\n";
        let patches = vec![
            two_hunks.to_owned(),
            new_file("src/safe_text.rs", &["pub fn escaped() {}"]),
        ];
        let model = plan(&[("Use", &[0]), ("Add", &[1])]);
        assert_eq!(order_of(&patches, model), ["Use", "Add"]);
    }

    #[test]
    fn reads_only_added_lines_and_not_those_kept_or_removed() {
        let removed = edited("src/main.rs", &["use crate::safe_text;"], &["fn main() {}"]);
        let kept = edited("src/main.rs", &[], &[]).replace(" kept", " use crate::safe_text;");
        for patch in [removed, kept] {
            let patches = vec![
                patch,
                new_file("src/safe_text.rs", &["pub fn escaped() {}"]),
            ];
            let model = plan(&[("Use", &[0]), ("Add", &[1])]);
            assert_eq!(order_of(&patches, model), ["Use", "Add"]);
        }
    }

    #[test]
    fn reads_only_the_hunks_a_commit_holds() {
        let two_hunks = "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n\
                         +++ b/src/main.rs\n@@ -1 +1 @@\n-a\n+fn first() {}\n\
                         @@ -20 +20 @@\n-b\n+mod safe_text;\n";
        let patches = vec![
            two_hunks.to_owned(),
            new_file("src/safe_text.rs", &["pub fn escaped() {}"]),
        ];
        let files = files_of(&patches);
        let units = crate::split::units(&files, true);
        assert_eq!(units.len(), 3);
        let model = plan(&[("First", &[0]), ("Module", &[1]), ("Add", &[2])]);
        let ordered = new_files_first(model, &files, &units, &HashSet::new());
        assert_eq!(ordered.plan.titles(), ["First", "Add", "Module"]);
    }

    #[test]
    fn counts_a_new_file_in_a_subfolder_and_a_folders_own_file() {
        for (added, mention) in [
            (
                "src/deep/er/safe_text.rs",
                "use crate::deep::er::safe_text;",
            ),
            ("src/safe_text/mod.rs", "use crate::safe_text::Staged;"),
            ("crates/safe_text/src/lib.rs", "use safe_text::Staged;"),
        ] {
            let patches = vec![
                edited("src/main.rs", &[], &[mention]),
                new_file(added, &["body"]),
            ];
            let model = plan(&[("Use", &[0]), ("Add", &[1])]);
            assert_eq!(order_of(&patches, model), ["Add", "Use"], "{added}");
        }
    }

    #[test]
    fn leaves_the_imports_of_other_languages_to_their_file_names() {
        let patches = vec![
            edited(
                "web/main.ts",
                &[],
                &["import { Button } from \"./widgets\";"],
            ),
            new_file("web/widgets/index.ts", &["export {}"]),
            new_file("web/widgets.ts", &["export {}"]),
        ];
        let model = plan(&[("Use", &[0]), ("Index", &[1]), ("File", &[2])]);
        assert_eq!(order_of(&patches, model), ["Use", "Index", "File"]);
    }

    #[test]
    fn counts_a_new_file_by_its_full_name_in_any_file() {
        for (path, mention, added) in [
            (
                "src/main.rs",
                "const TEXT: &str = include_str!(\"guide.md\");",
                "docs/guide.md",
            ),
            (
                "src/main.rs",
                "include_bytes!(\"../assets/logo.png\")",
                "assets/logo.png",
            ),
            (
                "site/index.astro",
                "import Terminal from '../components/Terminal.astro';",
                "site/components/Terminal.astro",
            ),
            (
                "src/term.c",
                "#include \"terminal.h\"",
                "include/terminal.h",
            ),
            (
                "site/page.mjs",
                "const text = fs.readFileSync('data.json');",
                "site/data.json",
            ),
            ("web/app.ts", "// notes are in guide.md.", "notes/guide.md"),
        ] {
            let binary = added.ends_with(".png");
            let file = if binary {
                binary_file(added)
            } else {
                new_file(added, &["text"])
            };
            let patches = vec![edited(path, &[], &[mention]), file];
            let model = plan(&[("Use", &[0]), ("Add", &[1])]);
            assert_eq!(order_of(&patches, model), ["Add", "Use"], "{mention}");
        }
    }

    #[test]
    fn counts_a_file_name_only_as_a_whole_word_with_its_extension() {
        for mention in [
            "include_str!(\"myguide.md\");",
            "include_str!(\"guide.md.bak\");",
            "include_str!(\"guide.mdx\");",
            "include_str!(\"guide_md\");",
            "see the guide",
            "include_str!(\"GUIDE.md\");",
        ] {
            let patches = vec![
                edited("src/main.rs", &[], &[mention]),
                new_file("docs/guide.md", &["text"]),
            ];
            let model = plan(&[("Use", &[0]), ("Add", &[1])]);
            assert_eq!(order_of(&patches, model), ["Use", "Add"], "{mention}");
        }
    }

    #[test]
    fn needs_a_file_name_to_have_an_extension_and_a_common_word_isnt_one() {
        for (added, mention) in [
            ("LICENSE", "see LICENSE"),
            ("docs/config.md", "let config = Config::new();"),
            ("docs/config.md", "// the config"),
            (".gitignore", "x = .gitignore"),
        ] {
            let patches = vec![
                edited("src/main.rs", &[], &[mention]),
                new_file(added, &["text"]),
            ];
            let model = plan(&[("Use", &[0]), ("Add", &[1])]);
            assert_eq!(order_of(&patches, model), ["Use", "Add"], "{added}");
        }
    }

    #[test]
    fn matches_words_that_arent_ascii_as_whole_words() {
        let patches = [
            edited("src/main.rs", &[], &["use crate::café;"]),
            new_file("src/caf.rs", &["pub fn a() {}"]),
            new_file("src/café.rs", &["pub fn b() {}"]),
            edited("src/other.rs", &[], &["include_str!(\"héllo.md\");"]),
            new_file("docs/llo.md", &["text"]),
            new_file("docs/héllo.md", &["text"]),
        ];
        assert_eq!(
            order_of(
                &patches[..2],
                plan(&[("Use café", &[0]), ("Add caf", &[1])])
            ),
            ["Use café", "Add caf"]
        );
        assert_eq!(
            order_of(
                &patches[..3],
                plan(&[("Use café", &[0]), ("Add caf", &[1]), ("Add café", &[2])])
            ),
            ["Add café", "Use café", "Add caf"]
        );
        let docs = vec![patches[3].clone(), patches[4].clone(), patches[5].clone()];
        assert_eq!(
            order_of(
                &docs,
                plan(&[("Use", &[0]), ("Add llo", &[1]), ("Add héllo", &[2])])
            ),
            ["Add héllo", "Use", "Add llo"]
        );
    }

    #[test]
    fn doesnt_read_documentation_or_files_the_model_doesnt_see() {
        let patches = [
            edited("README.md", &[], &["use crate::safe_text; see guide.md"]),
            edited("Cargo.lock", &[], &["use crate::safe_text;"]),
            new_file("src/safe_text.rs", &["pub fn escaped() {}"]),
            new_file("docs/guide.md", &["text"]),
        ];
        let shown: HashSet<String> = ["README.md", "src/safe_text.rs", "docs/guide.md"]
            .map(str::to_owned)
            .into();
        let files = crate::split::parse(&patches.concat(), &shown);
        let units = each_file_a_unit(&files);
        let model = plan(&[
            ("Readme", &[0]),
            ("Lock", &[1]),
            ("Add", &[2]),
            ("Guide", &[3]),
        ]);
        let ordered = new_files_first(model, &files, &units, &HashSet::new());
        assert_eq!(ordered.plan.titles(), ["Readme", "Lock", "Add", "Guide"]);
    }

    #[test]
    fn takes_a_renamed_file_for_documentation_by_its_new_name() {
        let patches = vec![
            renamed(
                "notes.txt",
                "docs/guide.md",
                "rename",
                &["Uses crate::safe_text and guide.md"],
            ),
            new_file("src/safe_text.rs", &["pub fn escaped() {}"]),
        ];
        let model = plan(&[("Guide", &[0]), ("Add", &[1])]);
        assert_eq!(order_of(&patches, model), ["Guide", "Add"]);
        let from_docs = vec![
            renamed(
                "docs/guide.md",
                "src/code.rs",
                "rename",
                &["use crate::safe_text;"],
            ),
            new_file("src/safe_text.rs", &["pub fn escaped() {}"]),
        ];
        let model = plan(&[("Code", &[0]), ("Add", &[1])]);
        assert_eq!(order_of(&from_docs, model), ["Add", "Code"]);
    }

    #[test]
    fn keeps_documentation_last_after_docs_last_except_for_a_file_code_includes() {
        let patches = vec![
            edited("README.md", &[], &["new text"]),
            edited("src/main.rs", &[], &["include_str!(\"guide.md\");"]),
            new_file("src/guide.md", &["# Guide"]),
            new_file("src/other.rs", &["fn other() {}"]),
        ];
        let files = files_of(&patches);
        let units = each_file_a_unit(&files);
        let model = plan(&[
            ("Readme", &[0]),
            ("Use", &[1]),
            ("Guide", &[2]),
            ("Other", &[3]),
        ]);
        let docs_last = kinds::docs_last(model, &files, &units);
        assert_eq!(docs_last.titles(), ["Use", "Other", "Readme", "Guide"]);
        let ordered = new_files_first(docs_last, &files, &units, &HashSet::new());
        assert_eq!(ordered.plan.titles(), ["Guide", "Use", "Other", "Readme"]);
    }

    #[test]
    fn counts_a_renamed_file_as_new_under_a_name_its_source_didnt_have() {
        for word in ["rename", "copy"] {
            let patches = vec![
                edited("src/main.rs", &[], &["mod new_name;"]),
                renamed(
                    "src/old_name.rs",
                    "src/new_name.rs",
                    word,
                    &["pub fn x() {}"],
                ),
            ];
            let model = plan(&[("Use", &[0]), ("Move", &[1])]);
            assert_eq!(order_of(&patches, model), ["Move", "Use"], "{word}");
        }
    }

    #[test]
    fn doesnt_count_a_file_moved_to_another_folder_under_the_same_name() {
        for word in ["rename", "copy"] {
            let patches = vec![
                edited(
                    "src/main.rs",
                    &[],
                    &["mod same_name;", "include_str!(\"same.md\");"],
                ),
                renamed(
                    "src/same_name.rs",
                    "src/sub/same_name.rs",
                    word,
                    &["pub fn x() {}"],
                ),
                renamed("docs/same.md", "doc/same.md", word, &["text"]),
            ];
            let model = plan(&[("Use", &[0]), ("Move", &[1]), ("Move docs", &[2])]);
            assert_eq!(
                order_of(&patches, model),
                ["Use", "Move", "Move docs"],
                "{word}"
            );
        }
    }

    #[test]
    fn reads_the_quoted_names_of_a_rename() {
        let quoted = moved(
            "\"a/src/caf\\303\\251.rs\" \"b/src/sub/caf\\303\\251.rs\"",
            "rename",
            ("\"src/caf\\303\\251.rs\"", "\"src/sub/caf\\303\\251.rs\""),
            &["pub fn x() {}"],
        );
        let patches = vec![edited("src/main.rs", &[], &["mod café;"]), quoted];
        let files = files_of(&patches);
        assert_eq!(files[1].path, "src/sub/café.rs");
        let model = plan(&[("Use", &[0]), ("Move", &[1])]);
        assert_eq!(order_of(&patches, model.clone()), ["Use", "Move"]);

        let other_name = moved(
            "\"a/src/caf\\303\\251.rs\" \"b/src/cr\\303\\250me.rs\"",
            "rename",
            ("\"src/caf\\303\\251.rs\"", "\"src/cr\\303\\250me.rs\""),
            &["pub fn x() {}"],
        );
        let patches = vec![edited("src/main.rs", &[], &["mod crème;"]), other_name];
        assert_eq!(order_of(&patches, model), ["Move", "Use"]);
    }

    #[test]
    fn doesnt_count_a_file_that_is_only_changed_or_deleted() {
        let deleted = "diff --git a/src/gone.rs b/src/gone.rs\ndeleted file mode 100644\n\
                       --- a/src/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";
        let patches = vec![
            edited("src/main.rs", &[], &["mod gone;", "mod changed;"]),
            deleted.to_owned(),
            edited("src/changed.rs", &[], &["fn x() {}"]),
        ];
        let model = plan(&[("Use", &[0]), ("Delete", &[1]), ("Change", &[2])]);
        assert_eq!(order_of(&patches, model), ["Use", "Delete", "Change"]);
    }

    #[test]
    fn merges_two_commits_that_need_each_others_new_files() {
        let patches = vec![
            new_file("src/a.rs", &["use crate::b;"]),
            new_file("src/b.rs", &["use crate::a;"]),
        ];
        let ordered = ordered(&patches, plan(&[("Add a", &[0]), ("Add b", &[1])]));
        assert_eq!(ordered.plan.titles(), ["Add a; Add b"]);
        assert_eq!(ordered.plan.commits[0].units, [0, 1]);
        assert_eq!(ordered.notes, ["\"Add a\" and \"Add b\" became one commit"]);
    }

    #[test]
    fn merges_a_cycle_through_a_third_commit_and_leaves_the_others_where_they_are() {
        let patches = vec![
            new_file("src/a.rs", &["use crate::b;"]),
            new_file("src/b.rs", &["use crate::c;"]),
            new_file("src/c.rs", &["use crate::a;"]),
            new_file("src/free.rs", &["fn free() {}"]),
        ];
        let model = plan(&[
            ("Add free", &[3]),
            ("Add a", &[0]),
            ("Add b", &[1]),
            ("Add c", &[2]),
        ]);
        let ordered = ordered(&patches, model);
        assert_eq!(ordered.plan.titles(), ["Add free", "Add a; Add b; Add c"]);
        assert_eq!(ordered.plan.commits[1].units, [0, 1, 2]);
        assert_eq!(
            ordered.notes,
            ["\"Add a\", \"Add b\" and \"Add c\" became one commit"]
        );
    }

    #[test]
    fn puts_a_merged_commit_where_its_first_commit_was_and_what_it_needs_before_it() {
        let patches = vec![
            new_file("src/a.rs", &["use crate::b;", "use crate::base;"]),
            new_file("src/other.rs", &["fn other() {}"]),
            new_file("src/b.rs", &["use crate::a;"]),
            new_file("src/base.rs", &["fn base() {}"]),
        ];
        let model = plan(&[
            ("Add a", &[0]),
            ("Add other", &[1]),
            ("Add b", &[2]),
            ("Add base", &[3]),
        ]);
        assert_eq!(
            order_of(&patches, model),
            ["Add base", "Add a; Add b", "Add other"]
        );
    }

    #[test]
    fn follows_a_chain_of_needs_to_its_start() {
        let patches = vec![
            new_file("src/top.rs", &["use crate::middle;"]),
            new_file("src/middle.rs", &["use crate::bottom;"]),
            new_file("src/bottom.rs", &["fn bottom() {}"]),
        ];
        let model = plan(&[("Top", &[0]), ("Middle", &[1]), ("Bottom", &[2])]);
        assert_eq!(order_of(&patches, model), ["Bottom", "Middle", "Top"]);
    }

    #[test]
    fn puts_what_a_commit_needs_before_it_in_the_models_order() {
        let patches = vec![
            new_file("src/user.rs", &["use crate::{first, second};"]),
            new_file("src/second.rs", &["fn second() {}"]),
            new_file("src/first.rs", &["fn first() {}"]),
        ];
        let model = plan(&[("User", &[0]), ("Second", &[1]), ("First", &[2])]);
        assert_eq!(order_of(&patches, model), ["Second", "First", "User"]);
    }

    #[test]
    fn doesnt_need_another_file_of_a_name_the_commit_adds_itself() {
        let patches = vec![
            new_file("src/a/util.rs", &["fn a() {}"]),
            new_file("src/b/util.rs", &["use crate::b::util;"]),
        ];
        let model = plan(&[("B", &[1]), ("A", &[0])]);
        assert_eq!(order_of(&patches, model), ["B", "A"]);
    }

    #[test]
    fn needs_no_file_whose_name_two_new_files_have() {
        let patches = vec![
            new_file("src/a/data.json", &["{}"]),
            edited("src/main.rs", &[], &["include_str!(\"data.json\");"]),
            new_file("src/b/data.json", &["{}"]),
        ];
        let model = plan(&[("Use", &[1]), ("Add a", &[0]), ("Add b", &[2])]);
        assert_eq!(order_of(&patches, model), ["Use", "Add a", "Add b"]);
        let same_commit = plan(&[("Use", &[1]), ("Add both", &[0, 2])]);
        assert_eq!(order_of(&patches, same_commit), ["Use", "Add both"]);
    }

    #[test]
    fn needs_no_file_whose_name_a_file_in_head_has() {
        let patches = vec![
            edited("src/main.rs", &[], &["include_str!(\"guide.md\");"]),
            new_file("site/guide.md", &["text"]),
        ];
        let files = files_of(&patches);
        let units = each_file_a_unit(&files);
        let model = plan(&[("Use", &[0]), ("Add", &[1])]);
        let in_head = HashSet::from(["guide.md".to_owned()]);
        let ordered = new_files_first(model.clone(), &files, &units, &in_head);
        assert_eq!(ordered.plan.titles(), ["Use", "Add"]);
        let elsewhere = HashSet::from(["guide.mdx".to_owned(), "Guide.md".to_owned()]);
        let ordered = new_files_first(model, &files, &units, &elsewhere);
        assert_eq!(ordered.plan.titles(), ["Add", "Use"]);
    }

    /// The failing shape of new files that use each other, whose module a commit declares.
    fn files_that_use_each_other() -> Vec<String> {
        vec![
            edited(
                "src/main.rs",
                &[],
                &[
                    "mod report;",
                    "mod safe_text;",
                    "mod staging;",
                    "fn main() { staging::stage(); }",
                ],
            ),
            new_file("src/report.rs", &["use crate::safe_text::escaped;"]),
            new_file("src/safe_text.rs", &["pub fn escaped() {}"]),
            new_file("src/staging.rs", &["use crate::report::report;"]),
        ]
    }

    #[test]
    fn counts_the_references_of_a_new_file_for_the_commit_that_declares_its_module() {
        let model = plan(&[
            ("Use everything", &[0]),
            ("Add staging", &[3]),
            ("Add report", &[1]),
            ("Add safe_text", &[2]),
        ]);
        let ordered = ordered(&files_that_use_each_other(), model);
        assert_eq!(
            ordered.plan.titles(),
            [
                "Add staging",
                "Add report",
                "Add safe_text",
                "Use everything"
            ]
        );
        assert!(ordered.notes.is_empty());
    }

    #[test]
    fn puts_the_use_of_a_waiting_file_after_the_declaration_of_what_it_uses() {
        let patches = vec![
            edited("src/main.rs", &[], &["mod x;"]),
            edited("src/other.rs", &[], &["mod y;"]),
            new_file("src/x.rs", &["use crate::y;"]),
            new_file("src/y.rs", &["pub fn y() {}"]),
        ];
        let model = plan(&[
            ("Declare x", &[0]),
            ("Declare y", &[1]),
            ("Add x", &[2]),
            ("Add y", &[3]),
        ]);
        assert_eq!(
            order_of(&patches, model),
            ["Add y", "Declare y", "Add x", "Declare x"]
        );
    }

    #[test]
    fn counts_the_references_of_a_new_file_where_it_is_when_no_commit_declares_it() {
        let patches = vec![
            edited("src/main.rs", &[], &["mod other;"]),
            new_file("src/x.rs", &["use crate::y;"]),
            new_file("src/y.rs", &["pub fn y() {}"]),
            new_file("src/other.rs", &["pub fn o() {}"]),
        ];
        let model = plan(&[("Add x", &[1]), ("Add y", &[2]), ("Declare", &[0, 3])]);
        assert_eq!(order_of(&patches, model), ["Add y", "Add x", "Declare"]);
    }

    #[test]
    fn counts_the_references_of_a_renamed_file_where_it_is() {
        let patches = vec![
            edited("src/main.rs", &[], &["mod x;"]),
            renamed("src/old.rs", "src/x.rs", "rename", &["use crate::y;"]),
            new_file("src/y.rs", &["pub fn y() {}"]),
        ];
        let model = plan(&[("Rename to x", &[1]), ("Add y", &[2]), ("Declare x", &[0])]);
        assert_eq!(
            order_of(&patches, model),
            ["Add y", "Rename to x", "Declare x"]
        );
    }

    #[test]
    fn keeps_the_references_of_a_file_cargo_compiles_without_a_declaration() {
        let patches = vec![
            new_file("tests/util.rs", &["use other::f;"]),
            edited("src/main.rs", &[], &["mod util;"]),
            new_file("src/other.rs", &["pub fn f() {}"]),
            new_file("src/util.rs", &["pub fn u() {}"]),
        ];
        let model = plan(&[
            ("Test", &[0]),
            ("Declare", &[1]),
            ("Add other", &[2]),
            ("Add util", &[3]),
        ]);
        assert_eq!(
            order_of(&patches, model),
            ["Add other", "Test", "Add util", "Declare"]
        );
    }

    #[test]
    fn doesnt_need_the_file_it_adds_itself() {
        let patches = vec![new_file("src/safe_text.rs", &["mod safe_text;"])];
        assert_eq!(order_of(&patches, plan(&[("Add", &[0])])), ["Add"]);
    }

    #[test]
    fn handles_a_plan_without_commits_and_with_one() {
        assert_eq!(ordered(&[], Plan::default()).plan, Plan::default());
        let patches = vec![new_file("src/a.rs", &["use crate::a;"])];
        let one = plan(&[("Only", &[0])]);
        assert_eq!(ordered(&patches, one.clone()).plan, one);
    }

    #[test]
    fn keeps_a_commit_with_no_units_as_it_is() {
        let patches = vec![new_file("src/a.rs", &["fn a() {}"])];
        let model = plan(&[("Empty", &[]), ("A", &[0])]);
        assert_eq!(order_of(&patches, model), ["Empty", "A"]);
    }

    #[test]
    fn needs_what_a_commit_with_several_files_refers_to_in_any_of_them() {
        let patches = vec![
            edited("a.rs", &[], &["fn a() {}"]),
            edited("b.rs", &[], &["use crate::dep;"]),
            new_file("dep.rs", &["fn dep() {}"]),
        ];
        let model = plan(&[("Both", &[0, 1]), ("Dep", &[2])]);
        assert_eq!(order_of(&patches, model), ["Dep", "Both"]);
    }

    #[test]
    fn keeps_the_order_when_a_declaration_changes_for_a_module_the_plan_doesnt_add() {
        let patches = vec![
            edited("src/report.rs", &[], &["use crate::config;"]),
            edited("src/main.rs", &["mod config;"], &["pub mod config;"]),
            edited("src/other.rs", &[], &["fn other() {}"]),
        ];
        let model = plan(&[
            ("Use config", &[0]),
            ("Make config public", &[1]),
            ("Other", &[2]),
        ]);
        let ordered = ordered(&patches, model.clone());
        assert_eq!(ordered.plan, model);
        assert!(ordered.notes.is_empty());
    }

    #[test]
    fn puts_a_declaration_before_a_use_only_when_the_plan_adds_the_file() {
        let patches = vec![
            edited("src/report.rs", &[], &["use crate::config;"]),
            edited("src/main.rs", &[], &["pub mod config;"]),
            new_file("src/config.rs", &["pub fn c() {}"]),
        ];
        let model = plan(&[("Use", &[0]), ("Declare", &[1]), ("Add", &[2])]);
        assert_eq!(order_of(&patches, model), ["Add", "Declare", "Use"]);
    }

    #[test]
    fn counts_the_references_of_a_new_file_in_a_folder_of_modules_that_waits_for_a_declaration() {
        let patches = vec![
            new_file("src/tests/helpers.rs", &["use crate::y;"]),
            edited("src/main.rs", &[], &["mod helpers;"]),
            new_file("src/y.rs", &["pub fn y() {}"]),
        ];
        let model = plan(&[("Add helpers", &[0]), ("Declare", &[1]), ("Add y", &[2])]);
        assert_eq!(
            order_of(&patches, model),
            ["Add helpers", "Add y", "Declare"]
        );
    }

    #[test]
    fn doesnt_take_a_standard_crate_for_a_new_module_of_the_same_name() {
        for line in [
            "use std::io;",
            "use std::error::Error;",
            "use std::{env, fs};",
            "use core::fmt;",
            "std::io::stdin()",
        ] {
            let patches = vec![
                edited("src/main.rs", &[], &[line]),
                new_file("src/io.rs", &["pub fn f() {}"]),
                new_file("src/error.rs", &["pub fn f() {}"]),
                new_file("src/env.rs", &["pub fn f() {}"]),
                new_file("src/fs.rs", &["pub fn f() {}"]),
                new_file("src/fmt.rs", &["pub fn f() {}"]),
            ];
            let model = plan(&[
                ("Use", &[0]),
                ("Add io", &[1]),
                ("Add error", &[2]),
                ("Add env", &[3]),
                ("Add fs", &[4]),
                ("Add fmt", &[5]),
            ]);
            let ordered = ordered(&patches, model.clone());
            assert_eq!(ordered.plan, model, "{line}");
        }
    }

    #[test]
    fn counts_a_name_a_use_block_brings_in_when_only_its_line_was_added() {
        let in_a_block = "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n\
                          +++ b/src/main.rs\n@@ -1,4 +1,5 @@\n use crate::{\n     config,\n\
                          +    safe_text,\n };\n fn main() {}\n";
        let patches = vec![
            in_a_block.to_owned(),
            new_file("src/safe_text.rs", &["pub fn escaped() {}"]),
        ];
        let model = plan(&[("Use", &[0]), ("Add", &[1])]);
        assert_eq!(order_of(&patches, model), ["Add", "Use"]);
    }

    #[test]
    fn mentions_a_file_name_only_in_the_lines_a_commit_adds() {
        let around = "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n\
                      +++ b/src/main.rs\n@@ -1,3 +1,3 @@\n include_str!(\"guide.md\");\n\
                      -fn old() {}\n+fn new() {}\n // see guide.md\n";
        let patches = vec![around.to_owned(), new_file("docs/guide.md", &["text"])];
        let model = plan(&[("Use", &[0]), ("Add", &[1])]);
        assert_eq!(order_of(&patches, model), ["Use", "Add"]);
    }

    #[test]
    fn ignores_a_line_of_prose_and_a_comment_where_a_name_would_count() {
        let patches = vec![
            edited(
                "src/main.rs",
                &[],
                &[
                    "const HELP: &str = r#\"",
                    "use short subjects and mention the config file",
                    "mention the safe_text file;",
                    "\"#;",
                    "// use crate::safe_text;",
                ],
            ),
            new_file("src/safe_text.rs", &["pub fn escaped() {}"]),
        ];
        let model = plan(&[("Use", &[0]), ("Add", &[1])]);
        assert_eq!(order_of(&patches, model), ["Use", "Add"]);
    }

    #[test]
    fn gives_a_crate_in_a_folder_with_hyphens_the_name_its_code_uses() {
        let patches = vec![
            edited("src/main.rs", &[], &["use safe_text::Staged;"]),
            new_file("crates/safe-text/src/lib.rs", &["pub struct Staged;"]),
        ];
        let model = plan(&[("Use", &[0]), ("Add", &[1])]);
        assert_eq!(order_of(&patches, model), ["Add", "Use"]);
    }

    #[test]
    fn follows_a_path_attribute_through_the_name_of_the_file() {
        let patches = vec![
            edited(
                "src/main.rs",
                &[],
                &["#[path = \"generated/renamed.rs\"]", "mod other_name;"],
            ),
            new_file("src/generated/renamed.rs", &["pub fn f() {}"]),
        ];
        let model = plan(&[("Declare", &[0]), ("Add", &[1])]);
        assert_eq!(order_of(&patches, model), ["Add", "Declare"]);
    }

    #[test]
    fn names_the_new_files_a_line_can_mention() {
        let patches = vec![
            new_file("src/a/guide.md", &["text"]),
            new_file("LICENSE", &["text"]),
            new_file("src/safe_text.rs", &["fn a() {}"]),
            renamed("docs/same.md", "doc/same.md", "rename", &["text"]),
            renamed("docs/old.md", "docs/new.md", "rename", &["text"]),
            edited("src/main.rs", &[], &["fn main() {}"]),
            binary_file("assets/logo.png"),
        ];
        let expected: HashSet<String> = ["guide.md", "safe_text.rs", "new.md", "logo.png"]
            .map(str::to_owned)
            .into();
        assert_eq!(candidate_file_names(&files_of(&patches)), expected);
        assert!(candidate_file_names(&files_of(&patches[5..6])).is_empty());
    }
}
