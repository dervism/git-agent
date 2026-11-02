//! Checking untracked files before `--all` stages them.
//!
//! Build output, downloaded dependencies, compiled files, environment files,
//! private keys and large files are usually meant to be ignored, not
//! committed. A new project often lacks the `.gitignore` entries that keep
//! them out, and `git add --all` would commit them. No model is involved:
//! the rules below decide.

use crate::ui::{bytes, thousands};
use anyhow::{Context, Result};
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
    pub files: usize,
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
        let size = fs::symlink_metadata(top.join(path)).map_or(0, |meta| meta.len());
        let index = *by_key.entry(key).or_insert_with(|| {
            findings.push(Finding {
                label,
                reason,
                pattern,
                exclude: Vec::new(),
                files: 0,
                bytes: 0,
            });
            findings.len() - 1
        });
        let finding = &mut findings[index];
        if !finding.exclude.contains(&leave_out) {
            finding.exclude.push(leave_out);
        }
        finding.files += 1;
        finding.bytes += size;
    }
    findings
}

/// What `classify` returns: the key that groups paths into one finding, its
/// label, reason and `.gitignore` pattern, and the pathspec that leaves the
/// path out when staging.
type Classified = (String, String, String, String, String);

/// A pathspec that leaves out exactly `path`, a file or a directory.
fn literal(path: &str) -> String {
    format!(":(top,exclude,literal){path}")
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
            format!("/{directory}/")
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
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_lowercase());
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
    if let Some(extension) = extension
        .as_deref()
        .filter(|extension| COMPILED.contains(extension))
    {
        return kind("compiled files", format!("*.{extension}"));
    }
    if is_env_file(name) {
        return single("environment file; it may hold secrets", name.to_owned());
    }
    if SSH_KEYS.contains(&name)
        || extension
            .as_deref()
            .is_some_and(|extension| KEY_FILES.contains(&extension))
    {
        return single("private key or key store", format!("/{path}"));
    }
    let size = fs::symlink_metadata(top.join(path)).map_or(0, |meta| meta.len());
    if size >= LARGE_FILE_BYTES {
        return single("large file", format!("/{path}"));
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
            fs::read_dir(top.join(parent))
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
        .map(|finding| finding.label.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for finding in findings.iter().take(MOST_LISTED) {
        let mut details = vec![finding.reason.clone()];
        if finding.files > 1 {
            details.push(format!("{} files", thousands(finding.files)));
        }
        if finding.files > 1 || finding.bytes >= 1024 * 1024 {
            details.push(bytes(finding.bytes));
        }
        let _ = writeln!(out, "   {:width$}  {}", finding.label, details.join(", "));
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

/// Adds the findings' patterns to the `.gitignore` at `top`, creating it if
/// needed, and returns the patterns it added.
pub fn add_to_gitignore(top: &Path, findings: &[Finding]) -> Result<Vec<String>> {
    let path = top.join(".gitignore");
    let mut text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
    };
    let existing: Vec<&str> = text.lines().map(str::trim).collect();
    let mut added: Vec<String> = Vec::new();
    for finding in findings {
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
    fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))?;
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
                    finding.files,
                )
            })
            .collect()
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
            files,
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
            files: 1,
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
}
