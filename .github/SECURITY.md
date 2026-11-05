# Security policy

## Reporting a vulnerability

Please don't report a security vulnerability in a public issue, pull request or
discussion. Report it privately through GitHub instead: open the repository's Security tab
and choose "Report a vulnerability", or go straight to
https://github.com/dervism/git-agent/security/advisories/new. The report goes privately to
the maintainer.

A good report has:

- the git-agent version (`git agent --version`) and your operating system
- the model server or provider, if one is involved
- what an attacker controls, such as a file, a commit message, a branch name, an issue or
  a model's reply, and what they gain
- the steps to reproduce it, as small as you can make them

## What counts

These are the kinds of problems to report privately:

- Text in a repository, commit, branch or GitHub issue that makes git-agent run commands,
  change files or rewrite history it shouldn't.
- A secret that reaches a model server, a file, the terminal or a commit, when git-agent
  should have caught it.
- An API key that leaks from the macOS Keychain, the environment or git-agent's output.
- Commands, paths or temporary files that git-agent builds unsafely.

A model that writes a poor commit message or a wrong explanation is not a vulnerability;
open a bug report for that.

## Supported versions

Fixes go into the latest release. Update with `brew upgrade git-agent` or
`cargo install git-agent --locked`.
