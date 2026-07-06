# Engram Alpha — Plan & Spec

> **Name (locked 2026-07-06).** The product is **"Engram Alpha"** — *Alpha is part of the name, not a version tag*. "Engram" = a physical memory trace stored in the brain — literally what this project stores; the qualifier exists because plain "engram" is heavily contested in the AI-memory space (openengram.ai, jamjet-labs/engram, lessthanno/engram-agent). Existing artifacts keep their short names — the `engram` binary, this repo, the JetBrains plugin — while the **VSCode extension publishes as `engram-alpha`**.

A graph-based, durable, **inspectable long-term project memory** for AI coding assistants (Claude Code first). Engram sits beside your assistant as a memory it can read from and write to — and that you can see, edit, and own. The graph is the product surface, not hidden plumbing.

---

## 0. Locked decisions

| Decision | Choice | Notes |
|---|---|---|
| **Goal** | Open-source / portfolio | Public repo; optimize for docs, DX, shareability. Portfolio-first; if real users show up, it gets supported as a real project. |
| **Name** | **Engram Alpha** — "Alpha" is part of the name, **not** a version | Plain "engram" is taken by three other AI-memory projects. Short `engram` stays for the binary, repo, and JetBrains plugin; VSCode extension = `engram-alpha`. |
| **License** | **MIT** | Maximally adoptable. |
| **Memory scope** | **Per-repo only** (v1) | One graph per project. Cross-project/shared layer is a future option. |
| **Build first** | **Browser standalone** | Fastest loop; doubles as Claude Desktop support. IDE wrappers later. |
| **Embeddings** | **Local-only** | `fastembed`/ONNX. No remote, no keys, no cost. Matches local-first. |
| **Backend language** | **Rust (locked)** | `rmcp`, `rusqlite`, `sqlite-vec`, `fastembed`. |
| **Capture (v1)** | **Cooperative (MCP) first** | Claude writes via MCP tools + a skill. Hooks (ambient) come later. |
| **Conflict handling (v1)** | **Passive, surfaced via RAG** | Conflicts shown in the graph pane; awareness comes through retrieval. Proactive mid-session push (MCP channels) is a planned later feature. |
| **Storage** | **`.engram/` in repo, git-ignored** | Per-repo, personal/local. Share later via JSON export, not the binary DB. |
| **Live updates** | **SSE** (axum → pane) | One-way server→client stream. |
| **Deletion** | **Supersede auto; hard-delete user-only** | History preserved by default; user may hard-delete (edges cascade). |
| **Backend run (v1)** | **`engram serve` daemon** | One local process: HTTP + MCP + SSE. |
| **Bootstrap / export** | **Empty start; JSON export in v1** | Grow organically; portable, diffable JSON. |
| **Secrets** | **Skill rule + redaction pass** | Defense in depth before write. |
| **Validation** | **Dogfood on Aggressive mode** | Build Engram using Engram (capture = Aggressive) to surface gaps fast. |
| **Engineering style** | **Minimalist; few comments** | Self-documenting names + deps' public docs over inline comments. |
| **Merge identity** | **Claude searches, then updates** | `add_note` creates only if `search` finds no match. |
| **Concurrency** | **SQLite WAL, serialized writes** | Last-write-wins; fine for local single-user. |
| **Daemon scope** | **One `engram serve` per repo** | Started in the repo dir, bound to its `.engram/`. |
| **MCP transport** | **Streamable HTTP (not stdio)** | Daemon hosts MCP over HTTP; Claude connects by URL. Daemon must be running first (the browser pane needs it anyway). One process owns the DB + SSE. |
| **Session id** | **From the MCP transport** | Reuse the MCP session id; daemon mints a fallback only if a client omits it. |
| **Distribution** | **Core loop: local binary; v1: thin plugin + skill-driven fetch** | Marketplace plugin ships skill/commands/MCP config; the skill detects a missing backend and downloads a checksum-verified binary from GitHub Releases (with user consent). Full CI/installer (`cargo-dist`) + code-signing after the core loop works. |

---

## 1. Vision

When you work with an AI assistant on a long-lived project, the valuable context — *why* you chose something, *what* bit you last time, *how* a gnarly bug got solved, *what* you prefer — evaporates between sessions and overflows any single context window. Engram captures that as a **structured, durable, user-owned graph** that the assistant consults and contributes to over the life of the project.

### What makes it different (the wedge)
The graph-memory space is crowded at the consumer-companion level (Nomi Mind Map, Kissable) and the dev-infra level (mem0, Zep/Graphiti, Cognee). Engram's distinct bets:

1. **An *active* graph, not a passive note pile.** The `replaces` and `conflicts-with` edges let Engram surface staleness and contradictions — *"you decided X, but this new insight conflicts with it."* Almost nobody does this.
2. **Durability-aware memory.** Every node knows whether it's stable (preferences), episodic (problems/insights), or volatile (implementation state). This is what stops the graph rotting — the core failure mode of every existing tool ("saves too much of the wrong information").
3. **Graph-first UI.** The user sees and edits the canon directly, in their own pane, not buried in a chat. Competitors can bolt on a graph view; Engram is built around it.
4. **Local-first and portable.** Your memory is a local SQLite graph you own; the same backend serves any MCP client (Claude Code, Claude Desktop, browser).

---

## 1A. Competitive positioning (mid-2026 scan)

"Knowledge graph for AI coding" is crowded, but it splits into two layers — and Engram sits in the under-served one:

- **Code-structure graphs** — Graphify (~58K★), CodeGraph, Event Horizon, getmycelium. Auto-derive files/imports/architecture from the codebase. Regenerable; go stale with the code. **Engram does not compete here — it is complementary.** The two can run side by side, and a code-structure graph could later *feed* Engram's Anchors. No contradiction.
- **Reasoning / decision memory** — the durable "why / what was decided / what bit us" layer. The closest concept is **Cairn** (`smcady/Cairn`): a reasoning graph with contradictions via Claude hooks + MCP — but it has **no UI and ~3★ (no traction)**. The MCP "memory visualizers" (memory-visualizer, MemoryMesh, MemoryGraph) have graph UIs but are **standalone browser viewers**, not curated reasoning memory.

**Validated wedge:** the *combination* nobody ships — durable reasoning/decision memory **+** a live, **editable graph pane embedded in the IDE** (dual view beside Claude) **+** conflict/supersede surfacing **+** local-first. JetBrains has **zero** memory-graph plugins at all.

**Honest caveat:** low star counts across the reasoning-memory niche mean *demand is unproven/early*. Fine for the portfolio / dogfood goal; a flag if this ever turns commercial.

**Name collisions (2026-07-06 scan):** four other AI-memory projects ship under "engram" — **Gentleman-Programming/engram** (4.9K★, Go, memory for AI coding agents: the most direct competitor *and* proof of demand for this niche, but flat FTS5 observations — no embeddings, no graph ontology, no trust/decay, no graph UI, global DB; its Claude plugin installs as `engram` from its own marketplace repo), **openengram.ai** (agent-memory infra, TS SDK, dashboard with graph viz, managed cloud coming), **jamjet-labs/engram** (Python multi-tenant memory library), **lessthanno/engram-agent** (behavioral habit coach). None ships an IDE-embedded editable per-repo reasoning graph; hence the product name **Engram Alpha** (see §0). Also scanned: **MemPalace** (57K★) — verbatim conversation memory, the opposite philosophy, no UI. The Gentleman-Programming traction softens the "demand unproven" caveat above: appetite for coding-agent memory is real; unproven is only the graph-pane wedge.

---

## 2. What it is

A **local core service** plus thin clients:

- **Core backend (Rust)** — owns the graph + RAG + librarian, and exposes two interfaces:
  - **HTTP API** — for the Vue frontend(s).
  - **stdio MCP server** — for Claude Code / Claude Desktop / any MCP client.
- **Core frontend (Vue + TS)** — single-page app rendering the graph. Browser standalone for dev; embedded into IDEs later.
- **VSCode wrapper** *(later)* — thin extension hosting the frontend in a Webview; manages backend lifecycle + config.
- **JetBrains wrapper** *(later)* — thin Kotlin plugin hosting the frontend in JCEF.
- **Standalone / browser mode** *(first)* — backend + frontend without an IDE; works with Claude Desktop and CLI Claude Code.

One backend, many faces.

---

## 3. Integration with Claude Code

Claude Code exposes three integration channels (confirmed against current docs). There is **no API to render custom UI inside Claude Code's own panel** — fine, Engram has its own pane.

| Channel | Direction | Use in Engram | v1? |
|---|---|---|---|
| **MCP server** (streamable HTTP) | Claude → Engram | Claude reads/writes the graph: `search`, `get_node`, `add_note`, `link`, `traverse`, `list_open`. Served over HTTP by the running `engram serve` daemon (not a stdio subprocess), so one process owns the DB and SSE. Resources let the user `@`-mention a node. A shipped **skill** tells Claude when to use them. | **Yes (primary)** |
| **Hooks** (`PostToolUse`, `UserPromptSubmit`, `Stop`) | Claude → Engram | Passive, guaranteed capture, fed to the librarian. | Later |
| **SDK / CLI** (`claude -p --output-format stream-json`, Agent SDK) | Engram → Claude | The pane can launch/observe Claude sessions. | Later |

**v1 = cooperative.** Claude *chooses* to save via MCP, guided by the skill. Ambient hooks (guaranteed but noisy, need the librarian) come after.

---

## 4. Graph ontology

The schema's organizing principle is **durability**, not just type. Names are abstract and "thought-shaped" so the assistant reaches for them naturally.

### 4.1 Node types (7 + 1 anchor)

| Node | Durability | The thought it captures |
|---|---|---|
| **Principle** | stable | "this is how I like things / what I value" (preferences, conventions, taste) |
| **Decision** | stable, supersedable | "we chose this, for a reason" (ADR-like) |
| **Caution** | stable | "watch out — this bites" (constraints, gotchas, future warnings, specs-as-rules) |
| **Problem** | episodic | "this was hard / went wrong" (the micro bug-tracker) |
| **Resolution** | episodic | "here's how it got solved" |
| **Insight** | episodic | "I realized something non-obvious" |
| **Intent** | volatile | "do this later" (TODO / deferred work) |
| **Anchor** | semi-stable | "what this is *about*" — a code subject (module/system), by **semantic identity, never line numbers** |

**Durability classes:**
- **stable** — persists indefinitely; changes only via explicit `replaces`.
- **episodic** — timestamped history; retrieved by relevance; ages but never "wrong".
- **volatile** — short TTL; must be re-validated or assumed stale. *Not stored unless explicitly asked.*

> Note: there is no separate `Concept` or `Spec` node — glossary/spec material folds into `Caution` (rules/invariants) or `Principle`/`Decision` as appropriate. Keeping the set at 7 is deliberate.

### 4.2 Node properties

```
id              uuid
type            one of the 8 node types
title           short label
body            free text
durability      stable | episodic | volatile
source          user | claude            (trust signal)
session_id      originating session
created_at      timestamp
valid_from      timestamp
valid_until     timestamp | null         (temporal validity / supersession)
status          (Problem/Intent) open | resolved | obsolete
confidence      0–1
embedding       vector                   (for RAG)
code_refs       loose semantic refs (module/responsibility, not line numbers)
last_seen_at    last retrieval/reference time   (decay input; stamped Phase 0, used Phase 1)
reconfirmed_at  last cross-session re-assertion / approval (trust input; used Phase 1)
```

### 4.3 Edge types

**Principle: a triple reads as a plain English sentence.** That's what makes edges friendly for the assistant — it's completing a sentence, not picking a foreign key.

| Edge | Reads as | Meaning |
|---|---|---|
| **about** | *Insight* **about** *Anchor* | what a node concerns (topic anchor) |
| **because** | *Decision* **because** *Principle* | the reason / justification behind it |
| **answers** | *Resolution* **answers** *Problem* | resolves or addresses |
| **builds-on** | *Insight* **builds-on** *Insight* | elaboration chains ("notes on notes") |
| **replaces** | *Decision* **replaces** *Decision* | temporal supersession (old kept, marked) |
| **conflicts-with** | *Insight* **conflicts-with** *Decision* | the contradiction surface |
| **needs** *(optional)* | *Intent* **needs** *Decision* | dependency / blocker |

**No generic `relates_to`.** If the assistant can't complete the sentence with a real verb, the link isn't worth making. `replaces` and `conflicts-with` are the high-value edges — they make the graph *active*.

### 4.4 Edge properties

```
type            one of the edge verbs
from / to       directional node ids (subject → object)
source          user | claude
created_at      timestamp
confidence      0–1
strength        0–1                      (weight for retrieval/traversal ranking)
note            short justification text
valid_from      timestamp
valid_until      timestamp | null        (edges can expire)
status          active | resolved | dismissed   (esp. conflicts-with / needs)
```

Payoffs: `status` on `conflicts-with`/`needs` makes the graph a **live worklist**; `source` + `confidence` let retrieval prefer user-asserted links and decay speculative ones.

---

## 5. Write-policy modes

A user-facing knob controlling **when the assistant writes to the graph**. Default: **relaxed**. (All modes are cooperative/MCP in v1.)

| Mode | Writes | Rationale |
|---|---|---|
| **Relaxed** | Principle, major Decision, Problem+Resolution **only when genuinely complex**. Auto-creates Anchors as attachment points. **Never volatile impl details.** | Capture durable, high-value knowledge; avoid stale noise. |
| **Normal** | + Caution, selective Insight, finer Decisions. | Balanced. |
| **Aggressive** | + Intent/TODO, all Insights, proactive Cautions. | Maximum capture; relies hard on the librarian. |

The user can also **reclassify** any node — promote durability, upgrade an episodic Insight to a stable Caution, etc. The graph people trust is one they can correct.

**Shipping form:** the three modes ship as **three self-contained `SKILL.md` variants** in `skills/engram/{aggressive,normal,relaxed}/` — the user installs exactly one. **Relaxed is the recommended default install**; this repo dogfoods **Aggressive** (where Engram acts as the spine of the project's decision history, complementing — never mirroring — Claude Code's own memory). The future installer (`.sh` / PowerShell from the `cargo-dist` stage, §8) **must install the chosen skill variant** (relaxed unless the user picks otherwise).

---

## 6. The librarian (quality layer)

The hardest problem and the real moat: **deciding what's worth a node.** In v1 (cooperative), Claude is the first filter via the skill; the librarian is a lighter dedupe/classify pass. When ambient hooks land, the librarian becomes essential.

- Runs **async** (never inline in the assistant's turn).
- Decides: durable signal or transient noise? Duplicate / refine / contradict an existing node? Which node + edge type?
- On contradiction, creates a `conflicts-with` edge rather than overwriting — surfacing it for the user.
- **Resolved:** the "worth-a-node" threshold is **Claude's judgment**, guided by the skill — no objective cost-gate. Details in §6A.

---

## 6A. Resolved behaviors — capture · anchors · retrieval · skill

### Capture & librarian (v1, cooperative)
- **Worth-a-node:** Claude's judgment via the skill (describes what's durable / non-obvious enough). No objective cost-signal gate.
- **Gatekeeper:** Claude alone, via the skill. **No separate librarian LLM pass in v1** — dedupe/classify happen inline in Claude's reflection. An async librarian is deferred to when ambient hooks land.
- **Duplicates:** default to **update / merge** the existing node and bump its confidence (keep the graph compact and current), rather than chaining new nodes. **Search-before-write is a recommendation, not a hard gate:** it matters once the graph is large enough that duplicates are likely (or the topic is plausibly already covered); on a small graph Claude may write directly. **Safety net (because writes are silent + batched):** `add_note` itself runs a same-type embedding similarity pre-check — free, since it must embed for storage anyway — and on a near-duplicate (cosine > θ) returns `{ matched, created: false }` so Claude redirects to `update_node` instead of silently creating a dupe when it forgets to search.
- **Trust & decay:** Claude-created nodes start **provisional** (lower confidence). Promoted to trusted by **reconfirmation across sessions or explicit user approval**; stale provisional nodes **decay out**. User-created nodes are trusted from the start. **Provisional is derived**, not a stored state: `source = 'claude' AND confidence < τ`. The schema carries `last_seen_at` / `reconfirmed_at` (stamped from Phase 0, decay/promotion logic in Phase 1) so the staleness clock exists before the logic does — no later migration on a populated DB.

### Anchors
- **Identity:** primary = **free-text responsibility label** (e.g. "auth flow"); **plus optional file binding** via `code_refs` when a node clearly maps to specific files.
- **Granularity:** **flexible — Claude decides** per note. (Tradeoff: inconsistent granularity makes fuzzy matching harder, so the flagging path below matters more.)
- **Creation:** Claude **auto-creates or reuses** anchors when saving a node.
- **Refactor drift:** on a new session, **fuzzy re-match** anchors by label/responsibility; **flag low-confidence matches** for user review rather than silently rebinding or breaking.

### Retrieval (RAG) — *implemented*
- **Trigger:** at **session start** via the `brief` tool (a budgeted digest of conflicts, open work, principles, decisions, cautions) **+ on-demand** `search` during work via the skill.
- **Returned:** **matches + their 1-hop neighbors**, `conflicts-with` and `replaces` **first**, capped (5/hit). This is how conflicts and stale decisions surface passively.
- **Ranking:** **hybrid** (keyword FTS5 with OR-recall + vector cosine), blended and **multiplied** by a trust factor (stable durability, high confidence, user-sourced) — trust modulates relevance, never dominates it.
- **Budget:** **small** — top ~5–8 nodes on search; the brief has a hard character cap (default ~6k chars).

### Skill write behavior
- **Timing:** **batch at natural stopping points** (task complete, end of turn) — never mid-flow.
- **Confirmation:** **fully silent** — Claude writes without asking or reporting. **Transparency lives in the graph pane, not the chat** — the user reviews/prunes there. (Makes the pane's review UX important.)
- **Invocation:** **both** — model-invoked automatically (ambient memory) **and** a manual trigger (`/engram`) for explicit "save this" / "recall X" moments.
- **Default mode:** **Relaxed.**

---

## 6B. Operational decisions — storage · sync · lifecycle · safety

### Storage
- **Per-repo graph DB at `.engram/graph.db`**, inside the repo, **git-ignored** (personal/local by default). Tooling adds `.engram/` to `.gitignore`.
- Team sharing (later) happens via **JSON export**, never by committing the binary SQLite.

### Live updates
- The pane reflects changes via **Server-Sent Events** from the Rust backend (`axum`) — one-way, simple, fits a read-mostly pane.

### Node lifecycle & deletion
- **Automatic flow prefers supersede** (`replaces`, `valid_until`) so history is preserved.
- **Hard delete is an explicit user-only action** in the pane; deleting a node **cascades its edges**. Claude never hard-deletes.
- Stale **provisional** episodic nodes **decay/archive** after a TTL (see §6A trust model); confirmed/trusted nodes persist. This is the answer to unbounded episodic growth.

### Safety (secrets / PII)
- **Defense in depth:** the skill instructs Claude to never store secrets/credentials/PII, **and** the backend runs a **redaction pass** (regex + high-entropy detection) on node `title`/`body` before write.

### Bootstrapping & portability
- New graphs start **empty** and grow from real work — no upfront scan/import (cleaner signal).
- **JSON export/import ships in v1** — human-readable, diffable; the basis for optional git-sharing and the "you own your memory" promise. **Export strips embeddings** (regenerated on import) so it stays small, diffable, and model-agnostic; the embedding-free node+edge dump *is* the git-shareable form.

### Concurrency
- SQLite in **WAL** mode (concurrent reads); writes are **serialized** (last-write-wins). Sufficient for a local single-user tool — no optimistic locking in v1.

### Backend lifecycle (v1)
- The user runs **`engram serve`** — a single local daemon exposing **HTTP + streamable-HTTP MCP + SSE**. Browser pane and Claude clients both connect to it by URL. (MCP is **not** a stdio subprocess Claude spawns — the daemon owns the socket so one process owns the DB and can broadcast SSE on write. The daemon must be running before Claude connects; the browser pane needs it running anyway.)
- **Session id** comes from the **MCP transport's session id**; the daemon mints a fallback only if a client omits one. This keeps "reconfirmation across sessions" aligned with what the client actually treats as a session.
- **One daemon per repo**, started in the repo directory and bound to that repo's `.engram/`. IDE wrappers manage this lifecycle in later phases.
- **Port resolution:** the daemon requests `--http-port` (default 8787) but **walks to the next free port** (up to +15) when it's taken — inevitable with one daemon per repo — and records the outcome in **`.engram/daemon.json`** (`{port, url, pid, db}`). Clients (IDE plugins, the skill) resolve the URL from that file, falling back to the default; `/health` returns JSON including the served `db` path so a discovered port can be verified as *this* repo's daemon. Stale files are harmless: readers health-check before trusting. The file is best-effort removed on graceful shutdown.

---

## 7. Conflict handling

- **v1 (default, passive):** contradictions are recorded as `conflicts-with` edges and shown as a queryable worklist in the graph pane. Awareness during work comes **through RAG** — when Claude retrieves relevant nodes, conflicting ones surface alongside, so the model notices naturally without an active interrupt.
- **v1 addition (pull-based warnings, shipped):** `add_note` / `update_node` compare the new text against nearby nodes and return `warnings` when it lands close to a node that is **in an active conflict** or **superseded** — the writing assistant self-corrects in the same turn, no push protocol needed. This is the cheap half of the mid-session-push milestone below.
- **Phase 1 — conflict scan (decided 2026-07-06; inspired by Gentleman-Programming/engram's `conflicts scan`, adapted to our architecture).** Split detection from judgment:
  - **Candidate detection is local and automatic.** We have embeddings (their FTS5-only design forces an LLM earlier); pairwise cosine + FTS term overlap over same-/related-type nodes is cheap and deterministic. Runs (a) **inline on write** — extend the existing dupe/warning pre-check to also *record* a suspected pair, not just warn; (b) as a **periodic daemon sweep** piggybacking on the decay sweep; (c) **on demand from the pane** ("Scan now" button → HTTP endpoint → same sweep → results stream back via SSE). Manual trigger exists for "after a big session" reassurance, not because the scan needs babysitting.
  - **Suspected-conflict queue:** candidate pairs get status `suspected` (not yet a `conflicts-with` edge). Stored so they survive sessions and feed the pane worklist and health strip.
  - **Judgment is cooperative — the daemon never calls an LLM.** Shelling out to `claude -p` (Gentleman's approach) is *possible* but rejected: it adds a PATH/CLI dependency, burns subscription quota in the background, is slow per judgment, and judges without session context. Instead the `brief` reports "N suspected conflicts pending" and the skill has Claude judge them in-session via MCP (confirm → `conflicts-with` or `replaces` edge; dismiss → pair marked ignored, never re-raised). The user can also resolve pairs directly in the pane worklist. Re-evaluate shell-out only if pending queues rot unjudged in dogfooding.
- **Later (planned):** proactive **mid-session push** via MCP **channels** — Engram tells Claude "this conflicts with an earlier decision" while it's working, so it self-corrects in real time. Behind a flag; opt-in.

This staging keeps v1 simple (retrieval does the work) while reserving the more powerful active-warning behavior as a clear future milestone.

---

## 8. Tech stack (locked: Rust + Vue, local-first)

The stable contract is the *interface* (local HTTP + stdio MCP + SQLite).

### Backend (Rust)
- **MCP:** `rmcp` (official Rust MCP SDK), served over **streamable HTTP** from the `engram serve` daemon (not stdio) so a single process owns the DB and the SSE broadcast. Pure-Rust — **no Node.js runtime dependency.**
- **DB:** `rusqlite` with `bundled` (SQLite statically compiled in).
- **Keyword search:** SQLite **FTS5**.
- **Vector search:** `sqlite-vec` → hybrid FTS5 + vector RAG in one DB.
- **Local embeddings:** `fastembed` (ONNX via `ort`), small model (`bge-small` / `MiniLM`).
- **HTTP API:** `axum`.

### Frontend (Vue + TS)
- **Vue 3.5** + **TypeScript**, **Vite** build, **Bun** as package manager/runtime.
- **State:** Pinia. **Routing:** vue-router.
- **Graph viz:** **Vue Flow** (`@vue-flow/core` + `background`, `controls`, `minimap`).
- **Styling:** Tailwind CSS 4 (`@tailwindcss/vite`); **Stylelint** (SCSS/Vue) + **ESLint 9** (flat config); editorconfig.
- **Utilities:** `@vueuse/core`; `markdown-it` + `dompurify` for safe rendering of node bodies.
- **Tooling/serving:** `vue-tsc`; Docker + Caddy for standalone serving.
- **Real-time updates:** the Rust backend pushes live graph updates via **Server-Sent Events** (`axum`).
- **Rendering (v1):** render the **full graph**; scaling (filters/focus/auto-layout) is deferred. At ~100 nodes it's busy but workable; the user can ask to merge nodes. Mostly the assistant operates on the graph while the human reads/curates.
- Runs in browser (dev/standalone), Webview (VSCode, later), JCEF (JetBrains, later).

### IDE wrappers (later)
- **VSCode:** TS extension; frontend in Webview; runs in VSCode's bundled Node (no user-installed Node). Drops in `.mcp.json` + skill (+ hooks later).
- **JetBrains:** Kotlin plugin; frontend in **JCEF** (custom bindings, but far less work than native Swing).

### Build & distribution
- **Staging:** the **core loop ships as a plain local binary** (built with `cargo build`, wired to this session via `claude mcp add --transport http engram <url>` or a repo `.mcp.json`). Distribution machinery is deliberately deferred until the core loop works — building installers before the binary does anything is premature.
- **v1 delivery = thin marketplace plugin** (skill + commands + MCP config), *not* a bundled binary (an ONNX-bearing binary is too big to commit to a plugin repo). The shipped **skill** is the bootstrapper: on a missing/mismatched backend it detects the platform, downloads the matching binary from **GitHub Releases**, **verifies a checksum** (Sigstore/minisign optional), and registers the MCP server — after a one-time **consent prompt** (the only non-silent skill action). Claude itself does the platform detection + fetch, which sidesteps needing a Node-free cross-platform launcher.
- **Full release pipeline (after the core loop):** cross-platform via **GitHub Actions matrix** + **`cargo-dist`** (generates the CI matrix *and* `sh` + PowerShell installers, optionally Homebrew/MSI) / `cross`. Targets: macOS (x86_64 + arm64), Windows (msvc), Linux (gnu/musl). The installers must also **install the chosen capture-skill variant** from `skills/engram/` (relaxed by default) into the user's Claude skills directory, alongside the binary.
- **Onboarding paths (cold-start, designed 2026-07-02 — not yet built):** GitHub is the single **root of trust**: Releases hold per-platform binaries + `SHA256SUMS`; the repo **and GitHub Pages** serve `install.sh` (`curl -fsSL https://<pages>/install.sh | sh` → detect platform, download from Releases, verify checksum, install to `~/.local/bin`, offer MCP registration + relaxed-skill install). Every link a user, plugin, or Claude follows resolves to the GitHub repo. Four entry paths, all converging on binary→daemon→MCP→skill:
  1. **JetBrains plugin only** — the backend-down card gains a "Set up Engram" action (shows the one-liner; later the plugin manages the daemon itself).
  2. **VSCode plugin only** — same affordance in the webview overlay; `engram.configureMcp` already covers the MCP half.
  3. **GitHub page** — Pages doubles as the landing page: demo GIF, one-liner, plugin links.
  4. **Binary + plugin present, daemon not running** — **Claude starts it** (`engram serve --http-only`, background, repo root); taught by the skill. Harmless today (stdio MCP works without the daemon), mandatory once MCP moves to daemon-hosted streamable HTTP.
  - **No-plugin usage** is a first-class path: Claude Code runs everything, and the skill tells it to point users at the localhost pane (default `http://127.0.0.1:8787`) when they ask where to see their memory.
- **Plugin publishing from GitHub Actions:** JetBrains via `publishPlugin` + token (known). VSCode via the official `@vscode/vsce` CLI with a VS Marketplace PAT (Azure DevOps), typically wrapped in the `HaaLeo/publish-vscode-extension` action — which also publishes to **Open VSX** (`ovsx` + token) for VSCodium/Cursor/Windsurf users. Publish to both registries. Marketplace ids per the naming rule (§0): JetBrains plugin stays `engram`; VSCode/Open VSX publish as **`engram-alpha`**.
- **Code-signing:** start with **checksums / minisign / Sigstore** (free; skill verifies these). OS code-signing (Apple notarization, Windows Authenticode) is added later only if Gatekeeper/SmartScreen friction actually bites — it costs certs + a CI step.
- SQLite static (`bundled`). One binary per platform; **caveat:** local embeddings pull in ONNX Runtime (a native lib `ort` manages) → "binary + one runtime lib per platform," not fully single-file. The `ort`/onnxruntime linking story on Windows is the main cross-platform risk to spike early.

---

## 9. Architecture (at a glance)

```
                 ┌─────────────────────────────────────────┐
                 │            Engram Core (Rust)            │
                 │  graph store (SQLite + FTS5 + vec)       │
                 │  RAG (hybrid search, local embeddings)   │
                 │  librarian (async filter/classify)       │
                 │  ── HTTP API ──┐   ┌── stdio MCP server ─┤
                 └────────────────┼───┼─────────────────────┘
                                  │   │
        ┌──────────────┬──────────┘   └──────────┬───────────────┐
        │              │                          │               │
   Vue frontend   Vue frontend              Claude Code      Claude Desktop
   (browser, v1)  (Webview / JCEF, later)   (MCP, v1)        (MCP)
```

---

## 10. Roadmap

**Phase 0 — Core loop (browser standalone):**
1. Rust core lib: SQLite graph (nodes + typed edges) per §4 schema, stored at `.engram/graph.db` (git-ignored).
2. `engram serve` daemon (`axum`): HTTP API + **SSE** + **streamable-HTTP MCP** server (`rmcp`) — one process, owning the DB — exposing `search`, `add_note`, `link`, `get_node`, `traverse`, `list_open`, `delete_node` (Appendix A).
3. Hybrid RAG: FTS5 + `sqlite-vec` + `fastembed` local embeddings; **secret-redaction pass** on write.
4. **Vue Flow** graph view in the browser, live-updating via SSE.
5. **JSON export/import.**
6. Shipped Claude Code **skill** (cooperative capture; batch + silent; Relaxed default).

**Phase 1 — Quality + first IDE** *(complete — shipped v0.2.0, 2026-07-06)*:
7. ~~Librarian + provisional-decay/archive~~ **Shipped:** the **conflict scan** of §7 — suspects table, write-time recording, 6-hourly daemon sweep + startup pass, `POST /conflicts/scan`; and the **decay pass** (`POST /decay`, policy `stale_since` + 14-day TTL, archives only Claude-authored/never-approved/episodic-or-volatile nodes). Threshold tuned on the dogfood graph: 0.75 flagged every topical cluster → **0.85**.
8. ~~Write-policy modes + reclassification/editing + hard-delete~~ **Shipped:** modes are the three skill variants; NodeDetail gained an **edit mode** (title/body/durability + **type reclassification** via `NodePatch.type`, also on MCP `update_node`); hard-delete was already user-only in the pane.
9. ~~Conflict worklist + review view~~ **Shipped:** Review drawer lists **suspected pairs** (Conflict / Replaces / Dismiss) above confirmed conflicts, with a **Scan now** action; **graph-health strip** (active/suspected/conflicts/stale/provisional counts) sits bottom-left; brief lists the top-8 suspects with judgment instructions and Claude judges via `list_suspects`/`resolve_suspect`.
10. ~~VSCode wrapper~~ **Shipped** earlier (see Status in CLAUDE.md; JetBrains too).

**Phase 2 — Second IDE + active features:**
11. JetBrains wrapper (JCEF).
12. Ambient capture via hooks → librarian.
13. **Mid-session conflict push** via MCP channels (the active-warning feature).
14. Cross-platform release pipeline.

**Phase 3 — Multi-harness memory (the "preserve memory between AI agents" pitch):** first-class support for the most-used CLI harnesses beyond Claude Code — **OpenCode, Codex CLI, Gemini CLI, Kilo CLI** — plus the **Codex VS Code integration**. All of them speak MCP, so the backend is already agnostic: the work is per-harness config bootstrap (install.sh + plugins writing each harness's MCP registration), capture-skill/prompt equivalents per harness, and docs. This upgrades the pitch from "memory for your assistant" to **one shared, local memory that persists across different AI agents** — Claude captures a decision, Codex recalls it. Multi-agent read/write concurrency (two or more agents on one graph) is already handled by the local single-daemon design: SQLite WAL + serialized writes through one engine; verify with a concurrency smoke test rather than new architecture.

**Near-term additions (from MemPalace competitive scan, 2026-07-06):**
- **`timeline` MCP tool + pane view:** walk a node's `replaces` chain chronologically ("how did the auth decision evolve"). Primitives already exist (`replaces` edges, `created_at`, superseded status) — this exposes them as a history. Also feeds the §11 "pane review UX" question.
- **`engram doctor`:** diagnostic/repair CLI command — daemon-vs-repo DB path match, embedding model presence, WAL health, FTS/vector index consistency. Guards the wrong-cwd-empty-DB failure class that `scripts/deploy-pane.sh` works around.
- **Ship as a Claude Code plugin:** a `.claude-plugin/` dir in this repo (skill + slash commands + `.mcp.json` template, hooks config later) so one plugin install wires everything — the packaging shape MemPalace validates across five harnesses. Complements the locked thin-marketplace-plugin distribution decision (§0) and the IDE extensions; plugin name follows the naming rule (plain `engram` unless the marketplace slug is taken).
- **Unhappy-path guidance in the skill(s):** what Claude should do when search returns nothing (say so, don't invent), when the MCP server is down (surface the error, don't silently fall back to model memory), and query hygiene (short keyword queries, never paste conversations). Extract into a shared protocol section all three variants reference so it can't drift.

**Later / maybe:** cross-project shared Principle layer; cloud sync; **Obsidian export** (far future — map nodes to notes and typed edges to `[[wikilinks]]`; our typed graph fits Obsidian's model natively, and it's a cheap integration with high appeal to the note-taking crowd).

**Later — tags + pane filtering:** free-form `tags` on nodes, settable by the user in the pane or by Claude on request ("tag everything about the auth rewrite"), with **filter chips in the pane** to focus the canvas on one concern. Tags complement Anchors: an Anchor says *what code a note concerns*; a tag says *how the user wants to slice the graph*. Ship **3–4 recommended default categories** rather than a free-for-all — e.g. `tech-decision` (stack/architecture choices), `preference` (abstract taste/conventions), `process` (how we work: build, release, review), `domain` (business/domain knowledge) — and let users add their own. Needs: a `tags` column (JSON array) + FTS inclusion and a skill line telling Claude to reuse existing tags before inventing new ones. The pane already has a client-side property filter (type/status/trust/source/durability/archived, options gathered from the loaded graph) — tags slot in as one more gathered group.

---

## 11. Open questions

*Behavioral design is resolved in §6A. Remaining items are implementation-level.*
*Resolved this round (see §0/§6A/§6B/Appendices): MCP transport = **streamable HTTP, single daemon**; `session_id` taken from the **MCP transport**; `add_note` **server-side similarity pre-check**; `provisional` **derived** + `last_seen_at`/`reconfirmed_at` columns added now; **export strips embeddings**; **distribution staged** (local binary → thin plugin + skill-driven verified fetch → `cargo-dist` pipeline).*

- **Skill wording (drafted, tuning via dogfood):** three variants exist in `skills/engram/{aggressive,normal,relaxed}/`; wording keeps evolving as dogfooding (Aggressive) surfaces low-value nodes or ambiguous guidance.
- **Decay & promotion mechanics:** concrete numbers — starting confidence for provisional nodes, what counts as "reconfirmation," decay rate/TTL before a stale provisional node drops.
- **Fuzzy re-match algorithm:** how anchors are matched by label on a new session (embedding similarity? threshold? how flagging is presented).
- **1-hop cap tuning (mostly resolved):** hits now carry ≤5 neighbor refs, conflicts/replaces first; remaining question is whether the cap and the brief's section quotas need tuning as graphs grow.
- **Pane review UX:** since writes are silent, the graph pane is the transparency surface — needs a clear "recently added / provisional / conflicts" review view.
- **Redaction coverage:** which secret/PII patterns the backend pass catches; false-positive handling.
- **Export schema (mostly resolved, §6B):** node+edge dump with **embeddings stripped** (regenerated on import); this *is* the git-shareable form. Only exact field naming remains for implementation.
- **Scope expansion (later):** when/whether to add the cross-project shared Principle layer.

---

## 12. Non-goals (v1)

- No companion/persona features — Engram is project memory, not a relationship.
- No custom UI inside the Claude Code chat panel (not supported; not needed).
- No cross-project / global graph — per-repo only for now.
- No remote embeddings or cloud sync — local-first first.
- No ambient hook capture in v1 — cooperative MCP first.

---

## 13. Development approach

- **Dogfood from day one, on Aggressive mode.** Build Engram while using Engram (and on other real work) with capture set to **Aggressive**, so the graph fills fast and gaps / false-positives surface early. The first real user is us.
- **Minimalist product and code.** Keep surface area small — fewer features, fewer knobs, less to maintain. Prefer **clear naming and the public docs of our dependencies** over inline comments; comment only where intent isn't obvious from the names.

---

## Appendix A — MCP tool contracts (draft)

MCP server (`rmcp`, **streamable HTTP** — hosted by the running `engram serve` daemon, not a stdio subprocess). Tool names namespaced by the server (`engram`).

```
search(query: string, types?: NodeType[], limit?: int=8)
  -> [{ id, type, title, snippet, score, durability, status,
        neighbors: [{ edge_id, edge_type, direction, edge_status, id, type, title, archived }] }]
  # hybrid FTS5 (OR-recall) + vector; score = relevance × (1 + trust boost) so
  # trust modulates rather than dominates. Each hit carries its 1-hop neighbors,
  # conflicts-with / replaces first, capped at 5.

brief(max_chars?: int=12000)
  -> markdown
  # session-start digest: unresolved conflicts, open problems/intents, principles,
  # decisions, cautions, recent adds — hard character budget. Inclusion refreshes
  # each node's decay clock (being briefed counts as reuse). Ids appear only in
  # "Recently added" — canon lines are prose; anything is re-findable via search.

get_node(id: string)
  -> { node, edges_out: [{type,to,note}], edges_in: [{type,from,note}] }

traverse(from: string, edge_types?: EdgeType[], depth?: int=2)
  -> { nodes: [...], edges: [...] }   # bounded subgraph

add_note(type: NodeType, title: string, body: string,
         durability?: Durability, code_refs?: string[])
  -> { id, created: true, warnings?: [...] }     # source defaults to "claude"
   | { matched: id, created: false, similarity } # near-duplicate found (same-type cosine ≥ θ)
  # search-before-write is a scale-dependent recommendation; the built-in
  # same-type similarity pre-check is the safety net against silent dupes.
  # warnings[] flags nearby nodes that are in-active-conflict or superseded
  # (the pull-based v1 of §7's conflict push) — the writer self-corrects.

link(from: string, to: string, type: EdgeType,
     note?: string, confidence?: float)
  -> { id }

unlink(id: string)                     # delete one edge — mislink repair is Claude's to do
  -> { ok }

update_edge(id: string, status?: EdgeStatus, note?: string, confidence?: float)
  -> { ok, id }                        # e.g. mark a conflicts-with resolved/dismissed

update_node(id: string, fields: Partial<Node>)
  -> { ok, warnings?: [...] }          # same conflict warnings as add_note when text changed

delete_node(id: string)                # user-facing; cascades edges. Claude does not call this.
  -> { ok }

list_open(types?: ("Problem"|"Intent")[] , include_conflicts?: bool=true)
  -> [ ... ]                           # the live worklist (open problems, intents, conflicts)
```

Resources: `engram://node/{id}` so users can `@`-mention a node in a prompt.

---

## Appendix B — SQLite schema (draft)

```sql
-- ids: 12-char lowercase base36, time-sortable (7 chars of seconds since
-- 2026-01-01 + 5 random) — short enough to quote in AI context. Not UUIDs.
CREATE TABLE nodes (
  id          TEXT PRIMARY KEY,
  type        TEXT NOT NULL,         -- Principle | Decision | Caution | Problem | Resolution | Insight | Intent | Anchor
  title       TEXT NOT NULL,
  body        TEXT,
  durability  TEXT NOT NULL,         -- stable | episodic | volatile
  source      TEXT NOT NULL,         -- user | claude
  session_id  TEXT,
  created_at  INTEGER NOT NULL,
  valid_from  INTEGER,
  valid_until INTEGER,               -- NULL = current
  status      TEXT,                  -- open | resolved | obsolete (nullable)
  confidence  REAL,
  code_refs   TEXT,                  -- JSON array of semantic refs
  last_seen_at    INTEGER,           -- last retrieval/reference (decay input; stamped Phase 0, logic Phase 1)
  reconfirmed_at  INTEGER            -- last cross-session re-assertion / approval (trust input; logic Phase 1)
);

CREATE TABLE edges (
  id          TEXT PRIMARY KEY,
  type        TEXT NOT NULL,         -- about | because | answers | builds-on | replaces | conflicts-with | needs
  from_id     TEXT NOT NULL REFERENCES nodes(id),
  to_id       TEXT NOT NULL REFERENCES nodes(id),
  source      TEXT NOT NULL,
  created_at  INTEGER NOT NULL,
  confidence  REAL,
  strength    REAL,
  note        TEXT,
  valid_from  INTEGER,
  valid_until INTEGER,
  status      TEXT                   -- active | resolved | dismissed
);

CREATE INDEX idx_edges_from ON edges(from_id);
CREATE INDEX idx_edges_to   ON edges(to_id);
CREATE INDEX idx_nodes_type ON nodes(type);
CREATE INDEX idx_nodes_stat ON nodes(status);

-- full-text (FTS5)
CREATE VIRTUAL TABLE nodes_fts USING fts5(title, body, content='nodes', content_rowid='rowid');

-- vectors (sqlite-vec)
CREATE VIRTUAL TABLE vec_nodes USING vec0(node_id TEXT, embedding FLOAT[384]);
```

---

## Appendix C — Repo layout (draft)

```
engram-alpha/
  Cargo.toml                 # workspace
  crates/
    engram-core/             # lib: graph store, RAG, librarian, domain types
    engram-mcp/              # rmcp stdio server (thin over core)
    engram-http/             # axum HTTP API (thin over core)
    engram-cli/              # binary: `engram serve` (runs http + mcp)
  frontend/                  # Vue 3 + TS + Vue Flow
  engram-vscode/             # TS extension + Webview (built)
  engram-jetbrains/          # Kotlin plugin + JCEF (built)
  hooks/                     # (Phase 2) Claude Code hook scripts
  skills/
    engram/
      aggressive/SKILL.md    # max capture — the dogfood variant
      normal/SKILL.md        # balanced
      relaxed/SKILL.md       # recommended default install
  PLAN.md
  CLAUDE.md
  LICENSE                    # MIT
  README.md
```
