# Keel icon: usage notes

**Mark:** "Heading". A hull seen from above with the bow pointing right, split along the keel line into two staggered halves.
**Construction:** flat fills and pure geometry: straight edges plus circular arcs, the same radius on both halves (bow arcs and the stern taper arcs). No fonts or live text are used anywhere in this pack.

## Files
- `symbol.svg`: the mark only, on a transparent background (Ink + Stone). PNGs: `png/symbol-512.png`, `png/symbol-1024.png`
- `app-icon.svg`: primary tile (Sand, Ink, Paper). `app-icon-alt.svg`: dark tile (Ink, Paper, Stone)
- `png/app-icon-1024.png` / `png/app-icon-alt-1024.png`: full square, no rounding (for the App Store / iOS; the OS applies the mask). `png/app-icon-1024-rounded-preview.png` is a preview only.
- `png/android-adaptive-foreground-432.png` + `png/android-adaptive-background-432.png`: the mark sits inside the 264 px (66 dp) safe circle
- `mono-black.svg`, `mono-white.svg`: one-colour, transparent background
- `favicon-small.svg`: simplified small-size version (larger mark, wider keel gap that lands on the pixel grid). Used for `favicon.ico` (16/32/48), `favicon-16.png`, `favicon-32.png`
- `apple-touch-icon.png`: 180 px, full square
- `contact-sheet.png`: all test renders

## Clear space
Keep clear space around the symbol equal to the height of one hull half (about 1/3 of the mark's height) on all sides. On tiles, the mark already sits inside an ~18% safe area. Don't scale it up.

## Minimum size
- Full mark (`symbol.svg`, `app-icon*.svg`): 48 px / 12 mm minimum.
- Below 48 px use `favicon-small.svg` (it is built to read at 16 px).

## Do
- Keep the bow pointing right (forward).
- Use the supplied colour pairings, or one-colour black or white.
- Use the Ink tile (favicon) for small and browser contexts.

## Don't
- Don't rotate, mirror, or flip the mark, or close the keel gap.
- Don't change the stagger or recolour the halves into saturated hues.
- Don't add gradients, shadows, outlines, or effects.
- Don't put the Paper-on-Sand version on busy or mid-tone backgrounds.

## Before adoption
This mark is original work, but no trademark search has been done. Run a trademark / design-mark search (e.g. WIPO Global Brand Database, USPTO, your local office) in your classes before you adopt it.
