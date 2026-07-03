# Engram — context for Claude Code

**What this is:** a graph-based, durable, inspectable long-term project memory for AI coding assistants (Claude Code first). Local-first, user-owned, graph-first UI. See **`PLAN.md`** — it is the source of truth; read it before doing anything.

**Positioning (PLAN.md §1A):** Engram is the **reasoning/decision memory** layer (why/decided/what-bit-us) — *not* a code-structure graph (Graphify/CodeGraph). Those are complementary and can coexist. The validated wedge: an **editable, IDE-embedded graph pane** for reasoning memory + conflict surfacing + local — the closest concept (Cairn) has no UI/traction.

## Status (2026-07-02)
- **Phase 0 complete & verified** (50 tests: core 36, http 11, mcp 3). `engram serve` serves the HTTP API + SSE + the embedded Vue pane at `http://127.0.0.1:8787`; release binary at `~/.cargo/bin/engram`.
- **JetBrains plugin** (`engram-jetbrains/`, package `dev.techtheist.engram` — don't rename): JCEF tool window (right anchor) + editor-tab mode; `./gradlew buildPlugin` → zip; user-tested in-IDE.
- **VSCode extension** (`engram-vscode/`): Webview pane in the **secondary sidebar (right)** + `.mcp.json` configurator + daemon status bar; `vsce package` → vsix; user-tested in-IDE.
- **Skill:** three capture variants in `skills/engram/` (**aggressive / normal / relaxed**; relaxed is the recommended default for users, **aggressive is what this repo dogfoods** via `.claude/skills/engram`).
- **Known divergence:** MCP currently runs as a stdio `engram mcp` process (wired via `.mcp.json`), not the daemon-hosted streamable HTTP from PLAN §0 — deliberate deferral; the user owns that migration. Don't rewrite it unprompted.
- **Pane rebuild cycle:** run **`scripts/deploy-pane.sh`** (add `--vsix` / `--jetbrains` to also rebuild plugins; real embeddings are the default — `--fake` only for throwaway DBs, since MCP writes real vectors and a fake-embedding daemon searching them is noise). It builds the pane, reinstalls the binary, restarts the daemon on the repo's absolute DB path, and verifies `/health` serves the right DB — never hand-chain `cd`/build/restart (relative `--db` from the wrong cwd silently creates an empty graph).
- **Next:** bundle skill + `.mcp.json` bootstrap into the plugins; `runIde`/`verifyPlugin`; MCP transport migration; decay/promotion numbers.

## Locked decisions (don't relitigate without reason)
- **Open-source, MIT.** Goal: portfolio/credibility → optimize for docs & DX.
- **Backend: Rust** (`rmcp`, `rusqlite` bundled, `sqlite-vec`, `fastembed`). No Node.js runtime dependency.
- **Frontend:** Vue 3.5 + TS + Vite + **Bun**, Pinia, vue-router, Tailwind 4, **Vue Flow** (`@vue-flow/*`) for the graph, vueuse, markdown-it + dompurify, ESLint (flat config) + Stylelint — `bun run lint` / `lint:style` in `frontend/`.
- **Scope: per-repo graph only** (no cross-project layer in v1).
- **Embeddings: local-only** (fastembed/ONNX). No remote, no keys.
- **Build first: browser standalone** (also serves Claude Desktop). IDE wrappers later.
- **Capture: cooperative (MCP) first** — Claude writes via MCP tools + a shipped skill. Ambient hooks later.
- **Conflicts: passive in v1**, surfaced via RAG retrieval. Mid-session push (MCP channels) is a planned Phase-2 feature.
- **Storage: `.engram/graph.db` in repo, git-ignored** (personal). Share later via JSON export, not the binary DB.
- **Backend run (v1): `engram serve`** — one local daemon (HTTP + MCP + **SSE**).
- **Deletion:** auto-supersede; **hard-delete is user-only** (cascades edges). **Bootstrap:** empty start. **Export:** JSON in v1. **Secrets:** skill rule + backend redaction pass. **Episodic growth:** stale provisional nodes decay/archive.
- **Merge:** search-before-write is **recommended once the graph is big enough that duplicates are likely** — not a hard gate on small graphs. `add_note` always runs a same-type similarity pre-check and returns `{matched, created: false}` on a near-dupe (then `update_node` the match). **Concurrency:** SQLite **WAL**, serialized writes. **Daemon:** **one `engram serve` per repo** (HTTP + MCP + SSE). **Pane (v1):** render full graph; scaling deferred.
- **Approach:** **dogfood on Aggressive mode** (we're the first user); **minimalist** code — clear names + deps' public docs over inline comments.

## Ontology conventions (see PLAN.md §4)
- **8 node types only:** Principle, Decision, Caution, Problem, Resolution, Insight, Intent, Anchor. Do **not** add new types.
- **7 edge types, sentence-shaped:** `about`, `because`, `answers`, `builds-on`, `replaces`, `conflicts-with`, `needs`. A triple must read as English (e.g. *Decision because Principle*).
- **No generic `relates_to`.** If you can't pick a real verb, don't link.
- **Durability** (`stable` / `episodic` / `volatile`) governs staleness. Never store volatile implementation details unless asked.
- High-value edges are `replaces` and `conflicts-with` — they make the graph *active*.

## Where things go
- `crates/engram-core` — graph store, RAG, librarian, domain types.
- `crates/engram-mcp` — rmcp stdio server. `crates/engram-http` — axum API + embedded pane (rust-embed). `crates/engram-cli` — `engram serve` / `engram mcp` binary.
- `frontend/` — Vue app (Bun). `engram-jetbrains/` — JetBrains plugin (JCEF). `engram-vscode/` — VSCode extension (Webview).
- `skills/engram/{aggressive,normal,relaxed}/SKILL.md` — the cooperative-capture skill variants (relaxed = user default, aggressive = dogfood).

## Resolved behaviors (v1 — see PLAN.md §6A)
- **Capture:** Claude judges what's worth saving (no objective gate); Claude is the sole gatekeeper via the skill (no separate librarian pass yet). Duplicates → **merge into existing** + bump confidence. Claude nodes start **provisional**, earn trust by reconfirmation/approval, stale ones decay.
- **Anchors:** free-text responsibility label (+ optional file refs); **flexible granularity** (Claude decides); auto-created/reused; on refactor, **fuzzy re-match and flag** if unsure.
- **Retrieval:** session-start (small, ~5–8, token-capped) **+ on-demand**; returns **matches + 1-hop subgraph** (conflict/replace neighbors prioritized); **hybrid ranking + trust boost**.
- **Skill writes:** **batch at stopping points**, **fully silent** (transparency is the pane, not chat), **both** auto- and manually-invoked (`/engram`), default mode **Relaxed**.

## Retrieval & write surface (implemented 2026-07-02)
`brief` (session-start digest, MCP tool + `GET /brief` + `engram brief`); search hits carry ≤5 1-hop neighbors (conflicts/replaces first) and use relevance×trust scoring (FTS5 OR-recall + cosine); `add_note` self-checks same-type dupes (`{matched, created:false}`); writes return `warnings` near conflicted/superseded nodes; `unlink`/`update_edge` let Claude repair mislinks (node hard-delete stays user-only).

## Open questions (implementation-level — PLAN.md §11)
Decay/promotion numbers (implemented, untested); fuzzy re-match algorithm; neighbor-cap/brief-quota tuning; pane review UX (matters because writes are silent). Future: tags + pane filtering (PLAN §10 Later).
