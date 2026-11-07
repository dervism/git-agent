//! Checking untracked files before `--all` stages them.
//!
//! Build output, downloaded dependencies, compiled files, environment files,
//! private keys and large files are usually meant to be ignored, not
//! committed. A new project often lacks the `.gitignore` entries that keep
//! them out, and `git add --all` would commit them. No model is involved:
//! the rules below decide.

use crate::git::{lossless, names};
use crate::ui::{bytes, thousands};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fmt::Write;
use std::fs;
use std::path::Path;

/// A file this size or larger counts as large.
const LARGE_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Most findings to list; the rest are counted.
const MOST_LISTED: usize = 8;

/// A directory that tools generate, recognized by its name and, where the
/// name alone is too common, by a file next to it that marks the project.
struct DirectoryRule {
    name: &'static str,
    /// Files in the parent directory, one of which must exist. An entry
    /// starting with '.' is a file extension, e.g. ".csproj". Empty: none needed.
    markers: &'static [&'static str],
    reason: &'static str,
    /// Whether the name means the same thing anywhere, so `.gitignore` can
    /// name it without its location.
    anywhere: bool,
}

const DIRECTORIES: &[DirectoryRule] = &[
    DirectoryRule {
        name: "node_modules",
        markers: &[],
        reason: "npm dependencies",
        anywhere: true,
    },
    DirectoryRule {
        name: "target",
        markers: &["Cargo.toml"],
        reason: "Rust build output",
        anywhere: false,
    },
    DirectoryRule {
        name: "target",
        markers: &["pom.xml"],
        reason: "Maven build output",
        anywhere: false,
    },
    DirectoryRule {
        name: "__pycache__",
        markers: &[],
        reason: "Python bytecode",
        anywhere: true,
    },
    DirectoryRule {
        name: ".pytest_cache",
        markers: &[],
        reason: "Python tool cache",
        anywhere: true,
    },
    DirectoryRule {
        name: ".mypy_cache",
        markers: &[],
        reason: "Python tool cache",
        anywhere: true,
    },
    DirectoryRule {
        name: ".ruff_cache",
        markers: &[],
        reason: "Python tool cache",
        anywhere: true,
    },
    DirectoryRule {
        name: ".tox",
        markers: &[],
        reason: "Python tool cache",
        anywhere: true,
    },
    DirectoryRule {
        name: ".venv",
        markers: &[],
        reason: "Python virtual environment",
        anywhere: true,
    },
    DirectoryRule {
        name: "dist",
        markers: &["package.json"],
        reason: "JavaScript build output",
        anywhere: false,
    },
    DirectoryRule {
        name: "build",
        markers: &["package.json"],
        reason: "JavaScript build output",
        anywhere: false,
    },
    DirectoryRule {
        name: "out",
        markers: &["package.json"],
        reason: "JavaScript build output",
        anywhere: false,
    },
    DirectoryRule {
        name: "coverage",
        markers: &["package.json"],
        reason: "test coverage report",
        anywhere: false,
    },
    DirectoryRule {
        name: ".next",
        markers: &[],
        reason: "Next.js build output",
        anywhere: true,
    },
    DirectoryRule {
        name: ".nuxt",
        markers: &[],
        reason: "Nuxt build output",
        anywhere: true,
    },
    DirectoryRule {
        name: ".svelte-kit",
        markers: &[],
        reason: "SvelteKit build output",
        anywhere: true,
    },
    DirectoryRule {
        name: ".astro",
        markers: &[],
        reason: "Astro generated types",
        anywhere: true,
    },
    DirectoryRule {
        name: ".turbo",
        markers: &[],
        reason: "Turborepo cache",
        anywhere: true,
    },
    DirectoryRule {
        name: ".parcel-cache",
        markers: &[],
        reason: "Parcel cache",
        anywhere: true,
    },
    DirectoryRule {
        name: ".gradle",
        markers: &[],
        reason: "Gradle cache",
        anywhere: true,
    },
    DirectoryRule {
        name: "build",
        markers: &[
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "settings.gradle.kts",
        ],
        reason: "Gradle build output",
        anywhere: false,
    },
    DirectoryRule {
        name: "build",
        markers: &["CMakeLists.txt"],
        reason: "CMake build output",
        anywhere: false,
    },
    DirectoryRule {
        name: "bin",
        markers: &[".csproj", ".fsproj", ".vbproj"],
        reason: ".NET build output",
        anywhere: false,
    },
    DirectoryRule {
        name: "obj",
        markers: &[".csproj", ".fsproj", ".vbproj"],
        reason: ".NET build output",
        anywhere: false,
    },
    DirectoryRule {
        name: "DerivedData",
        markers: &[],
        reason: "Xcode build output",
        anywhere: true,
    },
    DirectoryRule {
        name: ".dart_tool",
        markers: &[],
        reason: "Dart tool cache",
        anywhere: true,
    },
];

/// Extensions of files that compilers produce.
const COMPILED: &[&str] = &["o", "a", "so", "dylib", "dll", "exe", "class", "pyc", "pyo"];
/// Files operating systems leave in folders.
const SYSTEM_FILES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];
/// SSH private keys, by their usual names.
const SSH_KEYS: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];
/// Extensions of private keys and key stores.
const KEY_FILES: &[&str] = &["pem", "key", "p12", "pfx", "jks", "keystore"];
/// Endings of `.env` files that are templates, meant to be committed.
const ENV_TEMPLATES: &[&str] = &["example", "sample", "template", "dist", "defaults"];

/// Untracked files that look like they should be ignored, grouped by why.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    /// What the user sees: a directory (ending with '/'), a file, or a
    /// pattern such as `*.pyc` for files of one kind.
    pub label: String,
    pub reason: String,
    /// The line that keeps these files out of the repository, for `.gitignore`.
    pub pattern: String,
    /// Pathspecs that leave these files out when staging.
    pub exclude: Vec<String>,
    /// The untracked files it covers, as git lists them.
    pub paths: Vec<String>,
    pub bytes: u64,
}

/// The findings among `untracked`, the untracked files as `git status` lists
/// them, relative to `top`, the top of the work tree.
pub fn check(top: &Path, untracked: &[String]) -> Vec<Finding> {
    let mut findings: Vec<Finding> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut markers = Markers::default();
    for path in untracked {
        let Some((key, label, reason, pattern, leave_out)) = classify(top, path, &mut markers)
        else {
            continue;
        };
        let size =
            fs::symlink_metadata(top.join(lossless::to_path(path))).map_or(0, |meta| meta.len());
        let index = *by_key.entry(key).or_insert_with(|| {
            findings.push(Finding {
                label,
                reason,
                pattern,
                exclude: Vec::new(),
                paths: Vec::new(),
                bytes: 0,
            });
            findings.len() - 1
        });
        let finding = &mut findings[index];
        if !finding.exclude.contains(&leave_out) {
            finding.exclude.push(leave_out);
        }
        finding.paths.push(path.clone());
        finding.bytes += size;
    }
    findings
}

/// What `classify` returns: the key that groups paths into one finding, its
/// label, reason and `.gitignore` pattern, and the pathspec that leaves the
/// path out when staging.
type Classified = (String, String, String, String, String);

/// A pathspec that leaves out exactly `path`, a file or a directory.
pub fn literal(path: &str) -> String {
    format!(":(top,exclude,literal){path}")
}

/// `path` as a `.gitignore` line takes it, so a character in it that means
/// something there can't make the line cover other files.
fn ignore_pattern_for(path: &str) -> String {
    let mut pattern = String::with_capacity(path.len());
    for character in path.chars() {
        if matches!(character, '\\' | '*' | '?' | '[' | ']') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    if pattern.ends_with(' ') {
        pattern.pop();
        pattern.push_str("\\ ");
    }
    pattern
}

/// A pathspec that leaves out every file matching `pattern`, wherever it is.
fn anywhere(pattern: &str) -> String {
    format!(":(top,exclude,glob)**/{pattern}")
}

fn classify(top: &Path, path: &str, markers: &mut Markers) -> Option<Classified> {
    // The outermost generated directory the file is in.
    let parts: Vec<&str> = path.split('/').collect();
    for depth in 0..parts.len() - 1 {
        let name = parts[depth];
        let parent = parts[..depth].join("/");
        let Some(rule) = DIRECTORIES
            .iter()
            .find(|rule| rule.name == name && markers.any(top, &parent, rule.markers))
        else {
            continue;
        };
        let directory = parts[..=depth].join("/");
        let pattern = if rule.anywhere {
            format!("{name}/")
        } else {
            format!("/{}/", ignore_pattern_for(&directory))
        };
        return Some((
            directory.clone(),
            format!("{directory}/"),
            rule.reason.to_owned(),
            pattern,
            literal(&directory),
        ));
    }

    let name = *parts.last()?;
    let extension = name.rsplit_once('.').map(|(_, extension)| extension);
    let lowercase = extension.map(str::to_lowercase);
    let single = |reason: &str, pattern: String| {
        Some((
            path.to_owned(),
            path.to_owned(),
            reason.to_owned(),
            pattern,
            literal(path),
        ))
    };
    // Files of one kind are grouped, and left out with one pattern however many there are.
    let kind = |reason: &str, pattern: String| {
        Some((
            format!("kind {pattern}"),
            pattern.clone(),
            reason.to_owned(),
            pattern.clone(),
            anywhere(&pattern),
        ))
    };
    if SYSTEM_FILES.contains(&name) {
        return kind("operating system file", name.to_owned());
    }
    // The pattern keeps the file's own case: `*.exe` doesn't match SETUP.EXE where
    // core.ignorecase is false, and a `.gitignore` line can't say otherwise.
    if let Some(extension) = extension.filter(|_| {
        lowercase
            .as_deref()
            .is_some_and(|lowercase| COMPILED.contains(&lowercase))
    }) {
        return kind("compiled files", format!("*.{extension}"));
    }
    if is_env_file(name) {
        return single(
            "environment file; it may hold secrets",
            ignore_pattern_for(name),
        );
    }
    if SSH_KEYS.contains(&name)
        || lowercase
            .as_deref()
            .is_some_and(|lowercase| KEY_FILES.contains(&lowercase))
    {
        return single(
            "private key or key store",
            format!("/{}", ignore_pattern_for(path)),
        );
    }
    let size = fs::symlink_metadata(top.join(lossless::to_path(path))).map_or(0, |meta| meta.len());
    if size >= LARGE_FILE_BYTES {
        return single("large file", format!("/{}", ignore_pattern_for(path)));
    }
    None
}

/// Whether `name` is an environment file such as `.env` or `.env.local`,
/// rather than a template such as `.env.example`.
fn is_env_file(name: &str) -> bool {
    name == ".env"
        || name.strip_prefix(".env.").is_some_and(|rest| {
            !ENV_TEMPLATES
                .iter()
                .any(|template| rest.ends_with(template))
        })
}

/// Remembers which marker files exist, so each directory is read once.
#[derive(Default)]
struct Markers {
    listings: HashMap<String, Vec<String>>,
}

impl Markers {
    /// Whether `parent` (relative to `top`) has one of `markers`; true for none.
    fn any(&mut self, top: &Path, parent: &str, markers: &[&str]) -> bool {
        if markers.is_empty() {
            return true;
        }
        let names = self.listings.entry(parent.to_owned()).or_insert_with(|| {
            fs::read_dir(top.join(lossless::to_path(parent)))
                .map(|entries| {
                    entries
                        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                        .collect()
                })
                .unwrap_or_default()
        });
        markers.iter().any(|marker| {
            if marker.starts_with('.') {
                names.iter().any(|name| name.ends_with(marker))
            } else {
                names.iter().any(|name| name == marker)
            }
        })
    }
}

/// The findings as lines to show, the first few in full.
pub fn list(findings: &[Finding]) -> String {
    let width = findings
        .iter()
        .take(MOST_LISTED)
        .map(|finding| names::shown(&finding.label).chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for finding in findings.iter().take(MOST_LISTED) {
        let mut details = vec![finding.reason.clone()];
        let files = finding.paths.len();
        if files > 1 {
            details.push(format!("{} files", thousands(files)));
        }
        if files > 1 || finding.bytes >= 1024 * 1024 {
            details.push(bytes(finding.bytes));
        }
        let label = names::shown(&finding.label);
        let _ = writeln!(out, "   {label:width$}  {}", details.join(", "));
    }
    if findings.len() > MOST_LISTED {
        let _ = writeln!(out, "   … and {} more", findings.len() - MOST_LISTED);
    }
    out.trim_end().to_owned()
}

/// Pathspecs that leave all the findings out when staging.
pub fn exclude(findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .flat_map(|finding| finding.exclude.iter().cloned())
        .collect()
}

/// A line of `.gitignore` can't hold a line break, and what's after one would
/// be a pattern of its own.
fn fits_gitignore(finding: &Finding) -> bool {
    !finding.pattern.chars().any(char::is_control)
}

/// Pathspecs that leave out the findings [`add_to_gitignore`] can't add. Only
/// those: `git add` refuses a pathspec that names an ignored file.
pub fn exclude_what_gitignore_cant_hold(findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .filter(|finding| !fits_gitignore(finding))
        .flat_map(|finding| finding.exclude.iter().cloned())
        .collect()
}

/// The paths of the findings that [`add_to_gitignore`] adds, which git should now ignore.
/// A `.gitignore` deeper down can still include them again, such as one that says `!*.pem`.
pub fn added_paths(findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .filter(|finding| fits_gitignore(finding))
        .flat_map(|finding| finding.paths.iter().cloned())
        .collect()
}

/// Adds the findings' patterns to the `.gitignore` at `top`, creating it if
/// needed, and returns the patterns it added.
pub fn add_to_gitignore(top: &Path, findings: &[Finding]) -> Result<Vec<String>> {
    let path = top.join(".gitignore");
    // git doesn't read a .gitignore that is a symbolic link, and writing through one would
    // change a file outside the repository.
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        bail!(
            "{} is a symbolic link, so git-agent won't add to it",
            path.display()
        );
    }
    let mut text = match fs::read(&path) {
        Ok(bytes) => lossless::to_text(&bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
    };
    let existing: Vec<&str> = text.lines().map(str::trim).collect();
    let mut added: Vec<String> = Vec::new();
    for finding in findings.iter().filter(|finding| fits_gitignore(finding)) {
        if !existing.contains(&finding.pattern.as_str()) && !added.contains(&finding.pattern) {
            added.push(finding.pattern.clone());
        }
    }
    if added.is_empty() {
        return Ok(added);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for pattern in &added {
        let _ = writeln!(text, "{pattern}");
    }
    fs::write(&path, lossless::to_bytes(&text))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Creates `files` under a new directory and returns it with the paths.
    fn tree(files: &[&str]) -> (TempDir, Vec<String>) {
        let dir = TempDir::new().unwrap();
        for file in files {
            let path = dir.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "x\n").unwrap();
        }
        let untracked = files.iter().map(|file| file.to_string()).collect();
        (dir, untracked)
    }

    fn summary(findings: &[Finding]) -> Vec<(&str, &str, &str, usize)> {
        findings
            .iter()
            .map(|finding| {
                (
                    finding.label.as_str(),
                    finding.reason.as_str(),
                    finding.pattern.as_str(),
                    finding.paths.len(),
                )
            })
            .collect()
    }

    #[test]
    fn writes_a_gitignore_line_that_covers_only_the_file_it_names() {
        assert_eq!(ignore_pattern_for("plain/name.pem"), "plain/name.pem");
        assert_eq!(ignore_pattern_for("k*y.pem"), "k\\*y.pem");
        assert_eq!(ignore_pattern_for("a\\b?[c].key"), "a\\\\b\\?\\[c\\].key");
        assert_eq!(ignore_pattern_for("space "), "space\\ ");

        let (dir, _repo) = crate::git::test_support::isolated_repo();
        let path = dir.path();
        fs::write(path.join("k*y.pem"), "key\n").unwrap();
        fs::write(path.join("kaaay.pem"), "my own\n").unwrap();
        let line = format!("/{}\n", ignore_pattern_for("k*y.pem"));
        fs::write(path.join(".gitignore"), line).unwrap();
        let ignored = |name: &str| {
            std::process::Command::new("git")
                .current_dir(path)
                .args(["check-ignore", "--quiet", name])
                .status()
                .unwrap()
                .success()
        };
        assert!(ignored("k*y.pem"));
        assert!(!ignored("kaaay.pem"));
    }

    #[test]
    fn keeps_a_name_with_a_line_break_out_of_gitignore_and_off_the_terminal() {
        let top = tempfile::TempDir::new().unwrap();
        let hostile = Finding {
            label: "secret.env\n*\u{1b}[31m".to_owned(),
            reason: "environment file".to_owned(),
            pattern: "/secret.env\n*".to_owned(),
            exclude: Vec::new(),
            paths: vec!["secret.env\n*".to_owned()],
            bytes: 1,
        };
        let plain = Finding {
            label: "other.env".to_owned(),
            pattern: "/other.env".to_owned(),
            ..hostile.clone()
        };
        let added = add_to_gitignore(top.path(), &[hostile.clone(), plain]).unwrap();
        assert_eq!(added, ["/other.env"]);
        assert_eq!(
            fs::read_to_string(top.path().join(".gitignore")).unwrap(),
            "/other.env\n"
        );
        let shown = list(&[hostile]);
        assert!(shown.contains("secret.env\\n*\\033[31m"), "{shown}");
        assert!(!shown.contains('\u{1b}'));
    }

    #[test]
    fn recognizes_build_output_by_its_project() {
        let (dir, untracked) = tree(&[
            "Cargo.toml",
            "target/debug/app",
            "target/debug/deps/libx.rlib",
            "web/package.json",
            "web/dist/index.js",
            "web/node_modules/left-pad/index.js",
            "docs/dist/manual.pdf",
            "src/main.rs",
        ]);
        let findings = check(dir.path(), &untracked);
        assert_eq!(
            summary(&findings),
            [
                ("target/", "Rust build output", "/target/", 2),
                ("web/dist/", "JavaScript build output", "/web/dist/", 1),
                ("web/node_modules/", "npm dependencies", "node_modules/", 1),
            ]
        );
        // Without a package.json next to it, docs/dist is somebody's documents.
        assert_eq!(findings[0].exclude, [":(top,exclude,literal)target"]);
        assert_eq!(
            findings[2].exclude,
            [":(top,exclude,literal)web/node_modules"]
        );
    }

    #[test]
    fn recognizes_files_by_name_and_kind() {
        let (dir, untracked) = tree(&[
            ".env",
            "api/.env.local",
            ".env.example",
            "keys/id_ed25519",
            "keys/id_ed25519.pub",
            "certs/server.pem",
            "lib/a.o",
            "lib/b.o",
            "pkg/__pycache__/mod.cpython-312.pyc",
            ".DS_Store",
            "docs/.DS_Store",
            "notes.txt",
        ]);
        let findings = check(dir.path(), &untracked);
        assert_eq!(
            summary(&findings),
            [
                (".env", "environment file; it may hold secrets", ".env", 1),
                (
                    "api/.env.local",
                    "environment file; it may hold secrets",
                    ".env.local",
                    1
                ),
                (
                    "keys/id_ed25519",
                    "private key or key store",
                    "/keys/id_ed25519",
                    1
                ),
                (
                    "certs/server.pem",
                    "private key or key store",
                    "/certs/server.pem",
                    1
                ),
                ("*.o", "compiled files", "*.o", 2),
                ("pkg/__pycache__/", "Python bytecode", "__pycache__/", 1),
                (".DS_Store", "operating system file", ".DS_Store", 2),
            ]
        );
        assert_eq!(findings[4].exclude, [":(top,exclude,glob)**/*.o"]);
        assert_eq!(findings[6].exclude, [":(top,exclude,glob)**/.DS_Store"]);
    }

    #[test]
    fn flags_large_files() {
        let (dir, untracked) = tree(&["data/small.csv", "data/dump.sql"]);
        let big = fs::File::create(dir.path().join("data/dump.sql")).unwrap();
        big.set_len(LARGE_FILE_BYTES + 1).unwrap();
        let findings = check(dir.path(), &untracked);
        assert_eq!(
            summary(&findings),
            [("data/dump.sql", "large file", "/data/dump.sql", 1)]
        );
        assert_eq!(list(&findings), "   data/dump.sql  large file, 10 MB");
    }

    #[test]
    fn lists_sizes_and_cuts_a_long_list() {
        let finding = |label: &str, files: usize, bytes: u64| Finding {
            label: label.to_owned(),
            reason: "Rust build output".to_owned(),
            pattern: String::new(),
            exclude: Vec::new(),
            paths: (0..files)
                .map(|number| format!("{label}{number}"))
                .collect(),
            bytes,
        };
        let findings = [
            finding("target/", 3412, 220 * 1024 * 1024),
            finding(".env", 1, 40),
        ];
        assert_eq!(
            list(&findings),
            "   target/  Rust build output, 3,412 files, 220 MB\n   .env     Rust build output"
        );
        let many: Vec<Finding> = (0..10).map(|n| finding(&format!("f{n}"), 1, 1)).collect();
        assert!(list(&many).ends_with("   … and 2 more"));
    }

    #[test]
    fn adds_each_pattern_to_gitignore_once() {
        let dir = TempDir::new().unwrap();
        let finding = |pattern: &str| Finding {
            label: String::new(),
            reason: String::new(),
            pattern: pattern.to_owned(),
            exclude: Vec::new(),
            paths: vec![pattern.to_owned()],
            bytes: 0,
        };
        let added = add_to_gitignore(dir.path(), &[finding("/target/"), finding(".env")]).unwrap();
        assert_eq!(added, ["/target/", ".env"]);
        assert_eq!(
            fs::read_to_string(dir.path().join(".gitignore")).unwrap(),
            "/target/\n.env\n"
        );

        fs::write(dir.path().join(".gitignore"), "*.log").unwrap();
        let added = add_to_gitignore(
            dir.path(),
            &[
                finding("*.log"),
                finding("node_modules/"),
                finding("node_modules/"),
            ],
        )
        .unwrap();
        assert_eq!(added, ["node_modules/"]);
        assert_eq!(
            fs::read_to_string(dir.path().join(".gitignore")).unwrap(),
            "*.log\nnode_modules/\n"
        );
    }

    /// A repository where names differ by case, as on Linux.
    fn case_sensitive_repo() -> (TempDir, crate::git::Repo) {
        let (dir, repo) = crate::git::test_support::isolated_repo();
        crate::git::test_support::run(dir.path(), &["config", "core.ignorecase", "false"]);
        (dir, repo)
    }

    #[test]
    fn keeps_the_case_of_an_extension_in_its_pattern() {
        let (_dir, untracked) = tree(&["SETUP.EXE", "low.exe", "n/DEEP.Exe", "a.O", "keep.txt"]);
        let findings = check(_dir.path(), &untracked);
        let patterns: Vec<&str> = findings.iter().map(|f| f.pattern.as_str()).collect();
        assert_eq!(patterns, ["*.EXE", "*.exe", "*.Exe", "*.O"]);
    }

    #[test]
    fn leaves_out_files_with_an_uppercase_extension_when_case_matters() {
        let (dir, repo) = case_sensitive_repo();
        let run = |args: &[&str]| crate::git::test_support::run(dir.path(), args);
        fs::create_dir(dir.path().join("n")).unwrap();
        for name in ["SETUP.EXE", "low.exe", "n/DEEP.Exe", "keep.txt"] {
            fs::write(dir.path().join(name), "x\n").unwrap();
        }
        let findings = check(dir.path(), &repo.untracked_files().unwrap());
        repo.stage_all_except(&exclude(&findings)).unwrap();
        assert_eq!(run(&["diff", "--cached", "--name-only"]), "keep.txt\n");

        run(&["reset", "-q"]);
        add_to_gitignore(dir.path(), &findings).unwrap();
        repo.stage_all_except(&exclude_what_gitignore_cant_hold(&findings))
            .unwrap();
        assert_eq!(
            run(&["diff", "--cached", "--name-only"]),
            ".gitignore\nkeep.txt\n"
        );
    }

    #[test]
    #[cfg(unix)]
    fn wont_write_through_a_gitignore_that_is_a_symbolic_link() {
        let (dir, _untracked) = tree(&["x.o"]);
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("target.txt");
        fs::write(&target, "mine\n").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(".gitignore")).unwrap();
        let findings = check(dir.path(), &["x.o".to_owned()]);
        let error = add_to_gitignore(dir.path(), &findings).unwrap_err();
        assert!(error.to_string().contains("symbolic link"), "{error}");
        assert_eq!(fs::read_to_string(&target).unwrap(), "mine\n");
    }

    #[test]
    fn keeps_a_gitignore_that_isnt_utf8_as_it_is() {
        let (dir, _untracked) = tree(&["x.o"]);
        let path = dir.path().join(".gitignore");
        fs::write(&path, b"caf\xe9.log\n").unwrap();
        let findings = check(dir.path(), &["x.o".to_owned()]);
        assert_eq!(add_to_gitignore(dir.path(), &findings).unwrap(), ["*.o"]);
        assert_eq!(fs::read(&path).unwrap(), b"caf\xe9.log\n*.o\n");
    }

    /// Writes a file whose name isn't valid UTF-8, or returns `false` where the file
    /// system refuses such a name, which only macOS's should.
    #[cfg(unix)]
    fn write_name_that_isnt_utf8(dir: &Path, name: &[u8], text: &str) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let path = dir.join(std::ffi::OsStr::from_bytes(name));
        if let Some(parent) = path.parent()
            && fs::create_dir_all(parent).is_err()
        {
            assert_eq!(
                std::env::consts::OS,
                "macos",
                "the file system refused {name:?}"
            );
            return false;
        }
        match fs::write(&path, text) {
            Ok(()) => true,
            Err(error) => {
                assert_eq!(
                    std::env::consts::OS,
                    "macos",
                    "couldn't write {name:?}: {error}"
                );
                false
            }
        }
    }

    #[test]
    #[cfg(unix)]
    fn leaves_out_a_private_key_whose_folder_name_isnt_utf8() {
        let (dir, repo) = case_sensitive_repo();
        let run = |args: &[&str]| crate::git::test_support::run(dir.path(), args);
        if !write_name_that_isnt_utf8(dir.path(), b"caf\xe9/id_rsa", "key\n") {
            return;
        }
        fs::write(dir.path().join("keep.txt"), "x\n").unwrap();
        let findings = check(dir.path(), &repo.untracked_files().unwrap());
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(
            list(&findings).contains("caf\u{fffd}/id_rsa"),
            "{findings:?}"
        );

        repo.stage_all_except(&exclude(&findings)).unwrap();
        assert_eq!(run(&["diff", "--cached", "--name-only"]), "keep.txt\n");

        run(&["reset", "-q"]);
        add_to_gitignore(dir.path(), &findings).unwrap();
        let line = fs::read(dir.path().join(".gitignore")).unwrap();
        assert!(line.windows(4).any(|part| part == b"caf\xe9"), "{line:?}");
        repo.stage_all_except(&exclude_what_gitignore_cant_hold(&findings))
            .unwrap();
        assert_eq!(
            run(&["diff", "--cached", "--name-only"]),
            ".gitignore\nkeep.txt\n"
        );
    }

    #[test]
    #[cfg(unix)]
    fn measures_a_large_file_whose_name_isnt_utf8() {
        let dir = TempDir::new().unwrap();
        let large = "x".repeat(LARGE_FILE_BYTES as usize);
        if !write_name_that_isnt_utf8(dir.path(), b"caf\xe9.bin", &large) {
            return;
        }
        let name = lossless::to_text(b"caf\xe9.bin");
        let findings = check(dir.path(), &[name]);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].reason, "large file");
        assert_eq!(findings[0].bytes, LARGE_FILE_BYTES);
    }
}
