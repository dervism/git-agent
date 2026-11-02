import { defineCollection } from 'astro:content'
import { glob } from 'astro/loaders'

// The docs page is the repository's own README, so the two never drift apart.
export const collections = {
  readme: defineCollection({ loader: glob({ pattern: 'README.md', base: '..' }) }),
}
