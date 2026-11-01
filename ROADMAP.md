# Roadmap

Because git runs git-agent as `git agent`, new features fit in as subcommands such as
`git agent pr`. Most of the ideas below work from a diff or the git history. That suits
the small context windows of local models, and it lets the features reuse what
git-agent already has: model servers, diff shortening and the review menu.

## Features inspired by IntelliJ

### `git agent split` (DONE)

Splits one large change into several logical commits. The model groups the changed
files or hunks by purpose and proposes a set of commits, and each commit message goes
through the same review as a single commit does today. IntelliJ users do this by hand
with changelists. It is also what git-agent already recommends when a diff doesn't
fit: commit in smaller parts. The plan can be edited, regenerated or revised with
feedback before any commit is made, and each commit is built in a temporary index,
so stopping halfway leaves the rest of the changes as they were.

### `git agent review`

Reviews staged changes before they are committed, like IntelliJ's before-commit
analysis. It looks for leftover debug output, commented-out code, new TODOs and likely
bugs. It could also be a key in the existing review menu.

### `git agent resolve`

Helps with conflicts. Changes in a conflicted file that don't overlap are merged
without the model, as IntelliJ's merge tool does. For the real conflicts it shows both
sides and a proposed resolution, which can be accepted, edited or skipped. Today
git-agent cancels the rebase when a push runs into a conflict, and this would give
that case a next step.

### `git agent why src/fit.rs:120`

Explains why a line is the way it is, from blame and the commits and diffs that
touched it. It is IntelliJ's Annotate, with the model reading the history.

### `git agent explain <commit or range>`

Summarizes a commit, a branch, or everything that arrived with the last pull.

### Checking untracked files

IntelliJ asks before it adds new files to version control. With `--all`, git-agent
could warn about build output, `.env` files, private keys and large binaries, and
offer to add them to `.gitignore`. This needs no model.

## Git workflows people do by hand

### `git agent pr` (DONE)

Writes a pull request title and description from the branch's commits and its diff
against the base branch, and runs `gh pr create` once they are reviewed. Opening a
pull request is usually the step after a push, which git-agent already does. If GitHub
lacks some of the branch's commits, they are pushed first, and the review can create
the pull request as a draft.

### `git agent branch "fix login timeout"`

Creates a branch named after a description, here `fix/login-timeout`, or after an
issue number looked up with `gh`.

### `git agent tidy`

Cleans up commits before they are pushed. It rewords messages like "wip" and "fix
typo", and folds fixups into the commits they fix. It only touches commits that aren't
on the remote yet, so git-agent still never force-pushes.

### `git agent undo` and `git agent rescue`

`undo` reverts the last commit but keeps the changes, like IntelliJ's Undo Commit, and
refuses if the commit was pushed. `rescue` finds lost work in the reflog after a bad
reset or rebase, describes each candidate in plain words, and restores the chosen one
onto a new branch.

### `git agent release`

Suggests the next version from the Conventional Commits since the last tag (feat means
minor, fix means patch, a breaking change means major) and writes release notes. The
version part needs no model. git-agent's own `scripts/release.sh` could use both: it
takes the version as an argument and publishes GitHub's generated release notes.

## Useful for every feature

### Checking for secrets before anything leaves the machine (DONE)

Scans what git-agent sends to the model for API keys, tokens, private keys and
passwords, and replaces each one with `[REDACTED]`. That covers the diff and, for pull
requests, the commit messages, with local servers and cloud providers alike. By
default, git-agent also refuses to commit, push or open a pull request when that would
add a secret to the repository, and the `on_secrets` setting can make it only warn.
This matters more with each new feature that sends more data, such as pull request
descriptions, reviews or a branch's history.

### A `prepare-commit-msg` hook (PARKED)

`git agent hook install` installs a hook that fills in the message for commits made
from any editor or git GUI. That covers the lightweight code editors git-agent is
meant to work with, without opening a terminal. Not a priority for now.
