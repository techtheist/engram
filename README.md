# Engram Alpha

[![Backend](https://github.com/techtheist/engram/actions/workflows/backend.yml/badge.svg)](https://github.com/techtheist/engram/actions/workflows/backend.yml)
[![Frontend](https://github.com/techtheist/engram/actions/workflows/frontend.yml/badge.svg)](https://github.com/techtheist/engram/actions/workflows/frontend.yml)
[![JetBrains plugin](https://github.com/techtheist/engram/actions/workflows/jetbrains.yml/badge.svg)](https://github.com/techtheist/engram/actions/workflows/jetbrains.yml)
[![VSCode extension](https://github.com/techtheist/engram/actions/workflows/vscode.yml/badge.svg)](https://github.com/techtheist/engram/actions/workflows/vscode.yml) \
[![JetBrains Marketplace](https://img.shields.io/jetbrains/plugin/v/32654-engram)](https://plugins.jetbrains.com/plugin/32654-engram)
[![Downloads](https://img.shields.io/jetbrains/plugin/d/32654-engram.svg)](https://plugins.jetbrains.com/plugin/32654-engram)
[![VS Marketplace](https://vsmarketplacebadges.dev/version/techtheist.engram-alpha.svg?label=VS%20Marketplace)](https://marketplace.visualstudio.com/items?itemName=techtheist.engram-alpha)
[![Open VSX](https://img.shields.io/open-vsx/v/techtheist/engram-alpha?label=Open%20VSX)](https://open-vsx.org/extension/techtheist/engram-alpha)
[![Downloads](https://img.shields.io/open-vsx/dt/techtheist/engram-alpha)](https://open-vsx.org/extension/techtheist/engram-alpha)

> The most powerful and feature-rich inspectable long-term graph memory for software development with AI agents — built on reproducible research.

Engram is the **reasoning and decision layer** for AI coding assistants: why
we chose this, what bit us, what's still open — kept as a graph you can see,
edit, and own. It lives in a file inside your repo, every model that reads it
runs on your machine, and one core serves every assistant you use.

**[Open the live demo →](https://techtheist.github.io/engram/demo/)** — the
real pane, in your browser, over an invented project's memory. Nothing to
install. Your edits stay in the tab.

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

<details>
<summary><b>The timeline feed</b> <i>(click to expand)</i></summary>
<br>

![The Feed screen: the same memory as a scrollable timeline — the centered card opens its full story, with the judgment bar at the bottom](.screenshots/layout-feed.png)
</details>

<details>
<summary><b>Why a graph, not a notes file</b> <i>(click to expand)</i></summary>

A flat memory file is whole below about forty notes and overtaken by
retrieval after that. Engram's graph is *active*: superseded knowledge is
archived behind a `replaces` edge instead of silently contradicting the new
canon, look-alike claims get flagged and judged, contradictions become
visible `conflicts-with` edges, deliberately removed knowledge leaves a
Tombstone so nobody re-learns it, and trust fades on scratch that never gets
re-confirmed while pinned decisions never fade at all. The payoff shows up
the second time something goes wrong: the graph already holds the
**Problem**, the **Resolution** that answered it, and the **Caution** that
would have prevented it.

- **Local-first.** Your memory is a file inside your repo. Embeddings and
  every scan run on your machine: no cloud, no keys, fully offline. Portable
  via JSON export, not a binary blob.
- **One memory, every agent.** The core speaks MCP, so **Claude Code, Codex
  (CLI and desktop app), Gemini CLI, OpenCode, Kilo, Google Antigravity, Bob
  (IDE and Shell), Windsurf, and Devin CLI** share the same per-repo graph.
  A decision captured by one assistant is recalled by the next.
- **Graph-first.** The graph is the product surface, not hidden plumbing.
  Reviewing, judging, and repairing memory all happen in the pane — in the
  browser, in JetBrains IDEs, or in VS Code.

Every screenshot on this page is Engram's own graph — the project is built by
dogfooding it.

</details>

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh
```

The installer fetches the binary and nothing else. From your project's root:

```sh
engram-alpha setup    # wires the assistants it finds (--cli names the ones you want)
engram-alpha serve    # starts the core; the pane is at http://127.0.0.1:8787
```

<details>
<summary><b>Other ways to install</b> <i>(IDE plugins, the Claude Code plugin, Windows, per-assistant wiring — click to expand)</i></summary>

Or skip the browser and open the pane inside your IDE with the
[JetBrains plugin](https://plugins.jetbrains.com/plugin/32654-engram) or the
[VS Code extension](https://marketplace.visualstudio.com/items?itemName=techtheist.engram-alpha).
Claude Code users can install everything as a plugin:
`/plugin marketplace add techtheist/engram`. Windows, per-assistant wiring,
and updating: [Getting started](./docs/getting-started.md).

</details>

## What you get

- **A graph you can read and edit** — three layouts, tags and filters,
  full by-hand editing, and a scrollable timeline of the same memory with
  version markers and judgment actions on the card. Hard delete is
  deliberately user-only. → [The pane](./docs/pane.md)
- **An assistant that starts briefed** — a session-start digest of conflicts
  to judge, open work, and standing decisions; mid-session hybrid search
  where every hit carries its conflicts and supersessions first.
  → [Recall & capture](./docs/recall-and-capture.md)
- **Silent capture, accountable review** — writes are quiet, and every one
  comes back checked: duplicates merge instead of piling up, and writes near
  superseded, conflicted, or tombstoned knowledge are warned. The Review
  drawer is where you approve what you vouch for.
  → [Recall & capture](./docs/recall-and-capture.md)
- **A memory that argues back** — a local NLI model checks claims against
  your canon with receipts, sweeps the graph for hidden conflicts and
  duplicates, and queues look-alike pairs for judgment. Models nominate;
  you (or your assistant) judge. → [Conflicts & Checkup](./docs/conflicts-and-checkup.md)
- **Retrieval that knows its own confidence** — a benchmark-calibrated floor
  trims weak hits, and every reply carries a `strong` / `weak` / `none`
  verdict so your assistant admits silence instead of inventing memory.
  Search takes time windows too (`after` / `before`, or a recorded
  version). → [Measured, not promised](#measured-not-promised)
- **Trust that stays honest** — computed live from deliberate acts only:
  time doesn't validate, retrieval doesn't validate, and stable knowledge
  falls only to judged evidence. Pins are yours alone. → [Trust & decay](./docs/trust.md)
- **Every change on the record** — an append-only audit journal with
  before/after values and session attribution, and an optional sealed
  recording of your assistant sessions that search can fall through to.
  → [The pane](./docs/pane.md)
- **Memory that tracks the code** — nodes point at code; refs that stop
  resolving badge their nodes as drifted, with a repair-or-retire contract.
  → [Recall & capture](./docs/recall-and-capture.md)
- **A model you can reshape** — the ontology, the trust and decay numbers,
  custom fields, and the brief are per-graph configuration edited in one
  Settings drawer, with curated presets. → [Customization](./docs/customization.md)
- **One memory across your projects** — one core process, a machine
  registry, and a home graph for knowledge that was never project-scoped.
  → [Multi-project memory](./docs/multi-project.md)
- **Storage and models you choose** — graphs live on a single
  self-describing [TepinDB](https://github.com/tepindb/tepindb) file (SQLite
  graphs migrate themselves, at-rest encryption is a switch); embeddings,
  reranker, and NLI are swappable from the pane.
  → [Storage](./docs/storage.md) · [Local models](./docs/models.md)

<details>
<summary><b>The memory model</b> <i>(node types, edge verbs, capture intensities — click to expand)</i></summary>

Nine node types — Principle, Decision, Caution, Problem, Resolution,
Insight, Intent, Anchor, Tombstone — and seven edge verbs that read as
sentences: a Decision **because** a Principle, a Resolution **answers** a
Problem, the newer **replaces** the older, two claims **conflict-with** each
other. Three capture intensities (`relaxed` / `normal` / `aggressive`) set
how much your assistant writes. It's the shipped default, and the one most
projects should keep — but every part of it is
[yours to reshape](./docs/customization.md). → [The memory model](./docs/memory-model.md)

</details>

## Measured, not promised

Engram ships its own offline evaluation harness — an invented-subject corpus
nothing can answer from pretraining, substring grading with no LLM judge,
seeded and reproducible — and since 0.8.0 the retrieval defaults are outputs
of benchmark runs published next to the code.

- **~9× the recall per token of a conventional vector stack.** At 1,500
  realistic notes, Engram answers from ~300 delivered tokens per query what
  pure-vector RAG needs ~2,700 for, at equal recall@5 and ahead on the
  phrasing-weighted headline (0.93 vs 0.91).
- **Where a maintained memory file loses, measured.** A 3,000-token
  CLAUDE.md is whole below ~40 notes and overtaken by retrieval at 100; an
  unusually diligent 30,000-token one wins up to ~200 notes, then falls off
  its capacity cliff by 500.
- **Attention metrics, not just recall.** Every table pairs recall with
  `focus` (how much of the delivery was the answer) and `noise` (how much
  was false positives). A full-context dump scores recall 1.00 and focus
  0.004 on the same run — *present* and *readable* are different claims.
- **Supersession works, with an ablation.** On re-decided ADR-shaped
  history the shipped stack delivers **zero** retired generations; the same
  store without supersession delivers one on 88% of questions.
- **Tested on an external corpus.** [LongMemEval](./eval/LONGMEMEVAL.md)
  (real multi-session chats, MIT) runs as-is under a chat ontology defined
  purely as data: rag's recall@1 matched at 8% of its delivered tokens, and
  the 30 never-answerable questions drew **zero** confident answers.
- **Rejected mechanisms stay on the record** — spreading activation, a
  deciding cross-encoder, deeper reranking — because a benchmark that only
  reports wins is an advertisement.

Method, tables, and the honest caveats: [`eval/`](./eval/README.md).

<details>
<summary><b>Documentation, security, and status</b> <i>(click to expand)</i></summary>

All user documentation lives in [`docs/`](./docs/README.md) — install and
wiring, the memory model, trust, the pane, conflicts, multi-project memory,
the [runtime architecture](./docs/runtime.md), storage, local models, and
troubleshooting (`engram-alpha doctor` diagnoses the whole chain,
`engram-alpha status` shows what's running, `engram-alpha stop` halts it).

Security posture: [`SECURITY.md`](./SECURITY.md). The roadmap lives in the
project's own memory graph — dogfooding is the spec. **Status:** early
development, heavily dogfooded, benchmark-driven — retrieval changes cite a
measured run or they don't ship.

</details>

<details>
<summary><b>Stack</b> <i>(Rust core, Vue pane, IDE hosts — click to expand)</i></summary>

- **Core — Rust.** One `engram-alpha` binary: the engine, the MCP server
  (`rmcp`, stdio and streamable HTTP), the HTTP API (`axum`), and the CLI.
  Storage on [TepinDB](https://github.com/tepindb/tepindb) (SQLite +
  `sqlite-vec` behind the same trait for older graphs). Local ONNX models
  through `fastembed` — embeddings, cross-encoder reranker, NLI — all
  user-swappable. No LLM ever runs in the core.
- **Pane — Vue 3 + TypeScript.** Vite and Bun, Pinia, Tailwind 4,
  [Vue Flow](https://vueflow.dev/) for the graph. Built once, embedded into
  the binary, and shared by every host below.
- **IDE hosts.** A JetBrains plugin (Kotlin, JCEF) and a VS Code extension
  (TypeScript, Webview) that embed the same pane and talk to the local core.
  A Claude Code plugin carries the MCP wiring and the capture skill.

</details>

## License

[MIT](./LICENSE)
