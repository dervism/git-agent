# Project setup template

This folder holds everything `git agent project init` needs. This document describes the
setup, `files/` holds every file it writes from a template, and `snippets/` holds text that
goes inside a file another tool generated.

The setup works for a project in any of the stacks below. It creates the source scaffold, an `AGENTS.md` with a team of
review agents, and the planning documents and work process for the roadmap, backlog,
iterations and items. It also adds GitHub issue forms, a security policy, licensing, CI,
and scripts for releasing and publishing.

The setup doesn't assume any coding agent. Its core works with any tool, or none, and small
add-ons are written only for the tools the owner picks: Claude Code, Codex, Cursor and
OpenCode.

Most of the setup needs no language model. The files are fixed templates with parameters
filled in, and the source comes from each stack's own scaffolding tool. Each step says
whether a script runs it or a model does. The setup commands and checks for Java, Haskell,
Astro and Next.js were run on fresh projects on 05-11-2026. The rest, including the GitHub
workflows and real deployments, was only checked against documentation.

## Conventions

- `{{NAME}}`, with `NAME` in capitals, digits and underscores, is a parameter, filled in
  before anything is written. Other text in double braces is left alone, such as the
  `${{ ... }}` expressions in GitHub workflows.
- `<!-- if NAME -->` … `<!-- end NAME -->` is a conditional block, kept when the
  condition holds and removed otherwise. When a block is kept, only its markers are
  removed. A marker that stands alone on its line is removed together with that line. In
  Markdown files, paragraphs that a block or a parameter touched are rewrapped to 90
  columns afterwards. Other files, and the YAML frontmatter of Markdown files, are never
  rewrapped.
- Parameters are replaced in one pass, so a value is never expanded again. A role's
  description is the one exception: the project's parameters are filled in first, because it
  is part of the template.
- A parameter's value is escaped for where it lands: quoted in shell commands, and escaped
  in JSON, TOML and double-quoted YAML. Free text only appears in those places or in
  Markdown.
- Each template's name ends in `.tmpl`, which the setup removes. Without that suffix, the
  `.gitignore`, `AGENTS.md` and agent tool settings in this folder would take effect on the
  repository that holds them.
- `[...]` and `{...}` inside the document templates are filled in later, by whoever writes
  that document.
- "Asks first" marks a step that changes something outside the repository, such as a
  GitHub setting or a package registry. The script shows what it will do, and waits for
  a yes.
- Nothing is committed or pushed. The owner reviews the result and commits it.

## Parameters

| Parameter | What it is | Default, or where it comes from |
| --- | --- | --- |
| `{{PROJECT_NAME}}` | The project's name, also its package name: lower-case letters, digits and hyphens, starting with a letter | Asked; the folder's name |
| `{{DESCRIPTION}}` | One sentence about the project | Asked |
| `{{REPOSITORY}}` | The GitHub repository, as owner/name | Asked; `gh repo view --json nameWithOwner` if it exists |
| `{{MAIN_BRANCH}}` | The branch we work on and release from | `gh repo view --json defaultBranchRef` if the repository is on GitHub; otherwise the current branch of an existing local repository; otherwise `main` |
| `{{VISIBILITY}}` | `public` or `private` | Asked |
| `{{STACKS}}` | One or more of `rust`, `java`, `haskell`, `astro` and `nextjs`, with at most one of the first three and at most one of the last two | Asked: the backend, if any, then whether to add a frontend, and whether it uses Astro or Next.js |
| `{{FRONTEND_DIR}}` | The frontend's folder, or `.` when the frontend is the whole project | `site` next to another stack, otherwise `.` |
| `{{RUST_KIND}}` | `binary` or `library` | Asked, with Rust; `binary` |
| `{{TOPICS}}`, `{{HOMEPAGE}}` | The repository's topics and home page, used in step 8 | Asked; both can be left empty |
| `{{LICENSE}}` | An SPDX license identifier that `gh api /licenses` lists | Asked; `MIT` |
| `{{COPYRIGHT_HOLDER}}` | The name in copyright lines | `git config user.name` |
| `{{AUTHOR_EMAIL}}` | The author's email, for the Haskell package | `git config user.email` |
| `{{CONDUCT_CONTACT}}` | Where to report conduct problems: an email or a URL | Asked |
| `{{YEAR}}` | The copyright year | The current year |
| `{{INITIAL_VERSION}}` | The first version | `0.1.0` |
| `{{PUBLISH_TARGETS}}` | Any of `crates.io`, `maven-central`, `npm` and `hackage`, or none | Asked |
| `{{DEPLOY_TARGET}}` | Where the frontend runs: `none`, `github-pages`, `cloudflare-static` or `cloudflare-server` | Asked, with a frontend; `none` |
| `{{ALLOWED_LICENSES}}` | The SPDX licenses that dependencies may use | `{{LICENSE}}` itself, because `cargo deny` also checks the project's own crate; `MIT`, `Apache-2.0`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `0BSD` and `MPL-2.0`; with Rust also `Unicode-3.0`, which `serde` depends on; with a frontend also `CC0-1.0`, `CC-BY-4.0` and `BlueOak-1.0.0`, which new Astro and Next.js projects depend on |
| `{{SUPPORTED_VERSIONS}}` | Which versions get security fixes | Asked; "the latest release" |
| `{{AGENT_TOOLS}}` | The coding agent tools to set up: any of `claude-code`, `codex`, `cursor` and `opencode`, or `none` | Asked, with the tools it finds installed preselected (`claude`, `codex`, `cursor-agent`, `opencode`) |
| `{{DESIGN_SKILL}}` | A skill the frontend agents use for UI blocks, or `none` | Asked, with a frontend and an agent tool; `none` |
| `{{JAVA_GROUP_ID}}` | The Maven group ID | Asked, with Java |

The script works these out itself:

| Parameter | How |
| --- | --- |
| `{{ABSOLUTE_TMP}}` | The repository's absolute path, followed by `/tmp` |
| `{{FRONTEND_ROOT}}` | `/` when `{{FRONTEND_DIR}}` is `.`, otherwise `/{{FRONTEND_DIR}}` |
| `{{LICENSE_KEY}}` | `{{LICENSE}}` in lower case, as `gh api /licenses/{key}` takes it |
| `{{ALLOWED_LICENSES_JSON}}`, `{{ALLOWED_LICENSES_PIPED}}` | `{{ALLOWED_LICENSES}}` as quoted JSON strings separated by commas, and separated by `\|` |
| `{{SKILLS_AGENT_FLAGS}}` | `-a` with each tool in `{{AGENT_TOOLS}}`, such as `-a claude-code -a codex` |
| `{{ROLE_CODEX_NAME}}` | `{{ROLE}}` with `_` in place of `-` |
| `{{REPOSITORY_OWNER}}`, `{{REPOSITORY_NAME}}` | The two parts of `{{REPOSITORY}}` |
| `{{E2E_BASE_PATH}}` | `/{{REPOSITORY_NAME}}/` with `PAGES`, otherwise `/`. A repository named `{owner}.github.io` serves from the root, so it uses `/` too |
| `{{TODAY_ISO}}` | Today's date as YYYY-MM-DD, the format Cloudflare's `compatibility_date` takes |
| `{{ARCHETYPE_VERSION}}` | `<release>` in https://repo1.maven.org/maven2/no/dervis/java-starter/maven-metadata.xml (1.0.15 on 05-11-2026) |
| `{{SPOTLESS_VERSION}}`, `{{GOOGLE_JAVA_FORMAT_VERSION}}`, `{{MAVEN_PMD_VERSION}}`, `{{PMD_VERSION}}`, `{{MYCILA_LICENSE_VERSION}}`, `{{MOJO_LICENSE_VERSION}}` | The newest release of `com.diffplug.spotless:spotless-maven-plugin`, `com.google.googlejavaformat:google-java-format`, `org.apache.maven.plugins:maven-pmd-plugin`, `net.sourceforge.pmd:pmd-java`, `com.mycila:license-maven-plugin` and `org.codehaus.mojo:license-maven-plugin`, from Maven Central's `maven-metadata.xml` (3.11.0, 2.0.0, 3.28.0, 7.29.0, 5.1.2 and 2.7.1 on 05-11-2026). A release candidate doesn't count |
| `{{RUST_VERSION}}` | The newest stable Rust: `version` under `[pkg.rust]` in https://static.rust-lang.org/dist/channel-rust-stable.toml, without its hash and date (1.99.1 on 05-11-2026) |
| `{{JDK_VERSION}}` | The newest stable JDK: `most_recent_feature_release` in https://api.adoptium.net/v3/info/available_releases (27 on 05-11-2026) |
| `{{GHC_VERSION}}`, `{{CABAL_VERSION}}` | The newest stable GHC and cabal: the releases tagged `Latest` in `ghcup-0.0.9.yaml` from `haskell/ghcup-metadata` (9.14.1 and 3.18.2.0 on 05-11-2026) |
| `{{NODE_VERSION}}` | The installed Node, from `node --version`; Astro needs 22.23.4 or newer, and Next.js 20.9.0 or newer |
| `{{CHECKOUT_PIN}}`, `{{INSTALL_ACTION_PIN}}`, `{{SETUP_JAVA_PIN}}`, `{{SETUP_NODE_PIN}}`, `{{HASKELL_SETUP_PIN}}`, `{{CACHE_PIN}}`, `{{ASTRO_ACTION_PIN}}`, `{{UPLOAD_PAGES_PIN}}`, `{{DEPLOY_PAGES_PIN}}`, `{{WRANGLER_ACTION_PIN}}` | The full commit of the latest release of `actions/checkout`, `taiki-e/install-action`, `actions/setup-java`, `actions/setup-node`, `haskell-actions/setup`, `actions/cache`, `withastro/action`, `actions/upload-pages-artifact`, `actions/deploy-pages` and `cloudflare/wrangler-action`, then its tag in a comment, as `{sha} # {tag}`. The tag comes from `gh api repos/actions/checkout/releases/latest --jq .tag_name`, and the commit from `gh api repos/actions/checkout/commits/{tag} --jq .sha` |

## Conditions

| Condition | Holds when |
| --- | --- |
| `RUST`, `JAVA`, `HASKELL`, `ASTRO`, `NEXTJS` | `{{STACKS}}` names that stack |
| `FRONTEND` | `ASTRO` or `NEXTJS` holds |
| `FRONTEND_IN_SUBDIR` | `FRONTEND` holds and `{{FRONTEND_DIR}}` isn't `.` |
| `FRONTEND_AT_ROOT` | `FRONTEND` holds and `{{FRONTEND_DIR}}` is `.` |
| `AI_APP` | The application sends data to a language model (asked) |
| `PUBLIC`, `PRIVATE` | `{{VISIBILITY}}` is `public` or `private` |
| `CRATES`, `MAVEN`, `NPM`, `HACKAGE` | `{{PUBLISH_TARGETS}}` names `crates.io`, `maven-central`, `npm` or `hackage` |
| `PUBLISH` | `{{PUBLISH_TARGETS}}` names at least one target |
| `NO_DEPLOY` | `{{DEPLOY_TARGET}}` is `none` |
| `PAGES` | `{{DEPLOY_TARGET}}` is `github-pages` |
| `PAGES_BASE` | `PAGES` holds and the repository isn't named `{{REPOSITORY_OWNER}}.github.io`, which serves from the root |
| `CLOUDFLARE` | `{{DEPLOY_TARGET}}` is `cloudflare-static` or `cloudflare-server` |
| `CLOUDFLARE_STATIC` | `{{DEPLOY_TARGET}}` is `cloudflare-static` |
| `CLOUDFLARE_SERVER` | `{{DEPLOY_TARGET}}` is `cloudflare-server` |
| `STATIC_EXPORT` | `NEXTJS` holds and `{{DEPLOY_TARGET}}` is `github-pages` or `cloudflare-static` |
| `VINEXT` | `NEXTJS` holds and `{{DEPLOY_TARGET}}` is `cloudflare-server` |
| `WRANGLER` | `CLOUDFLARE` holds and `VINEXT` doesn't |
| `CLAUDE_CODE`, `CODEX`, `CURSOR`, `OPENCODE` | `{{AGENT_TOOLS}}` names `claude-code`, `codex`, `cursor` or `opencode` |
| `AGENT_TOOL` | `{{AGENT_TOOLS}}` isn't `none` |
| `MCP` | `AGENT_TOOL` holds, and `ASTRO` or `CLOUDFLARE` holds |
| `DESIGN_SKILL` | `FRONTEND` and `AGENT_TOOL` hold, and `{{DESIGN_SKILL}}` isn't `none` |
| `READ_ONLY`, `ROLE_USES_DESIGN_SKILL` | The role being written has them, as "The agent roles" lists |

## What gets created

```text
.
├── AGENTS.md                        the process and the repository's description
├── README.md
├── CHANGELOG.md
├── CONTRIBUTING.md
├── CODE_OF_CONDUCT.md               Contributor Covenant
├── LICENSE
├── .editorconfig
├── .gitattributes
├── .gitignore
├── .agents/skills/.gitkeep          skills for any agent tool, in the Agent Skills format
├── .github/
│   ├── ISSUE_TEMPLATE/              config.yml and four issue forms
│   ├── pull_request_template.md
│   ├── SECURITY.md
│   ├── dependabot.yml
│   └── workflows/
│       ├── ci.yml
│       ├── pages.yml                with GitHub Pages
│       └── cloudflare.yml           with Cloudflare Workers
├── docs/
│   ├── agents/                      the agent team's briefs, as plain Markdown
│   ├── templates/                   the templates for the documents the process creates
│   ├── planning/                    private: git-ignored
│   │   ├── ROADMAP.md
│   │   └── backlog/
│   │       ├── BACKLOG.md
│   │       └── items/
│   ├── iterations/                  private: git-ignored
│   ├── architecture/
│   │   ├── design/                  private: git-ignored
│   │   ├── decisions/.gitkeep       architecture decision records
│   │   └── c4/.gitkeep              C4 diagrams, as Mermaid or D2 sources
│   └── security/
│       ├── threat-model.md
│       └── standards.md
├── scripts/
│   ├── check.sh
│   ├── release.sh                   releases to GitHub
│   ├── publish.sh                   publishes to the chosen targets
│   └── rollback.sh
├── tmp/.gitkeep                     everything else in tmp/ is ignored
└── the stack's own files, from its scaffolding tool
```

Each tool in `{{AGENT_TOOLS}}` adds a few files of its own, which step 7 lists.

## Steps

### 1. Ask the questions, and check the prerequisites (script)

The script first checks `git`, and that `gh` is logged in (`gh auth status`), because the
defaults above need them. It then asks for the parameters, and works out the others, which
needs network access. Last, it checks the rest of this list. It names everything that is
missing and stops, and it never installs anything without asking.

- With `CLAUDE_CODE`: Claude Code 2.1.277 or newer (`claude --version`).
- With `FRONTEND`: Node at the version the framework needs (`node --version`).
- With `DESIGN_SKILL`: the skill at `~/.claude/skills/{{DESIGN_SKILL}}/` with `CLAUDE_CODE`,
  and at `~/.agents/skills/{{DESIGN_SKILL}}/` with the other tools.
- With `RUST`: `cargo`. With `rustup`, `rust-toolchain.toml` installs `{{RUST_VERSION}}` by
  itself. Without it, as with Homebrew's Rust, that file has no effect on this machine,
  though CI still uses it.
- With `JAVA`: JDK `{{JDK_VERSION}}` and Maven (`mvn --version`). SDKMAN (`sdk`) pins the JDK
  when it's installed.
- With `HASKELL`: `ghc-{{GHC_VERSION}}` on the `PATH`, and `cabal` `{{CABAL_VERSION}}`,
  installed with GHCup or Homebrew.

It only warns about these, and carries on:

- `d2` is missing. Only C4 diagrams need it.
- There is no `rustup`, and `rustc` is older than `{{RUST_VERSION}}`.
- `github-pages` with a private repository. GitHub Free only publishes Pages from public
  repositories.

### 2. Create the repository (script, asks first)

- If the folder isn't a git repository yet, run `git init -b {{MAIN_BRANCH}}`.
- If `{{REPOSITORY}}` doesn't exist on GitHub, ask, then run
  `gh repo create {{REPOSITORY}} --{{VISIBILITY}} --source . --remote origin --description "{{DESCRIPTION}}"`.
- If it does exist and the local repository has no `origin`, run
  `git remote add origin https://github.com/{{REPOSITORY}}.git`.
- In a repository that already has files, show the plan first. The script records which
  files exist before it starts. Those are skipped and listed, never overwritten, with one
  exception: an existing `.gitignore` gets the entries it's missing. A stack whose manifest
  is already there isn't scaffolded again. The same rules make it safe to run the setup
  again after a failure.
- If git already tracks `docs/planning/`, `docs/iterations/` or `docs/architecture/design/`,
  ask, then stop tracking them with `git rm -r --cached`, because `.gitignore` alone doesn't.
  Their earlier commits stay in the history, so warn when the repository is public.

### 3. Scaffold the source (script)

Each scaffolding tool writes into `tmp/scaffold/{{PROJECT_NAME}}/`, named after the project
because some tools take the package's name from the folder. Move its files into place, and
delete `tmp/scaffold/`. The backend stack goes at the root, and a frontend in
`{{FRONTEND_DIR}}/`.
Some tools write files that step 4 also writes, such as `README.md`, `LICENSE`,
`CHANGELOG.md` and `.gitignore`. At the root, step 4's version replaces the tool's, but the
stack's own ignore entries are kept. A frontend in a subfolder keeps its tool's files.

- Rust: `cargo init tmp/scaffold/{{PROJECT_NAME}} --name {{PROJECT_NAME}} --vcs none`, with
  `--lib` when `{{RUST_KIND}}` is `library`. Then set `version = "{{INITIAL_VERSION}}"`,
  and add `license = "{{LICENSE}}"` and `description = "{{DESCRIPTION}}"` to `[package]` in
  `Cargo.toml`, because `cargo deny` fails on a crate without a license.
- Java, with Maven and the `no.dervis:java-starter` archetype
  (https://github.com/dervism/java-archetype). Run it in `tmp/scaffold/`, since it writes
  the project into a folder named after the artifact ID:

  ```sh
  mvn archetype:generate -DarchetypeGroupId=no.dervis \
      -DarchetypeArtifactId=java-starter \
      -DarchetypeVersion={{ARCHETYPE_VERSION}} \
      -DgroupId={{JAVA_GROUP_ID}} \
      -DartifactId={{PROJECT_NAME}} \
      -Dversion={{INITIAL_VERSION}} \
      -DjdkVersion={{JDK_VERSION}} \
      -DjavaPreviewFeatures=no \
      -DinteractiveMode=false
  ```

  Then add the Maven wrapper with `mvn wrapper:wrapper`, since the archetype doesn't
  include one. The archetype also declares JUnit without a scope, so set
  `<scope>test</scope>` on it.
- Haskell, with cabal, run in `tmp/scaffold/{{PROJECT_NAME}}/`. This writes cabal's
  recommended layout: `app/Main.hs`, `src/MyLib.hs`, `test/Main.hs` and the `.cabal` file:

  ```sh
  cabal init --non-interactive --libandexe --tests -w ghc-{{GHC_VERSION}} \
      --package-name={{PROJECT_NAME}} --version={{INITIAL_VERSION}} --license={{LICENSE}} \
      --author="{{COPYRIGHT_HOLDER}}" --email={{AUTHOR_EMAIL}} --language=GHC2021
  ```

- Astro, with React and Tailwind CSS. TypeScript is always strict in new Astro projects:

  ```sh
  npm create astro@latest -- tmp/scaffold/{{PROJECT_NAME}} --template minimal \
      --add react --add tailwind --install --no-git --yes --skip-houston
  ```

  When the frontend is the whole project, add `--no-ai`, so Astro doesn't write agent
  files that would compete with the root `AGENTS.md`. In a subfolder, keep Astro's own
  `AGENTS.md`: agent tools read it when they work in that folder.
  This command checks the template on GitHub first, and that check can fail for a few
  minutes with "Template minimal could not be found!". Stop with the tool's message, because
  running the setup again is safe.
- Next.js, with React, TypeScript and Tailwind CSS:

  ```sh
  npx create-next-app@latest tmp/scaffold/{{PROJECT_NAME}} --ts --tailwind --no-linter --app \
      --src-dir --import-alias "@/*" --use-npm --disable-git --yes
  ```

  When the frontend is the whole project, add `--no-agents-md`, for the same reason.

### 4. Write the shared files (script)

Write the files that the table in "File templates" lists for step 4, then the empty folders
from "What gets created". Those that git tracks get a `.gitkeep`. Scripts are written executable. Fill in
`LICENSE` and `CODE_OF_CONDUCT.md` from GitHub's API, as that section shows.

`release.sh`, `publish.sh` and `rollback.sh` follow "The scripts". Their templates aren't
written yet, so they come with the implementation of `project init`.

### 5. Pin the toolchains, and configure the checks (script)

Rust, JVM languages and Haskell always use the newest stable release, and never a preview.

| Stack | Pin | Formatting, linting and complexity | Tests | Dependency licenses |
| --- | --- | --- | --- | --- |
| Rust | `rust-toolchain.toml` with `channel = "{{RUST_VERSION}}"` and `components = ["rustfmt", "clippy"]` | `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings`; `-W clippy::cognitive_complexity -W clippy::too_many_lines` | `cargo test` | `cargo deny check licenses` |
| Java | `{{JDK_VERSION}}` in `pom.xml`; `.sdkmanrc` from `sdk env init` when SDKMAN is installed | Spotless with google-java-format; PMD's `CognitiveComplexity` and `CyclomaticComplexity` rules | `./mvnw verify` | MojoHaus's `license-maven-plugin`, which covers transitive dependencies. Mycila's plugin of the same name checks source file headers |
| Haskell | `cabal.project` with `packages: .` and `with-compiler: ghc-{{GHC_VERSION}}`; `cabal freeze` | ormolu | `cabal test all` | `cabal-plan license-report`, compared with the allowed list |
| Astro | `.nvmrc`, and `engines.node` in `package.json` | Oxfmt; Oxlint; `tsc` through `@astrojs/ts-content-mapper` | Vitest and Playwright | `licensee` |
| Next.js | `.nvmrc`, and `engines.node` in `package.json` | Oxfmt; Oxlint; `tsc` | Vitest and Playwright | `licensee` |

`template/files/scripts/check.sh.tmpl` has the exact commands. Every license check allows only
`{{ALLOWED_LICENSES}}`. Write the files that the table in "File templates" lists for this
step. Ask before installing a tool that isn't there, such as `cargo-deny`,
`ormolu` or `cabal-plan`.

For Rust, write `deny.toml`, and install the tool with `cargo install cargo-deny --locked`.

For Java, add `template/snippets/pom-plugins.xml.tmpl` inside `<build><plugins>` in `pom.xml`.

For Haskell, install the tools. There is no linter or complexity tool, because hlint and
Argon don't build on the newest GHC:

```sh
cabal update
cabal install ormolu
cabal install cabal-plan -fexe -flicense-report
```

For a frontend, set the version and install the tools in `{{FRONTEND_DIR}}/`, then write the
configuration files:

```sh
npm version {{INITIAL_VERSION}} --no-git-tag-version --allow-same-version
npm pkg set engines.node=">={{NODE_VERSION}}"
npm install -D @types/node@latest typescript@^7.1.0 oxlint oxlint-tsgolint vitest \
    @playwright/test licensee
npm install -D --save-exact oxfmt
npx playwright install chromium
```

- `typescript@^7.1.0` matches nothing until TypeScript 7.1 is stable, planned for
  24-11-2026. When `npm view typescript@^7.1.0 version` prints nothing, use
  `typescript@beta`, which writes `^7.1.0-beta` and accepts the stable release later.
- `@types/node@latest` replaces the `^20` that `create-next-app` writes, which conflicts
  with Vitest's dependencies.
- Oxfmt is pinned to an exact version because it hasn't reached 1.0.
- Oxlint doesn't check the markup in Astro templates, so accessibility there is left to
  review.

With `ASTRO`, also add the content mapper, so `tsc` can check `.astro` files, and the
compiler Oxfmt needs to format them. `astro check` isn't used: it refuses TypeScript 7, and
Astro says it will be deprecated. The mapper is experimental, and `--runExternalCode` lets
it run during type checks:

```sh
npm install -D @astrojs/ts-content-mapper
npm install -D --save-exact @astrojs/compiler-rs
```

```json
"contentMappers": [{ "package": "@astrojs/ts-content-mapper", "extensions": [".astro"] }]
```

The last block goes in `tsconfig.json`, at the top level.

### 6. Set up the deployment (script)

With a frontend, run these in `{{FRONTEND_DIR}}/` for its `{{DEPLOY_TARGET}}`, then write
its `astro.config.mjs` or `next.config.ts` from `template/files/`, in place of what the
scaffolding tool wrote:

| `{{DEPLOY_TARGET}}` | Astro | Next.js |
| --- | --- | --- |
| `none` or `github-pages` | Nothing | Nothing |
| `cloudflare-static` | `npm install -D wrangler`, and write `wrangler.jsonc` | `npm install -D wrangler`, and write `wrangler.jsonc` |
| `cloudflare-server` | `npm install -D wrangler`, and `npx astro add cloudflare --yes` | `npx vinext@latest init --platform=cloudflare --cdn-cache=none --data-cache=none --image-optimization=none` |

Step 4 already wrote each target's workflow, and step 8 sets up what GitHub needs.

- `create-next-app` turns on `cacheComponents` and `partialPrefetching`. Static export fails
  with them ("PPR cannot be enabled in export mode"), and vinext's deploy check stops without
  a cache adapter, so `next.config.ts` only keeps them with `none`.
- When the frontend is at the root, `agentRules: false` stops `next dev` from writing its own
  block into the root `AGENTS.md`, which it does when it detects a coding agent, even after
  `--no-agents-md`. In a subfolder, Next.js keeps its own `AGENTS.md`, as Astro does.
- `session: false` and `imageService: "compile"` stop Astro's adapter from asking for a KV
  namespace and an Images binding on deploy. A project can turn them back on.
- vinext is Cloudflare's recommended route for new Next.js apps. It is in beta, and
  reimplements Next.js on Vite. `next build` keeps working next to it, so a project can
  change route later. It writes its own configuration, scripts and ignore entries, and
  upgrades React.
- On Cloudflare's free plan, requests for static assets are free and unlimited. Server code
  gets 100,000 requests a day and 10 ms of CPU per request, and Cloudflare says server
  rendering typically uses 10 to 20 ms, so try a real deploy early.

### 7. Set up the agent tools (script)

Skip this step when `{{AGENT_TOOLS}}` is `none`. The project is complete without it:
`AGENTS.md`, the briefs in `docs/agents/` and the scripts work with any tool.

For each tool in `{{AGENT_TOOLS}}`, write its files from `template/files/`. A tool gets
one agent file for each role in "The agent roles". An agent file holds the tool's settings
and points at the role's brief, so every tool follows the same brief.

| Tool | Agent files | Asks before commits, pushes and releases | MCP servers, with `MCP` |
| --- | --- | --- | --- |
| Claude Code | `.claude/agents/{{ROLE}}.md` | `.claude/settings.json` | `.mcp.json` |
| Codex | `.codex/agents/{{ROLE}}.toml` | `.codex/rules/default.rules` | `.codex/config.toml` |
| Cursor | `.cursor/agents/{{ROLE}}.md` | `.cursor/hooks.json` and `.cursor/hooks/ask-first.sh` | `.cursor/mcp.json` |
| OpenCode | `.opencode/agents/{{ROLE}}.md` | `opencode.json` | `opencode.json` |

With `CLOUDFLARE`, also install Cloudflare's skills, adding `--skill nextjs-on-cloudflare`
with `VINEXT`. This puts them in `.agents/skills/`, adds symlinks for Claude Code, and writes
`skills-lock.json`, which is committed:

```sh
npx skills add https://github.com/cloudflare/skills --skill cloudflare --skill wrangler \
    --skill workers-best-practices {{SKILLS_AGENT_FLAGS}} -y
```

Claude Code:

- It reads `AGENTS.md` itself, from version 2.1.277, when the folder and the folders above
  it have no `CLAUDE.md`, `.claude/CLAUDE.md` or `CLAUDE.local.md`. If it can't, write a
  `CLAUDE.md` holding only the import line `@AGENTS.md`.
- It reads skills from `.claude/skills/`, not `.agents/skills/`, so a skill added by hand
  needs a symlink there: `ln -s ../../.agents/skills/{name} .claude/skills/{name}`.
- `.claude/settings.local.json` sets `TMPDIR` and `GIT_CEILING_DIRECTORIES` for the commands
  Claude Code runs. It holds this machine's absolute path, so it is never committed. Claude
  Code keeps a file it creates itself out of git, but this one is written by the script, so
  `.gitignore` lists it.
- Its own temporary files follow `CLAUDE_CODE_TMPDIR`, which project settings can't change.
  They stay outside the project, and that's fine: the rule about `tmp/` covers the files
  the agents make.
- If `.claude/agents/` didn't exist when a session started, that session only sees the
  agents after a restart.
- The servers in `.mcp.json` wait until someone approves them in an interactive session.

Codex:

- It only reads a project's `.codex/` folder once the owner has marked the project as
  trusted.
- A custom agent can't be made read-only, because it inherits the parent's sandbox. The
  briefs say not to change files, and that's the only limit.
- Its examples name agents with underscores, so `{{ROLE_CODEX_NAME}}` is `{{ROLE}}` with
  `_` in place of `-`.

Cursor:

- `readonly: true` also stops state-changing shell commands, so a read-only agent can't run
  a build. The briefs tell it to say which checks it couldn't run.
- It also reads `.claude/agents/` and `.codex/agents/`, and `.cursor/` wins when the same
  agent is in several.
- `.cursor/hooks/ask-first.sh` must be executable.

The other tools get `TMPDIR` the way `AGENTS.md` describes: the scripts set it for
themselves, and the agent exports it for other commands.

### 8. Configure GitHub (script, asks first for each)

```sh
# The labels the issue forms use
gh label create bug --description "Something doesn't work" --force
gh label create enhancement --description "An improvement or a new feature" --force
gh label create documentation --description "Documentation and the website" --force

# Dependabot alerts and security updates
gh api -X PUT repos/{{REPOSITORY}}/vulnerability-alerts
gh api -X PUT repos/{{REPOSITORY}}/automated-security-fixes

# The description, and the topics and home page unless they were left empty
gh repo edit {{REPOSITORY}} --description "{{DESCRIPTION}}" --add-topic {{TOPICS}} --homepage {{HOMEPAGE}}
```

The rest of this step needs `PUBLIC`. On GitHub Free, a private repository doesn't get
private vulnerability reporting, secret scanning, rulesets or Pages, so the script lists
them as skipped:

```sh
# Private vulnerability reporting
gh api -X PUT repos/{{REPOSITORY}}/private-vulnerability-reporting

# Secret scanning, and push protection
gh repo edit {{REPOSITORY}} --enable-secret-scanning --enable-secret-scanning-push-protection
```

Add a ruleset that protects `{{MAIN_BRANCH}}` from deletion and force pushes. It doesn't
require pull requests, because the work process allows working directly on the main
branch:

```sh
gh api -X POST repos/{{REPOSITORY}}/rulesets --input - <<'EOF'
{
  "name": "Protect {{MAIN_BRANCH}}",
  "target": "branch",
  "enforcement": "active",
  "conditions": { "ref_name": { "include": ["~DEFAULT_BRANCH"], "exclude": [] } },
  "rules": [{ "type": "deletion" }, { "type": "non_fast_forward" }]
}
EOF
```

With `PAGES`, publish the site from GitHub Actions:

```sh
gh api -X POST repos/{{REPOSITORY}}/pages -f build_type=workflow
```

This applies to any visibility. With `CLOUDFLARE`, the owner creates an API token in the
Cloudflare dashboard, from its "Edit Cloudflare Workers" template. Each of these commands asks for its value, so it stays
out of the shell history and every file:

```sh
gh secret set CLOUDFLARE_API_TOKEN --repo {{REPOSITORY}}
gh secret set CLOUDFLARE_ACCOUNT_ID --repo {{REPOSITORY}}
```

### 9. The parts that need a model

These need judgment, so the script prints this prompt rather than running it. Any coding
agent can take it, in a session in the repository, and the owner can also do the work by
hand. For a brand-new project, it can wait until there is something to describe:

> Read `AGENTS.md`, and follow its rules.
>
> 1. With me, brainstorm the roadmap, as section 2.3.1 describes.
> 2. Fill in `docs/security/threat-model.md` and `docs/security/standards.md`, using what we
>    know about the project. Leave out the details of any vulnerability that isn't fixed
>    yet.
> 3. When the project has a version command or other diagnostics, add them to the issue
>    forms. Before a form asks people to paste a command's output, check in the code that
>    the command never prints secrets.
>
> Verify what you write, and report what you couldn't verify. Don't commit anything.

### 10. Verify the result (script)

First format everything the setup wrote, so the first check passes. Run the frontend's
command in `{{FRONTEND_DIR}}/`. Mycila's command needs its full coordinates, because both
license plugins use the prefix `license`:

```sh
# Java
./mvnw com.mycila:license-maven-plugin:format
./mvnw spotless:apply

# Haskell
ormolu --no-cabal --mode inplace $(find app src test -name '*.hs')

# Rust
cargo fmt

# Astro and Next.js
npx oxfmt
```

`--no-cabal` is there because ormolu can't read the `cabal-version: 3.18` that `cabal init`
writes. Then check these:

- `scripts/check.sh` passes on the new scaffold.
- `git check-ignore docs/planning docs/iterations docs/architecture/design` lists all three.
- No file holds a leftover parameter, or a condition marker.
- After `scripts/check.sh` has run, `git ls-files --others --exclude-standard tmp` lists only
  `tmp/.gitkeep`.
- The script lists what it created, what it skipped, and what the owner declined.

git-agent's own unit tests render the templates for every combination of conditions and
check the rest, so the binary needs no YAML parser:

- Each agent file parses: the frontmatter as YAML, and Codex's files as TOML.
- With `none` in `{{AGENT_TOOLS}}`, no file names a specific agent tool.
- Each issue form passes GitHub's form schema: the required keys, `id`s of letters,
  digits, `-` and `_` that are unique in the form, and distinct dropdown options.

## The scripts

All of them are Bash with `set -euo pipefail`. They run from the repository's root, and set
`TMPDIR` and `GIT_CEILING_DIRECTORIES` to `tmp/` for everything they run. They make temporary
folders with `mktemp -d "$PWD/tmp/name.XXXXXX"`, since macOS `mktemp -d` ignores `TMPDIR`.
All but `check.sh` take `--dry-run`, and never force-push. Running one again after a failure
is safe, because each step that already happened is detected and skipped.

`scripts/check.sh` runs every check for each stack in step 5: formatting, linting,
complexity, tests and dependency licenses. It exits with an error as soon as one fails.
CI, the agents and the release script all use it. Its template is `template/files/scripts/check.sh.tmpl`.

`scripts/release.sh X.Y.Z` releases to GitHub:

1. It checks that `gh` is logged in, the working tree is clean, the branch is
   `{{MAIN_BRANCH}}` and not behind `origin`, and `X.Y.Z` is newer than the current
   version.
2. It runs `scripts/check.sh`.
3. It sets the version in every manifest in the repository, and their lock files:
   `Cargo.toml` and `Cargo.lock`, `pom.xml` through `./mvnw versions:set`, the `.cabal`
   file, and `package.json` and `package-lock.json` through
   `npm version --no-git-tag-version`. Then it moves `CHANGELOG.md`'s Unreleased section
   under `X.Y.Z`.
4. It commits `chore(release): vX.Y.Z` with the owner's own git identity, and makes an
   annotated tag `vX.Y.Z`.
5. It pushes the branch and the tag together (`git push --atomic`), and creates the
   GitHub release with `gh release create vX.Y.Z --verify-tag --notes-file {file}`, where
   the file in `tmp/` holds the version's section of the changelog.

`scripts/publish.sh [target ...]`, written with `PUBLISH`, publishes the release at `HEAD`'s
tag to each target,
by default all of `{{PUBLISH_TARGETS}}`. Publishing to a registry can't be undone, so it
asks before each target, unless it gets `--yes`. It skips a target that already has the
version. Credentials stay in each tool's own login or the system keychain, never in a
file in the repository.

| Target | How |
| --- | --- |
| crates.io | `cargo publish`, after `cargo publish --dry-run` |
| Maven Central | `./mvnw deploy` with Sonatype's `central-publishing-maven-plugin`; the token and the GPG key stay in `~/.m2/settings.xml` and the keyring |
| npm | `npm publish --access public`, with `--provenance` when it runs in CI |
| Hackage | `cabal sdist`, then `cabal upload --publish` |

Deploying isn't part of publishing. `ci.yml` deploys by itself, as "CI" describes.

`scripts/rollback.sh X.Y.Z` undoes a GitHub release that nothing has published yet. It
deletes the GitHub release and the tag on GitHub, after asking, and then locally. It
refuses once any registry has the version. A pushed version commit stays; the next
release simply goes past it.

## CI

CI is GitHub Actions only, on Ubuntu runners. `.github/workflows/ci.yml` runs
`scripts/check.sh` on every push to `{{MAIN_BRANCH}}` and every pull request, with
read-only permissions, and can also be run by hand from the Actions tab. Each action is
pinned to the full commit of its latest release, looked up when the project is set up,
because a tag can move while a job holds secrets. Dependabot keeps the pins current.

On GitHub's Ubuntu runners, rustup is installed, and `RUSTUP_AUTO_INSTALL` (on by
default) installs the toolchain that `rust-toolchain.toml` names. The other stacks set up
their toolchain with an action:

- Java: `actions/setup-java`, with Temurin and Maven caching.
- Haskell: `haskell-actions/setup`, then `cabal install` for ormolu and cabal-plan, with
  the cabal store cached.
- Astro and Next.js: `actions/setup-node`, reading `.nvmrc`, then `npm ci` and Playwright's
  Chromium.

Rust also gets `cargo-deny`, from `taiki-e/install-action`.

With a deploy target, `ci.yml` has a second job, which runs on `{{MAIN_BRANCH}}` once the
checks have passed. It calls `pages.yml` or `cloudflare.yml`, which have no trigger of their
own, so every deploy goes through the checks, a commit that fails them doesn't deploy, and
pull requests never reach the deploy secrets. Only the job that deploys to Pages gets its
write permissions. To deploy again by hand, run `ci.yml` on `{{MAIN_BRANCH}}`.

## Decisions for `project init`

- `release.sh`, `publish.sh` and `rollback.sh` are written when `project init` is built,
  because testing them properly needs a real GitHub repository. That includes what Maven
  Central needs in `pom.xml`: Sonatype's plugin, sources, Javadoc and signing.
- `docs/planning/`, `docs/iterations/` and `docs/architecture/design/` are git-ignored in every
  project, public or private, so they can hold information that shouldn't be public. The
  generated `AGENTS.md` explains what follows from that.
- A backlog item can belong to one group, through the Group field in its metadata, and an
  iteration can include single items, one or more groups, or both.
- The setup doesn't need a coding agent. The first version has add-ons for Claude Code,
  Codex, Cursor and OpenCode.
- A repository holds one backend stack and an optional frontend, and all their manifests
  share one version.
- The templates are the real files in this folder, kept in the crate and embedded in the
  binary with `include_str!`. That macro can't list a folder, so the Rust code holds the
  table from "File templates" and the role table as lists, and a unit test walks
  `template/files/` to check that no file is missing from them. They work offline and match the installed version, and
  changing one needs a release.
- Upgrading a project that was set up earlier (`project update`) isn't planned for now.
- Homebrew isn't a publish target.

## Open questions for the design of `project init`

- Whether server-rendered projects stay within the 10 ms of CPU on Cloudflare's free plan.
  That needs a real deploy, which hasn't been tried.

## File templates

Every template is a real file. `template/files/` holds the files the setup writes, each at the
path in the table plus `.tmpl`. A file's place in the new project is its "Goes in" folder
followed by that path. A path with `{{ROLE}}` is written once
for each role in "The agent roles". `template/snippets/pom-plugins.xml.tmpl` goes inside
`<build><plugins>` in the `pom.xml` the archetype generated.

| File | Written when | Step | Goes in |
| --- | --- | --- | --- |
| `AGENTS.md` | Always | 4 | The root |
| `README.md` | Always | 4 | The root |
| `CHANGELOG.md` | Always | 4 | The root |
| `CONTRIBUTING.md` | Always | 4 | The root |
| `.editorconfig` | Always | 4 | The root |
| `.gitattributes` | Always | 4 | The root |
| `.gitignore` | Always | 4 | The root |
| `scripts/check.sh` | Always | 4 | The root |
| `docs/agents/application-architect.md` | Always | 4 | The root |
| `docs/agents/security-architect.md` | Always | 4 | The root |
| `docs/agents/lead-frontend-developer.md` | `FRONTEND` | 4 | The root |
| `docs/agents/product-ux-designer.md` | `FRONTEND` | 4 | The root |
| `docs/planning/ROADMAP.md` | Always | 4 | The root |
| `docs/planning/backlog/BACKLOG.md` | Always | 4 | The root |
| `docs/security/threat-model.md` | Always | 4 | The root |
| `docs/security/standards.md` | Always | 4 | The root |
| `docs/templates/roadmap.md` | Always | 4 | The root |
| `docs/templates/iteration.md` | Always | 4 | The root |
| `docs/templates/backlog-item.md` | Always | 4 | The root |
| `docs/templates/design-document.md` | Always | 4 | The root |
| `docs/templates/decision-record.md` | Always | 4 | The root |
| `docs/templates/NOTICE` | Always | 4 | The root |
| `.github/pull_request_template.md` | Always | 4 | The root |
| `.github/SECURITY.md` | Always | 4 | The root |
| `.github/dependabot.yml` | Always | 4 | The root |
| `.github/ISSUE_TEMPLATE/config.yml` | Always | 4 | The root |
| `.github/ISSUE_TEMPLATE/1-bug-report.yml` | Always | 4 | The root |
| `.github/ISSUE_TEMPLATE/2-improvement.yml` | Always | 4 | The root |
| `.github/ISSUE_TEMPLATE/3-feature-request.yml` | Always | 4 | The root |
| `.github/ISSUE_TEMPLATE/4-documentation.yml` | Always | 4 | The root |
| `.github/workflows/ci.yml` | Always | 4 | The root |
| `.github/workflows/pages.yml` | `PAGES` | 4 | The root |
| `.github/workflows/cloudflare.yml` | `CLOUDFLARE` | 4 | The root |
| `rust-toolchain.toml` | `RUST` | 5 | The root |
| `deny.toml` | `RUST` | 5 | The root |
| `cabal.project` | `HASKELL` | 5 | The root |
| `pmd-ruleset.xml` | `JAVA` | 5 | The root |
| `.nvmrc` | `FRONTEND` | 5 | `{{FRONTEND_DIR}}/` |
| `.oxlintrc.json` | `FRONTEND` | 5 | `{{FRONTEND_DIR}}/` |
| `.oxfmtrc.json` | `FRONTEND` | 5 | `{{FRONTEND_DIR}}/` |
| `.licensee.json` | `FRONTEND` | 5 | `{{FRONTEND_DIR}}/` |
| `vitest.config.ts` | `FRONTEND` | 5 | `{{FRONTEND_DIR}}/` |
| `playwright.config.ts` | `FRONTEND` | 5 | `{{FRONTEND_DIR}}/` |
| `e2e/home.spec.ts` | `FRONTEND` | 5 | `{{FRONTEND_DIR}}/` |
| `next.config.ts` | `NEXTJS` | 6 | `{{FRONTEND_DIR}}/` |
| `astro.config.mjs` | `ASTRO` | 6 | `{{FRONTEND_DIR}}/` |
| `wrangler.jsonc` | `CLOUDFLARE_STATIC` | 6 | `{{FRONTEND_DIR}}/` |
| `.claude/agents/{{ROLE}}.md` | `CLAUDE_CODE`, once per role | 7 | The root |
| `.claude/settings.json` | `CLAUDE_CODE` | 7 | The root |
| `.claude/settings.local.json` | `CLAUDE_CODE` | 7 | The root |
| `.mcp.json` | `CLAUDE_CODE` and `MCP` | 7 | The root |
| `.codex/agents/{{ROLE}}.toml` | `CODEX`, once per role | 7 | The root |
| `.codex/rules/default.rules` | `CODEX` | 7 | The root |
| `.codex/config.toml` | `CODEX` and `MCP` | 7 | The root |
| `.cursor/agents/{{ROLE}}.md` | `CURSOR`, once per role | 7 | The root |
| `.cursor/hooks.json` | `CURSOR` | 7 | The root |
| `.cursor/hooks/ask-first.sh` | `CURSOR` | 7 | The root |
| `.cursor/mcp.json` | `CURSOR` and `MCP` | 7 | The root |
| `.opencode/agents/{{ROLE}}.md` | `OPENCODE`, once per role | 7 | The root |
| `opencode.json` | `OPENCODE` | 7 | The root |

`LICENSE` and `CODE_OF_CONDUCT.md` have no template, because they come from GitHub's API:

```sh
# LICENSE: replace [year] with {{YEAR}} and [fullname] with {{COPYRIGHT_HOLDER}}, where the
# text has them (the MIT and BSD licenses do)
gh api /licenses/{{LICENSE_KEY}} --jq .body

# CODE_OF_CONDUCT.md: the Contributor Covenant, version 2.0. Replace
# [INSERT CONTACT METHOD] with {{CONDUCT_CONTACT}}
gh api /codes_of_conduct/contributor_covenant --jq .body
```

### The agent roles

The agent files in the table above are written once per role, for each tool in
`{{AGENT_TOOLS}}`. The
last two roles only exist with `FRONTEND`. `ROLE_USES_DESIGN_SKILL` also needs
`DESIGN_SKILL`. The Claude settings only go in Claude Code's files, and the other tools use
their own default model.

| `{{ROLE}}` | `{{ROLE_DESCRIPTION}}` | `READ_ONLY` | `ROLE_USES_DESIGN_SKILL` | `{{ROLE_CLAUDE_MODEL}}` | `{{ROLE_CLAUDE_EFFORT}}` | `{{ROLE_CLAUDE_COLOR}}` |
| --- | --- | --- | --- | --- | --- | --- |
| `application-architect` | Application architect for {{PROJECT_NAME}}. Use for design reviews of the documents in docs/architecture/design/, for architecture code reviews (domain model, bounded contexts, packaging by feature, cohesion and coupling, complexity, testability), and for proposals to restructure or refactor the code. Reports findings and recommendations; doesn't change project files. | Yes | No | `opus` | `xhigh` | `blue` |
| `security-architect` | Security architect for {{PROJECT_NAME}}. Use for threat modelling, risk assessments and security code reviews, especially of the application core, for keeping docs/security/ current, and for designing or implementing security guardrails when the lead asks. | No | No | `fable` | `high` | `red` |
| `lead-frontend-developer` | Lead frontend developer for {{PROJECT_NAME}}'s frontend in {{FRONTEND_DIR}}/. Use to review frontend changes for semantic HTML, accessibility, responsive design, performance, TypeScript and framework usage, Node tooling, and use of the design system. Reports findings and recommendations; doesn't change project files. | Yes | Yes | `opus` | `xhigh` | `green` |
| `product-ux-designer` | Product UX designer for {{PROJECT_NAME}}'s frontend. Use to review the design (visual hierarchy, layout, typography, color, motion, accessibility and the premium feel) and to propose design updates within the project's design system. Reports findings and proposals; doesn't change project files. | Yes | Yes | `opus` | `xhigh` | `purple` |

### Notes on the files

- `docs/planning/ROADMAP.md` and `docs/planning/backlog/BACKLOG.md`: the setup writes these
  even though they are git-ignored, so they exist only on the owner's machine.
- `docs/templates/NOTICE`: a new project has no code or assets from others, so there is no
  `NOTICE` at the root until it includes some. It starts from this file.
- `.gitattributes`: git has no brace patterns here, so each extension gets its own line.
- `.github/dependabot.yml`: Dependabot has no ecosystem for cabal, so a Haskell project only
  gets updates for its GitHub Actions.
- `.github/workflows/pages.yml`: the `site` and `base` settings in Astro's `astro.config.mjs`,
  and `basePath` in Next.js's `next.config.ts`, must match the Pages address, such as
  `https://{{REPOSITORY_OWNER}}.github.io/{{REPOSITORY_NAME}}/`.
- `.claude/settings.local.json`: after the setup, check in a new Claude Code session that
  `echo "$TMPDIR"` prints `{{ABSOLUTE_TMP}}`.
- `wrangler.jsonc`: `astro add cloudflare` writes its own with `cloudflare-server`, and vinext
  uses `cloudflare.config.ts`.
- `.oxlintrc.json`: JSX and Astro markup take up many lines, so those files get a limit of 100
  lines per function, where other code gets 60. The ignored folders aren't the project's own
  code, and matter when the frontend is at the repository's root.
- `.oxfmtrc.json`: Oxfmt also formats Markdown, YAML, TOML and JSON. When the frontend is at
  the repository's root, its patterns keep it away from the documents and the agent tools'
  files.
- `scripts/check.sh`: the `awk` line skips the packages that come with GHC, which `cabal-plan`
  prints in bold and which can have no license data. `grep` prints what remains that isn't
  allowed, and the script then fails.
- `.licensee.json`: the `img` scope holds the prebuilt binaries of sharp, whose libvips is
  under the LGPL-3.0-or-later. Astro and Next.js both install it to process images.
- `vitest.config.ts`: the `include` keeps Vitest away from Playwright's tests in `e2e/`.
- `playwright.config.ts`: Astro runs `astro preview` in the background when a coding agent
  starts it, and Playwright then stops with "Process from config.webServer exited early".
  `--ignore-lock` keeps it in the foreground.
- `pom-plugins.xml`: the names in `licenseMerges` are how Maven packages spell their licenses,
  mapped to the SPDX identifiers. Add a spelling when a build reports it as forbidden. `force`
  makes the license check run every time, because without it the plugin says "All files are
  up to date" and passes.
