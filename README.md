# git-agent

[![crates.io](https://img.shields.io/crates/v/git-agent.svg)](https://crates.io/crates/git-agent)

git-agent writes git commit messages for your changes with a language model, either
one running on your own machine (LM Studio, Ollama, llama.cpp, vLLM) or one from a
cloud provider such as OpenAI, Anthropic or OpenRouter. You review each proposed
message, then commit it, edit it, regenerate it, or tell the model what to change.
Models make mistakes, so read each message before you commit it.
[Mistakes and speed](#mistakes-and-speed) says what to expect.

The same model also names new branches, splits a large change into several commits,
tidies commits before you push them, and writes pull requests for GitHub. It can
explain why code looks the way it does, or what a set of commits changed.

## Why I made git-agent

I made git-agent mainly to get the commit message generation from IntelliJ IDEA
without running IDEA. IDEA uses nearly 3-4 GB of RAM, and I need that memory for
running local language models. git-agent has since evolved into more than just generating single commit messages; it now has advanced features to help you automate more of your workflow and chores in a git repo.

## Install

Because the binary is called `git-agent`, git also runs it as `git agent`. It needs git
2.26 or newer.

### With Homebrew

```sh
brew install dervism/tap/git-agent
```

Homebrew builds git-agent from source, and installs its own Rust for the build if you
don't have it yet. `brew upgrade git-agent` updates git-agent, and
`brew uninstall git-agent` removes it.

### From crates.io

git-agent is published on [crates.io](https://crates.io/crates/git-agent). With Rust
1.88 or newer installed, run:

```sh
cargo install git-agent --locked
```

Cargo builds an optimized binary and copies it to `~/.cargo/bin/` (or
`$CARGO_HOME/bin/`). `--locked` uses the exact dependency versions the release was
tested with. Run the same command again to update to the latest release, and
`cargo uninstall git-agent` to remove it.

### From a local checkout

To try changes that aren't released yet, install from a clone of the repository:

```sh
cargo install --path . --locked
```

### If you installed Rust with Homebrew or a Linux package manager

`~/.cargo/bin` must be on your `PATH`. rustup adds it for you, but Homebrew and Linux
package managers don't, so add it yourself, for example in `~/.zshrc`:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

## Usage

### Commands

```sh
git agent                  # describe staged changes, then review
git agent --all            # stage everything (including untracked files) first
git agent -H "fixes #42"   # give the model extra context
git agent --commit         # commit right away, no review
git agent --push           # review, then push after committing
git agent --commit --push  # commit and push, no review
git agent --raw            # print just the message, e.g. for `| git commit -F -`
git agent -- --signoff     # pass extra arguments to `git commit`
git agent branch fix login # name a new branch after what it is for, then create it
git agent split            # split the changes into several commits
git agent tidy             # reword unclear messages and fold fixes, before you push
git agent pr               # write a pull request for the branch, then create it
git agent why src/fit.rs:120  # explain why a line is the way it is
git agent explain          # explain the last commit, a range, a branch or a pull
```

### Which changes are included

If you have staged changes, git-agent describes and commits only those. If nothing is
staged, it describes all changes to tracked files and commits them with
`git commit --all`. Untracked files are left out unless you use `--all`.

If files have unresolved conflicts, git-agent lists them and stops, before `--all` could
mark them as resolved. A merge whose conflicts you've resolved commits as usual.

If a submodule's `ignore` setting hides its change from `git diff`, neither you nor the
model would see it, so git-agent leaves it out of the commit and says so. `--all` doesn't
stage it either. If you stage it yourself, it's committed, with a note, and `--all` keeps it
at the commit you staged even if the submodule has moved on since.

### Untracked files

Before `--all` stages anything, git-agent checks the untracked files for ones that are
usually ignored rather than committed:

- build output and downloaded dependencies, such as `target/` next to a `Cargo.toml`,
  `dist/` next to a `package.json`, or `node_modules/`
- compiled files, such as `.o`, `.class` or `.pyc`, and files like `.DS_Store` that the
  operating system leaves behind
- `.env` files and private keys, which may hold secrets
- files of 10 MB or more

It lists what it found and asks what to do:

```text
warning: some untracked files look like they should be ignored rather than committed:
   .env                environment file; it may hold secrets
   data.bin            large file, 12 MB
   target/             Rust build output, 29 files, 991 KB
Add them to .gitignore? [Y]es  [n]o, commit them too  [q]uit ›
```

`y` adds them to `.gitignore`, which is staged and committed with your changes. `n` stages
them anyway, and `q` stops before anything is staged. When git-agent can't ask, as with
`--commit`, `--raw` or input from a pipe, it leaves them out of the commit and changes
nothing else. Common names such as `dist/` or `build/` only count when the file that marks
their project sits next to them, so a `dist/` folder of your own is left alone. The same
check runs for `git agent split --all`.

### Reviewing the message

git-agent shows the proposed message and waits for one of these keys:

| Key | Action |
| --- | --- |
| `c` | Commit with this message |
| `p` | Commit with this message, then push |
| `e` | Open it in your git editor (`GIT_EDITOR`, `core.editor`, `VISUAL`, `EDITOR`) |
| `r` | Generate a new message |
| `f` | Tell the model what to change, e.g. `f shorter, and mention the CLI flags` |
| `q` | Quit without committing |

git-agent takes control and invisible characters out of the model's reply, and says so, so
that what you review is what gets committed.

### If the commit fails

When `git commit` fails, for example because a hook rejects it, git-agent saves the
message to `.git/GIT_AGENT_MSG`. You can reuse it with
`git commit --file .git/GIT_AGENT_MSG`.

## Starting a branch

`git agent branch` names a new branch after what it is for, creates it and switches
to it.

```sh
git agent branch fix login timeout        # name it after a description
git agent branch 42                       # after GitHub issue #42, looked up with gh
git agent branch                          # after the uncommitted changes
git agent branch -c add dark mode         # create it right away, no review
git agent branch --from origin/main ...   # start it at origin/main instead of HEAD
git agent branch --raw fix login timeout  # print just the name
```

The name starts with the kind of work, as in `fix/login-timeout` or
`feat/42-dark-mode`. The model sees your recent branches and follows their pattern, so
in a repository with branches like `feature/PROJ-12-avatars`, the names look like
`feature/dark-mode`. A name has an issue number or ticket key only when the
description or the issue has one; git-agent removes any that the model makes up or
copies from another branch.

Without a description, git-agent names the branch after your uncommitted changes. That
helps when you started on main and want the work on a branch of its own. The changes
come along to the new branch.

The review works like the one for commit messages: `c` creates the branch, `e` lets
you edit the name in place, `r` asks for a new name and `f` tells the model what to
change. git-agent won't create a branch with the name of an existing local or remote
branch.

The new branch starts at HEAD, or where `--from` says, and has no upstream yet. Its
first push sets an upstream of the same name, so it never pushes to the branch it
started from. Looking up an issue needs the [GitHub CLI](https://cli.github.com), as
pull requests do.

## Splitting a change into commits

When your changes do several things at once, `git agent split` turns them into
several commits. The model groups the changes by what they are for, and you review
its plan and then each commit's message.

```sh
git agent split                # plan the commits, then review each one
git agent split --all          # stage everything, including untracked files, first
git agent split -H "why"       # give the model extra context
git agent split --commit       # make the commits without any review
git agent split --push         # push after the last commit
git agent split --check "cargo check"   # make sure each commit builds
git agent split -- --signoff   # pass extra arguments to each `git commit`
```

### How git-agent plans the commits

git-agent splits the changes a commit would include: the staged ones, or all changes
to tracked files if nothing is staged. It cuts them into parts. A part is one hunk of
a changed file, or a whole file when the file is new, deleted, renamed or binary, or
excluded like a lockfile. The model sees the numbered parts and groups them into
commits, ordered so that each builds on those before it. If the plan leaves out three
or more parts, git-agent asks the model once more to place them, and keeps whichever
plan leaves out fewer. A part still left out joins the commit with the rest of its file,
or a last commit. When the hunks are too many to show the model one by one, it groups
whole files instead. Commits that only change documentation go last in a new plan.

### Reviewing the plan

```text
── Proposed commits ────────────────────────────────────────────
1. Introduce LRU caching for configuration loading
   Cargo.lock, Cargo.toml, README.md, src/cache.rs (new file), src/config.rs (1 of 3 changes)
2. Refine config parsing logic and documentation
   src/config.rs (2 of 3 changes)

7 parts in 2 commits
────────────────────────────────────────────────────────────────
[c]ommit  [a]ll commits  [e]dit  [r]egenerate  [f]eedback  [q]uit  [?] ›
```

`c` commits them one at a time, and you review each message. `a` makes all the commits
without stopping at each message; you still see the messages as they are committed.
`e` opens the plan in your git editor, where each part has a numbered
line under its commit; move a line to move the part to another commit. `r` asks for a
new plan, and `f` tells the model what to change, such as
`f put the feature and its documentation in one commit`.

### Making the commits

git-agent then makes the commits in order. For each one it writes the message from
that commit's own changes. If you chose `c` at the plan, you review each message like
any other, with two differences: `c` commits, and `s` skips the commit and leaves
its changes as they were. `q` stops; the commits made so far stay. Changes that aren't committed stay
where they were, staged or not.

git-agent keeps to the changes as they were when you started: what you save or stage while
it waits for the model stays uncommitted, and what you stage stays staged. If a commit
touches a file you staged something else in, git-agent unstages that file and says so, and
the file itself keeps its content. If another commit lands on the branch before the split is
done, it stops, and the commits it made so far stay. A commit hook that changes a commit is
fine, but the check doesn't cover what the hook did.

Before the first commit, git-agent works out what each commit will hold. A commit that
can't be made on its own, such as one that adds a file to a folder that a later commit
still has to clear, merges with the next one. If the plan doesn't add up to your changes,
git-agent stops and commits nothing. A commit with only documentation or only tests gets
the type `docs` or `test`, if its message has a Conventional Commits type.

Secrets are checked as for any commit, and git-agent won't split changes while a merge
or rebase is in progress, or while files have unresolved conflicts. You can run
`git agent split` from any folder of the repository.

### Checking that each commit builds

Without `--check`, git-agent doesn't build the commits, so a commit may not compile on
its own if the plan separates code that belongs together. With `--check`, it runs a
command you choose on each planned commit:

```sh
git agent split --check "cargo check --all-targets"
git agent split --check "mvn -o -q test-compile" --check-timeout 1200
git agent split --check "test -d node_modules || npm ci; npm run build"
```

The check runs after you accept the plan and before any message is written. It starts
with the whole change, and if that fails, git-agent stops and commits nothing. Then it
checks each earlier commit in order. A commit that fails merges with the next one, and
git-agent shows which commits became one and the line of the output that says why.

The command runs in a clean temporary checkout of your project, through `sh`, with no
input and without your hooks. Ignored files aren't there, such as `node_modules`, `.env`
and generated code, and submodule folders are empty, so the command has to install or
generate what it needs. Build output is removed after the whole change, and before a
commit that deletes or renames a file, so it can't hide what a commit lacks.

Each run may take 600 seconds, or what `--check-timeout` says. A timeout stops the split
with nothing committed, and so does Ctrl-C, a command that a signal ends, or one that can't
start on the whole change. On an earlier commit, a command that can't start counts as a
failure, because a later commit may be what adds it. If you skip a commit when you review the messages, the
commits after it weren't checked without it.

The check runs your project's code, as any build does. The command only ever comes from
the flag, so a repository you clone can't choose one.

### Which model to use

No model we tried split a change reliably on its own, so use `--check`, whatever the model. The
larger the change, the longer the plan takes and the more it can get wrong, so split a large
change in pieces: stage part of it, run `git agent split` without `--all`, and then do the rest.

## Tidying commits before you push

`git agent tidy` cleans up the commits you haven't pushed yet. A commit whose message
doesn't say what it does, such as "wip" or "asdf", gets a new message written from its
changes. A commit that only fixes an earlier one, such as "fix typo", is folded into
the commit it fixes.

```sh
git agent tidy                 # plan the tidy-up, then review it and each new message
git agent tidy --base HEAD~5   # tidy the commits after HEAD~5
git agent tidy -H "why"        # give the model extra context
git agent tidy --apply         # tidy without any review
```

### Reviewing the plan

```text
── Proposed tidy-up ────────────────────────────────────────────
keep   a6fbe58 feat: add csv export
reword ed39c0d wip
fold   842f69d fix typo
keep   8e249fb docs: document the export
fold   7f944d8 fixup! docs: document the export
reword 237af0f asdf

6 commits become 4: 2 folded into others, 2 with a new message
────────────────────────────────────────────────────────────────
[a]ccept  [e]dit  [r]egenerate  [f]eedback  [q]uit  [?] ›
```

A folded commit joins the commit on the line above it and keeps that commit's message.
Commits named `fixup! <subject>` or `squash! <subject>`, as `git commit --fixup` makes
them, fold into the commit they name unless you tell the model otherwise. `e` opens
the plan in your git editor as lines like those of `git rebase -i`: change a line's
first word to keep, reword or fold, and move a fold line under the commit it fixes.
Every commit must stay in the plan.

After you accept the plan, git-agent writes the new messages one by one, and you review
each like any commit message, except that `u` uses the new message and `k` keeps the
old one. `q` stops before anything has changed.

### What git-agent changes

git-agent only tidies commits that no remote branch has, so it never has to
force-push. In a repository without a remote, name the commit to tidy after with
`--base`. It doesn't tidy across merges, or more than 40 commits at a time.

The rewrite runs `git rebase` in a temporary worktree, without hooks, so your work
tree and index stay as they are, uncommitted changes included. The branch moves only
if the tidied commits end with exactly the same files as before, and git-agent then
prints the command that undoes the tidy-up:

```text
✅ Tidied 6 commit(s) into 4:
   a6fbe58 feat: add csv export
   63b8816 feat(export): add optional CSV header support
   7d6754a docs: document the export
   bf40777 test(export): add a test for the CSV header
   To undo: git reset --keep 237af0f
```

If folding a commit into an earlier one conflicts with the commits between them,
git-agent stops and leaves the branch as it was.

## Pushing

git-agent pushes with a plain `git push`, so it works with GitHub or any other remote
and uses your existing git credentials. The first time you push a branch that has no
upstream, git-agent sets one: it uses `remote.pushDefault` if you've configured it,
otherwise your only remote, otherwise `origin`. If the repository has no remote yet,
create one on GitHub with `gh repo create --source . --push`.

If the push fails, the commit stays. Fix the problem and run `git push`.

### When the remote has new commits

Before pushing, git-agent fetches from the remote. If the remote has commits you don't
have, such as a file you edited on GitHub, it lists them and rebases your commit on
top of them. Your history stays linear and the remote commits stay as they are. From
the review menu, git-agent asks before it rebases; `--commit --push` rebases without
asking. Any uncommitted changes are set aside during the rebase and restored
afterwards.

### Conflicts

If your commit conflicts with the remote commits, git-agent cancels the rebase and
leaves your branch as it was. Run `git pull --rebase`, resolve the conflicts, then run
`git push`. git-agent never force-pushes.

## Pull requests

`git agent pr` writes a title and description for the current branch from its
commits and its diff, then creates the pull request on GitHub. The pull request
merges into the repository's default branch unless you name another with `--base`.
git-agent fetches that branch first, so commits that reached it from elsewhere aren't
described as yours.

```sh
git agent pr                  # write the pull request, then review it
git agent pr --base develop   # merge into develop instead of the default branch
git agent pr --draft          # create it as a draft
git agent pr -H "fixes #42"   # give the model extra context
git agent pr --create         # create it right away, no review
git agent pr --raw            # print just the title and description
git agent pr -- --label bug   # pass extra arguments to `gh pr create`
```

### Reviewing the pull request

The first line of the proposal is the title, and the rest is the description in
Markdown. The review works like the one for commit messages, except for its first two
keys: `c` creates the pull request and `d` creates it as a draft. If the branch isn't
on GitHub yet, or GitHub lacks some of its commits, git-agent pushes it first, the
same way it pushes after a commit. Uncommitted changes are not part of the pull
request, and git-agent tells you when you have some.

### What it needs

git-agent creates pull requests with the [GitHub CLI](https://cli.github.com), so it
uses your existing GitHub login. Install `gh` and run `gh auth login` once. Only
`--raw` together with `--base` works without it.

If the branch already has an open pull request, git-agent shows its address and
stops. If `gh pr create` fails, git-agent saves the description to
`.git/GIT_AGENT_PR.md` and prints the command that creates the pull request from it.

## Explaining code and history

Two commands answer questions about code that is already committed. Neither changes
anything in the repository.

### Why a line is the way it is

`git agent why` explains lines of code from their history.

```sh
git agent why src/fit.rs:120              # one line
git agent why src/fit.rs:120-140          # a range of lines
git agent why src/fit.rs:share            # a function
git agent why src/fit.rs:120 "why 400?"   # ask something specific
```

The model gets the lines with some code around them, the commit that last changed
each line, and the history of the lines: every commit that changed them, newest
first, with its message and its diff of those lines. The history follows the lines
through renames, and blame skips commits that only change whitespace or move the
lines. When the history is too long for the context, git-agent cuts the diffs short
and keeps the newest commits and the oldest one, since the commit that added the
lines often says why they are there.

git-agent finds a function the way `git log -L :name:file` does, by the line that
starts it. It turns on git's built-in function detection for common languages, among
them Rust, Python, Go, Java, Kotlin, C, C++, C#, Ruby and PHP, so methods inside
classes and impl blocks are found too. Your own `.gitattributes` still takes
precedence.

Lines with uncommitted changes are marked, and the rest are explained from the
committed version. If none of the lines are committed yet, there is no history to
explain, and git-agent says so.

git-agent reads the lines from the file in your work tree. When git stores the file
through a filter, as git-crypt does to keep it encrypted in the repository while it
stays readable in your work tree, or as Git LFS does, git-agent asks before it sends
those lines to the model. Pressing Enter answers no, and without a terminal to ask
at, it doesn't send them. It doesn't follow a symbolic link, or a path that leads out
of the repository: name the file the link points to instead.

### Explaining commits

`git agent explain` summarizes commits: what they change and why, and what you may
need to act on, such as a breaking change or a new setting.

```sh
git agent explain                          # the last commit
git agent explain a1b2c3d                  # another commit
git agent explain v0.5.0..v0.6.0           # a range of commits
git agent explain --branch                 # the current branch since it left the default branch
git agent explain --branch feature         # another branch
git agent explain --pull                   # what the last git pull brought in
git agent explain -q "anything breaking?"  # ask something specific
```

A merge commit is explained through the commits it merged. `--branch` compares the
branch with the remote's default branch, such as `origin/main`, unless you name
another with `--base`. `--pull` looks in the current branch's reflog for the last
`git pull` that changed it. A pull with rebase puts new copies of your own commits on
top of the ones that arrived; git-agent leaves the copies out and explains only what
arrived.

The model gets the commit messages and the diff of everything the commits change
together. A long range is shortened the same way as a large commit (see
[Large changes](#large-changes)).

### Follow-up questions

After the answer, both commands wait for a follow-up question, and the model answers
it with the same code and history at hand. Press Enter on an empty line to finish.
git-agent stops asking when the conversation no longer fits in the model's context.
When the output goes to a pipe or a file, it prints the answer and exits.

Secrets in the code and the history are replaced with `[REDACTED]` before anything is
sent, as for commits. These commands commit and push nothing, so they only warn about
secrets, whatever `on_secrets` is set to.

## Choosing a model

git-agent sends your changes to the model server at `base_url`. By default that is
`http://localhost:1234/v1`, the address of LM Studio running on your own machine.
To use another local server, such as Ollama, or a cloud provider, choose it in
[`git agent config`](#changing-settings) or change `base_url` in the config file.

### Local servers

With a local server, you don't have to configure a model. git-agent asks the server
which model is loaded and uses that one. If several models are loaded, or none is and
the server has more than one, git-agent asks you to pick one with `git agent config`
or `--model`. `git agent models` lists the server's models and marks the one
git-agent uses.

### Mistakes and speed

A model can write a message that describes a change that isn't in the diff, or gives a reason
nobody stated, and such a message reads as well as a correct one. Read each message before you
commit it. `--commit`, `a` at a split plan and `tidy --apply` skip that review.

The models small enough to run on a laptop trade one problem for another. In our use, the fast
ones make these mistakes often, and the ones that make fewer can take minutes to answer. Every
model also gets slower as your changes grow, because it reads the whole prompt before it writes
anything.

- Commit small changes often. A small diff is quicker, and the model sees all of it.
- With a slow model, raise `timeout_secs`. Its default of 300 seconds covers the whole reply.
- With `git agent split`, use `--check`, so that each commit builds whatever the model planned.

### Cloud providers

git-agent works with any provider that has an OpenAI-compatible API, and with
Anthropic through its own Messages API. Your diffs then leave your machine and go to
that provider, without the [secrets](#secrets-in-your-changes) git-agent finds in them.

To set one up, run `git agent config`, choose Provider, and pick it from the list:
OpenAI, Anthropic, OpenRouter, Google Gemini, Mistral, Groq, DeepSeek, xAI, Together
AI, Fireworks AI or Cerebras. "Other" takes the address of any other OpenAI-compatible
API. git-agent asks for your API key, then lists the provider's models for you to pick
from. You can type to filter the list.

Most providers don't report a model's context window, so the menu asks for it after
you pick a model. You'll find it on the provider's model page. Without it, git-agent
assumes 4,096 tokens and shortens large diffs much more than it needs to. Anthropic
and OpenRouter report it, so the menu doesn't ask for their models.

You can also set the provider in the config file:

```toml
base_url = "https://api.example.com/v1"   # the provider's OpenAI-compatible endpoint
model = "model-id"                        # an ID from `git agent models`
context_length = 128000                   # from the provider's model documentation
```

### API keys

git-agent never stores API keys in the config file. On macOS, `git agent config`
saves each key in your keychain, under the name `git-agent` and the provider's host
(such as `api.openai.com`), and git-agent reads it from there on every run. Each
provider keeps its own key, so you can switch between them. After you install a new
version of git-agent, macOS may ask whether git-agent can use the saved key; choose
Always Allow.

On other systems, set the `GIT_AGENT_API_KEY` environment variable. It also works on
macOS, and like `--api-key`, it overrides a saved key:

```sh
export GIT_AGENT_API_KEY="your-api-key"
```

## Secrets in your changes

Before git-agent sends anything to the model, it looks for secrets in it and replaces
each one with `[REDACTED]`. That covers the diff, the commit messages of pull requests
and explained commits, the commits that `git agent tidy` sends, and the code and
history that `git agent why` sends. The model still sees that something was there,
but the secret itself never goes into a prompt, whichever server you use.

git-agent reads every diff as git stores the files, without the textconv drivers
that turn a file into readable text for `git diff`. So a file that git-crypt
encrypts reaches the model as a binary file, not decrypted.

### Rejecting or warning

By default, git-agent also refuses to commit or push secrets. When your changes add
one, it stops before it asks the model and lists what it found:

```text
error: the changes add what look like secrets, so git-agent didn't commit them:
   .env:2  DB_PASSWORD value
   src/client.rs:14  GitHub token
Take them out of the changes. If they aren't real secrets, add --on-secrets warn, or choose Warn under Secrets in `git agent config`.
```

The checks read what git publishes, which is more than the model reads: every file,
including lockfiles and the other files that `exclude` keeps out of the prompt, and
what a merge commit adds itself. A file that git shows only as binary, or that
`.gitattributes` marks `-diff`, is read whole: a text file line by line, and a binary
file by the runs of printable characters in it, a byte to a character or in UTF-16,
up to 16 MiB. git-agent lists a larger
binary file as not checked, and carries on. Git LFS sends its content to the LFS
server without passing it through git, and the inside of a compressed archive can't
be read this way, so neither is checked.

Before every push, after any rebase onto the remote, git-agent checks each commit the
push would send, including its message, because a secret that a later commit removes
is still in the history. `git agent pr` checks every commit on the branch the same way
before it writes the pull request. A secret on a line that your change leaves alone or
removes doesn't stop anything; git-agent only lists it.

To be warned instead, set `on_secrets = "warn"` in the config file, choose Warn under
Secrets in `git agent config`, or pass `--on-secrets warn` for one run. git-agent then
lists the secrets and carries on, and the model still gets `[REDACTED]` in their place.
Secrets in files the model doesn't read, such as lockfiles, are listed too, and a push
lists every secret its commits add, even one the commit already listed.

### What git-agent recognizes

- API keys and tokens in the formats of GitHub, GitLab, Slack, Stripe, AWS, OpenAI,
  Anthropic, Google, Hugging Face, Groq, xAI, npm and PyPI, and JSON web tokens
- private keys (`-----BEGIN … PRIVATE KEY-----`)
- passwords and tokens in URLs, such as `postgres://app:s3cret@db.example.com`,
  and bearer tokens
- values assigned to names such as `password`, `secret`, `token` or `API_KEY`, in
  `.env` files, YAML, JSON or code

It leaves placeholders such as `your-api-key` and `changeme` alone, and references
such as `${API_KEY}` too. A secret in a format git-agent doesn't know can still get
through.

## Large changes

Each run starts with a line that names the model and shows how much of its context
the prompt uses:

```text
🤖 google/gemma-4-e4b (loaded on the server) · prompt about 24,578 of 131,072 context tokens
```

### Context window

git-agent asks the server how large the model's context window is, so it knows how
much of your changes it can send. LM Studio, Ollama, llama.cpp, vLLM, Anthropic and
OpenRouter report it. For other servers, and for models that aren't loaded yet,
git-agent assumes 4,096 tokens and tells you so. Set `context_length` to give it the
real size.

Large prompts take longer to process. To keep them smaller, set `context_length`
below the model's real context.

### How git-agent shortens a diff

The prompt always leaves room for the reply. When your changes don't fit, git-agent
shortens them in steps and stops as soon as they fit:

1. It leaves out the unchanged lines that git shows around each change.
2. It shares the remaining room between the files. Files with small diffs are sent
   whole and larger ones are cut short, so every file shows at least its first
   changes.
3. If there are too many files even for that, the largest ones are sent by name
   only. They still appear in the list of changed files.

### What git-agent tells you

When it shortens your changes, git-agent prints what it left out:

```text
warning: the changes need about 24,656 tokens, more than fit in the context, so git-agent sent part of them:
   46 of 58 files in full, without the unchanged lines around the changes
   12 cut short: src/builder/arg.rs (6 of 23 changed lines), src/builder/command.rs (6 of 93 changed lines), and 10 more
```

The message may then miss details from the shortened files. For better messages,
commit in smaller parts: [`git agent split`](#splitting-a-change-into-commits) does
that for you, or you can stage some of the files, run `git agent`, and repeat.
Loading the model with a larger context also helps.

## Configuration

### Changing settings

Run `git agent config` to change settings in a menu:

```text
git-agent settings · ~/.config/git-agent/config.toml
? Setting:
> Provider        LM Studio · http://localhost:1234/v1
  Model           automatic: the model loaded on the server
  API key         none (local servers usually need none)
  Context length  automatic: asked from the server
  Output limit    16,000 tokens (sent only to Anthropic)
  Temperature     automatic: 0.4 on local servers
  Timeout         300 seconds
  Secrets         reject: don't commit or push them
  Excluded files  common lockfiles
  System prompt   default: Conventional Commits
  Done
```

Move with the arrow keys and press Enter to change a setting. git-agent saves each
change to the config file right away. Esc goes back one step, and quits from the main
menu. Excluded files and the system prompt open in your git editor. The menu writes
the same commented file as `git agent config init`, so comments you added to the file
yourself are not kept.

These commands work with the settings too:

```sh
git agent config            # change settings in a menu
git agent config init       # write a commented config file with the defaults
git agent config show       # print the effective settings
git agent models            # list models on the server; * marks the one git-agent uses
```

### Where settings come from

Settings come from these sources, and later ones override earlier ones:

1. Built-in defaults
2. The config file: `~/.config/git-agent/config.toml` (or `$XDG_CONFIG_HOME/git-agent/config.toml`)
3. Environment variables
4. Command-line flags

API keys come from `--api-key`, then `GIT_AGENT_API_KEY`, then the macOS keychain.

### Settings

| Key | Flag | Environment variable | Default |
| --- | --- | --- | --- |
| `base_url` | `--base-url` | `GIT_AGENT_BASE_URL` | `http://localhost:1234/v1` |
| `model` | `-m`, `--model` | `GIT_AGENT_MODEL` | the model loaded on the server |
| (API key) | `--api-key` | `GIT_AGENT_API_KEY` | the macOS keychain, never the file |
| `context_length` | `--context-length` | `GIT_AGENT_CONTEXT_LENGTH` | reported by the server, or 4096 |
| `max_output_tokens` | `--max-output-tokens` | `GIT_AGENT_MAX_OUTPUT_TOKENS` | `16000` |
| `temperature` | `-t`, `--temperature` | | 0.4 on local servers, else the provider's default |
| `on_secrets` | `--on-secrets` | `GIT_AGENT_ON_SECRETS` | `reject` ([or `warn`](#rejecting-or-warning)) |
| `timeout_secs` | | | `300` |
| `exclude` | | | common lockfiles |
| `system_prompt` | | | Conventional Commits instructions |
| (file location) | `--config` | `GIT_AGENT_CONFIG` | see above |

These flags work before or after a subcommand, so `git agent -m qwen3 split` is the same as
`git agent split -m qwen3`. The other options belong to one command: a subcommand's go after its
name, and those of plain `git agent` only work without a subcommand.

### Temperature

On local servers, git-agent uses a temperature of 0.4 unless you set another one,
because a low temperature gives more predictable messages. Many cloud models accept
only their default temperature and reject requests that set another one, so with a
cloud provider git-agent sends a temperature only if you set one. When you switch to a
cloud provider, the menu offers to stop sending it. Anthropic's API takes no
temperature, so git-agent never sends one there.

### Output limit

Anthropic's API needs a limit on how many tokens the model may write in one reply.
git-agent asks for at most 16,000, which leaves current models plenty of room to think
before they write the message. To change it, use "Output limit" in `git agent config`,
`max_output_tokens` in the config file, or `--max-output-tokens`. Much higher limits
can make a reply take longer than the timeout. OpenAI-compatible servers use their own
limit.

### Using another model for one run

Flags and environment variables apply to a single run:

```sh
git agent -m qwen/qwen3-8b
GIT_AGENT_BASE_URL=http://localhost:11434/v1 git agent -m llama3.2   # Ollama
```

### Notes

- Files matching `exclude` (by default `Cargo.lock`, `package-lock.json` and similar)
  appear in the list of changed files sent to the model, but their diffs don't. The
  secret checks still read them, and `git agent tidy` names the files of a commit that
  changes only such files.
  Deleted files are sent by name, without their old content.
- git-agent removes reasoning blocks (`<think>…</think>`) and code fences around the
  model's reply.
- File names can hold letters from any language, whatever `core.quotePath` says. Control
  characters in a name stay escaped, as in `\033`.
- `git agent split` commits files that aren't UTF-8, such as Latin-1, byte for byte. The
  model sees `�` for the bytes it can't read.
- Set `NO_COLOR` to turn off colored output.

## Contributing

Contributions are welcome! Open an [issue](https://github.com/dervism/git-agent/issues)
for bugs and feature ideas, or send a pull request. For larger changes, please open an
issue first so we can discuss the approach. Please report security vulnerabilities
privately rather than in an issue, as the
[security policy](https://github.com/dervism/git-agent/security/policy) describes.

### Before you send a pull request

Make sure these pass:

```sh
cargo fmt --check
cargo clippy --all-targets
cargo test
```

The tests create temporary git repositories and don't need a running model server.

## License

git-agent is licensed under the [MIT License](LICENSE). The website in `site/` has its own
license, the GPL-3.0-or-later, with exceptions listed in `site/NOTICE`; among them, the parts
adapted from Tailwind Plus are not open source. By contributing, you agree that your
contributions are licensed under the terms of the part you change.
