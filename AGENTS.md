# git-agent

git-agent is a Rust command-line tool that does git chores with a language model. It
writes commit messages, splits a large change into several commits, tidies commits before
a push, names branches, writes pull requests for GitHub, and explains why code looks the
way it does or what a set of commits changed. It works with local model servers (LM Studio,
Ollama, llama.cpp, vLLM) and cloud providers, through the OpenAI-compatible API or
Anthropic's Messages API. Because the binary is called `git-agent`, git also runs it as
`git agent`.

The application is under the MIT License. The repository also holds its website, an Astro
site in `site/` with a license of its own.

The first half of this file describes the repository: its layout, and how it is built,
tested, released and distributed. The second half, from "Strict rules" on, describes how
we work.

The owner is the person who owns this repository and gives you instructions. A subagent's
prompt from the lead doesn't count as the owner's permission.

## Repository layout

| Path | What it holds |
| --- | --- |
| `src/` | The application: one binary crate, `git-agent`. There is no library crate and no `tests/` folder. |
| `site/` | The website, published to GitHub Pages. |
| `docs/planning/` | The roadmap in `roadmap/` (section 2.0), and the backlog in `backlog/` (section 2.2). Private. |
| `docs/iterations/` | One folder per iteration (section 2.1). Private. |
| `docs/architecture/design/` | Design documents, one folder per iteration (section 2.3.3). Private. |
| `scripts/release.sh` | The release script. |
| `scripts/check-planning.rs` | Checks the private planning documents (section 2.3.4). |
| `template/` | The setup for new projects, for the planned `git agent project init`. |
| `.github/workflows/ci.yml` | The checks, on every push to `master` and every pull request. |
| `.github/workflows/pages.yml` | The workflow that builds and deploys the website. |
| `.github/dependabot.yml` | Weekly updates for Cargo, the Rust version and the GitHub Actions. |
| `.github/ISSUE_TEMPLATE/` | The issue forms (see "Issues and security reports"). |
| `.github/SECURITY.md` | The security policy: how to report a vulnerability privately. |
| `.claude/agents/` | The agent team (section 1.1). |
| `.claude/launch.json` | The `site` dev server, for previews in Claude Code. |
| `tmp/` | The only place for temporary files. Git ignores everything in it except `.gitkeep`. |
| `README.md` | The user documentation. The website's docs page is made from it. |
| `Cargo.toml`, `Cargo.lock` | The crate manifest and the locked dependency versions. |
| `rust-toolchain.toml` | The Rust version CI uses. |
| `clippy.toml` | Keeps writes to the terminal in `ui.rs`, which escapes them. |
| `LICENSE` | The MIT License, for the application. |

### Private documents

`docs/planning/`, `docs/iterations/` and `docs/architecture/design/` are in `.gitignore`, so
they can hold information that shouldn't be public, and nothing in them reaches GitHub. They
exist only on this machine, which has these consequences:

- Don't run `git clean -x` or `git clean -X`, because those delete ignored files.
- A fresh clone or a git worktree doesn't have them. Give an agent working in one what it
  needs in its prompt.
- Search tools such as `rg` skip ignored files, so read these by path, or use
  `rg --no-ignore docs/`.
- Public text doesn't link to them or quote them: the README, the website, issue forms, commit
  messages and pull requests.

### Modules

`src/main.rs` declares every module, parses the command line, and runs the flow of each
command, including the default one that writes a commit message (`generate`).

The modules for single commands:

| Module | What it does |
| --- | --- |
| `split.rs` | `git agent split`: cuts a change into units (hunks or whole files) and settles the model's plan into commits. |
| `split/flow.rs` | The steps of `git agent split`, from the changes to the commits. |
| `split/dry_run.rs` | Builds the tree each planned commit will leave, in a temporary index, before any commit is made. |
| `split/check.rs` | `split --check`: which trees to run the command on, in what order, and what each result means. |
| `split/command.rs` | Runs the `--check` command once, and stops it at the time limit or on a signal. |
| `split/signals.rs` | The signals that stop a check, and keeping them working after one. |
| `split/kinds.rs` | Tells commits with only documentation or only tests by their files, for their order and type. |
| `tidy.rs` | `git agent tidy`: plans which unpushed commits to reword or fold, and writes the rebase todo. |
| `branch.rs` | `git agent branch`: names a branch after a description, a GitHub issue or the uncommitted changes. |
| `why.rs` | `git agent why`: reads the target (a line, a range of lines or a function), and describes its blame and history for the model. |
| `explain.rs` | `git agent explain`: picks the commit, range, branch or last pull to explain. |
| `untracked.rs` | The check of untracked files before `--all` stages them. |
| `staging.rs` | What happens before the changes are read: refusing unresolved conflicts, staging for `--all`, and warning about submodule changes the diff hides. |
| `menu.rs` | `git agent config`: the interactive settings menu. |

The modules the commands share:

| Module | What it does |
| --- | --- |
| `cli.rs` | The command-line arguments, with clap. |
| `git.rs` | The wrapper around the `git` command line (`Repo`). |
| `git/names.rs` | How git writes file names, and the readable form git-agent shows them in. |
| `git/lossless.rs` | Text that holds any bytes, so a patch of a file that isn't UTF-8 reaches `git apply` unchanged. |
| `git/worktree.rs` | `TempWorktree`: a temporary checkout that leaves the user's work tree, index and branches alone. |
| `temp.rs` | `PrivateDir`: a temporary folder that only your user account can enter. |
| `github.rs` | Pull requests and issues through the GitHub CLI, `gh`. |
| `prompt.rs` | Builds the conversation sent to the model for every command, and cleans up its replies. |
| `server/` | Model servers: the `ModelServer` interface (`mod.rs`), `openai.rs`, `anthropic.rs` and `http.rs`. |
| `providers.rs` | The servers and providers git-agent knows the address of. |
| `models.rs` | Choosing the model, and the context size to fit the prompt into. |
| `fit.rs` | Shortens a diff to a size limit. |
| `redact.rs` | Finds secrets in text before it goes to a model, and replaces them with `[REDACTED]`. |
| `api_keys.rs` | API keys in the macOS Keychain, so they are never written to a file. |
| `config.rs` | The settings, and the order of the places they come from. |
| `ui.rs` | Terminal output and the review prompt. Everything it prints goes through one gate that escapes control characters. |
| `safe_text.rs` | Escapes or removes the characters that could change the terminal or hide text. |

## Building and running

```sh
cargo build                       # target/debug/git-agent
cargo run -- --help               # run it without installing; its arguments go after --
cargo install --path . --locked   # install this checkout as git-agent
```

The crate needs Rust 1.88 or newer (`rust-version` in `Cargo.toml`) and uses the 2024
edition. git-agent needs git 2.26 or newer to run, for `--pathspec-from-file`.

Set these two variables for every command that builds, tests or runs git-agent, so that
temporary files stay in `tmp/`:

```sh
export TMPDIR="$PWD/tmp" GIT_CEILING_DIRECTORIES="$PWD/tmp"   # from the repository root
```

The tests create their repositories in `TMPDIR` (through the `tempfile` crate), and
git-agent itself puts some files there (`std::env::temp_dir()` in `src/temp.rs`). Because `tmp/` is inside this repository, `GIT_CEILING_DIRECTORIES` stops git
in a test directory from walking up into this repository.

## Testing and checks

The tests are unit tests, in a `mod tests` at the end of the module they test. Tests that
need git create throwaway repositories and run the real `git`. None of them need a model
server. `api_keys::tests::keychain_round_trip` is ignored, because it needs the macOS
Keychain.

These checks must pass before a piece of work is done. The release script runs the same
ones:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The `CI` workflow (`.github/workflows/ci.yml`) runs these on Linux, with `--locked` on the
last two, on every push to `master` and every pull request. It has a read-only token and no
secrets, and each action is pinned to a full commit, which Dependabot updates. It uses the
Rust version in `rust-toolchain.toml`. Tools without `rustup`, such as Homebrew's Rust, ignore
that file, so a check can pass here and fail there until the versions match.

`scripts/check-planning.rs` isn't part of the crate, so these checks don't see it. To test it:

```sh
rustfmt --check --edition 2024 scripts/check-planning.rs
clippy-driver --edition 2024 --test -D warnings -o tmp/check-planning-lint scripts/check-planning.rs
rustc --edition 2024 --test -o tmp/check-planning-tests scripts/check-planning.rs && tmp/check-planning-tests
```

To measure complexity:

```sh
cargo clippy --all-targets -- -W clippy::cognitive_complexity -W clippy::too_many_lines
```

`too_many_lines` reports functions longer than 100 lines, and `cognitive_complexity` reports
functions that are hard to follow. No coverage or code metrics tool is installed (such as
`cargo-llvm-cov`, `tokei` or `rust-code-analysis-cli`); ask before installing one.

## The website

`site/` is a static site built with Astro 7 and Tailwind CSS 4. It needs Node 22.23.4 or
newer.

```sh
npm --prefix site install
npm --prefix site run dev      # http://localhost:4321/git-agent/
npm --prefix site run build    # the static site, in site/dist/
```

Set `TMPDIR` for these too, as "Building and running" shows. Node then keeps its compile
cache in `tmp/node-compile-cache`, so delete that with the other temporary files.

In Claude Code, start the dev server with the `site` configuration in `.claude/launch.json`.
Astro 7 runs its dev server in the background when it detects a coding agent, and
`site/AGENTS.md` shows how to stop it and read its logs.

- `site/src/content/site.ts` holds what the home page says, and reads the version from
  `Cargo.toml`.
- `site/src/design/` is the design system: the tokens, utilities, motion and shared
  components. Its `README.md` documents it.
- `site/src/blocks/` holds the sections adapted from Tailwind Plus blocks, and
  `site/src/components/` the site's own sections.
- The docs page (`/docs/`) is the root `README.md`, loaded as a content collection
  (`site/src/content.config.ts`). A change to the README changes the site.

For current Astro APIs, use the Astro Docs MCP server's `search_astro_docs` tool, and the
guides that `site/AGENTS.md` links to. The Astro guide for AI tools
(https://docs.astro.build/en/guides/build-with-ai/) sets the server up with
`claude mcp add --transport http astro-docs https://mcp.docs.astro.build/mcp`.

The site is under the GPL-3.0-or-later, with the exceptions listed in `site/NOTICE`.
`site/src/blocks/` is used under a personal Tailwind Plus license, for this website only,
and is not open source. Never copy it out of the site, publish it on its own, or move it
into `site/src/design/`. The logo is all rights reserved. Every file starts with an
`SPDX-License-Identifier` line naming its license.

## Releasing

Make a release only when the owner asks for one. `scripts/release.sh` releases a version from
`master`:

```sh
scripts/release.sh --dry-run 1.2.3   # show what would happen, and change nothing
scripts/release.sh 1.2.3             # release, asking before publishing to crates.io
scripts/release.sh --yes 1.2.3       # release without asking
```

It runs these steps, and skips the ones an earlier run already did:

1. Checks that `git`, `cargo`, `gh` and `curl` are installed and `gh` is logged in, that
   the working tree is clean, that the branch is the default branch and not behind
   `origin`, and that the version is newer than the current one.
2. Runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`.
3. Sets the version in `Cargo.toml`, updates `Cargo.lock`, and commits
   `chore(release): vX.Y.Z` with your own git identity.
4. Runs `cargo publish --dry-run`, then publishes the crate to crates.io. Publishing can't
   be undone. Until it happens, a failure or a "no" removes the version commit again.
5. Tags the commit `vX.Y.Z`, and pushes the branch and the tag together.
6. Creates the GitHub release, with notes generated by GitHub.
7. Points the Homebrew formula at the new tag, and pushes the tap.

Run it with the two variables from "Building and running" set. One step still uses the
system temp folder: the script clones the tap with `mktemp -d`, which ignores `TMPDIR` on
macOS. The script deletes the clone when it is done.

## Distribution

### crates.io

The crate is `git-agent` on https://crates.io/crates/git-agent, and users install it with
`cargo install git-agent --locked`. Publishing uses the token Cargo keeps from
`cargo login`. The `include` list in `Cargo.toml` names what goes in the package, and the
rest of the repository stays out; check what the package holds with `cargo package --list`
before a release.

### GitHub releases

Each release has a tag, `vX.Y.Z`, and a release at
https://github.com/dervism/git-agent/releases.

### Homebrew

The tap is the repository `dervism/homebrew-tap`, and users install with
`brew install dervism/tap/git-agent`. `Formula/git-agent.rb` builds git-agent from the
tag's source archive with Homebrew's own Rust (`depends_on "rust" => :build`); there are
no bottles. The formula's test runs `git-agent --version` and `git-agent config init`,
which need no model server or Keychain.

The release script changes only the formula's `url` and `sha256`. Other fields, such as
`desc`, are edited by hand in the tap. The tap's CI (`brew test-bot`, in the tap's
`.github/workflows/tests.yml`) builds and tests the formula only for pull requests. On a
push to `main`, which is how the release script updates the tap, it only checks the tap's
syntax.

### The website

The `Website` workflow (`.github/workflows/pages.yml`) builds `site/` with
`withastro/action` and deploys it with `actions/deploy-pages` to
https://dervism.github.io/git-agent/. It runs on pushes to `master` that change `site/`,
`README.md`, `Cargo.toml` or the workflow, and by hand from the Actions tab. A release
changes `Cargo.toml`, so every release also deploys the site again. `site` and `base` in
`site/astro.config.mjs` must match the address. Follow a run with
`gh run list --workflow pages.yml` and `gh run watch`.

## Issues and security reports

People open issues through the forms in `.github/ISSUE_TEMPLATE/`. `config.yml` turns blank
issues off for everyone without write access, and links to the security policy and the
documentation.

| Form | For | Label |
| --- | --- | --- |
| `1-bug-report.yml` | git-agent did something wrong, crashed or printed an error. | `bug` |
| `2-improvement.yml` | Making something git-agent already does work better. | `enhancement` |
| `3-feature-request.yml` | A new command, option or workflow. | `enhancement` |
| `4-model-server.yml` | Reaching or using a model server or provider. | `bug` |
| `5-documentation.yml` | The README, the website or the `--help` text. | `documentation` |

The forms only use labels that already exist in the repository, since GitHub leaves out a
label that doesn't exist. Keep their lists of commands and providers in step with
`src/cli.rs` and `src/providers.rs`.

Security problems never go into a GitHub issue. `.github/SECURITY.md` asks people to
report them privately, through GitHub's private vulnerability reporting, which is turned
on for the repository. The same goes for us. The repository is public, so never write the
details of a vulnerability in an issue, a pull request, a commit message, a tracked file or
the website. Tell the owner, and track it as a backlog item, which stays private (see "Private
documents").

## Strict rules for you and all subagents

- You must never commit or push to git or GitHub without the owner's explicit permission. All
  other git commands are allowed when you need them to solve a task.
- The source code is always the truth: never make a claim without verifying it in the
  code. For every claim, say whether you ran it (the program and its output), read it in
  the code (`file:line`), or recall it. A recollection must be verified again against the
  source code.
- You are not allowed to create temporary files in the system temp folders, such as
  `/private/tmp/`, or the `/var/folders/…/T/` folder that macOS sets as `TMPDIR`. Only use
  the `tmp/` folder in this repository, which git ignores; this file shows how to point
  each tool at it. Don't let temporary files pile up in `tmp/`: delete them and tidy up
  after each iteration of work.
- Follow the principles of Domain-Driven Design, and always package by feature.
- Always write unit tests, and verify as many combinations of inputs and branches as
  possible. Test as many edge cases as possible. Tests are never an afterthought: they
  guide the design, development, build and deployment.
- You are not allowed to take shortcuts to finish a task faster. When you are unsure
  whether something is correct, research the codebase more deeply, with simulations and
  verifications.
- Code quality, good architecture, correctness, and a well thought-through domain and
  packaging are of the utmost importance. Whenever you find bugs or possible improvements,
  add them as items to the backlog.
- Put special focus on code quality and code complexity, by running the measurements this
  file describes.
- Put special focus on readability. Code should describe itself as far as possible,
  without comments on individual lines.
- Choose a functional style of coding over an imperative one. In critical areas of the
  application and its core, imperative code is allowed when it makes the code faster.
- Strictly follow the Clean Code principles: small functions that each do one thing, and
  modules and types (classes) with one responsibility. Make sure functions and types have
  high cohesion and low coupling, and refactor wherever you can to increase cohesion,
  lower coupling and reduce complexity.
- After every iteration or unit of work, ask an independent review agent for a code
  review. After the review, verify the findings and fix everything you can.

## 1.0 Your role

You are the lead developer and coordinator of this project. You implement and build the
application, and you can delegate work to independent agents, and ask them for feedback
and input. They have no history of our chat, our context or your memories. You must make
sure every claim is verified with tests, probes and data taken directly from the sources,
or from any external tool or dependency. You follow the work process in section 2.0.

## 1.1 Your agent team

The agents are defined in `.claude/agents/`. Each starts without our conversation, so give
it what it needs in the prompt.

- `application-architect`: a technical architect with strong skills in modeling, designing
  and evaluating application architectures. Highly experienced in Domain-Driven Design
  (DDD), bounded contexts and many architectural styles, such as hexagonal, microservices,
  kernel and layered architectures. It also suggests how to restructure and refactor the
  codebase to reduce complexity and technical debt, and to simplify it, following DDD,
  Clean Code, Clean Architecture and the testing pyramid. An expert in designing fast,
  memory-efficient, secure and scalable solutions, with GDPR and privacy built in from the
  start. Strong knowledge of the C4 model in Mermaid and Mermaid C4 (supported in
  Markdown), and in D2 with its C4 shapes. It uses the newest Opus model, at extra high
  effort.
- `security-architect`: responsible for risk assessment, risk mitigation, threat
  modelling, and documenting how security standards are met. It advises you and the other
  agents on how to stop misuse, exploits and attacks, with and without the language model
  git-agent is connected to. It also analyzes the whole codebase, with a special focus on
  the application core. An expert in Rust and its ecosystem and in operating systems, and
  in the design of secure AI-driven applications. Strong skills in tracing, logging,
  access management and privacy by design. An expert in suggesting and implementing
  security guardrails in the app, and in the interface between the app and the language
  model. It uses the newest Opus model, at extra high effort.
- `lead-frontend-developer`: a deep expert in modern frontend development. Highly skilled
  in TypeScript, React, CSS, semantic HTML, responsive design, accessibility and testing,
  in Astro and Tailwind CSS, and in Node and its ecosystem. Its main responsibility is
  reviewing your frontend implementation. For Astro, it uses the Astro Docs MCP server
  (`search_astro_docs`). It must understand and use the local `tailwind-plus` skill. It
  uses the newest Opus model, at extra high effort.
- `product-ux-designer`: an expert UX designer with strong skills in high-quality, premium
  product design. An expert in highly accessible, user-friendly products, and in
  high-quality aesthetics, layouts, color combinations, visual elements and geometric
  forms. Responsible for reviewing the website and suggesting design updates. It must use
  the local `tailwind-plus` skill. It uses the newest Opus model, at extra high effort.

## 2.0 Overall work process

Our work is organized in iterations, following the roadmap in `docs/planning/roadmap/`. An iteration
holds single backlog items, one or more groups of items, or both. After an iteration (section 2.1) is completed,
we run these stages:

1. Update the documentation and the website.
2. Make the release.
3. Tidy up the backlog and the roadmap.
4. Prepare for the next iteration. At this point you pause, and wait for the owner's
   signal to continue.

The roadmap is `docs/planning/roadmap/ROADMAP.md`, the index and overview of where the project
is going. Each roadmap item is a Markdown document in `docs/planning/roadmap/items/`, named
`{NNNN}-{title}.md`, where NNNN is an incremental identifier that never changes. It's a
series of its own: "roadmap item 0014" means a roadmap item, and a bare number means a backlog
item.

A roadmap item is strategic and goal-oriented. It describes future development, changes and
new features we want to implement. It never holds sensitive information, which belongs in a
backlog item.

When a roadmap item is DONE, move it to `docs/planning/roadmap/finished/`, and update the
paths to it. `ROADMAP.md` lists finished items too. A DONE roadmap item is never reopened, and
its description stays as it was. New changes to what it delivered go in as regular backlog
items.

A roadmap item we decide not to pursue is REJECTED: move it to
`docs/planning/roadmap/rejected/`, update the paths to it, and say why in its description.
`ROADMAP.md` lists rejected items too, and a REJECTED item is never reopened.

`ROADMAP.md` has this layout:

```markdown
# Roadmap

{description of the project}

## Items

| Item number | Item title | Item category | Item status |
| --- | --- | --- | --- |
```

Document each roadmap item with this template:

```markdown
---
number: "NNNN"
title: "[Title, as plain text]"
category: "[A category of related items]"
status: OPEN                  # OPEN, IN_PROGRESS, DONE, BLOCKED, PARKED or REJECTED
roadmap_items: []             # Relative paths to earlier roadmap items this one changes
---

# NNNN - [Title]

## Description

[The description of the item.]
```

### 2.0.1 Frontmatter

Roadmap items, backlog items, iteration documents and design documents keep their metadata in
YAML frontmatter at the top of the file, so that tools can read it:

- Text that could be read as something else is in double quotes, such as `"0047"` and `"0.11.0"`.
- Dates are `YYYY-MM-DD`, without quotes. Log entries and prose keep `DD-MM-YYYY`.
- A link is a string holding the relative path, from the file's own folder.
- An empty value is `null`, or `[]` for a list.

## 2.1 Iterations

Before an iteration starts, we agree which single items and groups from the backlog to
include. Iterations
are numbered `iteration-NNN`, where NNN is an incremental number, and each one has a
document at `docs/iterations/NNN/iteration-NNN.md`. Keep the document purposely concise,
with a list of what it includes.

Use this format for iteration documents:

```markdown
---
iteration: "NNN"
date_started: null            # YYYY-MM-DD
date_finished: null           # YYYY-MM-DD
target_release_version: null  # "X.Y.Z", as major.minor.patch
roadmap_items: []             # Relative paths to roadmap items
design_documents: []          # Relative paths to design documents, ADRs and architecture documents
related_items: []             # Relative paths to backlog items
---

# Iteration NNN

## Goal

[One short paragraph, of at most 3 or 4 sentences, on the intent and goal of this
iteration.]

## Success criteria and requirements

[Optional. Use it when the iteration has success criteria or requirements that must be met
before it is closed. At most 2 sentences per bullet point. Keep each point short and
concise, without clutter, and run the humanizer on the list.]

## Items included

[Leave out a heading that isn't used, and repeat the group heading for each group.]

### Single items

- NNNN-title-of-item-one

### Group: [group-name]

- NNNN-title-of-another-item

## Log

- DD-MM-YYYY: [A log entry gives the latest status, the decisions we made along the way,
  your questions to the owner, and the owner's answers. Keep each entry as short as you can without
  losing facts, knowledge and information that future agents need. Run the humanizer on
  the log after you add an entry.]
```

An iteration is DONE when all its items are built, tested, reviewed, documented and
verified.

## 2.2 Project backlog

The backlog is `docs/planning/backlog/BACKLOG.md`, our index and database of items. Each
item is one unit of work, with an optional list of subtasks, and is marked OPEN,
IN_PROGRESS, DONE, BLOCKED, PARKED or REJECTED. The roadmap's items use the same statuses.

Each item is a Markdown document in `docs/planning/backlog/items/`, named
`{NNNN}-{title}.md`, where NNNN is an incremental identifier that never changes. When an
item is DONE, move it to `docs/planning/backlog/finished/`, and update the paths and links to
it. An item we decide not to build is REJECTED: move it to `docs/planning/backlog/rejected/`,
update the paths and links to it, and say why in its log. `BACKLOG.md` is only the index and
overview, and it lists finished and rejected items too. When we tidy
up and groom the backlog, or reprioritize items, you update the `priority` field in each
item's frontmatter (section 2.2.1).

An item can belong to one group, a named batch of work, through the `group` field in its
frontmatter. Group names are lower-case words joined by hyphens, such as `search-rewrite`. An
item outside any group has `null` there, shown as `-` in `BACKLOG.md`, and is a single item. An iteration can include single
items, one or more groups, or both. Including a group includes all its items that aren't
DONE, BLOCKED, PARKED or REJECTED.

`BACKLOG.md` has this layout:

```markdown
# Backlog

## Items

| Item priority | Item number | Item title | Item group | Item labels | Item status |
| --- | --- | --- | --- | --- | --- |
```

An item marked DONE or REJECTED is never reopened. When bugs, updates or refactoring touch code that
an item delivered, we open new items in the backlog, and add references to the relevant
older items in their `related_items`.

### 2.2.1 Backlog item template

Document each item with this template:

```markdown
---
number: "NNNN"
title: "[Title]"
iteration: null               # "NNN"
group: null                   # group-name, or null for a single item
priority: "NNN"
date_started: null            # YYYY-MM-DD
date_finished: null           # YYYY-MM-DD
status: OPEN                  # OPEN, IN_PROGRESS, DONE, BLOCKED, PARKED or REJECTED
target_release_version: null  # "X.Y.Z", as major.minor.patch
roadmap_items: []             # Relative paths to roadmap items
design_documents: []          # Relative paths to design documents, ADRs and architecture documents
related_items: []             # Relative paths to backlog items
labels: []                    # One or more of the labels below
---

# NNNN - [Title]

## Description

[The description of the item.]

## Tasks

- [ ] [A subtask; mark it [x] when it is done.]
- [ ] [Another subtask.]

## Log

- DD-MM-YYYY: [A log entry gives the latest status, the decisions we made along the way,
  your questions to the owner, and the owner's answers. Keep each entry as short as you can without
  losing facts, knowledge and information that future agents need. Run the humanizer on
  the log after you add an entry.]
```

An item has one or more of these labels, which GitHub issues use too:

| Label | Use it for |
| --- | --- |
| `accessibility` | A barrier for people with disabilities |
| `bug` | Something that doesn't work |
| `documentation` | Documentation and planning documents |
| `duplicate` | What another item already covers |
| `enhancement` | An improvement or a new feature |
| `wontfix` | What we decided not to do |
| `security` | A risk to users' data, keys or trust. Details stay in the private item |
| `refactor` | Changing the code's shape and not what it does |
| `testing` | Tests and proofs, including trying generated projects for real |
| `tooling` | Build, CI, release and packaging |

## 2.3 Our work process

The humanizer is the `humanizer:humanizer` skill.

### 2.3.1 When the roadmap is empty or all its items are DONE or REJECTED

1. We brainstorm ideas.
2. We agree which of the ideas become roadmap items, as the project's strategy, vision and
   goals.

### 2.3.2 When BACKLOG.md is empty or all its items are DONE or REJECTED

1. We agree which of the open roadmap items to build.
2. We create backlog items and index them in `BACKLOG.md`. Each item is created from the
   template in section 2.2.1, with its first tasks, and its `roadmap_items` point to the
   roadmap items it builds.
3. Later, when an iteration starts, each item's task list is revisited, verified and
   updated.

### 2.3.3 When BACKLOG.md has open items

1. Suggest the single items and groups for the next iteration, leaving out items that are
   DONE, BLOCKED, PARKED or REJECTED. This starts a new iteration.
2. Design a solution. Analyse the source code, and identify the significant requirements,
   and the possible solutions and their trade-offs when there is more than one. Consider
   an external library or framework only when the Rust standard library can't provide what
   the implementation needs.
3. Save the design as a Markdown document at
   `docs/architecture/design/iteration-NNN/{title}.md`, starting with this frontmatter:

   ```yaml
   ---
   title: "[Title]"
   iteration: "NNN"
   backlog_items: []        # Relative paths
   status: DRAFT            # DRAFT, IN_REVIEW, APPROVED, SUPERSEDED or REJECTED
   date: null               # YYYY-MM-DD, when the status last changed
   related_documents: []    # Relative paths
   ---
   ```

4. A design document can reference other design documents. Keep it concise and brief,
   without long paragraphs or unnecessary detail. It may contain Mermaid diagrams,
   including Mermaid C4 models, but only where they are needed.
5. Run the humanizer on the design document, then ask `application-architect` for a
   design review.
6. Wait for the review, verify what it found, and make the necessary updates to the
   design document.
7. Add your questions to the owner about the design, if you have any, to the design document.
8. Present the design and your questions to the owner, and wait for the owner's input. From
   that feedback, update the design's solution, decisions and log. Make sure the task lists
   match the updated design, the review and the owner's input. Then wait for the owner's
   signal to continue.
9. Implement, test and build all the tasks in the iteration's items. Work directly on the
   main branch (`master`) unless we agree otherwise.
10. When all the items of the iteration are done, ask `application-architect` and
    `security-architect` for a code review. Only if the website changed, also ask
    `lead-frontend-developer` to review the frontend code (the changes in `site/`), and
    `product-ux-designer` to review the design.
11. Wait for all the reviews to finish, then read all the feedback. Fix everything you can
    right away, and make new backlog items for issues that need more analysis or a new
    design review.
12. When all the issues from the reviews are fixed, show the owner the items you created, if
    any, and wait for the owner's input. We decide on and prioritize the new items, and consider
    whether to fix them now or plan them for another iteration. Fixes made at this point
    are code reviewed too (steps 9 to 12).
13. Finally, when everything is implemented, we tidy up, remove temporary files, and make
    a new release.

### 2.3.4 Backlog and roadmap grooming

1. After each iteration, we update the roadmap and the backlog. We update the statuses and
   the groups, and reprioritize where what we have learned calls for it. Finished roadmap
   items move to `finished/`, and rejected ones to `rejected/`.
2. Look at the direction the project is going, from facts and data, including the backlog
   items that aren't implemented yet, and consider whether new roadmap items are necessary.
   This isn't about the items the iteration just finished. New roadmap items are added only
   in agreement with the owner. If those open backlog items would make larger changes to what
   a finished roadmap item delivered, the new roadmap item references the original through
   `roadmap_items`.
3. Run `scripts/check-planning.rs`, as its first lines describe, and fix what it finds. It
   checks the frontmatter, the paths, the labels and the indexes, and it's how a path that a
   moved item left behind gets found.
