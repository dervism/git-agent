//! What git-agent checks and stages before it reads the changes to commit.

use crate::git::{IndexEntry, Repo, names, shell_quote};
use crate::{ui, untracked};
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::path::Path;

/// Most names to list in a message; the rest are counted.
const MOST_LISTED: usize = 8;

/// Fails if the index holds conflicts that aren't resolved, naming the files. This has to
/// come before `--all`, because `git add --all` marks a conflict as resolved, markers and
/// all. `doing` says what can't go ahead, such as "committing".
pub fn refuse_unresolved_conflicts(repo: &Repo, doing: &str) -> Result<()> {
    let conflicted = repo.unmerged_paths()?;
    let summary = match conflicted.len() {
        0 => return Ok(()),
        1 => format!("1 file has unresolved conflicts; resolve them before {doing}"),
        count => format!("{count} files have unresolved conflicts; resolve them before {doing}"),
    };
    bail!("{summary}:\n{}", listed(&conflicted))
}

/// The first few names, one per line.
fn listed(names_to_show: &[String]) -> String {
    let mut lines: Vec<String> = names_to_show
        .iter()
        .take(MOST_LISTED)
        .map(|name| format!("   {}", names::shown(name)))
        .collect();
    if names_to_show.len() > MOST_LISTED {
        lines.push(format!(
            "   … and {} more",
            names_to_show.len() - MOST_LISTED
        ));
    }
    lines.join("\n")
}

/// Stages everything for a command's `--all`, after checking the untracked files for
/// build output, dependencies, secrets and large files that a `.gitignore`
/// should keep out. With `ask`, the user decides what happens to them;
/// otherwise they are left out.
///
/// A hidden submodule pointer the user staged stays exactly as they staged it, as does a
/// staged removal of one, whatever the work tree holds now; those the work tree has moved on
/// from get a note. `splitting` says whether the note is for a split. Returns `false`, having
/// said so, if the user quit.
pub fn stage_all_and_report(repo: &Repo, ask: bool, splitting: bool) -> Result<bool> {
    let Some(kept) = stage_all(repo, ask)? else {
        ui::info("Nothing staged or committed.");
        return Ok(false);
    };
    report_kept(&kept, splitting);
    Ok(true)
}

/// Stages everything as [`stage_all_and_report`] describes. Returns `None` if the user quit,
/// and otherwise the pointers it kept that the work tree has moved on from.
fn stage_all(repo: &Repo, ask: bool) -> Result<Option<Kept>> {
    let Some(staged) = stage_all_but_findings(repo, ask)? else {
        return Ok(None);
    };
    let kept: Vec<String> = staged.into_iter().map(|entry| entry.path).collect();
    leave_out_hidden_pointers(repo, &kept)?;
    kept_and_moved_on(repo, &kept).map(Some)
}

/// Stages everything but the untracked files that should be ignored, as
/// [`stage_all_and_report`] describes. Returns `None` if the user quit, and otherwise the hidden entries the user had
/// staged, which it kept.
fn stage_all_but_findings(repo: &Repo, ask: bool) -> Result<Option<Vec<IndexEntry>>> {
    let top = repo.toplevel()?;
    let findings = untracked::check(&top, &repo.untracked_files()?);
    if findings.is_empty() {
        return add_all(repo, &[]).map(Some);
    }
    ui::warn("some untracked files look like they should be ignored rather than committed:");
    ui::info(untracked::list(&findings));
    if !ask {
        let staged = add_all(repo, &untracked::exclude(&findings))?;
        ensure_left_out(repo, &findings)?;
        ui::info(ui::dim(
            "   git-agent left them out. Add them to .gitignore, or stage them yourself with git add.",
        ));
        return Ok(Some(staged));
    }
    let staged = match ui::ask_untracked()? {
        ui::UntrackedChoice::Ignore => {
            let staged = ignore_and_stage_the_rest(repo, &top, &findings)?;
            ensure_left_out(repo, &findings)?;
            staged
        }
        ui::UntrackedChoice::Commit => add_all(repo, &[])?,
        ui::UntrackedChoice::Quit => return Ok(None),
    };
    Ok(Some(staged))
}

/// Runs `git add --all`, except on what the `exclude` pathspecs name, and then puts back the
/// hidden submodule entries the user staged, which git would otherwise stage again at wherever
/// the work tree has them. They're read just before the add, and put back even if it fails.
/// Returns them.
fn add_all(repo: &Repo, exclude: &[String]) -> Result<Vec<IndexEntry>> {
    let staged = repo.hidden_staged_entries()?;
    let added = repo.stage_all_except(exclude);
    repo.restore_index_entries(&staged)
        .with_context(|| how_to_put_back(&staged))?;
    added.map(|()| staged)
}

/// What to run to put `entries` back by hand, for when git-agent couldn't.
fn how_to_put_back(entries: &[IndexEntry]) -> String {
    let commands: Vec<String> = entries
        .iter()
        .map(|entry| {
            let path = names::shown(&entry.path);
            if entry.is_removal() {
                format!(
                    "   git update-index --force-remove -- {}",
                    shell_quote(&path)
                )
            } else {
                let info = format!("{},{},{path}", entry.mode, entry.object);
                format!("   git update-index --cacheinfo {}", shell_quote(&info))
            }
        })
        .collect();
    format!(
        "git-agent couldn't put back the submodule pointers you staged; to do it yourself:\n{}",
        commands.join("\n")
    )
}

/// The pointers in `kept` that the work tree no longer has where the user staged them: those
/// whose submodule has moved to another commit, and those whose folder is gone or is something
/// else now.
#[derive(Debug, Default, PartialEq)]
struct Kept {
    moved_on: Vec<String>,
    replaced: Vec<String>,
}

fn kept_and_moved_on(repo: &Repo, kept: &[String]) -> Result<Kept> {
    if kept.is_empty() {
        return Ok(Kept::default());
    }
    let changed = repo.changed_in_work_tree()?;
    let modified = repo.modified_in_work_tree()?;
    let (moved_on, replaced) = kept
        .iter()
        .filter(|path| changed.contains(path))
        .cloned()
        .partition(|path| modified.contains(path));
    Ok(Kept { moved_on, replaced })
}

/// Says which pointers `--all` kept where the user staged them though the work tree has moved
/// on, as [`stage_all`] returns them. A commit holds the staged commit, and a split leaves the
/// pointer staged.
fn report_kept(kept: &Kept, splitting: bool) {
    if !kept.moved_on.is_empty() {
        ui::info(moved_on_note(&kept.moved_on, splitting));
    }
    if !kept.replaced.is_empty() {
        ui::info(replaced_note(&kept.replaced));
    }
}

/// Takes out of the index again the submodule pointers that `git add --all` staged though
/// the diff hides them, so that neither the model nor the user would see them in the commit.
/// Those in `staged_before` stay: the user staged them.
fn leave_out_hidden_pointers(repo: &Repo, staged_before: &[String]) -> Result<()> {
    let added: Vec<String> = repo
        .hidden_staged_pointers()?
        .into_iter()
        .filter(|pointer| !staged_before.contains(pointer))
        .collect();
    if added.is_empty() {
        return Ok(());
    }
    match repo.resolve("HEAD")? {
        Some(head) => repo.reset_paths(&head, &added)?,
        // Before the first commit, they're new in the index.
        None => repo.unstage_new_files(&added)?,
    }
    ui::info(left_out_by_all_note(&added));
    Ok(())
}

/// Fails if git staged any of the files in `findings` after all, taking them out of the
/// index again first. The rules above should keep them out; this makes sure, since a key
/// that git-agent said it left out must not end up in a commit.
fn ensure_left_out(repo: &Repo, findings: &[untracked::Finding]) -> Result<()> {
    let staged: HashSet<String> = repo.staged_paths()?.into_iter().collect();
    let leaked: Vec<String> = findings
        .iter()
        .flat_map(|finding| finding.paths.iter())
        .filter(|path| staged.contains(*path))
        .cloned()
        .collect();
    if leaked.is_empty() {
        return Ok(());
    }
    repo.unstage_new_files(&leaked)?;
    bail!(
        "git staged files that git-agent meant to leave out, so it took them out again and \
         stopped; check the .gitignore files that apply to them:\n{}",
        listed(&leaked)
    )
}

/// Adds `findings` to `.gitignore`, and stages everything else. A file that another
/// `.gitignore` includes again, such as one in a folder that says `!*.pem`, is left out by
/// name, since the new line doesn't keep it out.
fn ignore_and_stage_the_rest(
    repo: &Repo,
    top: &Path,
    findings: &[untracked::Finding],
) -> Result<Vec<IndexEntry>> {
    let added = untracked::add_to_gitignore(top, findings)?;
    if !added.is_empty() {
        let shown: Vec<String> = added.iter().map(|pattern| names::shown(pattern)).collect();
        ui::info(format!("📝 Added to .gitignore: {}", shown.join(", ")));
    }
    let meant_to_be_ignored = untracked::added_paths(findings);
    let ignored: HashSet<String> = repo.ignored(&meant_to_be_ignored)?.into_iter().collect();
    let included_again: Vec<String> = meant_to_be_ignored
        .into_iter()
        .filter(|path| !ignored.contains(path))
        .collect();
    if !included_again.is_empty() {
        ui::warn(format!(
            "another .gitignore includes these again, so git-agent left them out by name:\n{}",
            listed(&included_again)
        ));
    }
    let mut exclude = untracked::exclude_what_gitignore_cant_hold(findings);
    exclude.extend(included_again.iter().map(|path| untracked::literal(path)));
    add_all(repo, &exclude)
}

/// Submodule pointers that a commit would hold though the diff hides them, so that neither
/// the model nor the user saw them.
#[derive(Debug, Default, PartialEq)]
pub struct HiddenPointers {
    /// Staged ones, which any commit of what's staged holds.
    pub staged: Vec<String>,
    /// Moved ones in the work tree, which `git commit --all` adds.
    pub changed: Vec<String>,
}

/// The hidden pointers a commit would hold. `commits_all` says whether it's made with
/// `git commit --all`, as git-agent's own commit of tracked changes is.
pub fn hidden_pointers(repo: &Repo, commits_all: bool) -> Result<HiddenPointers> {
    Ok(HiddenPointers {
        staged: repo.hidden_staged_pointers()?,
        changed: if commits_all {
            repo.hidden_changed_pointers()?
        } else {
            Vec::new()
        },
    })
}

/// Says what happens to the [`hidden_pointers`]: `git agent` commits those the user
/// staged, and leaves the others out. A split leaves them all out, since its commits only
/// hold what its plan shows.
pub fn report_hidden_pointers(hidden: &HiddenPointers, splitting: bool) {
    if !hidden.staged.is_empty() {
        ui::warn(hidden_staged_note(&hidden.staged, splitting));
    }
    if !hidden.changed.is_empty() {
        ui::info(hidden_changed_note(&hidden.changed));
    }
}

const HIDDEN: &str = "a submodule whose `ignore` setting hides the change from the diff";

fn hidden_staged_note(pointers: &[String], splitting: bool) -> String {
    let what_happens = if splitting {
        "The split's commits leave it out, and it stays staged.".to_owned()
    } else {
        format!(
            "You staged it, so the commit includes it; `git restore --staged -- {}` takes it out.",
            quoted(pointers)
        )
    };
    format!(
        "{} is staged, in {HIDDEN}, so the model doesn't see it. {what_happens}",
        joined_names(pointers)
    )
}

fn hidden_changed_note(pointers: &[String]) -> String {
    format!(
        "{} changed, in {HIDDEN}, so git-agent leaves it out of the commit. To commit it, stage \
         it with `git add -- {}` and run git-agent again.",
        joined_names(pointers),
        quoted(pointers)
    )
}

fn left_out_by_all_note(pointers: &[String]) -> String {
    format!(
        "   --all left out {}, in {HIDDEN}. To commit it, stage it with `git add -- {}`.",
        joined_names(pointers),
        quoted(pointers)
    )
}

fn replaced_note(pointers: &[String]) -> String {
    format!(
        "   --all kept {} as you staged it, though the work tree has something else there now.",
        joined_names(pointers)
    )
}

fn moved_on_note(pointers: &[String], splitting: bool) -> String {
    let kept = format!(
        "   --all kept {} at the commit you staged, though the submodule has moved on since.",
        joined_names(pointers)
    );
    if splitting {
        return kept;
    }
    format!(
        "{kept} To commit where it is now, stage it with `git add -- {}`.",
        quoted(pointers)
    )
}

fn quoted(pointers: &[String]) -> String {
    let quoted: Vec<String> = pointers
        .iter()
        .map(|name| shell_quote(&names::shown(name)))
        .collect();
    quoted.join(" ")
}

fn joined_names(pointers: &[String]) -> String {
    let shown: Vec<String> = pointers.iter().map(|name| names::shown(name)).collect();
    shown.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support::{isolated_repo, run};
    use std::fs;

    #[test]
    fn keeps_a_file_that_gitignore_cant_name_out_of_what_it_stages() {
        let (dir, repo) = isolated_repo();
        fs::write(dir.path().join("x\ny.pem"), "key\n").unwrap();
        fs::write(dir.path().join("plain.pem"), "key\n").unwrap();
        fs::write(dir.path().join("other.txt"), "text\n").unwrap();
        let untracked = repo.untracked_files().unwrap();
        let findings = untracked::check(dir.path(), &untracked);
        assert_eq!(findings.len(), 2, "{findings:?}");

        ignore_and_stage_the_rest(&repo, dir.path(), &findings).unwrap();
        let staged = run(dir.path(), &["diff", "--cached", "--name-only", "-z"]);
        let staged: Vec<&str> = staged.split('\0').filter(|name| !name.is_empty()).collect();
        assert_eq!(staged, [".gitignore", "other.txt"]);
        assert_eq!(
            fs::read_to_string(dir.path().join(".gitignore")).unwrap(),
            "/plain.pem\n"
        );
    }

    #[test]
    fn leaves_out_a_key_that_another_gitignore_includes_again() {
        let (dir, repo) = isolated_repo();
        fs::create_dir(dir.path().join("keys")).unwrap();
        fs::write(dir.path().join("keys/.gitignore"), "!*.pem\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "fixtures"]);
        fs::write(dir.path().join("keys/id.pem"), "key\n").unwrap();
        fs::write(dir.path().join("keys/id.p12"), "key\n").unwrap();
        fs::write(dir.path().join("a.o"), "x\n").unwrap();
        fs::write(dir.path().join("other.txt"), "text\n").unwrap();
        let findings = untracked::check(dir.path(), &repo.untracked_files().unwrap());

        ignore_and_stage_the_rest(&repo, dir.path(), &findings).unwrap();
        ensure_left_out(&repo, &findings).unwrap();
        assert_eq!(repo.staged_paths().unwrap(), [".gitignore", "other.txt"]);
    }

    #[test]
    fn leaves_out_what_it_should_when_run_from_a_subfolder() {
        let (dir, _repo) = isolated_repo();
        for folder in ["keys", "sub"] {
            fs::create_dir(dir.path().join(folder)).unwrap();
        }
        fs::write(dir.path().join("keys/.gitignore"), "!*.pem\n").unwrap();
        fs::write(dir.path().join("sub/keep.txt"), "x\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "fixtures"]);
        fs::write(dir.path().join("keys/id.pem"), "key\n").unwrap();
        fs::write(dir.path().join("id_rsa"), "key\n").unwrap();
        fs::write(dir.path().join("sub/other.txt"), "text\n").unwrap();
        let repo = Repo::open(dir.path().join("sub")).unwrap();
        let findings = untracked::check(dir.path(), &repo.untracked_files().unwrap());

        ignore_and_stage_the_rest(&repo, dir.path(), &findings).unwrap();
        ensure_left_out(&repo, &findings).unwrap();
        assert_eq!(
            repo.staged_paths().unwrap(),
            [".gitignore", "sub/other.txt"]
        );

        run(dir.path(), &["add", "-f", "keys/id.pem", "id_rsa"]);
        let error = ensure_left_out(&repo, &findings).unwrap_err();
        assert!(error.to_string().contains("   keys/id.pem"), "{error}");
        assert_eq!(
            repo.staged_paths().unwrap(),
            [".gitignore", "sub/other.txt"]
        );
    }

    #[test]
    fn takes_a_file_it_meant_to_leave_out_out_of_the_index_and_stops() {
        let (dir, repo) = isolated_repo();
        fs::write(dir.path().join("id_rsa"), "key\n").unwrap();
        fs::write(dir.path().join("other.txt"), "text\n").unwrap();
        let findings = untracked::check(dir.path(), &repo.untracked_files().unwrap());
        repo.stage_all_except(&[]).unwrap();

        let error = ensure_left_out(&repo, &findings).unwrap_err();
        assert!(error.to_string().contains("   id_rsa"), "{error}");
        assert_eq!(repo.staged_paths().unwrap(), ["other.txt"]);
        assert!(dir.path().join("id_rsa").exists());
    }

    #[test]
    fn finds_the_pointers_that_each_kind_of_commit_would_hold() {
        let (dir, repo, _library, moved) = crate::git::test_support::with_submodule();
        run(dir.path(), &["config", "submodule.library.ignore", "all"]);
        crate::git::test_support::move_pointer(dir.path(), &moved);
        let library = vec!["library".to_owned()];
        assert_eq!(
            hidden_pointers(&repo, true).unwrap(),
            HiddenPointers {
                staged: Vec::new(),
                changed: library.clone()
            }
        );
        assert_eq!(
            hidden_pointers(&repo, false).unwrap(),
            HiddenPointers::default()
        );
        run(dir.path(), &["add", "--all"]);
        for commits_all in [true, false] {
            assert_eq!(hidden_pointers(&repo, commits_all).unwrap().staged, library);
        }
    }

    #[test]
    fn all_leaves_out_a_hidden_pointer_it_would_stage_and_keeps_one_the_user_staged() {
        let (dir, repo, _library, moved) = crate::git::test_support::with_submodule();
        run(dir.path(), &["config", "submodule.library.ignore", "all"]);
        crate::git::test_support::move_pointer(dir.path(), &moved);
        fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        assert!(stage_all(&repo, false).unwrap().is_some());
        assert_eq!(repo.staged_paths().unwrap(), ["new.txt"]);
        assert_eq!(repo.hidden_changed_pointers().unwrap(), ["library"]);

        run(dir.path(), &["add", "library"]);
        assert_eq!(stage_all(&repo, false).unwrap(), Some(Kept::default()));
        assert_eq!(repo.hidden_staged_pointers().unwrap(), ["library"]);
    }

    #[test]
    fn leaves_out_a_hidden_pointer_before_the_first_commit() {
        let dir = tempfile::TempDir::new().unwrap();
        run(dir.path(), &["init", "-q", "--initial-branch", "main"]);
        run(dir.path(), &["config", "diff.ignoreSubmodules", "all"]);
        let inner = dir.path().join("inner");
        fs::create_dir(&inner).unwrap();
        run(&inner, &["init", "-q", "--initial-branch", "main"]);
        run(
            &inner,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "one",
            ],
        );
        fs::write(dir.path().join("f.txt"), "f\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();

        assert!(stage_all(&repo, false).unwrap().is_some());
        assert_eq!(repo.staged_paths().unwrap(), ["f.txt"]);
    }

    #[test]
    fn tells_a_split_that_it_leaves_a_staged_pointer_out() {
        let names = ["library".to_owned()];
        assert!(hidden_staged_note(&names, true).contains("The split's commits leave it out"));
        assert!(hidden_staged_note(&names, false).contains("so the commit includes it"));
    }

    #[test]
    fn says_nothing_without_conflicts() {
        let (_dir, repo) = isolated_repo();
        refuse_unresolved_conflicts(&repo, "committing").unwrap();
    }

    #[test]
    fn lists_at_most_a_few_names() {
        let few: Vec<String> = (1..=2).map(|number| format!("{number}.txt")).collect();
        assert_eq!(listed(&few), "   1.txt\n   2.txt");
        let many: Vec<String> = (1..=10).map(|number| format!("{number}.txt")).collect();
        assert!(listed(&many).ends_with("   8.txt\n   … and 2 more"));
    }

    #[test]
    fn shows_names_safely_in_its_lists_and_warnings() {
        let odd = ["a\nb\u{1b}[2J".to_owned()];
        assert_eq!(listed(&odd), "   a\\nb\\033[2J");
        assert!(hidden_changed_note(&odd).starts_with("a\\nb\\033[2J changed"));
        assert!(hidden_staged_note(&odd, false).starts_with("a\\nb\\033[2J is staged"));
        assert!(left_out_by_all_note(&odd).contains("left out a\\nb\\033[2J,"));
        assert!(moved_on_note(&odd, false).contains("kept a\\nb\\033[2J at the commit"));
        assert!(moved_on_note(&odd, false).contains("git add -- 'a\\nb\\033[2J'"));
        assert!(replaced_note(&odd).contains("kept a\\nb\\033[2J as you staged it"));
    }

    #[test]
    fn says_how_to_put_back_a_pointer_and_a_removal_by_hand() {
        let entry = |mode: &str, object: &str, path: &str| IndexEntry {
            mode: mode.to_owned(),
            object: object.to_owned(),
            path: path.to_owned(),
        };
        let message = how_to_put_back(&[
            entry("160000", "abc", "my lib"),
            entry("000000", "000", "gone"),
        ]);
        assert!(
            message.ends_with(
                "\n   git update-index --cacheinfo '160000,abc,my lib'\
                 \n   git update-index --force-remove -- 'gone'"
            ),
            "{message}"
        );
    }

    #[test]
    fn all_says_what_the_user_staged_when_it_cant_put_it_back() {
        let (dir, repo, _library, moved) = crate::git::test_support::with_submodule();
        run(dir.path(), &["config", "submodule.library.ignore", "all"]);
        crate::git::test_support::move_pointer(dir.path(), &moved);
        run(dir.path(), &["add", "library"]);
        let lock = dir.path().join(".git/index.lock");
        fs::write(&lock, "").unwrap();

        let error = format!("{:#}", stage_all(&repo, false).unwrap_err());
        fs::remove_file(&lock).unwrap();
        assert!(
            error.contains(&format!("--cacheinfo '160000,{moved},library'")),
            "{error}"
        );
        assert_eq!(staged_at(dir.path(), "library"), moved);
    }

    #[test]
    fn tells_a_commit_how_to_take_the_newer_checkout_and_a_split_only_what_it_kept() {
        let names = ["library".to_owned()];
        assert!(moved_on_note(&names, false).contains("To commit where it is now"));
        let split = moved_on_note(&names, true);
        assert!(split.ends_with("has moved on since."), "{split}");
        assert!(!split.contains("git add"), "{split}");
    }

    /// The commit the index holds for the submodule pointer `name`.
    fn staged_at(dir: &Path, name: &str) -> String {
        let entry = run(
            dir,
            &[
                "ls-files",
                "--stage",
                "-z",
                "--",
                &format!(":(literal){name}"),
            ],
        );
        let fields: Vec<&str> = entry.split_whitespace().collect();
        assert_eq!(fields.first(), Some(&"160000"), "{entry:?}");
        fields[1].to_owned()
    }

    /// Makes a repository at `name` inside `dir` with a commit, stages it there as a submodule
    /// pointer, and then commits again in it. Returns the staged commit and the newer one.
    fn staged_then_moved_on(dir: &Path, name: &str) -> (String, String) {
        let inner = dir.join(name);
        fs::create_dir_all(&inner).unwrap();
        run(&inner, &["init", "-q", "--initial-branch", "main"]);
        let commit = |message: &str| {
            run(
                &inner,
                &[
                    "-c",
                    "user.name=T",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=no-hooks",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    message,
                ],
            );
            run(&inner, &["rev-parse", "HEAD"]).trim().to_owned()
        };
        let staged = commit("one");
        run(dir, &["add", "--", &format!(":(literal){name}")]);
        (staged, commit("two"))
    }

    /// The submodule `library`, hidden by `setting`, with its pointer staged at a newer commit
    /// and its checkout moved back to the one `HEAD` holds. Returns the staged commit.
    fn library_staged_then_moved_back(dir: &Path, moved: &str, setting: &str) -> String {
        run(dir, &["config", setting, "all"]);
        let in_head = staged_at(dir, "library");
        crate::git::test_support::move_pointer(dir, moved);
        run(dir, &["add", "library"]);
        run(&dir.join("library"), &["checkout", "-q", &in_head]);
        staged_at(dir, "library")
    }

    #[test]
    fn all_keeps_a_staged_pointer_where_the_user_staged_it_whatever_hides_it() {
        for setting in ["submodule.library.ignore", "diff.ignoreSubmodules"] {
            let (dir, repo, _library, moved) = crate::git::test_support::with_submodule();
            let staged = library_staged_then_moved_back(dir.path(), &moved, setting);
            assert_eq!(staged, moved, "{setting}");
            fs::write(dir.path().join("new.txt"), "new\n").unwrap();

            let kept = stage_all(&repo, false).unwrap().unwrap();
            assert_eq!(kept.moved_on, ["library"], "{setting}");
            assert!(kept.replaced.is_empty(), "{setting}");
            assert_eq!(staged_at(dir.path(), "library"), moved, "{setting}");
            assert!(repo.staged_paths().unwrap().contains(&"new.txt".to_owned()));
        }
    }

    #[test]
    fn all_keeps_a_staged_pointer_when_run_from_a_subfolder() {
        let (dir, _repo, _library, moved) = crate::git::test_support::with_submodule();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/keep.txt"), "x\n").unwrap();
        run(dir.path(), &["add", "sub"]);
        run(dir.path(), &["commit", "-q", "-m", "sub"]);
        library_staged_then_moved_back(dir.path(), &moved, "submodule.library.ignore");
        fs::write(dir.path().join("sub/new.txt"), "new\n").unwrap();
        let repo = Repo::open(dir.path().join("sub")).unwrap();

        assert!(stage_all(&repo, false).unwrap().is_some());
        assert_eq!(staged_at(dir.path(), "library"), moved);
        assert!(
            repo.staged_paths()
                .unwrap()
                .contains(&"sub/new.txt".to_owned())
        );
    }

    #[test]
    fn all_keeps_staged_pointers_with_odd_names_before_the_first_commit() {
        let dir = tempfile::TempDir::new().unwrap();
        run(dir.path(), &["init", "-q", "--initial-branch", "main"]);
        run(dir.path(), &["config", "diff.ignoreSubmodules", "all"]);
        let names = ["my lib*æøå", "a\nb", "tab\there"];
        let staged: Vec<(&str, String)> = names
            .iter()
            .map(|name| (*name, staged_then_moved_on(dir.path(), name).0))
            .collect();
        fs::write(dir.path().join("my libXæøå"), "a file a glob would match\n").unwrap();
        let repo = Repo::open(dir.path()).unwrap();

        let kept = stage_all(&repo, false).unwrap().unwrap();
        assert_eq!(kept.moved_on.len(), names.len(), "{kept:?}");
        for (name, commit) in &staged {
            assert_eq!(&staged_at(dir.path(), name), commit, "{name:?}");
        }
        assert_eq!(repo.staged_paths().unwrap(), ["my libXæøå"]);
    }

    #[test]
    fn all_keeps_a_staged_pointer_beside_untracked_files_it_leaves_out() {
        let (dir, repo, _library, moved) = crate::git::test_support::with_submodule();
        library_staged_then_moved_back(dir.path(), &moved, "submodule.library.ignore");
        fs::write(dir.path().join("id_rsa"), "key\n").unwrap();
        fs::write(dir.path().join("new.txt"), "new\n").unwrap();

        assert!(stage_all(&repo, false).unwrap().is_some());
        assert_eq!(staged_at(dir.path(), "library"), moved);
        let staged = repo.staged_paths().unwrap();
        assert!(staged.contains(&"new.txt".to_owned()), "{staged:?}");
        assert!(!staged.contains(&"id_rsa".to_owned()), "{staged:?}");
    }

    #[test]
    fn all_says_nothing_of_a_kept_pointer_whose_submodule_is_where_it_was_staged() {
        let (dir, repo, _library, moved) = crate::git::test_support::with_submodule();
        run(dir.path(), &["config", "submodule.library.ignore", "all"]);
        crate::git::test_support::move_pointer(dir.path(), &moved);
        run(dir.path(), &["add", "library"]);
        fs::write(dir.path().join("library/loose.txt"), "dirty\n").unwrap();

        assert_eq!(stage_all(&repo, false).unwrap(), Some(Kept::default()));
        assert_eq!(staged_at(dir.path(), "library"), moved);
    }

    #[test]
    fn all_keeps_a_staged_pointer_whose_submodule_folder_is_gone_and_says_so() {
        let (dir, repo, _library, moved) = crate::git::test_support::with_submodule();
        run(dir.path(), &["config", "submodule.library.ignore", "all"]);
        crate::git::test_support::move_pointer(dir.path(), &moved);
        run(dir.path(), &["add", "library"]);
        fs::remove_dir_all(dir.path().join("library")).unwrap();

        let kept = stage_all(&repo, false).unwrap().unwrap();
        assert!(kept.moved_on.is_empty());
        assert_eq!(kept.replaced, ["library"]);
        assert_eq!(staged_at(dir.path(), "library"), moved);
    }

    /// Whether the index holds a submodule pointer at `name`.
    fn holds_pointer(dir: &Path, name: &str) -> bool {
        run(dir, &["ls-files", "--stage", "-z"])
            .split('\0')
            .any(|entry| entry.starts_with("160000 ") && entry.ends_with(&format!("\t{name}")))
    }

    #[test]
    fn all_keeps_a_staged_removal_and_stages_the_folder_that_replaced_the_submodule() {
        for setting in ["submodule.library.ignore", "diff.ignoreSubmodules"] {
            let (dir, repo, _library, _moved) = crate::git::test_support::with_submodule();
            run(dir.path(), &["config", setting, "all"]);
            run(dir.path(), &["rm", "-q", "--cached", "library"]);
            fs::remove_file(dir.path().join("library/.git")).unwrap();
            fs::write(dir.path().join("library/vendored.rs"), "fn v() {}\n").unwrap();

            assert_eq!(
                stage_all(&repo, false).unwrap(),
                Some(Kept::default()),
                "{setting}"
            );
            assert!(!holds_pointer(dir.path(), "library"), "{setting}");
            let staged = repo.staged_paths().unwrap();
            assert!(
                staged.contains(&"library/vendored.rs".to_owned()),
                "{setting}: {staged:?}"
            );
        }
    }

    #[test]
    fn all_keeps_a_staged_removal_of_a_submodule_that_is_still_checked_out() {
        for setting in ["submodule.library.ignore", "diff.ignoreSubmodules"] {
            let (dir, repo, _library, _moved) = crate::git::test_support::with_submodule();
            run(dir.path(), &["config", setting, "all"]);
            run(dir.path(), &["rm", "-q", "--cached", "library"]);

            assert_eq!(
                stage_all(&repo, false).unwrap(),
                Some(Kept::default()),
                "{setting}"
            );
            assert!(!holds_pointer(dir.path(), "library"), "{setting}");
        }
    }
}
