# The browser demo

The pane, minus the daemon. `ENGRAM_DEMO=1 vite build` swaps one module and
the whole app runs against a mock backend that lives in the visitor's tab —
that build is what gets published to
<https://techtheist.github.io/engram/demo/> by `.github/workflows/pages.yml`.

```
bun run build:demo          # → frontend/dist-demo/
ENGRAM_DEMO=1 bun run dev   # the demo with hot reload
```

The demo is also the stage for the documentation screenshots: everything in
`.screenshots/` except the standalone, VS Code and JetBrains shots is
generated from this build by `scripts/screenshots.sh`, so the docs show a
pane anyone can click through — and never this repo's own working notes.

## How the swap works

`vite.config.ts` aliases two specifiers when `ENGRAM_DEMO=1`:

| specifier | normal build | demo build |
| --- | --- | --- |
| `@/services/api` | `src/services/http.ts` (fetch + SSE) | `demo/api.ts` (mock daemon) |
| `@/services/hostChrome` | `null` | `demo/chrome.ts` (the DEMO badge) |

Nothing else in `src/` knows a demo exists, and neither bundle contains the
other's code — the daemon's pane ships no fixture data, the demo ships no
HTTP client.

## What keeps it honest

`demo/api.ts` is annotated `EngramApi`, a type derived from the real HTTP
client (`src/types/api.ts`). Add an endpoint to the client and the demo stops
type-checking until it answers for it too, so the demo cannot quietly become
a different product. CI runs that check before publishing.

## What is real and what is not

**Real** — `demo/engine.ts` is a small memory engine: CRUD on nodes and
edges, approve / pin / reconfirm, supersession archiving its older
generation, suspect judging, the decay preview, the audit journal and its
undo previews (applying an undo needs the daemon), ontology
renames as bulk retypes, import/export, and a generated session brief. Trust
is a direct port of `engram_core::policy::trust`, so the numbers move for the
reasons the daemon would move them. State persists to `sessionStorage`.

**Keyword search** — the memory search box runs a plain keyword match over
the live graph (`demo/match.ts`, shared with the history search): current
notes only, terms scored by where they land (title over tags over body), with
snippets built the way the daemon builds them. Notes you add are findable at
once. It is not semantic recall, and the banner says so — dressing a keyword
match up as recall is the one lie a memory tool cannot afford.

**Pre-recorded** — claim checks and the Checkup sweep verdicts
(`demo/data/canned.ts`). Those are the local cortex's work: an embedding
encoder, a cross-encoder reranker and an NLI model. None of that is going in
a browser tab, so the demo says which results are canned.

**Refused** — anything that needs a filesystem: registering a project,
installing the capture skill, swapping a model. They throw a plain-language
error the panels already know how to render.

## The graph

`demo/data/lantern.ts` — the invented memory of *Lantern*, a fictional
local-first reading app, ~49 notes across every type with two live conflicts,
two unjudged suspects, a three-generation supersession chain, stale notes and
drifted code refs. Ages are relative days resolved at load, so the demo never
looks dated.

The notes tagged `memory-practice` are the exception to the fiction: they
state Engram's load-bearing rules (research-first, models nominate and people
judge, trust reads deliberate acts, retired means retired, conflicts are
surfaced never auto-resolved). Those hold across releases — which is why they
are safe to put in a demo that nobody will re-check for a year.

## Embedded in the docs landing page

`?embed` (`demo/embed.ts`) is the same build running inside the
documentation site's landing page (`website/`), driven by the page's scroll.
Two things change:

- **Storage is in-memory.** The frame shares an origin with the full demo and
  the docs, so a tour that switches themes and screens would otherwise leave
  them in the visitor's real `localStorage`. Every embedded load starts from
  the shipped graph, and nothing it does outlives the frame.
- **The tour bridge runs** (`demo/tour.ts`, started from `DemoChrome.vue`,
  which hides its badge). The page posts `{ type: 'engram-tour', chapter, t }`;
  each chapter maps to a scene (a screen, a selection, an open panel, a task
  moved, a session opened), applied through the pane's own stores. Scenes are
  states, so scrolling back walks the pane back, and a virtual cursor plays
  the clicks. `{ theme }` follows the page's skin. The embedded pane is
  watch-only (no pointer events): the full demo is a link away.
