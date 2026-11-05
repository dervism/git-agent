//! The steps of `git agent split`: from the changes to a plan the user accepts,
//! from that plan to commits that hold up, and from those to the commits.

use super::check::{self, CommitCheck, Progress};
use super::command::BuildCheck;
use super::dry_run::{CommitPlan, Snapshot, dry_run};
use super::kinds::{self, Kind};
use super::{Committed, FilePatch, Patch, Plan, Planned, Unit};
use crate::cli::{SettingArgs, SplitArgs};
use crate::config::{Config, OnSecrets};
use crate::git::worktree::TempWorktree;
use crate::git::{Changes, Repo, Scope};
use crate::models::Model;
use crate::server::{Message, ModelServer};
use crate::ui::{self, Action, Choice, Review};
use crate::{
    check_outgoing, commit, commit_message, connect, conversation_tokens, generate_message, prompt,
    push, redact, reject_added, report_fit, report_model, report_secrets, review, separated, split,
    stage_all,
};
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::{self, IsTerminal};
use std::time::Duration;

/// Accepting the plan of a split: the value says whether to review each message.
const PLAN_REVIEW: Review<bool> = Review {
    heading: "Proposed commits",
    first_line: "first line",
    noun: "plan",
    what: "plan",
    edit: "edit the plan in your editor",
    accept: &[
        Choice {
            word: "all commits",
            help: "make all the commits, without stopping to review their messages",
            value: false,
        },
        Choice {
            word: "commit",
            help: "make the commits one at a time, reviewing each message",
            value: true,
        },
    ],
    quit: "quit without committing anything",
};

/// A plan that leaves out at least this many parts is asked for once more.
const ASK_AGAIN_MISSING: usize = 3;

/// Accepting one commit of a split: the value says whether to make it.
const SPLIT_COMMIT_REVIEW: Review<bool> = Review {
    heading: "Proposed commit message",
    first_line: "subject line",
    noun: "message",
    what: "commit message",
    edit: "edit the message in your editor",
    accept: &[
        Choice {
            word: "commit",
            help: "commit these changes with this message",
            value: true,
        },
        Choice {
            word: "skip",
            help: "leave these changes uncommitted and go on to the next commit",
            value: false,
        },
    ],
    quit: "stop here; the commits made so far stay, and the rest stays uncommitted",
};

/// Runs `git agent split`.
pub async fn run(args: &SplitArgs, settings: &SettingArgs) -> Result<()> {
    let config = Config::load(settings)?.config;
    let repo = Repo::open_at_top(".")?;
    let Some(work) = changes_to_split(&repo, args, &config)? else {
        return Ok(());
    };
    let (server, model) = connect(&config).await?;
    split_changes(&repo, args, &config, &work, &server, &model).await
}

/// The changes to split, as they were before the model was asked anything.
struct Work {
    scope: Scope,
    changes: Changes,
    files: Vec<FilePatch>,
    snapshot: Snapshot,
}

/// Reads the changes, and checks everything that doesn't need the model.
fn changes_to_split(repo: &Repo, args: &SplitArgs, config: &Config) -> Result<Option<Work>> {
    if let Some(operation) = repo.operation_in_progress()? {
        bail!("{operation} is in progress; finish or abort it before splitting changes");
    }
    if repo.has_unmerged_paths()? {
        bail!("some files have unresolved conflicts; resolve them before splitting changes");
    }
    let ask = !args.commit && io::stdin().is_terminal();
    if args.all && !stage_all(repo, ask)? {
        ui::info("Nothing staged or committed.");
        return Ok(None);
    }
    let Some((scope, changes)) = repo.changes(&config.exclude)? else {
        ui::info("✨ No changes to split.");
        return Ok(None);
    };
    let redacted = redact::diff(&changes.diff);
    if config.on_secrets == OnSecrets::Reject {
        reject_added(
            &redacted.findings,
            "the changes add what look like secrets, so git-agent didn't split them",
            "Take them out of the changes.",
        )?;
    }
    report_secrets(
        &redacted.findings,
        "If they are real, take them out of the files before you commit.",
    );
    let patch = repo.patch(scope)?;
    let files = split::parse(&patch, &repo.included_paths(scope, &config.exclude)?);
    // Only uncommitted work inside a submodule, which is no change a commit can hold.
    if files.is_empty() {
        ui::info("✨ No changes to split.");
        return Ok(None);
    }
    let snapshot = Snapshot::take(repo, scope, &patch)?;
    Ok(Some(Work {
        scope,
        changes,
        files,
        snapshot,
    }))
}

async fn split_changes<S: ModelServer>(
    repo: &Repo,
    args: &SplitArgs,
    config: &Config,
    work: &Work,
    server: &S,
    model: &Model,
) -> Result<()> {
    let room = prompt::room(
        prompt::SPLIT_PROMPT,
        model.context_length,
        config.max_output_tokens,
    );
    let described = split::describe(&work.files, room)?;
    if described.units.len() < 2 {
        ui::info(
            "✨ The changes are a single hunk, so there is nothing to split; commit them with \
             `git agent`.",
        );
        return Ok(());
    }
    report_what_is_split(work, described.by_file);
    let text = redact::text(&described.text, "changes").text;
    let conversation = prompt::split_messages(&text, args.hint.as_deref());
    report_model(model, conversation_tokens(&conversation));

    let splitter = Splitter {
        repo,
        config,
        args,
        work,
        units: &described.units,
        server,
        model,
    };
    let Some((plan, review_messages)) = splitter.accepted_plan(&conversation).await? else {
        return Ok(());
    };
    let prepared = splitter.prepare_commits(&plan).await?;
    let outcome = splitter
        .make_commits(&prepared, review_messages)
        .await
        .context(
            "the split stopped; the commits made so far stay, and the rest of the changes are as \
             they were",
        )?;
    finish(repo, args, config, work.scope, &outcome)
}

fn report_what_is_split(work: &Work, by_file: bool) {
    let summary = work.changes.stat.lines().last().unwrap_or_default().trim();
    match work.scope {
        Scope::Staged => ui::info(format!("🔍 Staged changes: {summary}")),
        Scope::Tracked => ui::info(format!(
            "🔍 Nothing staged; splitting all changes to tracked files: {summary}"
        )),
    }
    if by_file {
        ui::info(ui::dim(
            "   There are too many changes to show the model one by one, so it groups whole files.",
        ));
    }
}

/// Tells the user how the split ended, and pushes if they asked for that.
fn finish(
    repo: &Repo,
    args: &SplitArgs,
    config: &Config,
    scope: Scope,
    outcome: &Outcome,
) -> Result<()> {
    const NOT_PUSHED: &str = "the commits were made, but not pushed";

    if outcome.made > 0 {
        ui::info(ui::green(&format!("\n✅ Made {} commit(s):", outcome.made)));
        for line in repo.recent_commits(outcome.made)?.iter().rev() {
            ui::info(format!("   {line}"));
        }
    }
    let left = match scope {
        Scope::Staged => repo.has_staged_changes()?,
        Scope::Tracked => repo.has_uncommitted_changes()?,
    };
    if left {
        let place = match scope {
            Scope::Staged => "staged",
            Scope::Tracked => "uncommitted",
        };
        if outcome.skipped > 0 || outcome.stopped {
            ui::info(format!("The changes you didn't commit are still {place}."));
        } else {
            ui::warn(format!(
                "the commits don't hold all of the changes; the rest are still {place} \
                 (see `git status`)"
            ));
        }
    }
    if args.push && outcome.made > 0 {
        check_outgoing(repo, config).context(NOT_PUSHED)?;
        push(repo, !args.commit).context(NOT_PUSHED)?;
    }
    Ok(())
}

/// What making the commits of a split needs.
struct Splitter<'a, S> {
    repo: &'a Repo,
    config: &'a Config,
    args: &'a SplitArgs,
    work: &'a Work,
    units: &'a [Unit],
    server: &'a S,
    model: &'a Model,
}

/// What happens to one commit once its message is written.
enum Decision {
    /// Make it, with this message.
    Commit(String),
    /// Leave its changes uncommitted, and go on to the next.
    Skip,
    /// Make no more commits.
    Stop,
}

/// How a split ended.
#[derive(Default)]
struct Outcome {
    made: usize,
    skipped: usize,
    /// Whether the user stopped before the last commit.
    stopped: bool,
}

impl<S: ModelServer> Splitter<'_, S> {
    fn show(&self, plan: &Plan) -> String {
        split::show(plan, &self.work.files, self.units)
    }

    /// The plan to carry out, and whether to review each message, or `None` if
    /// nothing should be committed.
    async fn accepted_plan(&self, conversation: &[Message]) -> Result<Option<(Plan, bool)>> {
        let plan = self.new_plan(conversation).await?;
        if self.args.commit {
            ui::show_framed(PLAN_REVIEW.heading, &self.show(&plan));
            return Ok(Some((plan, false)));
        }
        if !io::stdin().is_terminal() {
            ui::show_framed(PLAN_REVIEW.heading, &self.show(&plan));
            ui::info(ui::dim(
                "Input is not a terminal, so not asking for review. \
                 Use --commit to make these commits directly.",
            ));
            return Ok(None);
        }
        let reviewed = self.review_plan(plan, conversation).await?;
        if reviewed.is_none() {
            ui::info("Nothing committed.");
        }
        Ok(reviewed)
    }

    /// A plan the user hasn't had a say in yet, with documentation last.
    async fn new_plan(&self, conversation: &[Message]) -> Result<Plan> {
        let plan = self.ask_for_plan(conversation).await?;
        Ok(kinds::docs_last(plan, &self.work.files, self.units))
    }

    /// A plan revised with the user's feedback, in the order the model gives,
    /// because the feedback may have been about the order.
    async fn refined_plan(
        &self,
        conversation: &[Message],
        plan: &Plan,
        feedback: &str,
    ) -> Result<Plan> {
        let refined = prompt::refine_plan(conversation, &plan.to_json(), feedback);
        self.ask_for_plan(&refined).await
    }

    /// Asks the model for a plan, reminding it once of the form if its first
    /// reply can't be read, and once more of the parts if it leaves out several.
    async fn ask_for_plan(&self, conversation: &[Message]) -> Result<Plan> {
        let mut reply = generate_message(self.server, self.model, conversation).await?;
        let mut settled = match split::parse_plan(&reply, &self.work.files, self.units) {
            Ok(settled) => settled,
            Err(err) => {
                let retry = prompt::retry_plan(
                    conversation,
                    &reply,
                    &format!("{err:#}"),
                    "with every unit in exactly one commit",
                );
                reply = generate_message(self.server, self.model, &retry).await?;
                split::parse_plan(&reply, &self.work.files, self.units)
                    .context("the model didn't reply with a plan git-agent could read")?
            }
        };
        if settled.missing.len() >= ASK_AGAIN_MISSING {
            ui::info(ui::dim(&format!(
                "   The plan left out {} of {} parts; asking the model to place them.",
                settled.missing.len(),
                self.units.len()
            )));
            let again =
                prompt::complete_plan(conversation, &reply, &settled.missing, &settled.repeated);
            let completed = generate_message(self.server, self.model, &again)
                .await
                .and_then(|reply| split::parse_plan(&reply, &self.work.files, self.units));
            // Keep whichever plan leaves out fewer parts.
            match completed {
                Ok(completed) if completed.missing.len() < settled.missing.len() => {
                    settled = completed
                }
                Ok(_) => {}
                Err(err) => ui::warn(format!("{err:#}; keeping the first plan")),
            }
        }
        for note in settled.notes {
            ui::warn(note);
        }
        Ok(settled.plan)
    }

    /// Lets the user review the plan until they accept or quit it. Returns
    /// the accepted plan and whether to review each message, or `None` on quit.
    async fn review_plan(
        &self,
        mut plan: Plan,
        conversation: &[Message],
    ) -> Result<Option<(Plan, bool)>> {
        loop {
            ui::show_framed(PLAN_REVIEW.heading, &self.show(&plan));
            let revised = match ui::ask_action(&PLAN_REVIEW)? {
                Action::Accept(review) => return Ok(Some((plan, review))),
                Action::Quit => return Ok(None),
                Action::Edit => self.edit_plan(&plan),
                Action::Regenerate => self.new_plan(conversation).await.map(Some),
                Action::Refine(feedback) => self
                    .refined_plan(conversation, &plan, &feedback)
                    .await
                    .map(Some),
            };
            // A failed edit or generation keeps the current plan so the review can go on.
            match revised {
                Ok(Some(revised)) => plan = revised,
                Ok(None) => ui::warn("the edited plan has no commits; keeping the previous one"),
                Err(err) => ui::error(&err),
            }
        }
    }

    fn edit_plan(&self, plan: &Plan) -> Result<Option<Plan>> {
        let text = ui::edit_plan(
            self.repo,
            "GIT_AGENT_SPLIT_PLAN",
            &split::to_text(plan, &self.work.files, self.units),
        )?;
        let (edited, notes) = split::from_text(&text, &self.work.files, self.units);
        for note in notes {
            ui::warn(note);
        }
        Ok((!edited.commits.is_empty()).then_some(edited))
    }

    /// Finds out what the commits of an accepted plan will hold, and with
    /// `--check`, which of them pass the command, before any message is written.
    /// The commits that can't stand alone are merged.
    async fn prepare_commits(&self, plan: &Plan) -> Result<CommitPlan> {
        let rehearsed = dry_run(
            self.repo,
            &self.work.snapshot,
            plan,
            &self.work.files,
            self.units,
        )?;
        report_merged(&rehearsed.merge_notes, "they can't be made one at a time");
        let prepared = match self.build_check() {
            Some(check) => self.run_check(&rehearsed, &check).await?,
            None => {
                if rehearsed.plan.commits.len() > 1 {
                    ui::info(ui::dim(
                        "   These commits weren't built. `--check <COMMAND>` builds each one \
                         first, and merges those that fail.",
                    ));
                }
                rehearsed
            }
        };
        if prepared.plan.commits.len() < plan.commits.len() {
            ui::show_framed("Commits to make", &self.show(&prepared.plan));
        }
        Ok(prepared)
    }

    fn build_check(&self) -> Option<BuildCheck> {
        self.args.check.as_ref().map(|command| BuildCheck {
            command: command.clone(),
            timeout: Duration::from_secs(self.args.check_timeout),
        })
    }

    async fn run_check(
        &self,
        rehearsed: &CommitPlan,
        build_check: &BuildCheck,
    ) -> Result<CommitPlan> {
        ui::info(format!(
            "🔨 Checking the commits with `{}`, in a clean checkout…",
            build_check.command
        ));
        let head = &self.work.snapshot.head;
        let worktree = TempWorktree::add(self.repo, head)?;
        let checks = check::check_trees(&worktree, build_check, head, &rehearsed.trees, |step| {
            ui::info(ui::dim(&check_progress(step)));
        })
        .await?;
        drop(worktree);
        for (commit, result) in rehearsed.plan.commits.iter().zip(&checks) {
            if let CommitCheck::Fail(output) = result {
                ui::warn(format!(
                    "\"{}\" fails `{}`, so it joins the next commit: {}",
                    commit.title,
                    build_check.command,
                    check::reason(output)
                ));
            }
        }
        let passed: Vec<bool> = checks.iter().map(CommitCheck::passed).collect();
        let checked = rehearsed.after_check(&passed);
        report_merged(
            &checked.merge_notes,
            "the earlier ones don't pass on their own",
        );
        Ok(checked)
    }

    /// Makes the planned commits one by one, each built in a temporary index
    /// from HEAD and its own changes, so the real index is left alone. With
    /// `review_messages`, the user reviews each commit's message.
    async fn make_commits(&self, prepared: &CommitPlan, review_messages: bool) -> Result<Outcome> {
        let path = self.repo.git_path("GIT_AGENT_SPLIT_INDEX")?;
        let index = self.repo.with_index(path.clone());
        let outcome = self
            .commit_each(
                prepared,
                &index,
                async |message: String, conversation: &[Message], kind: Option<Kind>| {
                    if review_messages {
                        self.reviewed(message, conversation, kind).await
                    } else {
                        Ok(Decision::Commit(message))
                    }
                },
            )
            .await;
        let _ = fs::remove_file(&path);
        // Nothing was staged before the split, so the real index stays at HEAD.
        if self.work.scope == Scope::Tracked {
            self.repo.reset_index()?;
        }
        outcome
    }

    /// Lets the user decide about a commit from its message.
    async fn reviewed(
        &self,
        message: String,
        conversation: &[Message],
        kind: Option<Kind>,
    ) -> Result<Decision> {
        let reviewed = review(
            &SPLIT_COMMIT_REVIEW,
            message,
            self.server,
            self.model,
            conversation,
            |message: &str| ui::edit_message(self.repo, message),
            |message| separated(message).map(|message| kinds::with_type(&message, kind)),
        )
        .await?;
        Ok(match reviewed {
            Some((true, message)) => Decision::Commit(message),
            Some((false, _)) => Decision::Skip,
            None => Decision::Stop,
        })
    }

    /// Stops if something else has committed since `parent`: the commit about
    /// to be made holds the tree it was built on, so it would undo that work.
    fn ensure_head_is(&self, parent: &str) -> Result<()> {
        if self.repo.resolve("HEAD")?.as_deref() != Some(parent) {
            bail!("HEAD moved while the split was running, so it stopped; run it again");
        }
        Ok(())
    }

    /// Makes the commits of `prepared`, asking `decide` about each from its
    /// message, the conversation that wrote it and the kind of commit it is.
    async fn commit_each(
        &self,
        prepared: &CommitPlan,
        index: &Repo,
        mut decide: impl AsyncFnMut(String, &[Message], Option<Kind>) -> Result<Decision>,
    ) -> Result<Outcome> {
        let mut committed = Committed::default();
        let mut outcome = Outcome::default();
        let mut parent = self.work.snapshot.head.clone();
        // A skipped commit, or a hook that changes one, changes what the later
        // ones hold, so they can no longer be held to what was planned.
        let mut follows_plan = true;
        let total = prepared.plan.commits.len();
        for ((number, planned), planned_tree) in
            (1..).zip(&prepared.plan.commits).zip(&prepared.trees)
        {
            ui::info(format!(
                "\n📦 Commit {number} of {total}: {}",
                planned.title
            ));
            let files = split::files_of(planned, &self.work.files, self.units);
            ui::info(ui::dim(&format!("   {}", files.join(", "))));
            self.ensure_head_is(&parent)?;
            let (patch, tree) = stage_commit(self, index, planned, &parent, &committed)
                .with_context(|| {
                    let cause = if follows_plan {
                        ""
                    } else {
                        ", maybe because of a commit you skipped or a hook changed"
                    };
                    format!("could not stage commit {number} of {total}{cause}")
                })?;
            if follows_plan && tree != *planned_tree {
                bail!(
                    "commit {number} of {total} doesn't hold what was planned, so it wasn't made"
                );
            }
            let Some((message, conversation)) = self.propose_message(planned, index).await? else {
                ui::warn("these changes are already committed; skipping them");
                continue;
            };
            let kind = kinds::kind_of(planned, &self.work.files, self.units);
            let message = match decide(message, &conversation, kind).await? {
                Decision::Commit(message) => message,
                Decision::Skip => {
                    ui::info(if self.args.check.is_some() {
                        "Skipped; these changes stay uncommitted, and the commits after this one \
                         weren't checked without them."
                    } else {
                        "Skipped; these changes stay uncommitted."
                    });
                    outcome.skipped += 1;
                    follows_plan = false;
                    continue;
                }
                Decision::Stop => {
                    outcome.stopped = true;
                    break;
                }
            };
            self.ensure_head_is(&parent)?;
            commit(index, &message, Scope::Staged, &self.args.git_args)?;
            committed.record(&patch);
            outcome.made += 1;
            parent = self
                .repo
                .resolve("HEAD")?
                .context("HEAD is gone after a commit")?;
            if self.repo.tree(&parent)? != tree {
                follows_plan = false;
                if self.args.check.is_some() {
                    ui::warn("a commit hook changed this commit, so it isn't what was checked");
                }
            }
            if self.work.scope == Scope::Tracked {
                self.repo.reset_index()?;
            }
        }
        Ok(outcome)
    }

    /// The model's message for the commit staged in `index`, of the right type,
    /// with the conversation that wrote it, or `None` if there's nothing staged.
    async fn propose_message(
        &self,
        planned: &Planned,
        index: &Repo,
    ) -> Result<Option<(String, Vec<Message>)>> {
        let Some((_, mut changes)) = index.changes(&self.config.exclude)? else {
            return Ok(None);
        };
        changes.diff = redact::diff(&changes.diff).text;
        let hint = prompt::split_commit_hint(&planned.title, self.args.hint.as_deref());
        let (conversation, fit) = prompt::initial_messages(
            &self.config.system_prompt,
            &changes,
            Some(&hint),
            self.model.context_length,
            self.config.max_output_tokens,
        );
        if !fit.is_complete() {
            report_fit(self.model, &fit, "message", None);
        }
        let kind = kinds::kind_of(planned, &self.work.files, self.units);
        let message = commit_message(self.server, self.model, &conversation).await?;
        let message = kinds::with_type(&message, kind);
        ui::show_proposal(&SPLIT_COMMIT_REVIEW, &message);
        Ok(Some((message, conversation)))
    }
}

/// Stages one commit on `parent` in `index`, and returns its patch and its tree.
fn stage_commit<S>(
    splitter: &Splitter<'_, S>,
    index: &Repo,
    planned: &Planned,
    parent: &str,
    committed: &Committed,
) -> Result<(Patch, String)> {
    let patch = split::patch(
        &splitter.work.files,
        splitter.units,
        &planned.units,
        committed,
    );
    index.read_tree(parent)?;
    index.apply_to_index(&patch.text)?;
    let tree = index.write_tree()?;
    Ok((patch, tree))
}

fn check_progress(step: Progress) -> String {
    match step {
        Progress::WholeChange => "   The whole change".to_owned(),
        Progress::Commit { number, total } => format!("   Commit {number} of {total}"),
    }
}

/// Tells the user which commits were merged, and `why`.
fn report_merged(merged: &[String], why: &str) {
    for note in merged {
        ui::warn(format!("{note}: {why}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support::{isolated_repo, numbered_lines, run, with_submodule};
    use crate::models::ContextSource;
    use crate::server::ModelInfo;
    use std::cell::{Cell, RefCell};
    use std::path::Path;
    use tempfile::TempDir;

    /// A model that replies with fixed text, and can do something while the user
    /// waits for its reply.
    struct FakeServer<'a> {
        plans: RefCell<Vec<&'a str>>,
        message: &'a str,
        message_requests: Cell<usize>,
        during_plan: RefCell<Option<Box<dyn FnOnce() + 'a>>>,
        during_message: RefCell<Option<Box<dyn FnOnce() + 'a>>>,
    }

    impl<'a> FakeServer<'a> {
        fn new(plan: &'a str, message: &'a str) -> Self {
            Self {
                plans: RefCell::new(vec![plan]),
                message,
                message_requests: Cell::new(0),
                during_plan: RefCell::new(None),
                during_message: RefCell::new(None),
            }
        }

        fn during_plan(self, action: impl FnOnce() + 'a) -> Self {
            *self.during_plan.borrow_mut() = Some(Box::new(action));
            self
        }

        fn during_message(self, action: impl FnOnce() + 'a) -> Self {
            *self.during_message.borrow_mut() = Some(Box::new(action));
            self
        }
    }

    impl ModelServer for FakeServer<'_> {
        async fn models(&self) -> Result<Vec<ModelInfo>> {
            Ok(Vec::new())
        }

        async fn complete(&self, _model: &str, messages: &[Message]) -> Result<String> {
            if messages[0].content == prompt::SPLIT_PROMPT {
                if let Some(action) = self.during_plan.borrow_mut().take() {
                    action();
                }
                let mut plans = self.plans.borrow_mut();
                let plan = if plans.len() > 1 {
                    plans.remove(0)
                } else {
                    plans[0]
                };
                return Ok(plan.to_owned());
            }
            self.message_requests.set(self.message_requests.get() + 1);
            if let Some(action) = self.during_message.borrow_mut().take() {
                action();
            }
            Ok(self.message.to_owned())
        }
    }

    fn model() -> Model {
        Model {
            id: "fake".to_owned(),
            picked: None,
            context_length: 100_000,
            context_source: ContextSource::Config,
        }
    }

    fn args(commit: bool, check: Option<&str>) -> SplitArgs {
        SplitArgs {
            all: false,
            commit,
            push: false,
            hint: None,
            check: check.map(str::to_owned),
            check_timeout: 60,
            git_args: Vec::new(),
        }
    }

    async fn split(repo: &Repo, args: &SplitArgs, server: &FakeServer<'_>) -> Result<()> {
        let config = Config::default();
        let work = changes_to_split(repo, args, &config)?.expect("changes to split");
        split_changes(repo, args, &config, &work, server, &model()).await
    }

    fn write(dir: &Path, name: &str, text: &str) {
        fs::write(dir.join(name), text).unwrap();
    }

    /// Stages a new README text, and new uses.txt and defs.txt files.
    fn staged_three_parts() -> (TempDir, Repo) {
        let (dir, repo) = isolated_repo();
        write(dir.path(), "README.md", "old\n");
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "add the readme"]);
        write(dir.path(), "README.md", "new\n");
        write(dir.path(), "uses.txt", "u\n");
        write(dir.path(), "defs.txt", "d\n");
        run(dir.path(), &["add", "--all"]);
        (dir, repo)
    }

    /// A plan for [`staged_three_parts`], whose units are README.md, defs.txt and
    /// uses.txt, in that order: the docs first, then uses.txt before defs.txt.
    const DOCS_USES_DEFS: &str = r#"{"commits":[{"title":"Update the readme","units":[1]},
        {"title":"Add uses","units":[3]},{"title":"Add defs","units":[2]}]}"#;

    /// Passes unless uses.txt is there without defs.txt.
    const NEEDS_DEFS: &str =
        r#"test ! -f uses.txt || test -f defs.txt || { echo "error: needs defs"; exit 1; }"#;

    fn subjects(dir: &Path, count: usize) -> Vec<String> {
        run(
            dir,
            &["log", "--format=%s", &format!("--max-count={count}")],
        )
        .lines()
        .rev()
        .map(str::to_owned)
        .collect()
    }

    fn files_of_commit(dir: &Path, revision: &str) -> Vec<String> {
        run(dir, &["show", "--format=", "--name-only", revision])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[tokio::test]
    async fn makes_the_commits_with_documentation_last_and_of_the_right_type() {
        let (dir, repo) = staged_three_parts();
        let server = FakeServer::new(DOCS_USES_DEFS, "feat: change something");
        split(&repo, &args(true, None), &server).await.unwrap();
        assert_eq!(
            subjects(dir.path(), 3),
            [
                "feat: change something",
                "feat: change something",
                "docs: change something"
            ]
        );
        assert_eq!(files_of_commit(dir.path(), "HEAD"), ["README.md"]);
        assert_eq!(files_of_commit(dir.path(), "HEAD~1"), ["defs.txt"]);
        assert_eq!(files_of_commit(dir.path(), "HEAD~2"), ["uses.txt"]);
        assert!(run(dir.path(), &["status", "--porcelain"]).is_empty());
    }

    #[tokio::test]
    async fn merges_a_commit_that_fails_the_check_and_every_commit_then_passes() {
        let (dir, repo) = staged_three_parts();
        let server = FakeServer::new(DOCS_USES_DEFS, "feat: change something");
        split(&repo, &args(true, Some(NEEDS_DEFS)), &server)
            .await
            .unwrap();
        assert_eq!(subjects(dir.path(), 2).len(), 2);
        assert_eq!(files_of_commit(dir.path(), "HEAD"), ["README.md"]);
        assert_eq!(
            files_of_commit(dir.path(), "HEAD~1"),
            ["defs.txt", "uses.txt"]
        );
        // The first commit of the repository's own is two further back.
        assert_eq!(
            run(dir.path(), &["log", "-1", "--format=%s", "HEAD~2"]).trim(),
            "add the readme"
        );
    }

    #[tokio::test]
    async fn commits_nothing_when_the_whole_change_fails_the_check() {
        let (dir, repo) = staged_three_parts();
        fs::remove_file(dir.path().join("defs.txt")).unwrap();
        run(dir.path(), &["add", "--all"]);
        let plan = r#"{"commits":[{"title":"Readme","units":[1]},{"title":"Uses","units":[2]}]}"#;
        let head = run(dir.path(), &["rev-parse", "HEAD"]);
        let server = FakeServer::new(plan, "feat: change something");
        let error = split(&repo, &args(true, Some(NEEDS_DEFS)), &server)
            .await
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("fails with your changes"));
        assert_eq!(run(dir.path(), &["rev-parse", "HEAD"]), head);
        assert_eq!(
            run(dir.path(), &["diff", "--cached", "--name-only"])
                .lines()
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn stops_rather_than_undo_a_commit_made_while_it_waits() {
        let (dir, repo) = staged_three_parts();
        let path = dir.path();
        let elsewhere = move || {
            run(path, &["stash", "-q"]);
            write(path, "elsewhere.txt", "x\n");
            run(path, &["add", "elsewhere.txt"]);
            run(path, &["commit", "-q", "-m", "made elsewhere"]);
        };
        let server =
            FakeServer::new(DOCS_USES_DEFS, "feat: change something").during_message(elsewhere);
        let error = split(&repo, &args(true, None), &server)
            .await
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("HEAD moved"), "{error:#}");
        assert_eq!(
            run(path, &["log", "-1", "--format=%s"]).trim(),
            "made elsewhere"
        );
        assert!(run(path, &["ls-files"]).contains("elsewhere.txt"));
    }

    #[tokio::test]
    async fn stops_when_head_moves_while_the_plan_is_made() {
        let (dir, repo) = staged_three_parts();
        let path = dir.path();
        let elsewhere = move || {
            run(
                path,
                &["commit", "-q", "--allow-empty", "-m", "made elsewhere"],
            );
        };
        let server = FakeServer::new(DOCS_USES_DEFS, "feat: x").during_plan(elsewhere);
        let error = split(&repo, &args(true, None), &server)
            .await
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("HEAD moved"), "{error:#}");
        assert_eq!(
            run(path, &["log", "-1", "--format=%s"]).trim(),
            "made elsewhere"
        );
        assert_eq!(
            server.message_requests.get(),
            0,
            "it stopped before the wait"
        );
    }

    /// A tracked file with two changes, not staged, for a two-commit plan.
    fn tracked_two_places() -> (TempDir, Repo) {
        let (dir, repo) = isolated_repo();
        write(dir.path(), "lib.txt", &numbered_lines(40));
        run(dir.path(), &["add", "--all"]);
        run(dir.path(), &["commit", "-q", "-m", "add the file"]);
        let changed = numbered_lines(40)
            .replace("line 2\n", "two\nand a half\n")
            .replace("line 37\n", "thirty-seven\n");
        write(dir.path(), "lib.txt", &changed);
        (dir, repo)
    }

    const TWO_COMMITS: &str =
        r#"{"commits":[{"title":"First","units":[1]},{"title":"Second","units":[2]}]}"#;

    #[tokio::test]
    async fn commits_what_was_there_when_a_file_is_saved_during_the_wait() {
        let (dir, repo) = tracked_two_places();
        let path = dir.path();
        let save = move || {
            let text = fs::read_to_string(path.join("lib.txt")).unwrap();
            write(path, "lib.txt", &format!("{text}saved while waiting\n"));
        };
        let server = FakeServer::new(TWO_COMMITS, "feat: change").during_plan(save);
        split(&repo, &args(true, None), &server).await.unwrap();
        let committed = run(path, &["show", "HEAD:lib.txt"]);
        assert!(committed.contains("two\n") && committed.contains("thirty-seven\n"));
        assert!(!committed.contains("saved while waiting"));
        assert!(
            fs::read_to_string(path.join("lib.txt"))
                .unwrap()
                .contains("saved while waiting")
        );
        assert!(run(path, &["diff", "--name-only"]).contains("lib.txt"));
    }

    #[tokio::test]
    async fn commits_what_was_staged_when_more_is_staged_during_the_wait() {
        let (dir, repo) = tracked_two_places();
        let path = dir.path();
        run(path, &["add", "--all"]);
        let stage = move || {
            write(path, "lib.rs", "fn staged_later() {}\n");
            run(path, &["add", "lib.rs"]);
        };
        let server = FakeServer::new(TWO_COMMITS, "feat: change").during_plan(stage);
        split(&repo, &args(true, None), &server).await.unwrap();
        assert!(!run(path, &["show", "HEAD:lib.rs"]).contains("staged_later"));
        assert!(run(path, &["diff", "--cached", "--name-only"]).contains("lib.rs"));
    }

    #[tokio::test]
    async fn splits_with_intent_to_add_skip_worktree_and_assume_unchanged_files() {
        for flag in ["intent-to-add", "skip-worktree", "assume-unchanged"] {
            let (dir, repo) = tracked_two_places();
            let path = dir.path();
            match flag {
                "intent-to-add" => {
                    write(path, "planned.txt", "later\n");
                    run(path, &["add", "-N", "planned.txt"]);
                }
                other => {
                    write(path, "local.conf", "committed\n");
                    run(path, &["add", "local.conf"]);
                    run(path, &["commit", "-q", "-m", "a local file"]);
                    run(path, &["update-index", &format!("--{other}"), "local.conf"]);
                    write(path, "local.conf", "local edit\n");
                }
            }
            let plan = if flag == "intent-to-add" {
                r#"{"commits":[{"title":"First","units":[1]},
                   {"title":"Second","units":[2,3]}]}"#
            } else {
                TWO_COMMITS
            };
            let server = FakeServer::new(plan, "feat: change");
            split(&repo, &args(true, None), &server)
                .await
                .unwrap_or_else(|error| panic!("{flag}: {error:#}"));
            assert!(
                run(path, &["show", "HEAD:lib.txt"]).contains("thirty-seven"),
                "{flag}"
            );
        }
    }

    #[tokio::test]
    async fn fails_early_without_a_first_commit() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), &["init", "-q", "--initial-branch", "main"]);
        write(dir.path(), "a.txt", "a\n");
        run(dir.path(), &["add", "--all"]);
        let repo = Repo::open(dir.path()).unwrap();
        let error = changes_to_split(&repo, &args(true, None), &Config::default())
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("no commits yet"), "{error:#}");
    }

    #[tokio::test]
    async fn splits_from_a_subfolder() {
        let (dir, _) = staged_three_parts();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let repo = Repo::open_at_top(dir.path().join("sub")).unwrap();
        let server = FakeServer::new(DOCS_USES_DEFS, "feat: change something");
        split(&repo, &args(true, Some(NEEDS_DEFS)), &server)
            .await
            .unwrap();
        assert!(run(dir.path(), &["diff", "--cached", "--name-only"]).is_empty());
    }

    #[tokio::test]
    async fn moves_docs_last_in_a_new_plan_but_keeps_the_order_of_a_refined_one() {
        let (_dir, repo) = staged_three_parts();
        let args = args(true, None);
        let config = Config::default();
        let work = changes_to_split(&repo, &args, &config).unwrap().unwrap();
        let units = split::describe(&work.files, 100_000).unwrap().units;
        let server = FakeServer::new(DOCS_USES_DEFS, "feat: x");
        let model = model();
        let splitter = Splitter {
            repo: &repo,
            config: &config,
            args: &args,
            work: &work,
            units: &units,
            server: &server,
            model: &model,
        };
        let titles =
            |plan: &Plan| -> Vec<String> { plan.commits.iter().map(|c| c.title.clone()).collect() };
        let conversation = [Message::system(prompt::SPLIT_PROMPT)];
        let new = splitter.new_plan(&conversation).await.unwrap();
        assert_eq!(titles(&new), ["Add uses", "Add defs", "Update the readme"]);
        let refined = splitter
            .refined_plan(&conversation, &new, "put the readme first")
            .await
            .unwrap();
        assert_eq!(
            titles(&refined),
            ["Update the readme", "Add uses", "Add defs"]
        );
    }

    #[tokio::test]
    async fn leaves_the_index_at_head_after_a_split_of_tracked_changes() {
        let (dir, repo) = tracked_two_places();
        let server = FakeServer::new(TWO_COMMITS, "feat: change");
        split(&repo, &args(true, None), &server).await.unwrap();
        assert!(run(dir.path(), &["diff", "--cached", "--name-only"]).is_empty());
        assert!(run(dir.path(), &["status", "--porcelain"]).is_empty());
        let first = run(dir.path(), &["show", "HEAD~1:lib.txt"]);
        assert!(first.contains("two\nand a half\n") && !first.contains("thirty-seven"));
    }

    /// A pre-commit hook that changes what's committed, as a formatter that stages
    /// its work does.
    fn hook_that_rewrites(dir: &Path) {
        let hooks = dir.join("hooks");
        fs::create_dir(&hooks).unwrap();
        let script = "#!/bin/sh\n\
                      blob=$(echo formatted | git hash-object -w --stdin)\n\
                      git update-index --add --cacheinfo 100644,$blob,formatted.txt\n";
        write(&hooks, "pre-commit", script);
        let executable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        fs::set_permissions(hooks.join("pre-commit"), executable).unwrap();
        run(dir, &["config", "core.hooksPath", "hooks"]);
    }

    #[tokio::test]
    async fn goes_on_when_a_hook_changes_a_commit() {
        for check in [None, Some("true")] {
            let (dir, repo) = staged_three_parts();
            hook_that_rewrites(dir.path());
            let server = FakeServer::new(DOCS_USES_DEFS, "feat: change");
            split(&repo, &args(true, check), &server)
                .await
                .unwrap_or_else(|error| panic!("{check:?}: {error:#}"));
            assert_eq!(
                run(dir.path(), &["rev-list", "--count", "HEAD~3..HEAD"]).trim(),
                "3",
                "{check:?}"
            );
        }
    }

    /// Runs `commit_each` on two-commit changes with scripted decisions, and returns
    /// the outcome.
    async fn commit_with_decisions(
        repo: &Repo,
        args: &SplitArgs,
        decisions: Vec<Decision>,
    ) -> Result<Outcome> {
        let config = Config::default();
        let work = changes_to_split(repo, args, &config)?.expect("changes to split");
        let units = split::describe(&work.files, 100_000)?.units;
        let server = FakeServer::new(TWO_COMMITS, "feat: change");
        let model = model();
        let splitter = Splitter {
            repo,
            config: &config,
            args,
            work: &work,
            units: &units,
            server: &server,
            model: &model,
        };
        let conversation = [Message::system(prompt::SPLIT_PROMPT)];
        let plan = splitter.new_plan(&conversation).await?;
        let prepared = splitter.prepare_commits(&plan).await?;
        let index = repo.with_index(repo.git_path("test-split-index")?);
        let decisions = RefCell::new(decisions.into_iter());
        splitter
            .commit_each(
                &prepared,
                &index,
                async |_: String, _: &[Message], _: Option<Kind>| {
                    Ok(decisions.borrow_mut().next().expect("a decision"))
                },
            )
            .await
    }

    #[tokio::test]
    async fn goes_on_after_a_skip_with_a_commit_that_no_longer_holds_what_was_planned() {
        let (dir, repo) = tracked_two_places();
        run(dir.path(), &["add", "--all"]);
        let outcome = commit_with_decisions(
            &repo,
            &args(true, None),
            vec![Decision::Skip, Decision::Commit("feat: second".to_owned())],
        )
        .await
        .unwrap();
        assert_eq!(
            (outcome.made, outcome.skipped, outcome.stopped),
            (1, 1, false)
        );
        let committed = run(dir.path(), &["show", "HEAD:lib.txt"]);
        assert!(committed.contains("thirty-seven") && !committed.contains("and a half"));
    }

    #[tokio::test]
    async fn stops_when_the_user_stops_and_keeps_what_it_made() {
        let (dir, repo) = tracked_two_places();
        run(dir.path(), &["add", "--all"]);
        let outcome = commit_with_decisions(
            &repo,
            &args(true, None),
            vec![Decision::Commit("feat: first".to_owned()), Decision::Stop],
        )
        .await
        .unwrap();
        assert_eq!(
            (outcome.made, outcome.skipped, outcome.stopped),
            (1, 0, true)
        );
        assert_eq!(
            run(dir.path(), &["log", "-1", "--format=%s"]).trim(),
            "feat: first"
        );
    }

    #[tokio::test]
    async fn refuses_changes_with_unresolved_conflicts_before_asking_the_model() {
        let (dir, repo) = tracked_two_places();
        let blob = run(dir.path(), &["rev-parse", "HEAD:lib.txt"]);
        let mut child = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["update-index", "--index-info"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let unmerged = format!(
            "0 {} 0\tlib.txt\n100644 {} 2\tlib.txt\n",
            "0".repeat(40),
            blob.trim()
        );
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), unmerged.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
        let error = changes_to_split(&repo, &args(true, None), &Config::default())
            .err()
            .unwrap();
        assert!(
            format!("{error:#}").contains("unresolved conflicts"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn has_nothing_to_split_when_only_a_submodule_is_dirty() {
        let (dir, repo, _library, _) = with_submodule();
        write(&dir.path().join("library"), "loose.txt", "x\n");
        let work = changes_to_split(&repo, &args(true, None), &Config::default()).unwrap();
        assert!(work.is_none());
    }
}
