<!--
SPDX-FileCopyrightText: 2026 Dervis Mansuroglu
SPDX-License-Identifier: GPL-3.0-or-later
-->

# git-agent website

The website for [git-agent](https://github.com/dervism/git-agent): a static site built with
[Astro](https://astro.build) and Tailwind CSS v4. Its sections are adapted from Tailwind Plus blocks.

```sh
npm install
npm run dev       # http://localhost:4321/git-agent/
npm run build     # the static site, in dist/
npm run preview   # serve dist/ locally
```

What the home page says lives in `src/content/site.ts`. The design system, with the tokens,
utilities, motion and shared components, is in `src/design/`, with its own README. Sections
adapted from Tailwind Plus blocks are in `src/blocks/`, and sections of the site's own in
`src/components/`.

The docs page (`/docs/`) is the repository's own `README.md`, loaded as a content collection
(`src/content.config.ts`) and tidied by the plugins in `src/lib/markdown.mjs`, so editing the README
updates it.

## Deployment

The site is published at https://dervism.github.io/git-agent/ by the `Website` workflow
(`.github/workflows/pages.yml`). It runs on every push to master that changes `site/` or the
README, and can be started by hand from the Actions tab. `site` and `base` in `astro.config.mjs`
match that address; change both if the site moves.

## License

The website is free software under the GNU General Public License, version 3 or (at your
option) any later version: see [LICENSE](LICENSE). The git-agent application in the rest of
the repository is under the MIT License.

Some parts are not covered by the GPL, and [NOTICE](NOTICE) lists them:

- `src/blocks/` is adapted from Tailwind Plus and used under a personal Tailwind Plus
  license, for this website only. No license to it is granted: don't copy or redistribute
  it, on its own or as part of anything else.
- The git-agent logo is all rights reserved.
- The Heroicons data is under the MIT License, and the fonts under the SIL Open Font License.

Each file names its license in an `SPDX-License-Identifier` line at its top.
