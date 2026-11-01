# git-agent

[![crates.io](https://img.shields.io/crates/v/git-agent.svg)](https://crates.io/crates/git-agent)

git-agent writes git commit messages for your changes with a language model, either
one running on your own machine (LM Studio, Ollama, llama.cpp, vLLM) or one from a
cloud provider such as OpenAI, Anthropic or OpenRouter. You review each proposed
message, then commit it, edit it, regenerate it, or tell the model what to change.

## Why I made git-agent

I made git-agent mainly to get the commit message generation from IntelliJ IDEA
without running IDEA. IDEA uses nearly 3-4 GB of RAM, and I need that memory for
running local language models. git-agent also has stronger automation features, for
use in pipelines or with lightweight code editors. The code was developed using
Gemma 4 locally, and refactored with Claude.

## Install

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

Because the binary is called `git-agent`, git also runs it as `git agent`.

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
```

### Which changes are included

If you have staged changes, git-agent describes and commits only those. If nothing is
staged, it describes all changes to tracked files and commits them with
`git commit --all`. Untracked files are left out unless you use `--all`.

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

### If the commit fails

When `git commit` fails, for example because a hook rejects it, git-agent saves the
message to `.git/GIT_AGENT_MSG`. You can reuse it with
`git commit --file .git/GIT_AGENT_MSG`.

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

### Cloud providers

git-agent works with any provider that has an OpenAI-compatible API, and with
Anthropic through its own Messages API. Your diffs then leave your machine and go to
that provider.

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

The message may then miss details from the shortened files. For a better message,
commit in smaller parts: stage some of the files, run `git agent`, and repeat.
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
| `timeout_secs` | | | `300` |
| `exclude` | | | common lockfiles |
| `system_prompt` | | | Conventional Commits instructions |
| (file location) | `--config` | `GIT_AGENT_CONFIG` | see above |

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
  appear in the list of changed files sent to the model, but their diffs don't.
  Deleted files are sent by name, without their old content.
- git-agent removes reasoning blocks (`<think>…</think>`) and code fences around the
  model's reply.
- Set `NO_COLOR` to turn off colored output.

## Contributing

Contributions are welcome! Open an [issue](https://github.com/dervism/git-agent/issues)
for bugs and feature ideas, or send a pull request. For larger changes, please open an
issue first so we can discuss the approach.

### Before you send a pull request

Make sure these pass:

```sh
cargo fmt --check
cargo clippy --all-targets
cargo test
```

The tests create temporary git repositories and don't need a running model server.

## License

git-agent is licensed under the [MIT License](LICENSE). By contributing, you agree
that your contributions are licensed under the same terms.
