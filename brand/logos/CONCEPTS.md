# lanius — logo concepts

*Lanius* is the shrike genus: the butcher bird. A small grey songbird with a black
bandit mask and a hooked beak that pins its catches on thorns — a larder. The
metaphor is the product: **the thorn** is the cage (work pinned in place), **the
larder** is the camera (everything caught, kept, auditable). Small but fierce is
the whole brand.

## Palette

Shrike-native, one accent, defensible everywhere:

| role | value | use |
|---|---|---|
| ink | `#16181D` | primary mark color on light |
| shrike grey | `#C9CFD6` | illustration midtone (stickers) |
| paper | `#FFFFFF` | die-cut borders, knockouts |
| **thorn red** | `#E5484D` | the one sharp accent — reserved for the thorn / impaled things |
| keyline | `#D9DDE1` | sticker die-cut rim |

Every flat mark is authored in `currentColor`, so it themes itself: set
`color: #16181D` on light, `color: #EDEFF2` on dark. Where red appears, the note
below says what to do for strict monochrome.

## The concepts

### 01-thorn.svg — the thorn, pure
One closed path, `currentColor`. The hero motif reduced to a Vercel-register
glyph: a curved thorn/talon sweeping up-right. Fully flat end of the spectrum;
survives 16px and one color by construction. Deliberately quiet on its own — it
earns its keep in the lockup (09) and as the wordmark's tittle.

### 02-shrike-head.svg — the masked head
Negative-space bandit mask: the head is one color, the mask *is the background
showing through*, and the eye floats inside the band; hooked beak with a real
gape notch. Flat mark with more personality than 01 — the one that says "bird"
without a full illustration. All `currentColor`; the inversion trick means it
works on any ground with zero variants.

### 03-larder-spindle.svg — events, impaled
The flight recorder as a receipt spike: three events skewered on a thorn,
standing on the ledger. This is the most *conceptually* lanius mark — cage and
camera in one gesture — and the only flat mark that carries the red accent.
Monochrome: set the dots to `currentColor`; the silhouette still reads
beads-on-a-spike. Reads clean at 32px.

### 04-l-thorn-monogram.svg — the l-thorn
A lowercase-l stem whose top tapers to a soft point, with one stout rose-thorn
barb. The letterform and the motif are the same object. All `currentColor` on
purpose — this is the strictest-context mark (mono app icon, terminal glyph,
favicon). Sits dead-center on the flat end of the spectrum.

### 05-mask-tile.svg — the mask tile
App-icon register: a rounded square cut through by a slanted mask band, eye
floating in the cut. The band is simultaneously the shrike's mask and a cage
bar. Pure geometry, all `currentColor`, and the strongest 16px survivor of the
set — the knockout is background, so light/dark both just work.

### 06-shrike-sticker.svg — the shrike, at home
The die-cut laptop sticker: a round, bright-eyed shrike perched on a thorned
branch — with one berry already impaled in its larder beside it. Full palette,
ink outlines, white die-cut border with a grey keyline so the cut edge reads on
white too. Charming AND a little dangerous: Lily's sticker, but the berry keeps
the butcher-bird truth in frame. Fixed-palette by design (stickers aren't
themed); the keyline makes it work on dark.

### 07-barbed-badge.svg — small but fierce
Round badge sticker: the masked, hook-beaked head inside a ring of barbed wire —
the shrike's actual preferred habitat, and a wink at the sandbox. Sits between
the flat marks and 06: bolder than a logo, simpler than an illustration. On dark
it becomes a white disc badge, classic sticker-sheet material.

### 08-wordmark.svg — lanius, set in thorns
Hand-drawn geometric monoline (no font dependency — every letter is path data),
lowercase as the product spells itself. The tittle of the *i* is a miniature of
the 01 thorn in red: the mark hides inside the name. Strokes are `currentColor`;
for strict mono, fill the tittle `currentColor` too.

### 09-lockup.svg — mark + wordmark
The 01 thorn at ascender height in red, seated on the text baseline, with the
wordmark beside it — the tittle-thorn rhymes with the big thorn so the lockup
reads as one system. This is the README-header / website-masthead asset.

## Honest ranking

1. **03-larder-spindle** — the deepest concept-to-form match: nothing else in
   the space looks like it, and it *explains the product* (events, pinned,
   recorded) in one glyph. My pick for primary mark.
2. **05-mask-tile** — the best pure-utility mark: instantly an app icon, perfect
   tiny, effortlessly theme-aware. Pair it with 03 (03 as brand mark, 05 as
   product/app tile) and the system is complete.
3. **06-shrike-sticker** — the personality anchor. It won't be the favicon, but
   it's the one people will actually stick on laptops, and it makes the whole
   bird story legible.

Honorable mention: **02** is the best "it's a bird" flat mark but flirts with
generic-mascot territory; **01/04** are clean but quieter — they work best as
supporting glyphs (tittle, list bullets, section markers) rather than the flag.

## Rebuilding the previews

```sh
rsvg-convert -h 512 --keep-aspect-ratio -b '#FFFFFF' \
  -s <(printf 'svg{color:#16181D}') 03-larder-spindle.svg -o out.png   # light
rsvg-convert -h 512 --keep-aspect-ratio -b '#101216' \
  -s <(printf 'svg{color:#EDEFF2}') 03-larder-spindle.svg -o out.png   # dark
```
