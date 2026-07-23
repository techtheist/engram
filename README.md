# Engram Alpha

[![Backend](https://github.com/techtheist/engram/actions/workflows/backend.yml/badge.svg)](https://github.com/techtheist/engram/actions/workflows/backend.yml)
[![Frontend](https://github.com/techtheist/engram/actions/workflows/frontend.yml/badge.svg)](https://github.com/techtheist/engram/actions/workflows/frontend.yml)
[![JetBrains plugin](https://github.com/techtheist/engram/actions/workflows/jetbrains.yml/badge.svg)](https://github.com/techtheist/engram/actions/workflows/jetbrains.yml)
[![VSCode extension](https://github.com/techtheist/engram/actions/workflows/vscode.yml/badge.svg)](https://github.com/techtheist/engram/actions/workflows/vscode.yml)
[![JetBrains Marketplace](https://img.shields.io/jetbrains/plugin/v/32654-engram)](https://plugins.jetbrains.com/plugin/32654-engram)
[![VS Marketplace](https://vsmarketplacebadges.dev/version/techtheist.engram-alpha.svg?label=VS%20Marketplace)](https://marketplace.visualstudio.com/items?itemName=techtheist.engram-alpha)
[![Open VSX](https://img.shields.io/open-vsx/v/techtheist/engram-alpha?label=Open%20VSX)](https://open-vsx.org/extension/techtheist/engram-alpha)

> Inspectable long-term project memory for AI coding assistants. Local-first, user-owned, graph-first.

![The Engram pane: the live memory graph with the review queue open on the left and the theme & layout menu on the right](.screenshots/engram-alpha-standalone.png)

<details>
<summary><b>Inside JetBrains IDEs</b> <i>(click to expand)</i></summary>
<br>

![Engram tool window in IntelliJ IDEA: the graph updates live while Claude Code works in the terminal below](.screenshots/engram-alpha-jetbrains.png)
</details>

<details>
<summary><b>Inside VS Code</b> <i>(click to expand)</i></summary>
<br>

![Engram pane in VS Code's secondary sidebar: the memory graph fills in while the assistant explains the project](.screenshots/engram-alpha-vscode.png)
</details>

Unlike a flat note pile, Engram's graph is *active*: superseded knowledge is
archived behind a `replaces` edge instead of silently contradicting the new
canon, look-alike claims get flagged and judged, contradictions become
visible `conflicts-with` edges, and trust fades on scratch that never gets
re-confirmed — while stable decisions hold their trust until contradicting
evidence lands, and anything you pin never fades at all. The payoff shows up
the second time something goes wrong: when your assistant meets a problem it
has fought before, the graph already holds the **Problem**, the
**Resolution** that answered it, and the **Caution** that would have
prevented it.

- **Local-first** — your memory is a file inside your repo. Embeddings and
  every scan run on your machine: no cloud, no keys, fully offline. Portable
  via JSON export/import, not a binary blob.
- **Portable across agents** — one local backend serves **Claude Code, Codex
  (CLI and desktop app), Gemini CLI, OpenCode, Kilo, and Google Antigravity**
  over MCP, plus a browser UI. Your agents share one memory: a decision
  captured by Claude is recalled by Codex.
- **Graph-first** — the graph is the product surface, not hidden plumbing.
  Reviewing, judging, and repairing memory all happen in the pane.

Every screenshot on this page is Engram's own graph — the project is built by
dogfooding it.

## Install

From your project's root:

```sh
curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh
```

Then `engram-alpha serve` and open `http://127.0.0.1:8787` — or use the
[JetBrains plugin](https://plugins.jetbrains.com/plugin/32654-engram) / VS
Code extension instead of the browser. Claude Code users can install
everything as a plugin: `/plugin marketplace add techtheist/engram`. Windows,
per-assistant wiring, and updating: [Getting started](./docs/getting-started.md).

## What you get

- **A graph you can read and edit** — the whole graph rendered live, four
  layouts, tags and filters that slice it by concern, and full by-hand
  editing. Hard-delete is deliberately user-only. → [The pane](./docs/pane.md)
- **An assistant that starts briefed** — a session-start digest of conflicts
  to judge, open work, and standing decisions; mid-session recall via hybrid
  search where every hit carries its conflicts and supersessions first.
  → [Recall & capture](./docs/recall-and-capture.md)
- **Silent capture, accountable review** — writes are batched and quiet, and
  every one comes back checked: duplicates merge instead of piling up, and
  writes near superseded or conflicted knowledge get warned. The Review
  drawer is where you approve what you vouch for.
  → [Recall & capture](./docs/recall-and-capture.md)
- **A memory that argues back** — a local NLI model checks claims against
  your canon with receipts, sweeps the graph for hidden conflicts and
  duplicates, and queues look-alike pairs for judgment. Models nominate;
  you (or your assistant) judge. → [Conflicts & Checkup](./docs/conflicts-and-checkup.md)
- **Trust that stays honest** — computed live from deliberate acts only:
  time doesn't validate, retrieval doesn't validate, and stable knowledge
  falls only to judged evidence. Pins are yours alone.
  → [Trust & decay](./docs/trust.md)
- **Every change on the record** — an append-only audit journal with
  before/after values and session attribution, plus per-decision history
  along `replaces` chains. → [The pane](./docs/pane.md)
- **Memory that tracks the code** — nodes point at code; refs that stop
  resolving badge their nodes as drifted, with a repair-or-retire contract,
  and an optional hook attaches a file's memory the moment your assistant
  reads it. → [Recall & capture](./docs/recall-and-capture.md)
- **A model you can reshape** — the ontology, the trust and decay numbers,
  and the brief are all per-graph configuration, edited in one Settings
  drawer: rename or replace types and verbs (the engine keys on roles, not
  names), tune how fast memory fades, pick from curated presets, and track a
  working version per note. → [Customization](./docs/customization.md)
- **One memory across your projects** — one core process, one pane, a
  machine registry, and a home graph for knowledge that was never
  project-scoped. Cross-project reads with local-canon priority; promotion
  by your approval. → [Multi-project memory](./docs/multi-project.md)
- **Storage that outgrows SQLite** — graphs live on SQLite or on
  [TepinDB](https://github.com/tepindb/tepindb)'s single self-describing
  `.tepin` file, migratable in one command with the audit journal intact —
  and inspectable with `npx tepindb` even while Engram is running.
  → [Storage & TepinDB](./docs/storage.md)
- **Local models you choose** — embeddings, reranker, and NLI run offline
  and are swappable from the pane (presets or any compatible ONNX export by
  URL); an embedding swap re-embeds your graphs in one guarded pass.
  → [Local models](./docs/models.md)

## The memory model

Eight node types (Principle, Decision, Caution, Problem, Resolution,
Insight, Intent, Anchor) and seven edge verbs that read as sentences — a
Decision **because** a Principle, a Resolution **answers** a Problem, the
newer **replaces** the older, two claims **conflict-with** each other. Three
capture intensities (`relaxed` / `normal` / `aggressive`) set how much your
assistant writes. It's the shipped default, and the one most projects should
keep — but every part of it is [yours to reshape](./docs/customization.md).
→ [The memory model](./docs/memory-model.md)

## Documentation

All user documentation lives in [`docs/`](./docs/README.md) — install and
wiring, the memory model, trust, the pane, conflicts, multi-project memory,
storage, local models, and troubleshooting (`engram-alpha doctor` diagnoses
the whole chain; `engram-alpha stop` halts everything cleanly).

Security posture: [`SECURITY.md`](./SECURITY.md). Full spec and roadmap:
[`PLAN.md`](./PLAN.md). **Status:** early development, heavily dogfooded.

## Stack

Rust core (`rmcp` — stdio + daemon-hosted streamable HTTP, `rusqlite` +
`sqlite-vec` with a [TepinDB](https://github.com/tepindb/tepindb) backend
behind the same storage trait, `fastembed` — local ONNX embeddings +
reranker + NLI, all user-swappable; no LLM ever runs in the daemon) · Vue 3
+ TypeScript + Vue Flow · JetBrains (JCEF) & VSCode (Webview) wrappers.

## License

[MIT](./LICENSE)
