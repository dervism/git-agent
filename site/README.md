# git-agent website

The website for [git-agent](https://github.com/dervism/git-agent): a single static page built with
[Astro](https://astro.build) and Tailwind CSS v4. Its sections are adapted from Tailwind Plus blocks.

```sh
npm install
npm run dev       # http://localhost:4321
npm run build     # the static site, in dist/
npm run preview   # serve dist/ locally
```

What the home page says lives in `src/content/site.ts`; the sections are in `src/components/`.
The docs page (`/docs/`) is the repository's own `README.md`, loaded as a content collection
(`src/content.config.ts`) and tidied by the plugins in `src/lib/markdown.mjs`, so editing the README
updates it. The
colors are design tokens in `src/styles/global.css`: the palette overrides Tailwind's `gray`
scale, and `brand` is the accent.

## Deployment

The site is published at https://dervism.github.io/git-agent/ by the `Website` workflow
(`.github/workflows/pages.yml`). It runs on every push to master that changes `site/` or the
README, and can be started by hand from the Actions tab. `site` and `base` in `astro.config.mjs`
match that address; change both if the site moves.
