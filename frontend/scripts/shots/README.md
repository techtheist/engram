# Documentation screenshots

`scripts/screenshots.sh` (or `bun run shots` in here) regenerates the images
under `.screenshots/` by driving the **browser demo** with headless Chromium.
Every automated shot is therefore the pane a reader can open and click for
themselves at <https://techtheist.github.io/engram/demo/>, showing the
invented Lantern graph rather than this repo's own working notes.

```
scripts/screenshots.sh                        # all of them, into .screenshots/
scripts/screenshots.sh --only review,system   # substring match on shot names
scripts/screenshots.sh --out /tmp/shots       # compare before overwriting
scripts/screenshots.sh --list                 # names and the docs that use them
scripts/screenshots.sh --headed               # watch it work
```

The wrapper installs `playwright` + Chromium on first run and always rebuilds
`dist-demo/` first, so the shots can never show a stale pane.

## The two files

`shots.mjs` is the manifest — one entry per PNG: the viewport, the theme and
layout to seed, the click sequence that stages the shot, and what to clip.
`run.mjs` is the driver: it serves `dist-demo/` on loopback, then per shot
opens a **fresh browser context** (so no shot inherits another's
`sessionStorage`), seeds `engram.theme` / `engram.layout` / `engram.view`,
freezes the clock, navigates, dismisses the first-run history notice, runs the
manifest's `act`, and writes a 2x PNG.

## What keeps the images reproducible

- **A fixed clock.** The demo resolves its ages against "now" at load, so
  `page.clock.setFixedTime` is what stops every rerun from rewriting every
  date. Change `FIXED_NOW` and every dated shot moves together.
- **Deterministic layouts.** Skyline, Archipelago and Orbit seed by
  phyllotaxis and jiggle with an LCG (`src/composables/useLayout.ts`) — no
  `Math.random`, so the same graph lands in the same pixels.
- **No motion.** Transitions and animations are switched off before the
  shutter, and the demo's own corner badge is hidden: it is scaffolding for
  the Pages site, not part of the product.

## What stays hand-made

`engram-alpha-standalone.png`, `engram-alpha-vscode.png` and
`engram-alpha-jetbrains.png` frame the pane inside a window or an IDE. No
headless browser can stage those, and the script never touches them.

## Not in CI

Font rasterization differs between macOS and the Linux runners, so a
regenerated image would never match the committed one — a pixel check would
fail forever and teach nothing. Run it locally when the pane changes shape,
and eyeball the diff.

## Gotchas

- **Menu rows are clicked by dispatch.** Chromium's scroll-into-view never
  settles for an element inside the gear menu's capped, scrolling body, which
  hangs a real click on the lower rows.
- **`engram.view` only restores `feed`.** The history screen has to be reached
  through the screen switch (see the layout store).
- A shot whose `clip` is a two-selector array captures the union of both
  boxes, clamped to the viewport — that is how a long settings section is
  framed from its heading down to a chosen card.
