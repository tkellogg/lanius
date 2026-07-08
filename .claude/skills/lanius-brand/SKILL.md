---
name: lanius-brand
description: >-
  The lanius visual identity — use when building, styling, or theming any UI or
  brand surface (ui/web, the README, a splash, an icon, an artifact/mockup). Sets
  the shrike/thorn identity, the palette and the one-red rule, the type system,
  and how the logo system is used. Marks live in brand/logos/; full direction in
  docs/handoffs/web-ui-redesign.md. Pair with the lanius-voice skill for copy.
---

# lanius brand — the butcher bird's flight recorder

**lanius** is a shrike — the "butcher bird" that impales prey on thorns and keeps
a larder. The product is a flight recorder you can talk to: it pins the work and
records everything. The identity says exactly that — quietly, like a precise
instrument, not a hacker terminal.

## The one rule that organizes everything: one reserved red
Red is the loudest, rarest thing on screen — the thorn. Only its intensity varies.
- **`--thorn #E5484D`** — resting brand accent: the active state, the one thing
  that needs attention, the wordmark tittle. Use sparingly, like the impaled berry.
- **`--pain #FF4A3D`** — algedonic / alarm: a failed run, a blast-radius warning,
  the signal lamp. The same red, escalated.
Everything else is neutral. If a second thing on a screen is red, one of them is
wrong.

## Palette (token-based; light + dark)
- ink `#16181D` · ground-dark `#0F1115` · paper `#F4F5F7` · panel (dark) `#171A20`
- shrike-grey `#C9CFD6` (secondary) · thorn `#E5484D` · pain `#FF4A3D`
- Neutrals are chosen, not default — a hair of blue-grey bias toward the ink.
- Speaker/voice colors stay muted cool-slate so the thorn always wins.

## Type
- **Systems voice — mono** (Commit Mono / IBM Plex Mono): labels, data, timestamps,
  badges, code, nav micro-labels. Set small, UPPERCASE, letter-spaced — it reads as
  an *instrument label / receipt*, not a terminal. `tabular-nums` for data.
- **Human voice — grotesque** (Hanken Grotesk): reading text, headings, buttons.
  Deliberately NOT Inter/Space Grotesk.
- **Editorial serif** (Instrument Serif): rare display moments only.
- Self-host fonts (`@font-face`, `font-display:swap`); no CDN.

## The logo system (brand/logos/)
- **05 mask-tile** = favicon / app icon (self-theming via currentColor; best at 16px).
- **08 wordmark** = the masthead (replaces any serif "lanius" text).
- **09 lockup** / **10 shrike-wordmark** = hero / README / splash.
- **01 thorn** = the active-state and "you are here" marker glyph.
- **06 shrike-sticker** = personality / swag, not app chrome.
- Flat marks are `currentColor`; reds carry a `var(--accent, #E5484D)` mono-hook.

## Texture & motion
- Keep a whisper of the flight-recorder texture (subtle scanline/vignette) — a
  hint, not a CRT costume. Retune it cool.
- Motion is precise and sparse; always honor `prefers-reduced-motion`.

## The line that keeps it honest
**The bird lives in the brand — the logo, the icon, the splash. It never becomes a
word the user has to learn.** (See the lanius-voice skill.) Butcher-bird flavor is
a thorn glyph and a mask icon, not "larder" in a tooltip.
