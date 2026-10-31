# git-agent

Generates git commit messages from your changes using a local LLM served through an
OpenAI-compatible API (LM Studio, Ollama, llama.cpp, vLLM, or a hosted provider).
You review each proposed message, then commit it, edit it, regenerate it, or tell the
model what to change.

## Install

```sh
cargo install --path . --locked
```

This builds an optimized binary in `target/release/` and copies it to `~/.cargo/bin/`
(or `$CARGO_HOME/bin/`). `--locked` builds with the exact dependency versions in
`Cargo.lock`; without it, Cargo picks the newest compatible versions. Run the same
command again to update after pulling changes, and `cargo uninstall git-agent` to
remove it.

`~/.cargo/bin` must be on your `PATH`. rustup sets this up for you, but if Rust came
from Homebrew or a Linux package manager, add it yourself, e.g. in `~/.zshrc`:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

Because the binary is called `git-agent`, git also runs it as `git agent`.

## Usage

```sh
git agent                  # describe staged changes, then review
git agent --all            # stage everything (including untracked files) first
git agent -H "fixes #42"   # give the model extra context
git agent --commit         # commit right away, no review
git agent --raw            # print just the message, e.g. for `| git commit -F -`
git agent -- --signoff     # pass extra arguments to `git commit`
```

If something is staged, only the staged changes are described and committed. If
nothing is staged, git-agent describes all changes to tracked files and commits them
with `git commit --all`. Untracked files are left out unless you use `--all`.

After a message is generated you choose what to do with it:

| Key | Action |
| --- | --- |
| `c` | Commit with this message |
| `e` | Open it in your git editor (`GIT_EDITOR`, `core.editor`, `VISUAL`, `EDITOR`) |
| `r` | Generate a new message |
| `f` | Tell the model what to change, e.g. `f shorter, and mention the CLI flags` |
| `q` | Quit without committing |

If `git commit` fails (for example, because a hook rejects it), the message is saved
to `.git/GIT_AGENT_MSG` so you can reuse it with `git commit --file .git/GIT_AGENT_MSG`.

## Configuration

Settings are resolved in this order, later ones winning:

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
| `model` | `-m`, `--model` | `GIT_AGENT_MODEL` | `google/gemma-4-e4b` |
| `api_key` | `--api-key` | `GIT_AGENT_API_KEY` | none |
| `temperature` | `-t`, `--temperature` | | `0.4` |
| `timeout_secs` | | | `300` |
| `max_diff_chars` | | | `16000` |
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
- Diffs longer than `max_diff_chars` are cut off. Lower it if your model has a small
  context window.
- Reasoning blocks (`<think>…</think>`) and code fences around the reply are removed.
- Set `NO_COLOR` to turn off colored output.
