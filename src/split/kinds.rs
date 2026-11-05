//! What a commit is, going by its files alone: only documentation, or only
//! tests. A model is often wrong about these, and the files can't be.

use super::{FilePatch, Plan, Planned, Unit};
use std::path::Path;

const DOC_EXTENSIONS: [&str; 6] = ["md", "mdx", "markdown", "rst", "adoc", "asciidoc"];
const TEST_FOLDERS: [&str; 4] = ["test", "tests", "__tests__", "testdata"];
const TEST_STEM_ENDINGS: [&str; 6] = ["Test", "Tests", "_test", "_spec", ".test", ".spec"];

/// The types of Conventional Commits a commit's files can overrule. A revert
/// stays one, whatever it reverts.
const OVERRULED_TYPES: [&str; 11] = [
    "feat", "fix", "refactor", "perf", "docs", "test", "tests", "build", "ci", "chore", "style",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Docs,
    Tests,
}

impl Kind {
    fn commit_type(self) -> &'static str {
        match self {
            Self::Docs => "docs",
            Self::Tests => "test",
        }
    }
}

/// Whether all of `commit` is documentation, or all of it tests.
pub fn kind_of(commit: &Planned, files: &[FilePatch], units: &[Unit]) -> Option<Kind> {
    let mut kinds = commit
        .units
        .iter()
        .map(|&unit| kind_of_file(&files[units[unit].file]));
    let first = kinds.next()??;
    kinds.all(|kind| kind == Some(first)).then_some(first)
}

/// A renamed file doesn't count, because only its new name is known, and its
/// old one may have been code.
fn kind_of_file(file: &FilePatch) -> Option<Kind> {
    match file.status {
        Some("renamed" | "copied") => None,
        _ => kind_of_path(&file.path),
    }
}

/// `.txt` isn't documentation, because of files such as `CMakeLists.txt`.
fn kind_of_path(path: &str) -> Option<Kind> {
    let path = Path::new(path);
    let extension = path.extension().and_then(|extension| extension.to_str());
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    let name = path.file_name().and_then(|name| name.to_str());
    let in_test_folder = path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|folder| folder.as_os_str().to_str())
        .any(|folder| TEST_FOLDERS.contains(&folder));
    if extension
        .is_some_and(|extension| DOC_EXTENSIONS.contains(&extension.to_lowercase().as_str()))
    {
        Some(Kind::Docs)
    } else if in_test_folder
        || name.is_some_and(|name| name.starts_with("test_"))
        || stem.is_some_and(|stem| TEST_STEM_ENDINGS.iter().any(|end| stem.ends_with(end)))
    {
        Some(Kind::Tests)
    } else {
        None
    }
}

/// The plan with its documentation-only commits at the end. Documentation can't
/// break a build, and it reads better after the change it describes.
pub fn docs_last(plan: Plan, files: &[FilePatch], units: &[Unit]) -> Plan {
    let (docs, others): (Vec<Planned>, Vec<Planned>) = plan
        .commits
        .into_iter()
        .partition(|commit| kind_of(commit, files, units) == Some(Kind::Docs));
    Plan {
        commits: others.into_iter().chain(docs).collect(),
    }
}

/// `message` with the type its subject should have for `kind`. A subject without
/// a known Conventional Commits type stays as it is, because the user's own
/// prompt may ask for another form.
pub fn with_type(message: &str, kind: Option<Kind>) -> String {
    let Some(kind) = kind else {
        return message.to_owned();
    };
    match conventional_type_length(message) {
        Some(length) => format!("{}{}", kind.commit_type(), &message[length..]),
        None => message.to_owned(),
    }
}

/// How long the type at the start of a subject such as `fix(parser)!: ...` is.
fn conventional_type_length(message: &str) -> Option<usize> {
    let subject = message.lines().next()?;
    let length = subject.find(|character: char| !character.is_ascii_alphabetic())?;
    let (commit_type, rest) = subject.split_at(length);
    if !OVERRULED_TYPES.contains(&commit_type.to_lowercase().as_str()) {
        return None;
    }
    let after_scope = match rest.strip_prefix('(') {
        Some(scope) => &scope[scope.find(')')? + 1..],
        None => rest,
    };
    let after_mark = after_scope.strip_prefix('!').unwrap_or(after_scope);
    after_mark.starts_with(": ").then_some(length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// One file per path, with the given status given as a `diff --git` header.
    fn parsed(headers: &[&str]) -> Vec<FilePatch> {
        let patch: String = headers
            .iter()
            .map(|header| format!("{header}\n@@ -1 +1 @@\n-a\n+b\n"))
            .collect();
        let files = crate::split::parse(&patch, &HashSet::new());
        assert_eq!(files.len(), headers.len());
        files
    }

    fn changed(path: &str) -> String {
        format!("diff --git a/{path} b/{path}")
    }

    fn each_file_a_unit(count: usize) -> Vec<Unit> {
        (0..count).map(|file| Unit { file, hunk: None }).collect()
    }

    fn commit(title: &str, units: &[usize]) -> Planned {
        Planned {
            title: title.to_owned(),
            units: units.to_vec(),
        }
    }

    #[test]
    fn tells_documentation_by_its_extension() {
        for path in ["README.md", "docs/guide.MD", "a/b.mdx", "x.rst", "y.adoc"] {
            assert_eq!(kind_of_path(path), Some(Kind::Docs), "{path}");
        }
        for path in [
            "CMakeLists.txt",
            "requirements.txt",
            "notes",
            "md",
            "src/main.rs",
        ] {
            assert_eq!(kind_of_path(path), None, "{path}");
        }
    }

    #[test]
    fn tells_tests_by_their_folder_and_name() {
        for path in [
            "tests/cli.rs",
            "src/test/java/no/Foo.java",
            "web/__tests__/a.js",
            "spec/models/a_spec.rb",
            "test_parser.py",
            "src/parser_test.go",
            "src/a.test.ts",
            "src/a.spec.ts",
            "src/main/FooTest.java",
            "src/main/FooTests.kt",
        ] {
            assert_eq!(kind_of_path(path), Some(Kind::Tests), "{path}");
        }
        for path in [
            "src/testing.rs",
            "src/contest.py",
            "src/latest.rs",
            "src/Attest.txt",
            "src/main/KeySpec.java",
            "specs/api.yaml",
            "spec/openapi.yaml",
            "test",
        ] {
            assert_eq!(kind_of_path(path), None, "{path}");
        }
    }

    #[test]
    fn counts_documentation_in_a_test_folder_as_documentation() {
        assert_eq!(kind_of_path("tests/README.md"), Some(Kind::Docs));
    }

    #[test]
    fn tells_a_commit_only_when_all_of_it_agrees() {
        let files = parsed(&[
            &changed("README.md"),
            &changed("docs/a.md"),
            &changed("tests/a.rs"),
            &changed("src/a.rs"),
        ]);
        let units = each_file_a_unit(4);
        let kind = |list: &[usize]| kind_of(&commit("c", list), &files, &units);
        assert_eq!(kind(&[0, 1]), Some(Kind::Docs));
        assert_eq!(kind(&[2]), Some(Kind::Tests));
        assert_eq!(kind(&[0, 2]), None);
        assert_eq!(kind(&[0, 3]), None);
        assert_eq!(kind(&[3]), None);
        assert_eq!(kind(&[]), None);
    }

    #[test]
    fn doesnt_trust_the_new_name_of_a_renamed_or_copied_file() {
        for (word_from, word_to) in [("rename from", "rename to"), ("copy from", "copy to")] {
            let header = format!(
                "diff --git a/code.rs b/moved.md\nsimilarity index 90%\n{word_from} code.rs\n\
                 {word_to} moved.md"
            );
            let files = parsed(&[&header]);
            assert_eq!(kind_of_file(&files[0]), None, "{word_from}");
        }
        let moved_and_executable = "diff --git a/tool.sh b/guide.md\nold mode 100644\n\
                                    new mode 100755\nrename from tool.sh\nrename to guide.md";
        assert_eq!(kind_of_file(&parsed(&[moved_and_executable])[0]), None);
        let deleted = "diff --git a/old.md b/old.md\ndeleted file mode 100644";
        assert_eq!(kind_of_file(&parsed(&[deleted])[0]), Some(Kind::Docs));
    }

    #[test]
    fn moves_documentation_last_and_keeps_the_rest_in_order() {
        let files = parsed(&[
            &changed("README.md"),
            &changed("src/a.rs"),
            &changed("docs/b.md"),
            &changed("src/c.rs"),
        ]);
        let units = each_file_a_unit(4);
        let plan = Plan {
            commits: vec![
                commit("readme", &[0]),
                commit("a", &[1]),
                commit("docs", &[2]),
                commit("c", &[3]),
            ],
        };
        let titles: Vec<String> = docs_last(plan, &files, &units)
            .commits
            .into_iter()
            .map(|commit| commit.title)
            .collect();
        assert_eq!(titles, ["a", "c", "readme", "docs"]);
    }

    #[test]
    fn leaves_a_commit_of_docs_and_code_where_it_is() {
        let files = parsed(&[
            &changed("README.md"),
            &changed("src/a.rs"),
            &changed("b.rs"),
        ]);
        let units = each_file_a_unit(3);
        let plan = Plan {
            commits: vec![commit("both", &[0, 1]), commit("code", &[2])],
        };
        assert_eq!(docs_last(plan.clone(), &files, &units), plan);
    }

    #[test]
    fn corrects_the_type_and_keeps_the_rest_of_the_message() {
        let docs = Some(Kind::Docs);
        assert_eq!(with_type("feat: add a guide", docs), "docs: add a guide");
        assert_eq!(with_type("refactor(api): tidy", docs), "docs(api): tidy");
        assert_eq!(
            with_type("feat(api)!: new\n\nbody: text", docs),
            "docs(api)!: new\n\nbody: text"
        );
        assert_eq!(with_type("feat!: new", docs), "docs!: new");
        assert_eq!(with_type("Feat: new", docs), "docs: new");
        assert_eq!(with_type("docs: right", docs), "docs: right");
        assert_eq!(
            with_type("feat: add tests", Some(Kind::Tests)),
            "test: add tests"
        );
        assert_eq!(with_type("tests: more", Some(Kind::Tests)), "test: more");
    }

    #[test]
    fn leaves_a_message_alone_when_it_has_no_known_type_or_no_kind() {
        for message in [
            "Add a guide",
            "note: this is not a type",
            "feat add a guide",
            "feat(api add a guide",
            "feat:no space",
            "revert: \"fix: the guide\"",
            "1.2: release",
            "",
        ] {
            assert_eq!(with_type(message, Some(Kind::Docs)), message, "{message}");
        }
        assert_eq!(with_type("feat: code", None), "feat: code");
    }
}
