//! Which module may use which, and the rules that keep the terminal and the model safe,
//! checked over the source.
//!
//! The modules form layers: the command line on top, then the features, the steps they
//! share, and the core. A feature uses shared steps and the core, never another feature.
//! Shared steps use the core, and one another only as [`SHARED_EDGES`] allows. The core uses
//! only the core. No module is part of a cycle. Every `crate::` path counts, outside comments
//! and strings, in tests too.
//!
//! [`KNOWN`] lists the uses that break a rule today. It may only shrink: the test also fails
//! when one of them is gone, so that it's taken off the list.

mod rust_source;

use rust_source::{
    attributes, code_only, crate_paths, lint_names, super_run, unfollowed_paths, without_tests,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// `main` and `cli`, which may use anything.
    Top,
    Feature,
    /// A step several features share, named after the module it belongs to.
    Shared(&'static str),
    Core,
}

use Layer::{Core, Feature, Shared, Top};

/// Every module and its layer.
const LAYERS: &[(&str, Layer)] = &[
    ("main", Top),
    ("cli", Top),
    ("branch", Feature),
    ("explain", Feature),
    ("menu", Feature),
    ("split", Feature),
    ("tidy", Feature),
    ("why", Feature),
    ("staging", Shared("staging")),
    ("untracked", Shared("staging")),
    ("redact", Shared("secrets")),
    ("conversation", Shared("conversation")),
    ("prompt", Shared("conversation")),
    ("api_keys", Core),
    ("config", Core),
    ("git", Core),
    ("github", Core),
    ("providers", Core),
    ("safe_text", Core),
    ("server", Core),
    ("temp", Core),
    ("ui", Core),
];

/// The shared steps that may use one another, as (user, used).
const SHARED_EDGES: &[(&str, &str)] = &[
    ("staging", "review"),
    ("delivery", "review"),
    ("delivery", "secrets"),
    ("review", "conversation"),
];

/// The uses that break a rule today, as (user, used), with the step of the refactoring that
/// ends each.
const KNOWN: &[(&str, &str)] = &[
    // Step 4: each feature owns its arguments.
    ("explain", "cli"),
    ("split", "cli"),
    // Step 3: the shared steps leave `main`.
    ("split", "main"),
    // Step 3: git's test of `stage_all_except` with untracked files moves to staging's tests.
    ("git", "untracked"),
    // Step 5: why's parsers of git's output move to `git/blame.rs`, and git's tests build their
    // line ranges themselves instead of with `why::Target` and `why::head_line`.
    ("git", "why"),
];

/// Lints that, allowed or expected, would let text reach the terminal past `ui`'s gate, or
/// hide a print.
const GATE_LINTS: [&str; 8] = [
    "clippy::disallowed_methods",
    "clippy::print_stdout",
    "clippy::print_stderr",
    "clippy::dbg_macro",
    "clippy::all",
    "clippy::style",
    "clippy::restriction",
    "warnings",
];

/// The module that does the checking. Its own tests hold paths in strings, and it may read
/// everything, so it's the one module the rules leave out.
const CHECKER: &str = "layers";

/// A source file: its text, its code without comments or the insides of strings, and that
/// code without its test items.
struct Source {
    module: String,
    path: PathBuf,
    /// Its path below `src/`, such as `split/command.rs`.
    file: String,
    /// Folders between `src/` and the file's module: 0 for `src/git.rs` and `src/server/mod.rs`.
    depth: usize,
    raw: String,
    code: String,
    /// The code outside tests: none in a test module's own file.
    outside_tests: String,
    /// The test modules it declares in files of their own, as `main` declares this one.
    test_modules: Vec<String>,
}

/// Every source file but the checker's. A test module's own file counts, all of it as tests.
fn sources() -> Vec<Source> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let all: Vec<Source> = rust_files(&src)
        .into_iter()
        .map(|path| read(&src, path))
        .filter(|source| source.module != CHECKER)
        .collect();
    let test_files: Vec<PathBuf> = all.iter().flat_map(test_module_paths).collect();
    all.into_iter()
        .map(|source| {
            if test_files.iter().any(|test| source.path.starts_with(test)) {
                Source {
                    outside_tests: String::new(),
                    ..source
                }
            } else {
                source
            }
        })
        .collect()
}

fn read(src: &Path, path: PathBuf) -> Source {
    let raw = fs::read_to_string(&path).unwrap();
    let code = code_only(&raw);
    let without = without_tests(&code);
    let relative = path.strip_prefix(src).unwrap();
    let name = relative
        .components()
        .next()
        .unwrap()
        .as_os_str()
        .to_string_lossy();
    let depth = relative.components().count() - 1 - usize::from(path.ends_with("mod.rs"));
    Source {
        module: name.strip_suffix(".rs").unwrap_or(&name).to_owned(),
        file: relative.to_string_lossy().into_owned(),
        depth,
        raw,
        code,
        outside_tests: without.code,
        test_modules: without.test_modules,
        path,
    }
}

/// The files of the test modules `source` declares with `#[cfg(test)] mod name;`: `name.rs`
/// and the folder `name`, next to where its own submodules live.
fn test_module_paths(source: &Source) -> Vec<PathBuf> {
    let declaring = &source.path;
    let folder = if declaring.ends_with("main.rs") || declaring.ends_with("mod.rs") {
        declaring.parent().unwrap().to_path_buf()
    } else {
        declaring.with_extension("")
    };
    source
        .test_modules
        .iter()
        .flat_map(|name| [folder.join(format!("{name}.rs")), folder.join(name)])
        .collect()
}

fn rust_files(folder: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(folder)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .flat_map(|path| {
            if path.is_dir() {
                rust_files(&path)
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                vec![path]
            } else {
                Vec::new()
            }
        })
        .collect();
    files.sort();
    files
}

fn layer_of(module: &str) -> Layer {
    LAYERS
        .iter()
        .find(|(name, _)| *name == module)
        .map(|(_, layer)| *layer)
        .unwrap_or_else(|| panic!("module {module} has no layer in LAYERS"))
}

fn may_use(user: Layer, used: Layer) -> bool {
    match (user, used) {
        (Top, _) => true,
        (_, Top | Feature) => false,
        (Shared(user), Shared(used)) => user == used || SHARED_EDGES.contains(&(user, used)),
        (Core, Shared(_)) => false,
        (Feature | Shared(_), _) | (Core, Core) => true,
    }
}

type Uses = BTreeSet<(String, String)>;

/// Every use of one module by another, as (user, used). A path to something that isn't a
/// module is to an item of the crate's root, `main`.
fn uses(sources: &[Source]) -> Uses {
    let modules: BTreeSet<&str> = LAYERS.iter().map(|(name, _)| *name).collect();
    sources
        .iter()
        .flat_map(|source| {
            crate_paths(&source.code).into_iter().map(|name| {
                let used = if modules.contains(name.as_str()) {
                    name
                } else {
                    "main".to_owned()
                };
                (source.module.clone(), used)
            })
        })
        .filter(|(user, used)| user != used)
        .collect()
}

fn known() -> Uses {
    KNOWN
        .iter()
        .map(|(user, used)| ((*user).to_owned(), (*used).to_owned()))
        .collect()
}

/// A cycle among `edges`, as the modules on it, if there's one.
fn cycle(edges: &Uses) -> Option<Vec<String>> {
    let mut graph: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (user, used) in edges {
        graph.entry(user).or_default().push(used);
    }
    let mut done = BTreeSet::new();
    graph
        .keys()
        .find_map(|start| path_back(&graph, start, &mut vec![*start], &mut done))
}

/// A path from the last module of `path` back to one already on it.
fn path_back<'a>(
    graph: &BTreeMap<&'a str, Vec<&'a str>>,
    module: &'a str,
    path: &mut Vec<&'a str>,
    done: &mut BTreeSet<&'a str>,
) -> Option<Vec<String>> {
    if done.contains(module) {
        return None;
    }
    for &next in graph.get(module).into_iter().flatten() {
        if let Some(start) = path.iter().position(|&on| on == next) {
            return Some(path[start..].iter().map(ToString::to_string).collect());
        }
        path.push(next);
        let found = path_back(graph, next, path, done);
        path.pop();
        if found.is_some() {
            return found;
        }
    }
    done.insert(module);
    None
}

/// The modules whose code outside tests holds `text`, with how often.
fn occurrences(sources: &[Source], text: &str) -> BTreeMap<String, usize> {
    let mut found = BTreeMap::new();
    for source in sources {
        let count = source.outside_tests.matches(text).count();
        if count > 0 {
            *found.entry(source.module.clone()).or_default() += count;
        }
    }
    found
}

/// The modules whose code outside tests uses `name`, such as `.complete` or `::complete`,
/// as a whole word, with how often: `.complete(` counts, `.complete_plan(` doesn't.
fn calls(sources: &[Source], name: &str) -> BTreeMap<String, usize> {
    let mut found = BTreeMap::new();
    for source in sources {
        let code = &source.outside_tests;
        let count = code
            .match_indices(name)
            .filter(|(index, _)| {
                let after = code[index + name.len()..].chars().next();
                !after.is_some_and(|character| character.is_alphanumeric() || character == '_')
            })
            .count();
        if count > 0 {
            *found.entry(source.module.clone()).or_default() += count;
        }
    }
    found
}

/// The files, below `src/`, whose code outside tests holds one of `texts`.
fn files_with(sources: &[Source], texts: &[&str]) -> BTreeSet<String> {
    sources
        .iter()
        .filter(|source| texts.iter().any(|text| source.outside_tests.contains(text)))
        .map(|source| source.file.clone())
        .collect()
}

fn modules_with(sources: &[Source], texts: &[&str]) -> BTreeSet<String> {
    texts
        .iter()
        .flat_map(|text| occurrences(sources, text).into_keys())
        .collect()
}

/// The lints among `lints` that code outside tests allows or expects, by module.
fn quieted(sources: &[Source], lints: &[&str]) -> Vec<(String, String)> {
    sources
        .iter()
        .flat_map(|source| {
            attributes(&source.outside_tests)
                .into_iter()
                .flat_map(|attribute| {
                    let mut names = lint_names(attribute.text, "allow");
                    names.extend(lint_names(attribute.text, "expect"));
                    names
                })
                .filter(|name| lints.contains(&name.as_str()))
                .map(|name| (source.module.clone(), name))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(ToString::to_string).collect()
}

mod tests {
    use super::*;

    #[test]
    fn every_module_has_a_layer() {
        let found: BTreeSet<String> = sources()
            .iter()
            .map(|source| source.module.clone())
            .collect();
        let listed = set(&LAYERS.iter().map(|(name, _)| *name).collect::<Vec<_>>());
        assert_eq!(
            found, listed,
            "LAYERS must name every module, and only those"
        );
    }

    #[test]
    fn every_use_follows_the_layers_or_is_known() {
        let broken: Uses = uses(&sources())
            .into_iter()
            .filter(|(user, used)| !may_use(layer_of(user), layer_of(used)))
            .collect();
        let known = known();
        let new: Vec<_> = broken.difference(&known).collect();
        let gone: Vec<_> = known.difference(&broken).collect();
        assert!(
            new.is_empty() && gone.is_empty(),
            "uses that break a rule and aren't in KNOWN: {new:?}; uses in KNOWN that are gone, \
             so take them off: {gone:?}"
        );
    }

    #[test]
    fn no_module_is_part_of_a_cycle_but_through_known_uses() {
        let known = known();
        let below_the_top: Uses = uses(&sources())
            .into_iter()
            .filter(|(user, _)| layer_of(user) != Top)
            .filter(|use_| !known.contains(use_))
            .collect();
        assert_eq!(cycle(&below_the_top), None);
    }

    #[test]
    fn the_reader_follows_every_path() {
        for source in sources() {
            let mut unfollowed = unfollowed_paths(&source.raw, &source.code);
            // A test module inside a file adds a level, which its own `super::` climbs back.
            let in_tests = source.depth + usize::from(!source.outside_tests.is_empty());
            if super_run(&source.outside_tests) > source.depth || super_run(&source.code) > in_tests
            {
                unfollowed.push("super:: above the module".to_owned());
            }
            assert!(
                unfollowed.is_empty(),
                "{}: {unfollowed:?}",
                source.path.display()
            );
        }
    }

    #[test]
    fn only_ui_may_write_to_the_terminal_and_it_has_no_submodules() {
        let sources = sources();
        assert_eq!(
            quieted(&sources, &GATE_LINTS),
            [("ui".to_owned(), "clippy::disallowed_methods".to_owned())]
        );
        let ui = sources.iter().find(|source| source.module == "ui").unwrap();
        assert!(ui.path.ends_with("src/ui.rs"), "ui must stay one file");
        assert!(
            !ui.outside_tests.contains("mod "),
            "ui's allow would reach its submodules"
        );
    }

    #[test]
    fn main_warns_about_printing_and_debugging() {
        let sources = sources();
        let main = sources
            .iter()
            .find(|source| source.module == "main")
            .unwrap();
        // Only an attribute that is the level itself counts: inside a `cfg_attr`, it wouldn't
        // hold for the build that ships.
        let warned: BTreeSet<String> = attributes(&main.outside_tests)
            .into_iter()
            .filter(|attribute| attribute.inner)
            .flat_map(|attribute| {
                ["warn", "deny", "forbid"]
                    .into_iter()
                    .filter(|level| {
                        attribute
                            .text
                            .trim_start()
                            .starts_with(&format!("{level}("))
                    })
                    .flat_map(|level| lint_names(attribute.text, level))
                    .collect::<Vec<_>>()
            })
            .collect();
        for lint in [
            "clippy::print_stdout",
            "clippy::print_stderr",
            "clippy::dbg_macro",
        ] {
            assert!(warned.contains(lint), "main must warn about {lint}");
        }
    }

    #[test]
    fn only_ui_draws_with_inquire_but_the_menu_for_now() {
        let drawing = modules_with(&sources(), &["inquire::", "use inquire"]);
        assert_eq!(drawing, set(&["menu", "ui"]));
    }

    #[test]
    fn one_place_sends_to_the_model() {
        let sources = sources();
        let mut sends = calls(&sources, ".complete");
        sends.remove("server");
        assert_eq!(sends, BTreeMap::from([("conversation".to_owned(), 1)]));
        let mut by_path = calls(&sources, "::complete");
        by_path.remove("server");
        assert!(by_path.is_empty(), "{by_path:?}");
    }

    #[test]
    fn one_place_pushes() {
        let sources = sources();
        assert_eq!(
            occurrences(&sources, ".push()"),
            BTreeMap::from([("main".to_owned(), 1)])
        );
        assert!(modules_with(&sources, &["Repo::push"]).is_empty());
    }

    #[test]
    fn only_server_talks_over_the_network() {
        let talking: BTreeSet<String> = sources()
            .iter()
            .filter(|source| {
                let code = &source.outside_tests;
                let http = code
                    .match_indices("reqwest")
                    .any(|(index, word)| !code[index + word.len()..].starts_with("::Url"));
                http || ["std::net", "TcpStream", "UdpSocket"]
                    .iter()
                    .any(|socket| code.contains(socket))
            })
            .map(|source| source.module.clone())
            .collect();
        assert_eq!(talking, set(&["server"]));
    }

    #[test]
    fn only_some_files_start_processes() {
        let starting = files_with(
            &sources(),
            &[
                "Command::new",
                "process::Command",
                ".spawn(",
                "isolated_command(",
            ],
        );
        let allowed = [
            "git.rs",
            "git/worktree.rs",
            "github.rs",
            "split/command.rs",
            "temp.rs",
        ];
        assert_eq!(starting, set(&allowed));
    }

    #[test]
    fn nothing_reaches_the_terminal_past_ui_by_its_file() {
        let sources = sources();
        let raw: BTreeSet<String> = sources
            .iter()
            .filter(|source| {
                ["/dev/tty", "/dev/stdout", "/dev/stderr"]
                    .iter()
                    .any(|device| source.raw.contains(device))
            })
            .map(|source| source.module.clone())
            .collect();
        assert!(raw.is_empty(), "{raw:?}");
        assert!(modules_with(&sources, &["FromRawFd", "from_raw_fd"]).is_empty());
    }

    #[test]
    fn the_gate_holds_outside_the_source_too() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let clippy = fs::read_to_string(root.join("clippy.toml")).unwrap();
        for handle in ["\"std::io::stdout\"", "\"std::io::stderr\""] {
            assert!(
                clippy.contains(handle),
                "clippy.toml must disallow {handle}"
            );
        }
        let cargo_folder = root.join(".cargo");
        let settings = [
            root.join("Cargo.toml"),
            cargo_folder.join("config.toml"),
            cargo_folder.join("config"),
        ];
        for path in settings.iter().filter(|path| path.exists()) {
            let text = fs::read_to_string(path).unwrap();
            let words: BTreeSet<String> = text
                .split(|character: char| !(character.is_alphanumeric() || character == '_'))
                .map(|word| word.replace('-', "_"))
                .collect();
            let named: Vec<&&str> = GATE_LINTS
                .iter()
                .filter(|lint| words.contains(lint.trim_start_matches("clippy::")))
                .collect();
            assert!(named.is_empty(), "{} names {named:?}", path.display());
        }
    }

    #[test]
    fn finds_a_cycle_and_its_modules() {
        let edges = |pairs: &[(&str, &str)]| -> Uses {
            pairs
                .iter()
                .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
                .collect()
        };
        assert_eq!(cycle(&edges(&[("a", "b"), ("b", "c")])), None);
        assert_eq!(
            cycle(&edges(&[("a", "b"), ("b", "c"), ("c", "a")])),
            Some(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()])
        );
    }

    #[test]
    fn lets_each_layer_use_only_what_it_may() {
        assert!(may_use(Top, Feature) && may_use(Feature, Core) && may_use(Core, Core));
        assert!(may_use(Shared("staging"), Shared("review")));
        assert!(!may_use(Shared("review"), Shared("staging")));
        assert!(!may_use(Feature, Feature) && !may_use(Feature, Top));
        assert!(!may_use(Core, Shared("conversation")));
    }
}
