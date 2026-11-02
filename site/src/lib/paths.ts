// SPDX-FileCopyrightText: 2026 Dervis Mansuroglu
// SPDX-License-Identifier: GPL-3.0-or-later

// Links within the site, wherever it is deployed (see `base` in astro.config.mjs).

const base = import.meta.env.BASE_URL.replace(/\/?$/, '/')

/** The site path for `path`, e.g. `docs/` or `#install` on the home page. */
export function url(path = ''): string {
  return base + path.replace(/^\//, '')
}
