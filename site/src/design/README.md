<!--
SPDX-FileCopyrightText: 2026 Dervis Mansuroglu
SPDX-License-Identifier: GPL-3.0-or-later
-->

# The git-agent design system

Everything that gives the site its look lives here: the tokens, the shared utilities, the
documentation's typography, the motion, and the components the pages are built from.
Pages and sections use these pieces rather than repeating their classes.

Everything here is original work, under the site's GPL-3.0-or-later, apart from the logo
and the Heroicons data (see [License](#license)). The sections adapted from Tailwind Plus
blocks, and the building blocks taken from them, live apart in `src/blocks/`: they are not
part of the design system and are not open source.

## Files

| File | What it holds |
| --- | --- |
| `index.css` | The entry point: Tailwind, the fonts, the typography plugin, and the files below. Every page imports this one file, through the layout. |
| `tokens.css` | The theme: fonts, the color scales, and the animations with their keyframes. |
| `base.css` | Page defaults: smooth scrolling, font features, the selection color. |
| `utilities.css` | Shared surfaces and effects, and the CSS for the motion. |
| `prose.css` | The docs page: the README set in the palette. |
| `motion.ts` | The script that starts the motion; the layout runs it once per page. |
| `terminal.ts` | Terminal output: a tone per line, and a color per tone. |
| `icons.ts` | The Heroicons the site uses, as plain SVG data (generated). |
| `components/` | The components, below. |

## Color

The site is always dark. The palette is Tailwind's `gray` scale, overridden: a blue gray
is still a gray, so the blocks' `gray` classes stay true. Their accent, renamed from
`indigo`, is `brand`, a steel blue drawn from the palette.

| Token | Value | Role |
| --- | --- | --- |
| `gray-950` | `#030b10` | Wells: code, inset panels |
| `gray-900` | `#06141b` | The page |
| `gray-800` | `#11212d` | Raised surfaces: cards, panels |
| `gray-700` | `#253745` | Lines and hover fills |
| `gray-600` | `#4a5c6a` | Quiet lines, rules in code |
| `gray-500` | `#72828a` | Muted text |
| `gray-400` | `#9ba8ab` | Secondary text |
| `gray-300` | `#b4bcbd` | Body text |
| `gray-200` | `#ccd0cf` | Strong text, the silver of primary buttons |
| `white` | | Headings |
| `brand-300`, `brand-400` | `#9cb9ca`, `#7fa2b8` | Accent text: eyebrows, links, icons |
| `brand-700`, `brand-800` | `#3b5568`, `#2d4252` | Glows |

Fonts are Inter for text and JetBrains Mono for code, both variable and served with the
site.

## Utilities

| Utility | Use |
| --- | --- |
| `text-sheen` | Silver gradient text; with `animate-shimmer`, a slow sheen. Use `<Sheen>`. |
| `chip` | Inline code and small labels. Use `<Chip>`. |
| `well` | An inset, darker area inside a card. Add a `rounded-*`. |
| `panel` | A raised glass surface with a soft shadow. Add a `rounded-*`. |
| `bg-stage` | The lit gradient a code window sits on. |
| `grid-lines` | A faint grid that fades toward the edges, behind a section. |
| `grain` | Film grain; the layout lays it over every page. |

`well` and `panel` leave the corner radius to their use, so a component can round only
some corners without two utilities competing.

## Motion

| Attribute or token | Effect |
| --- | --- |
| `data-reveal` | Fades and lifts an element in when it scrolls into view. `style="--reveal-delay: 120ms"` staggers elements that appear together. |
| `data-spotlight` | A soft light follows the pointer across a card. The element needs a radius and `position: relative`. |
| `animate-drift`, `animate-drift-slow` | Slow drifting, for glows. |
| `animate-shimmer` | The sheen passing over `text-sheen`. |
| `animate-marquee` | An endless horizontal scroll, for two copies of a list side by side. |
| `animate-blink` | A terminal caret. |
| `animate-float` | A gentle bob. |
| `animate-key`, `animate-swap-out`, `animate-swap-in` | The small demos in the feature cards. |

Elements revealed with `data-reveal` only start hidden once the script runs, so the page
reads without JavaScript. `prefers-reduced-motion` turns off every animation and reveal.

## Components

| Component | Props | Use |
| --- | --- | --- |
| `Section` | `id`, `labelledby`, `rhythm` (`normal` or `tight`), slot `background` | A page section's rhythm and background. The content brings its own container. |
| `Chip` | `size` (`sm` or `md`) | Inline code: a command, a file name. |
| `Sheen` | | Silver words in a heading. |
| `Glow` | `tone` (`brand`, `gray`, `deep`), `speed`, `class` for place and size | A blurred light drifting behind a section. |
| `Divider` | | A hairline between sections. |
| `Icon` | `name` | A Heroicon from `icons.ts`. |
| `Logo` | | The git-agent mark. |

## Tailwind Plus blocks

`src/blocks/` holds what is adapted from Tailwind Plus: the page sections (header, hero,
features, explain, safety, call to action, footer, docs navigation) and four building blocks
taken from them, `SectionHeading`, `Button`, `Badge` and `CodeWindow`. They use the tokens and
utilities here, but they are Tailwind Labs' work, used under a personal Tailwind Plus license
for this website only. No license to them is granted; see `site/NOTICE`.

## Adding to it

- A new section adapted from a Tailwind Plus block goes in `src/blocks/`, with the Tailwind
  Plus header. Anything adapted from Tailwind Plus stays there and never moves into
  `src/design/`, so the design system stays free of it.
- A pattern of your own that a second section needs belongs here: as a utility in
  `utilities.css` if it is only styling, or as a component if it has structure.
- New colors come from the palette. Use the `gray` and `brand` scales rather than new hex
  values, and add a token in `tokens.css` if the scales don't have it.
- Every new file starts with an SPDX header naming its license, like the files around it.

## License

The design system is free software under the GNU General Public License, version 3 or (at
your option) any later version: see `site/LICENSE`. Two files are not covered:

- `components/Logo.astro`, the git-agent logo: all rights reserved.
- `icons.ts`, the Heroicons data: MIT License, copyright Tailwind Labs, Inc.

`site/NOTICE` has the details.
