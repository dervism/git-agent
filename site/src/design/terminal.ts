// SPDX-FileCopyrightText: 2026 Dervis Mansuroglu
// SPDX-License-Identifier: GPL-3.0-or-later

// Terminal output as the site shows it: each line has a tone, and each tone a color.

/** A line of terminal output; `tone` picks its color. */
export interface TerminalLine {
  readonly text: string
  readonly tone?: Tone
}

export type Tone = 'plain' | 'muted' | 'accent' | 'bright' | 'success' | 'rule' | 'menu'

export const tones: Record<Tone, string> = {
  plain: 'text-gray-300',
  muted: 'text-gray-500',
  accent: 'text-brand-300',
  bright: 'font-medium text-white',
  success: 'text-gray-100',
  rule: 'text-gray-600',
  menu: 'text-gray-400',
}

/** The classes for a line's tone. */
export function tone(line: TerminalLine): string {
  return tones[line.tone ?? 'plain']
}
