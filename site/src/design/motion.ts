// SPDX-FileCopyrightText: 2026 Dervis Mansuroglu
// SPDX-License-Identifier: GPL-3.0-or-later

// The site's motion, started once per page by the layout.
//
//   data-reveal       fades and lifts an element in when it scrolls into view;
//                     --reveal-delay staggers elements that appear together
//   data-spotlight    a soft light that follows the pointer across a card
//
// The styles are in utilities.css, and prefers-reduced-motion turns them off.

export function startMotion(): void {
  const reveal = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          entry.target.classList.add('is-visible')
          reveal.unobserve(entry.target)
        }
      }
    },
    { rootMargin: '0px 0px -8% 0px', threshold: 0.12 },
  )
  document.querySelectorAll('[data-reveal]').forEach((element) => reveal.observe(element))

  document.querySelectorAll<HTMLElement>('[data-spotlight]').forEach((card) => {
    card.addEventListener('pointermove', (event) => {
      const box = card.getBoundingClientRect()
      card.style.setProperty('--x', `${event.clientX - box.left}px`)
      card.style.setProperty('--y', `${event.clientY - box.top}px`)
    })
  })
}
