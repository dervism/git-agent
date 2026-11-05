//! Runs a command the user chooses on the trees a plan's commits will leave, so
//! a commit that doesn't build can be merged into the next one before it's made.
//!
//! The command runs in a clean temporary checkout. The whole change goes first,
//! because if it fails, no split can pass. Then the checkout is cleaned, so
//! build output from the whole change can't hide what an earlier commit lacks,
//! and the other commits follow in order.

use super::command::{BuildCheck, Ran, run_command};
use super::signals::{StopSignals, end_on_stop_signals};
use crate::git::worktree::TempWorktree;
use anyhow::{Context, Result, anyhow, bail};
use std::pin::pin;

/// What the command says about one commit.
#[derive(Debug, Clone, PartialEq)]
pub enum CommitCheck {
    Pass,
    /// With the end of the command's output.
    Fail(String),
}

impl CommitCheck {
    pub fn passed(&self) -> bool {
        *self == Self::Pass
    }
}

/// What the check is working on, for showing progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Progress {
    WholeChange,
    Commit { number: usize, total: usize },
}

/// What the command says about each of `trees`, in order. It fails if the whole
/// change fails, or the command times out or is killed, because merging commits
/// wouldn't help with those. It listens for signals while the command runs, and
/// sees to it that they end the program afterward, as they did before.
pub async fn check_trees(
    worktree: &TempWorktree,
    check: &BuildCheck,
    head: &str,
    trees: &[String],
    report: impl FnMut(Progress),
) -> Result<Vec<CommitCheck>> {
    let mut signals = StopSignals::new().context("failed to listen for signals")?;
    let mut shown: Option<String> = None;
    let checks = walk(
        check,
        head,
        trees,
        async |tree: &str| {
            // Output of what's being deleted could hide that something needs it.
            if let Some(shown) = &shown
                && worktree.deletes_files(shown, tree)?
            {
                worktree.clean()?;
            }
            worktree.show_tree(tree)?;
            shown = Some(tree.to_owned());
            run_command(worktree, check, pin!(signals.next())).await
        },
        || worktree.clean(),
        report,
    )
    .await?;
    end_on_stop_signals();
    Ok(checks)
}

/// Lines that say a build failed without saying why.
const HEADLINES: [&str; 3] = [
    "build failure",
    "compilation error",
    "failed to execute goal",
];

/// The line of a command's output that says most about why it failed: the first
/// that starts by saying it's an error, or else that mentions one. Headlines only
/// count if there's nothing better, and what follows a bare `[ERROR]` line, as
/// Maven writes, is advice.
pub fn reason(output: &str) -> &str {
    let before_advice: Vec<&str> = output
        .lines()
        .take_while(|line| !is_bare_error(line))
        .collect();
    let is_headline = |line: &&&str| {
        let line = line.to_lowercase();
        HEADLINES.iter().any(|headline| line.contains(headline))
    };
    let not_headline = || before_advice.iter().filter(|line| !is_headline(line));
    not_headline()
        .find(|line| starts_with_error(line))
        .or_else(|| not_headline().find(|line| mentions_error(line)))
        .or_else(|| before_advice.iter().find(|line| starts_with_error(line)))
        .copied()
        .or_else(|| output.lines().last())
        .unwrap_or_default()
}

fn starts_with_error(line: &str) -> bool {
    let line = line.trim_start().trim_start_matches('[').to_lowercase();
    line.strip_prefix("error")
        .is_some_and(|rest| !continues_word(rest.chars().next()))
}

/// Whether `line` says it's an error and nothing more.
fn is_bare_error(line: &str) -> bool {
    let line = line.trim().trim_start_matches('[').to_lowercase();
    line.strip_prefix("error")
        .is_some_and(|rest| rest.trim_start_matches(']').trim().is_empty())
}

/// Whether `line` has "error" as a word of its own, unlike `thiserror` or `errors`.
fn mentions_error(line: &str) -> bool {
    let line = line.to_lowercase();
    line.match_indices("error").any(|(start, word)| {
        !continues_word(line[..start].chars().next_back())
            && !continues_word(line[start + word.len()..].chars().next())
    })
}

fn continues_word(character: Option<char>) -> bool {
    character.is_some_and(|character| character.is_alphanumeric() || matches!(character, '-' | '_'))
}

/// The order of the runs and what each result means, apart from running them.
async fn walk(
    check: &BuildCheck,
    head: &str,
    trees: &[String],
    mut run: impl AsyncFnMut(&str) -> Result<Ran>,
    mut clean: impl FnMut() -> Result<()>,
    mut report: impl FnMut(Progress),
) -> Result<Vec<CommitCheck>> {
    let Some((whole_change, earlier)) = trees.split_last() else {
        return Ok(Vec::new());
    };
    report(Progress::WholeChange);
    match run(whole_change).await? {
        Ran::Pass => {}
        Ran::Fail(output) => {
            clean()?;
            return Err(whole_change_fails(check, &run(head).await?, &output));
        }
        Ran::TimedOut => return Err(timed_out(check)),
        Ran::Killed => return Err(killed(check)),
        Ran::CouldNotStart(output) => {
            bail!(
                "`{}` couldn't start, so nothing was committed\n{output}",
                check.command
            )
        }
    }
    if !earlier.is_empty() {
        clean()?;
    }
    let mut checks = Vec::with_capacity(trees.len());
    for (index, tree) in earlier.iter().enumerate() {
        report(Progress::Commit {
            number: index + 1,
            total: trees.len(),
        });
        checks.push(match run(tree).await? {
            Ran::Pass => CommitCheck::Pass,
            // It starts with the whole change, so what's missing here is
            // something a later commit adds, such as the script itself.
            Ran::Fail(output) | Ran::CouldNotStart(output) => CommitCheck::Fail(output),
            Ran::TimedOut => return Err(timed_out(check)),
            Ran::Killed => return Err(killed(check)),
        });
    }
    checks.push(CommitCheck::Pass);
    Ok(checks)
}

/// Why the split stops when the whole change fails, going by how the command
/// does without the change.
fn whole_change_fails(check: &BuildCheck, at_head: &Ran, output: &str) -> anyhow::Error {
    let command = &check.command;
    match at_head {
        Ran::Pass => anyhow!(
            "`{command}` fails with your changes, so no split of them can pass; nothing was \
             committed\n{output}"
        ),
        _ => anyhow!(
            "`{command}` also fails without your changes, in the clean checkout it runs in, so \
             it may need setup first, such as installing dependencies; nothing was committed\n\
             {output}"
        ),
    }
}

fn timed_out(check: &BuildCheck) -> anyhow::Error {
    anyhow!(
        "`{}` didn't finish within {} seconds, so nothing was committed; raise the limit with \
         --check-timeout",
        check.command,
        check.timeout.as_secs()
    )
}

fn killed(check: &BuildCheck) -> anyhow::Error {
    anyhow!(
        "`{}` was killed before it finished, which says nothing about your changes, so nothing \
         was committed",
        check.command
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support::{isolated_repo, run};
    use std::cell::RefCell;
    use std::fs;
    use std::future::pending;
    use std::time::Duration;

    fn check(command: &str) -> BuildCheck {
        BuildCheck {
            command: command.to_owned(),
            timeout: Duration::from_secs(60),
        }
    }

    fn trees(count: usize) -> Vec<String> {
        (1..=count).map(|tree| format!("tree{tree}")).collect()
    }

    /// Runs `walk` with `ran_on` deciding by tree name, and returns the result,
    /// the trees it ran on in order with "clean" where it cleaned, and what it
    /// reported.
    async fn walk_with(
        count: usize,
        ran_on: impl Fn(&str) -> Ran,
    ) -> (Result<Vec<CommitCheck>>, Vec<String>, Vec<Progress>) {
        let log = RefCell::new(Vec::new());
        let mut progress = Vec::new();
        let trees = trees(count);
        let result = walk(
            &check("the command"),
            "head",
            &trees,
            async |tree: &str| {
                log.borrow_mut().push(tree.to_owned());
                Ok(ran_on(tree))
            },
            || {
                log.borrow_mut().push("clean".to_owned());
                Ok(())
            },
            |step| progress.push(step),
        )
        .await;
        (result, log.into_inner(), progress)
    }

    fn failed(text: &str) -> Ran {
        Ran::Fail(text.to_owned())
    }

    fn commit_failed(text: &str) -> CommitCheck {
        CommitCheck::Fail(text.to_owned())
    }

    #[tokio::test]
    async fn runs_the_whole_change_first_then_cleans_and_goes_in_order() {
        let (result, ran, progress) = walk_with(3, |_| Ran::Pass).await;
        assert_eq!(result.unwrap(), vec![CommitCheck::Pass; 3]);
        assert_eq!(ran, ["tree3", "clean", "tree1", "tree2"]);
        assert_eq!(
            progress,
            [
                Progress::WholeChange,
                Progress::Commit {
                    number: 1,
                    total: 3
                },
                Progress::Commit {
                    number: 2,
                    total: 3
                },
            ]
        );
    }

    #[tokio::test]
    async fn checks_a_single_commit_once_and_doesnt_clean() {
        let (result, ran, _) = walk_with(1, |_| Ran::Pass).await;
        assert_eq!(result.unwrap(), [CommitCheck::Pass]);
        assert_eq!(ran, ["tree1"]);
    }

    #[tokio::test]
    async fn has_nothing_to_do_without_trees() {
        let (result, ran, progress) = walk_with(0, |_| Ran::Pass).await;
        assert!(result.unwrap().is_empty());
        assert!(ran.is_empty() && progress.is_empty());
    }

    #[tokio::test]
    async fn reports_each_commit_that_fails() {
        let (result, ran, _) = walk_with(4, |tree| match tree {
            "tree1" | "tree3" => failed("broken"),
            _ => Ran::Pass,
        })
        .await;
        assert_eq!(
            result.unwrap(),
            [
                commit_failed("broken"),
                CommitCheck::Pass,
                commit_failed("broken"),
                CommitCheck::Pass
            ]
        );
        assert_eq!(ran, ["tree4", "clean", "tree1", "tree2", "tree3"]);
    }

    #[tokio::test]
    async fn all_but_the_whole_change_may_fail() {
        let (result, _, _) = walk_with(3, |tree| match tree {
            "tree3" => Ran::Pass,
            _ => failed("broken"),
        })
        .await;
        let checks = result.unwrap();
        assert_eq!(checks.iter().filter(|c| c.passed()).count(), 1);
        assert!(checks[2].passed());
    }

    #[tokio::test]
    async fn counts_a_command_a_later_commit_adds_as_a_failure_of_the_earlier_one() {
        let (result, _, _) = walk_with(2, |tree| match tree {
            "tree1" => Ran::CouldNotStart("sh: ./build.sh: not found".to_owned()),
            _ => Ran::Pass,
        })
        .await;
        assert_eq!(
            result.unwrap(),
            [
                commit_failed("sh: ./build.sh: not found"),
                CommitCheck::Pass
            ]
        );
    }

    #[tokio::test]
    async fn stops_when_the_whole_change_fails_and_head_passes() {
        let (result, ran, _) = walk_with(3, |tree| match tree {
            "head" => Ran::Pass,
            _ => failed("error: broken"),
        })
        .await;
        let error = format!("{:#}", result.err().unwrap());
        assert!(error.contains("fails with your changes"), "{error}");
        assert!(error.contains("error: broken"), "{error}");
        assert_eq!(ran, ["tree3", "clean", "head"]);
    }

    #[tokio::test]
    async fn suggests_setup_when_head_fails_too() {
        for at_head in [
            failed("no such"),
            Ran::TimedOut,
            Ran::CouldNotStart("x".to_owned()),
        ] {
            let (result, _, _) = walk_with(2, |tree| match tree {
                "head" => at_head.clone(),
                _ => failed("error: broken"),
            })
            .await;
            let error = format!("{:#}", result.err().unwrap());
            assert!(error.contains("may need setup"), "{error}");
        }
    }

    #[tokio::test]
    async fn stops_when_the_command_cant_start_on_the_whole_change() {
        let (result, ran, _) =
            walk_with(3, |_| Ran::CouldNotStart("sh: tool: not found".to_owned())).await;
        let error = format!("{:#}", result.err().unwrap());
        assert!(error.contains("couldn't start"), "{error}");
        assert!(error.contains("not found"), "{error}");
        assert_eq!(ran, ["tree3"]);
    }

    #[tokio::test]
    async fn stops_at_a_timeout_or_a_killed_command_anywhere() {
        for tree_that_cant in ["tree3", "tree1"] {
            for (ran, text) in [
                (Ran::TimedOut, "--check-timeout"),
                (Ran::Killed, "was killed"),
            ] {
                let (result, _, _) = walk_with(3, |tree| {
                    if tree == tree_that_cant {
                        ran.clone()
                    } else {
                        Ran::Pass
                    }
                })
                .await;
                let error = format!("{:#}", result.err().unwrap());
                assert!(error.contains(text), "{error}");
                assert!(error.contains("nothing was committed"), "{error}");
            }
        }
    }

    #[tokio::test]
    async fn passes_on_an_error_from_cleaning() {
        let trees = trees(2);
        let result = walk(
            &check("the command"),
            "head",
            &trees,
            async |_: &str| Ok(Ran::Pass),
            || bail!("could not clean"),
            |_| {},
        )
        .await;
        assert!(format!("{:#}", result.err().unwrap()).contains("could not clean"));
    }

    #[test]
    fn finds_the_line_that_says_why() {
        let maven = "[INFO] Building\n[ERROR] Foo.java:[1,2] cannot find symbol\n\
                     [ERROR] -> [Help 1]\n[ERROR] http://example.com/help";
        assert_eq!(reason(maven), "[ERROR] Foo.java:[1,2] cannot find symbol");
        let cargo = "   Compiling x\nerror[E0425]: cannot find value\n\nerror: could not compile";
        assert_eq!(reason(cargo), "error[E0425]: cannot find value");
        assert_eq!(reason("first\nlast"), "last");
        assert_eq!(reason(""), "");
        // Words that only hold "error" don't count, and advice comes after a real one.
        let words = "Compiling thiserror v2.0.12\n0 errors, 1 failed\nerror: real one";
        assert_eq!(reason(words), "error: real one");
        assert_eq!(reason("Compiling thiserror v2\nfailed"), "failed");
        let advice = "For more information about this error, try this\nerror: could not compile";
        assert_eq!(reason(advice), "error: could not compile");
        assert_eq!(
            reason("noise\nA compile error here\nmore"),
            "A compile error here"
        );
        // What Maven really writes, with the headline first.
        let maven = "[ERROR] COMPILATION ERROR : \n[INFO] -----\n\
                     [ERROR] /x/A.java:[1,22] cannot find symbol\n\
                     [ERROR] Failed to execute goal org.apache.maven.plugins:compiler\n\
                     [ERROR] -> [Help 1]";
        assert_eq!(reason(maven), "[ERROR] /x/A.java:[1,22] cannot find symbol");
        assert_eq!(reason("[ERROR] BUILD FAILURE"), "[ERROR] BUILD FAILURE");
        // When Maven's only message is its headline, and advice follows.
        let only_headline = "[ERROR] Failed to execute goal enforcer: No rules are configured\n\
                             [ERROR] \n\
                             [ERROR] To see the full stack trace, re-run Maven\n\
                             [ERROR] -> [Help 1]";
        assert_eq!(
            reason(only_headline),
            "[ERROR] Failed to execute goal enforcer: No rules are configured"
        );
        assert_eq!(reason("[ERROR] \n[ERROR] advice"), "[ERROR] advice");
    }

    #[tokio::test]
    async fn checks_real_trees_and_cleans_output_from_the_whole_change() {
        let (dir, repo) = isolated_repo();
        let tree_with = |name: &str| {
            fs::write(dir.path().join(name), "x\n").unwrap();
            run(dir.path(), &["add", "--all"]);
            run(dir.path(), &["write-tree"]).trim().to_owned()
        };
        fs::write(dir.path().join(".gitignore"), "built-*\n").unwrap();
        let trees = vec![tree_with("needs.txt"), tree_with("gives.txt")];
        run(dir.path(), &["reset", "-q", "--hard"]);
        let head = repo.resolve("HEAD").unwrap().unwrap();
        let worktree = TempWorktree::add(&repo, &head).unwrap();
        // It reports "stale" if output from the whole change is still there,
        // and "missing" if needs.txt is there without gives.txt.
        let check = check(
            "test ! -e built-output || { echo stale; exit 1; }; touch built-output; \
             test ! -f needs.txt || test -f gives.txt || { echo missing; exit 1; }",
        );
        let mut seen = Vec::new();
        let checks = walk(
            &check,
            &head,
            &trees,
            async |tree: &str| {
                worktree.show_tree(tree)?;
                let never = pending();
                run_command(&worktree, &check, pin!(never)).await
            },
            || worktree.clean(),
            |step| seen.push(step),
        )
        .await
        .unwrap();
        assert_eq!(checks, [commit_failed("missing"), CommitCheck::Pass]);
        assert_eq!(seen.len(), 2);
    }

    #[tokio::test]
    async fn runs_clean_at_head_when_the_whole_change_fails() {
        let (dir, repo) = isolated_repo();
        fs::write(dir.path().join("new.txt"), "x\n").unwrap();
        run(dir.path(), &["add", "--all"]);
        let tree = run(dir.path(), &["write-tree"]).trim().to_owned();
        run(dir.path(), &["reset", "-q", "--hard"]);
        let head = repo.resolve("HEAD").unwrap().unwrap();
        let worktree = TempWorktree::add(&repo, &head).unwrap();
        // It leaves output behind, and fails if it's already there or new.txt is.
        let check = check("mkdir out && ! test -f new.txt");
        let trees = [tree];
        let error = walk(
            &check,
            &head,
            &trees,
            async |tree: &str| {
                worktree.show_tree(tree)?;
                let never = pending();
                run_command(&worktree, &check, pin!(never)).await
            },
            || worktree.clean(),
            |_| {},
        )
        .await
        .err()
        .unwrap();
        assert!(
            format!("{error:#}").contains("fails with your changes"),
            "{error:#}"
        );
    }
}
