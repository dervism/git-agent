// SPDX-FileCopyrightText: 2026 Dervis Mansuroglu
// SPDX-License-Identifier: GPL-3.0-or-later

// Markdown plugins for rendering the repository's README as the docs page.

const repository = 'https://github.com/dervism/git-agent/blob/master/'

/** Visits every node of a markdown or HTML tree, parents before children. */
function visit(node, callback, parent = null) {
  callback(node, parent)
  for (const child of node.children ?? []) visit(child, callback, node)
}

/**
 * Drops the README's own title and the badge line under it (the page has its
 * own heading), and points relative links such as `LICENSE` at GitHub.
 */
export function remarkReadme() {
  return (tree) => {
    const isBadge = (node) =>
      node?.type === 'paragraph' &&
      node.children.every(
        (child) =>
          child.type === 'image' ||
          (child.type === 'link' && child.children.every((inner) => inner.type === 'image')) ||
          (child.type === 'text' && !child.value.trim()),
      )
    const first = tree.children.findIndex((node) => node.type === 'heading' && node.depth === 1)
    if (first >= 0) {
      const remove = isBadge(tree.children[first + 1]) ? 2 : 1
      tree.children.splice(first, remove)
    }
    visit(tree, (node) => {
      if ((node.type === 'link' || node.type === 'image') && !/^([a-z]+:|#|\/)/i.test(node.url)) {
        node.url = repository + node.url
      }
    })
  }
}

/** Wraps each table in a scrolling container, so wide tables fit on phones. */
export function rehypeScrollTables() {
  return (tree) => {
    visit(tree, (node, parent) => {
      if (node.type !== 'element' || node.tagName !== 'table' || !parent) return
      if (parent.properties?.className?.includes('table-scroll')) return
      const index = parent.children.indexOf(node)
      parent.children[index] = {
        type: 'element',
        tagName: 'div',
        properties: { className: ['table-scroll'] },
        children: [node],
      }
    })
  }
}
