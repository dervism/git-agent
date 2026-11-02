// @ts-check
// SPDX-FileCopyrightText: 2026 Dervis Mansuroglu
// SPDX-License-Identifier: GPL-3.0-or-later

import { defineConfig } from 'astro/config'
import { unified } from '@astrojs/markdown-remark'
import tailwindcss from '@tailwindcss/vite'
import { rehypeScrollTables, remarkReadme } from './src/lib/markdown.mjs'
import theme from './src/lib/shiki-theme.mjs'

// https://astro.build/config
export default defineConfig({
  // Served by GitHub Pages as a project site.
  site: 'https://dervism.github.io',
  base: '/git-agent',
  markdown: {
    shikiConfig: { theme },
    // The docs page is the README, tidied by remark and rehype plugins.
    processor: unified({
      remarkPlugins: [remarkReadme],
      rehypePlugins: [rehypeScrollTables],
    }),
  },
  vite: {
    plugins: [tailwindcss()],
  },
})
