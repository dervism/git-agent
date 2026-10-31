# git-agent

[![crates.io](https://img.shields.io/crates/v/git-agent.svg)](https://crates.io/crates/git-agent)

git-agent writes git commit messages for your changes using a local LLM served through
an OpenAI-compatible API (LM Studio, Ollama, llama.cpp, vLLM, or a hosted provider).
You review each proposed message, then commit it, edit it, regenerate it, or tell the
model what to change.

## Why I made git-agent

I made git-agent mainly to get the commit message generation from IntelliJ IDEA
without running IDEA. IDEA uses nearly 3-4 GB of RAM, and I need that memory for
running local language models. git-agent also has stronger automation features, for
use in pipelines or with lightweight code editors. The code was developed using
Gemma 4 locally, and refactored with Claude.

## Install

git-agent is published on [crates.io](https://crates.io/crates/git-agent). With Rust
1.88 or newer installed, run:

```sh
cargo install git-agent --locked
```

Cargo builds an optimized binary and copies it to `~/.cargo/bin/` (or
`$CARGO_HOME/bin/`). `--locked` uses the exact dependency versions the release was
tested with. Run the same command again to update to the latest release, and
`cargo uninstall git-agent` to remove it.

Because the binary is called `git-agent`, git also runs it as `git agent`.

To install from a local checkout instead, for example to try unreleased changes:

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

```sh
git agent                  # describe staged changes, then review
git agent --all            # stage everything (including untracked files) first
git agent -H "fixes #42"   # give the model extra context
git agent --commit         # commit right away, no review
git agent --push           # review, then push after committing
git agent --commit --push  # commit and push, no review
git agent --raw            # print just the message, e.g. for `| git commit -F -`
git agent -- --signoff     # pass extra arguments to `git commit`
```

If you have staged changes, git-agent describes and commits only those. If nothing is
staged, it describes all changes to tracked files and commits them with
`git commit --all`. Untracked files are left out unless you use `--all`.

After git-agent generates a message, you choose what to do with it:

| Key | Action |
| --- | --- |
| `c` | Commit with this message |
| `p` | Commit with this message, then push |
| `e` | Open it in your git editor (`GIT_EDITOR`, `core.editor`, `VISUAL`, `EDITOR`) |
| `r` | Generate a new message |
| `f` | Tell the model what to change, e.g. `f shorter, and mention the CLI flags` |
| `q` | Quit without committing |

If `git commit` fails (for example, because a hook rejects it), git-agent saves the
message to `.git/GIT_AGENT_MSG` so you can reuse it with
`git commit --file .git/GIT_AGENT_MSG`.

Pushing runs a plain `git push`, so it works with GitHub or any other remote and uses
your existing git credentials. The first time you push a branch that has no upstream,
git-agent sets one: it uses `remote.pushDefault` if you've configured it, otherwise
your only remote, otherwise `origin`. If the repository has no remote yet, create one
on GitHub with `gh repo create --source . --push`. If the push fails, the commit is
kept; fix the problem and run `git push`.

Before pushing, git-agent fetches from the remote. If the remote has commits you don't
have, such as a file you edited on GitHub, git-agent lists them and rebases your
commit on top of them. That keeps the history linear and leaves the remote commits in
place. The review menu asks before rebasing; `--commit --push` rebases without
asking. Uncommitted changes are set aside during the rebase and restored afterwards.

If your commit conflicts with the remote commits, git-agent cancels the rebase and
leaves your branch as it was. Run `git pull --rebase`, resolve the conflicts, and
`git push`. git-agent never force-pushes.

## Choosing a model

You don't have to configure a model. git-agent asks the server which model is loaded
and uses it. If several models are loaded, or none is and the server has more than
one, git-agent asks you to choose with `--model` or `model` in the config file.
`git agent models` lists the server's models and marks the one git-agent uses.

git-agent also asks the server how large the model's context window is, so it knows
how much of your changes it can send. LM Studio, Ollama, llama.cpp and vLLM report
it. For other servers, and for models that aren't loaded yet, git-agent assumes
4,096 tokens and says so. Set `context_length` to tell it the real size.

## Large changes

Before each run, git-agent prints the model it uses and how much of the context the
prompt takes:

```text
🤖 google/gemma-4-e4b (loaded on the server) · prompt about 24,578 of 131,072 context tokens
```

The prompt always leaves room for the reply. When your changes don't fit,
git-agent shortens them in steps and stops as soon as they fit:

1. It leaves out the unchanged lines that git shows around each change.
2. It shares the remaining room between the files. Files with small diffs are
   sent whole, and the larger ones are cut short, so every file shows at least its
   first changes.
3. If there are too many files even for that, the largest ones are sent by name
   only. The list of changed files always includes them.

git-agent then tells you what it left out:

```text
warning: the changes need about 24,656 tokens, more than fit in the context, so git-agent sent part of them:
   46 of 58 files in full, without the unchanged lines around the changes
   12 cut short: src/builder/arg.rs (6 of 23 changed lines), src/builder/command.rs (6 of 93 changed lines), and 10 more
```

When that happens, the message may miss details from the shortened files. You get a
better message by committing in smaller parts: stage some of the files, run
`git agent`, and repeat. Loading the model with a larger context also helps.

## Configuration

Settings come from these sources, and later ones override earlier ones:

1. Built-in defaults
2. The config file: `~/.config/git-agent/config.toml` (or `$XDG_CONFIG_HOME/git-agent/config.toml`)
3. Environment variables
4. Command-line flags

```sh
git agent config init       # write a commented config file with the defaults
git agent config show       # print the effective settings
git agent models            # list models on the server; * marks the configured one
```

| Key | Flag | Environment variable | Default |
| --- | --- | --- | --- |
| `base_url` | `--base-url` | `GIT_AGENT_BASE_URL` | `http://localhost:1234/v1` |
| `model` | `-m`, `--model` | `GIT_AGENT_MODEL` | the model loaded on the server |
| `context_length` | `--context-length` | `GIT_AGENT_CONTEXT_LENGTH` | reported by the server, or 4096 |
| `api_key` | `--api-key` | `GIT_AGENT_API_KEY` | none |
| `temperature` | `-t`, `--temperature` | | `0.4` |
| `timeout_secs` | | | `300` |
| `exclude` | | | common lockfiles |
| `system_prompt` | | | Conventional Commits instructions |
| (file location) | `--config` | `GIT_AGENT_CONFIG` | see above |

To switch models for a single run:

```sh
git agent -m qwen/qwen3-8b
GIT_AGENT_BASE_URL=http://localhost:11434/v1 git agent -m llama3.2   # Ollama
```

Notes:

- Files matching `exclude` (by default `Cargo.lock`, `package-lock.json`, and similar)
  still appear in the list of changed files sent to the model, but their diffs don't.
  Deleted files are sent by name, without their old content.
- Large prompts take longer to process. To keep them smaller, set `context_length`
  below the model's real context.
- git-agent removes reasoning blocks (`<think>…</think>`) and code fences around the
  model's reply.
- Set `NO_COLOR` to turn off colored output.

## Contributing

Contributions are welcome! Open an [issue](https://github.com/dervism/git-agent/issues)
for bugs and feature ideas, or send a pull request. For larger changes, please open an
issue first so we can discuss the approach.

Before you send a pull request, make sure these pass:

```sh
cargo fmt --check
cargo clippy --all-targets
cargo test
```

The tests create temporary git repositories and don't need a running model server.

## Releasing

Maintainers release a new version with `scripts/release.sh <version>`. The script
runs the checks, bumps the version in `Cargo.toml`, publishes the crate to crates.io,
tags and pushes the release commit, and creates a GitHub release. Run it with
`--dry-run` first to see each step without changing anything. It needs the `gh` CLI
and a crates.io token from `cargo login`.

## License

git-agent is licensed under the [MIT License](LICENSE). By contributing, you agree
that your contributions are licensed under the same terms.
