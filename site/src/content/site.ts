// What the site says. Every claim here comes from the project's README.

import type { IconName } from '../lib/icons'

export const repository = 'https://github.com/dervism/git-agent'
export const readme = `${repository}#readme`
export const readmeSource = `${repository}/blob/master/README.md`
export const crate = 'https://crates.io/crates/git-agent'
export const version = '0.8.0'

export interface Link {
  readonly name: string
  readonly href: string
}

/** Site paths, resolved with `url()` from `lib/paths`. */
export const navigation: readonly Link[] = [
  { name: 'Features', href: '#features' },
  { name: 'Explain', href: '#explain' },
  { name: 'Safety', href: '#safety' },
  { name: 'Install', href: '#install' },
  { name: 'Docs', href: 'docs/' },
]

/**
 * How the docs sidebar groups the README's sections, by their heading slugs.
 * A section the README gains later shows up under "More".
 */
export const docsGroups: readonly { readonly name: string; readonly sections: readonly string[] }[] = [
  { name: 'Getting started', sections: ['why-i-made-git-agent', 'install', 'usage'] },
  {
    name: 'Commands',
    sections: [
      'starting-a-branch',
      'splitting-a-change-into-commits',
      'tidying-commits-before-you-push',
      'pushing',
      'pull-requests',
      'explaining-code-and-history',
    ],
  },
  { name: 'Models and safety', sections: ['choosing-a-model', 'secrets-in-your-changes', 'large-changes'] },
  { name: 'Reference', sections: ['configuration', 'contributing', 'license'] },
]

/** Model servers and providers git-agent works with. */
export const providers: readonly string[] = [
  'LM Studio',
  'Ollama',
  'llama.cpp',
  'vLLM',
  'OpenAI',
  'Anthropic',
  'OpenRouter',
  'Google Gemini',
  'Mistral',
  'Groq',
  'DeepSeek',
  'xAI',
  'Together AI',
  'Fireworks AI',
  'Cerebras',
]

/** A line of terminal output; `tone` picks its color. */
export interface TerminalLine {
  readonly text: string
  readonly tone?: 'muted' | 'accent' | 'bright' | 'success' | 'rule' | 'menu'
}

/** One run of a command in the hero's terminal. */
export interface Scene {
  readonly tab: string
  readonly command: string
  readonly output: readonly TerminalLine[]
  /** The key typed at the review menu, if the scene shows one. */
  readonly answer?: string
  readonly after: readonly TerminalLine[]
}

export const scenes: readonly Scene[] = [
  {
    tab: 'commit',
    command: 'git agent',
    output: [
      { text: '🔍 Staged changes: 2 files changed, 41 insertions(+), 6 deletions(-)', tone: 'muted' },
      { text: '🤖 google/gemma-4-e4b · prompt about 1,912 of 131,072 context tokens', tone: 'muted' },
      { text: '' },
      { text: '── Proposed commit message ─────────────────────', tone: 'rule' },
      { text: 'feat(export): add an optional CSV header row', tone: 'bright' },
      { text: '' },
      { text: 'export_csv takes an optional header and writes it as' },
      { text: 'the first line of the output.' },
      { text: '────────────────────────────────────────────────', tone: 'rule' },
      { text: '[c]ommit  [p]ush  [e]dit  [r]egenerate  [f]eedback  [q]uit ›', tone: 'menu' },
    ],
    answer: 'c',
    after: [{ text: '✅ Committed.', tone: 'success' }],
  },
  {
    tab: 'branch',
    command: 'git agent branch fix login timeout',
    output: [
      { text: '🤖 google/gemma-4-e4b · prompt about 231 of 131,072 context tokens', tone: 'muted' },
      { text: '' },
      { text: '── Proposed branch name ────────────────────────', tone: 'rule' },
      { text: 'fix/login-timeout', tone: 'bright' },
      { text: '────────────────────────────────────────────────', tone: 'rule' },
      { text: '[c]reate  [e]dit  [r]egenerate  [f]eedback  [q]uit ›', tone: 'menu' },
    ],
    answer: 'c',
    after: [
      { text: "Switched to a new branch 'fix/login-timeout'", tone: 'muted' },
      { text: '   Your uncommitted changes came along.', tone: 'muted' },
    ],
  },
  {
    tab: 'tidy',
    command: 'git agent tidy',
    output: [
      { text: '🔍 6 commit(s) to tidy, from a6fbe58 to 237af0f', tone: 'muted' },
      { text: '' },
      { text: '── Proposed tidy-up ────────────────────────────', tone: 'rule' },
      { text: 'keep   a6fbe58 feat: add csv export' },
      { text: 'reword ed39c0d wip', tone: 'accent' },
      { text: 'fold   842f69d fix typo', tone: 'accent' },
      { text: 'keep   8e249fb docs: document the export' },
      { text: 'fold   7f944d8 fixup! docs: document the export', tone: 'accent' },
      { text: 'reword 237af0f asdf', tone: 'accent' },
      { text: '' },
      { text: '6 commits become 4: 2 folded into others, 2 with a new message', tone: 'muted' },
      { text: '[a]ccept  [e]dit  [r]egenerate  [f]eedback  [q]uit ›', tone: 'menu' },
    ],
    answer: 'a',
    after: [
      { text: '✅ Tidied 6 commit(s) into 4.', tone: 'success' },
      { text: '   To undo: git reset --keep 237af0f', tone: 'muted' },
    ],
  },
  {
    tab: 'why',
    command: 'git agent why src/fit.rs:share',
    output: [
      { text: '🔍 src/fit.rs:share, last changed in 39c76cd; 2 commit(s) in its history', tone: 'muted' },
      { text: '' },
      { text: '── Why src/fit.rs:share ────────────────────────', tone: 'rule' },
      { text: 'Small files get their share first, so they are sent' },
      { text: 'whole, and the larger files split what is left.' },
      { text: '' },
      { text: '- (a870123) added it to fit large diffs into the context.' },
      { text: '- (39c76cd) only made the function public.' },
      { text: '────────────────────────────────────────────────', tone: 'rule' },
      { text: 'Follow-up question (Enter to finish) ›', tone: 'menu' },
    ],
    after: [],
  },
]

export interface Feature {
  readonly name: string
  readonly description: string
  readonly icon: IconName
}

/** The `why` and `explain` commands. */
export const explainFeatures: readonly Feature[] = [
  {
    name: 'Why a line is the way it is.',
    description:
      'git agent why reads blame and every commit that changed the lines, follows them through renames, and finds functions inside classes and impl blocks.',
    icon: 'MagnifyingGlass',
  },
  {
    name: 'Explain commits, branches and pulls.',
    description:
      'git agent explain summarizes a commit, a range, a branch, or what your last pull brought in, and points out what you may need to act on.',
    icon: 'DocumentText',
  },
  {
    name: 'Ask a follow-up.',
    description:
      'Both commands wait for a follow-up question and answer it with the same code and history at hand.',
    icon: 'ChatBubbleLeftRight',
  },
]

/** Shown in the explain section's terminal, one tab each. */
export const explainSamples: readonly { readonly tab: string; readonly lines: readonly TerminalLine[] }[] = [
  {
    tab: 'explain --pull',
    lines: [
      { text: '$ git agent explain --pull', tone: 'bright' },
      { text: '🔍 Pull into main, 2 hours ago: 4 commit(s), 9 files changed', tone: 'muted' },
      { text: '' },
      { text: '── Pull into main, 2 hours ago ─────────────────', tone: 'rule' },
      { text: 'The pull adds CSV export with an optional header row,' },
      { text: 'and documents it in the README.' },
      { text: '' },
      { text: '- export_csv writes rows as CSV (a6fbe58).' },
      { text: '- A header argument adds a first line (63b8816).' },
      { text: '- New: export_csv takes an optional header argument.', tone: 'accent' },
      { text: '────────────────────────────────────────────────', tone: 'rule' },
    ],
  },
  {
    tab: 'why',
    lines: [
      { text: '$ git agent why src/git.rs:open "what does it check?"', tone: 'bright' },
      { text: '🔍 src/git.rs:open, last changed in 39c76cd', tone: 'muted' },
      { text: '' },
      { text: '── Why src/git.rs:open ─────────────────────────', tone: 'rule' },
      { text: 'It creates the repository object for the given working' },
      { text: 'directory, and checks that the directory is inside a' },
      { text: 'git work tree.' },
      { text: '────────────────────────────────────────────────', tone: 'rule' },
      { text: 'Follow-up question (Enter to finish) ›', tone: 'menu' },
    ],
  },
]

/** How git-agent keeps your code, keys and history safe. */
export const safetyFeatures: readonly Feature[] = [
  {
    name: 'Secrets stay out of prompts.',
    description:
      'API keys, tokens, private keys and passwords become [REDACTED] before anything reaches the model. By default, git-agent also refuses to commit or push them.',
    icon: 'ShieldCheck',
  },
  {
    name: 'No force-push, ever.',
    description:
      'Before a push it rebases onto new remote commits, and cancels cleanly on a conflict. tidy only rewrites commits the remote doesn’t have.',
    icon: 'ArrowPath',
  },
  {
    name: 'Fits any context window.',
    description:
      'Large diffs are shortened in steps: unchanged lines first, then a share of the room per file, so every file still shows its first changes.',
    icon: 'ArrowsPointingIn',
  },
  {
    name: 'Local by default.',
    description:
      'Out of the box it talks to LM Studio on your machine and uses the model that is loaded. Ollama, llama.cpp and vLLM work too.',
    icon: 'CpuChip',
  },
  {
    name: 'Keys in your keychain.',
    description:
      'API keys never go into the config file. On macOS each provider’s key is saved in the keychain, so you can switch between them.',
    icon: 'Key',
  },
  {
    name: 'Plain git underneath.',
    description:
      'Commits go through git commit, so your hooks run, and every proposal opens in your own git editor when you want to change it.',
    icon: 'CommandLine',
  },
]

export interface InstallMethod {
  readonly name: string
  readonly commands: readonly string[]
  readonly note: string
}

export const installMethods: readonly InstallMethod[] = [
  {
    name: 'Homebrew',
    commands: ['brew install dervism/tap/git-agent'],
    note: 'Homebrew builds git-agent from source and installs Rust for the build if you don’t have it.',
  },
  {
    name: 'crates.io',
    commands: ['cargo install git-agent --locked'],
    note: 'Needs Rust 1.88 or newer. --locked uses the exact dependency versions the release was tested with.',
  },
  {
    name: 'From source',
    commands: [
      'git clone https://github.com/dervism/git-agent',
      'cd git-agent',
      'cargo install --path . --locked',
    ],
    note: 'For changes that aren’t released yet. Make sure ~/.cargo/bin is on your PATH.',
  },
]

export interface Step {
  readonly name: string
  readonly description: string
  readonly command?: string
}

export const quickStart: readonly Step[] = [
  {
    name: 'Install',
    description: 'The binary is called git-agent, so git also runs it as git agent.',
  },
  {
    name: 'Choose a model',
    description:
      'Start LM Studio and load a model, or pick Ollama or a cloud provider in the settings menu.',
    command: 'git agent config',
  },
  {
    name: 'Commit',
    description: 'Stage your changes and let the model describe them. You review before anything is committed.',
    command: 'git agent',
  },
]
