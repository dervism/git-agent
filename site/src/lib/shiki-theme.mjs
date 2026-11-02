// SPDX-FileCopyrightText: 2026 Dervis Mansuroglu
// SPDX-License-Identifier: GPL-3.0-or-later

// A Shiki theme drawn from the site's palette, for code blocks in the docs.
export default {
  name: 'git-agent',
  type: 'dark',
  colors: {
    'editor.background': '#030b10',
    'editor.foreground': '#ccd0cf',
  },
  tokenColors: [
    { scope: ['comment', 'punctuation.definition.comment'], settings: { foreground: '#72828a', fontStyle: 'italic' } },
    { scope: ['string', 'string.quoted', 'string.unquoted'], settings: { foreground: '#bdd2de' } },
    { scope: ['keyword', 'storage', 'keyword.operator'], settings: { foreground: '#7fa2b8' } },
    { scope: ['entity.name.function', 'support.function', 'entity.name.command'], settings: { foreground: '#ffffff' } },
    { scope: ['constant', 'constant.numeric', 'constant.language', 'constant.other'], settings: { foreground: '#9cb9ca' } },
    {
      scope: ['entity.name.tag', 'support.type.property-name', 'entity.other.attribute-name', 'variable.other.readwrite'],
      settings: { foreground: '#9cb9ca' },
    },
    { scope: ['variable', 'variable.parameter'], settings: { foreground: '#e2e5e4' } },
    { scope: ['punctuation', 'meta.brace', 'punctuation.definition'], settings: { foreground: '#9ba8ab' } },
  ],
}
