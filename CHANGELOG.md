# Changelog

Release notes for Engram Alpha. Each release's section below becomes the
body of its GitHub Release (draft-release.yml lifts it automatically).

## v0.7.1 — the timeline release

The graph gets a second screen: your memory as a story you can scroll.

### The timeline feed

The new **Graph / Feed** toggle in the topbar switches the pane to a
vertical feed of node cards — neighbors peeking above and below, the
centered card in focus.

- **Two lenses.** *Timeline* shows everything in chronological order, with
  version markers wherever the project's working version moved on. *Review*
  shows only what needs a human eye (provisional / stale / drifted).
- **Read without leaving.** Every card renders its full markdown clipped to
  a fixed preview; the card you stop on opens to its whole body, however
  long. Card positions never shift under you — far-away jump targets always
  land. `j`/`k` and the arrow keys navigate.
- **Judge without leaving.** A bottom action bar carries the side drawer's
  controls for the centered card: Approve / Still true / Pin / Edit /
  Delete.
- **Traverse the graph.** Click any edge chip and the feed jumps to that
  node — even one the current lens filters out — and **← back** walks the
  trail home (session-scoped, forty hops).
- **One "current node" across screens.** Select a node on the canvas and
  the feed opens centered on it; leave the feed and the canvas selects and
  centers the card you were reading.

### MCP

- `set_version` now auto-enables version tracking when it was off — asking
  for a version *is* opting in; the reply says so explicitly. Clearing a
  version never toggles tracking.

### Pane polish

- **New control kit** — the Graph settings drawer (and checkboxes/radios
  app-wide) trade native inputs for a hue rail, steppers, segmented
  controls, and toggle chips.
- **Topbar consistency** — brand chip, ⌘K / Ctrl-K search shortcut with a
  keycap hint, and the Graph/Feed switch, all in one glass row.
- **Accent spines** — node/card accent strips are now background gradients
  that follow rounded corners cleanly, with a per-theme wash
  (full-strength in Engram Purple, subtle in the IDE themes).
- **Zoom-gated glass** — canvas card blur switches off below ~0.7 zoom
  (with hysteresis), keeping big-graph panning smooth; archived cards went
  grayscale instead of translucent.
- **Dynamic-center drawers** — the left and right drawers now push each
  other instead of clipping at the midline.
- **Responsive fixes** — the search bar hides under 530 px and the brand
  chip / project switcher under 400 px (IDE side panels); the burger
  dropdown became opaque, which also fixes drawers opened from it
  positioning off-screen (a CSS containing-block trap: `backdrop-filter`
  on an ancestor hijacks `position: fixed` descendants).
- The search bar is opaque in the feed view — Chromium's `backdrop-filter`
  cannot sample content inside a composited scroller, so glass there read
  as a bug. The graph view keeps its blur.
- The graph-health strip is graph-view-only now.

### Docs

- New [timeline feed](./docs/pane.md#the-timeline-feed) section in the pane
  guide, README updates, and refreshed screenshots.
- This file: release notes live in `CHANGELOG.md` from now on, and the
  release workflow publishes each version's section as the GitHub Release
  body.
