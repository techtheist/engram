# Engram Alpha — Plan & Spec

> **Name (locked 2026-07-06).** The product is **"Engram Alpha"** — *Alpha is part of the name, not a version tag*; plain "engram" is contested four ways in the AI-memory space (see §1A). The binary is **`engram-alpha`** (renamed from `engram` in v0.4.0 so shell completions and docs match the product); this repo and the JetBrains plugin keep the short name; the **VSCode extension publishes as `engram-alpha`**.

A graph-based, durable, **inspectable long-term project memory** for AI coding assistants (Claude Code first). Engram sits beside your assistant as a memory it can read from and write to — and that you can see, edit, and own. The graph is the product surface, not hidden plumbing.

> **History note (2026-07-10):** shipped work is summarized here in one line each — the detailed rationale and post-mortems live in the Engram graph itself (`engram-alpha serve` → pane, or the `search`/`timeline` MCP tools) and in CLAUDE.md's Status block.

---

## 0. Locked decisions

| Decision | Choice |
|---|---|
| **Goal** | Open-source / portfolio; optimize for docs, DX, shareability |
| **Name** | **Engram Alpha** ("Alpha" part of the name, not a version) |
| **License** | MIT |
| **Memory scope** | **Per-repo only** (v1); cross-project layer is a future option |
| **Build first** | Browser standalone (doubles as Claude Desktop support); IDE wrappers later |
| **Embeddings** | **Local-only** — `fastembed`/ONNX; no remote, no keys |
| **Backend language** | **Rust** — `rmcp`, `rusqlite` (bundled), `sqlite-vec`, `fastembed` |
| **Capture (v1)** | **Cooperative (MCP) first**; ambient hooks later |
| **Conflict handling (v1)** | Passive, surfaced via RAG; mid-session push is a Phase-2 feature |
| **Storage** | `.engram/graph.db` in repo, **git-ignored**; share via JSON export, never the binary DB |
| **Live updates** | SSE (axum → pane) |
| **Deletion** | Supersede auto; **hard-delete user-only** (cascades edges) |
| **Backend run (v1)** | `engram-alpha serve` — one local daemon per repo (HTTP + MCP + SSE) |
| **Bootstrap / export** | Empty start; JSON export in v1 (embeddings stripped, regenerated on import) |
| **Secrets** | Skill rule + backend redaction pass (regex + high-entropy) |
| **Validation** | **Dogfood on Aggressive mode** — we're the first user |
| **Engineering style** | Minimalist; clear names + deps' public docs over inline comments |
| **Merge identity** | Claude searches, then updates; `add_note` self-checks same-type dupes |
| **Concurrency** | SQLite WAL, serialized writes, last-write-wins |
| **MCP transport** | Streamable HTTP from the daemon (planned; **current divergence:** stdio `engram-alpha mcp`, see CLAUDE.md) |
| **Session id** | From the MCP transport; daemon mints a fallback if omitted |
| **Distribution** | Local binary first; thin marketplace plugin + skill-driven checksum-verified fetch; `cargo-dist` pipeline later |

---

## 1. Vision

When you work with an AI assistant on a long-lived project, the valuable context — *why* you chose something, *what* bit you last time, *how* a gnarly bug got solved — evaporates between sessions and overflows any context window. Engram captures that as a **structured, durable, user-owned graph** the assistant consults and contributes to over the life of the project.

### What makes it different (the wedge)
1. **An *active* graph, not a passive note pile** — `replaces` and `conflicts-with` edges surface staleness and contradictions. Almost nobody does this.
2. **Durability-aware memory** — every node knows if it's stable, episodic, or volatile; this is what stops the graph rotting (the core failure mode of every memory tool).
3. **Graph-first UI** — the user sees and edits the canon directly in their own pane, not buried in a chat.
4. **Local-first and portable** — a local SQLite graph you own; one backend serves any MCP client.

---

## 1A. Competitive positioning (mid-2026 scan)

The space splits into two layers; Engram sits in the under-served one:

- **Code-structure graphs** (Graphify ~58K★, CodeGraph, …) auto-derive architecture from the codebase. Regenerable, stale-with-the-code. **Complementary, not competition** — one could later feed Engram's Anchors.
- **Reasoning / decision memory** — the durable "why / what was decided / what bit us" layer. Closest concept: **Cairn** (reasoning graph + contradictions via hooks/MCP) — no UI, ~3★. The MCP "memory visualizers" have graph UIs but are passive viewers, not curated reasoning memory.

**Validated wedge:** the combination nobody ships — durable reasoning memory **+** an editable graph pane embedded in the IDE **+** conflict/supersede surfacing **+** local-first. JetBrains has zero memory-graph plugins.

**Name collisions:** four other "engram" AI-memory projects — most notably **Gentleman-Programming/engram** (4.9K★, Go; the most direct competitor *and* proof of demand, but flat FTS5 observations: no embeddings, no ontology, no trust/decay, no graph UI) — hence the product name **Engram Alpha**. The traction there softens the "demand unproven" caveat: appetite for coding-agent memory is real; unproven is only the graph-pane wedge.

---

## 2. What it is

A **local core service** plus thin clients: a Rust backend owning graph + RAG (HTTP API for the pane, MCP for assistants), a Vue single-page graph pane (browser standalone first, embedded in VSCode Webview / JetBrains JCEF), and shipped capture skills. One backend, many faces.

---

## 3. Integration with Claude Code

Three channels, all shipped or planned: **MCP server** (primary, v1 — Claude reads/writes the graph, guided by a shipped skill), **hooks** (session-start brief shipped; file-read match later), **SDK/CLI** (pane-launched sessions; later). There is no API to render custom UI inside Claude Code's own panel — fine, Engram has its own pane. v1 is cooperative: Claude *chooses* to save.

---

## 4. Graph ontology

The schema's organizing principle is **durability**, not just type. Names are abstract and "thought-shaped" so the assistant reaches for them naturally.

### 4.1 Node types (7 + 1 anchor)

| Node | Durability | The thought it captures |
|---|---|---|
| **Principle** | stable | "this is how I like things / what I value" |
| **Decision** | stable, supersedable | "we chose this, for a reason" (ADR-like) |
| **Caution** | stable | "watch out — this bites" (constraints, gotchas, specs-as-rules) |
| **Problem** | episodic | "this was hard / went wrong" (the micro bug-tracker) |
| **Resolution** | episodic | "here's how it got solved" |
| **Insight** | episodic | "I realized something non-obvious" |
| **Intent** | volatile | "do this later" (TODO / deferred work) |
| **Anchor** | semi-stable | "what this is *about*" — a code subject, by **semantic identity, never line numbers** |

**Durability classes:** **stable** persists until explicitly `replaces`-superseded; **episodic** ages but is never "wrong"; **volatile** has a short TTL and is not stored unless explicitly asked. No separate `Concept`/`Spec` node — that material folds into Caution/Principle/Decision. Keeping the set at 8 is deliberate; do not add types.

### 4.2 Node properties

`id` (12-char time-sortable base36), `type`, `title`, `body`, `durability`, `source` (user|claude — trust signal), `session_id`, `created_at`, `valid_from/valid_until` (supersession), `status` (open|resolved|obsolete for Problem/Intent), `embedding`, `code_refs` (loose semantic refs), `tags`, plus the trust timestamps `last_seen_at` / `reconfirmed_at` / `approved_at` — trust is **computed at read time** from these (stored confidence was removed in v0.1.15).

### 4.3 Edge types

**Principle: a triple reads as a plain English sentence** — the assistant completes a sentence, not a foreign key.

| Edge | Reads as |
|---|---|
| **about** | *Insight* **about** *Anchor* |
| **because** | *Decision* **because** *Principle* |
| **answers** | *Resolution* **answers** *Problem* |
| **builds-on** | *Insight* **builds-on** *Insight* |
| **replaces** | *Decision* **replaces** *Decision* (old kept, marked) |
| **conflicts-with** | *Insight* **conflicts-with** *Decision* |
| **needs** | *Intent* **needs** *Decision* |

**No generic `relates_to`** — if no real verb completes the sentence, the link isn't worth making. `replaces` and `conflicts-with` are the high-value edges: they make the graph *active*. Edges carry `type`, directional `from/to`, `source`, `note`, timestamps, validity, and `status` (active|resolved|dismissed) — which makes `conflicts-with`/`needs` a live worklist.

---

## 5. Write-policy modes

A user-facing knob for **when the assistant writes**: **Relaxed** (durable, high-value only — recommended default), **Normal** (+ Cautions, selective Insights), **Aggressive** (+ Intents, all Insights — maximum capture; what this repo dogfoods). Ships as three self-contained `SKILL.md` variants in `skills/engram/{aggressive,normal,relaxed}/`; the user installs exactly one, and can always reclassify nodes in the pane — the graph people trust is one they can correct.

---

## 6. The librarian (quality layer)

The hardest problem: **deciding what's worth a node.** In v1 Claude is the sole gatekeeper via the skill (no separate librarian LLM pass); dedupe/classify happen inline. An async librarian becomes essential when ambient hooks land. On contradiction: record a `conflicts-with` edge, never overwrite.

---

## 6A. Resolved behaviors — capture · anchors · retrieval · skill

All implemented; summary of the resolved design:

- **Capture:** worth-a-node is Claude's judgment via the skill — no objective gate. Duplicates → merge into the existing node. Search-before-write is a recommendation that matters once the graph is big; the safety net is `add_note`'s built-in same-type similarity pre-check (`{matched, created:false}` on a near-dupe → `update_node` instead).
- **Trust & decay:** Claude nodes start **provisional** (derived, not stored: claude-sourced + never approved), earn trust via cross-session reconfirmation or user approval; stale provisional episodic/volatile nodes decay out. User nodes are trusted from the start.
- **Anchors:** free-text responsibility label + optional `code_refs` file binding; flexible granularity (Claude decides); auto-created/reused; on refactor, fuzzy re-match and flag if unsure.
- **Retrieval:** session-start `brief` (budgeted digest: conflicts, open work, principles, decisions, cautions, recent adds) + on-demand `search`. Hits carry **matches + ≤5 1-hop neighbors, conflicts/replaces first**; ranking is hybrid (FTS5 OR-recall + cosine) × trust boost — trust modulates, never dominates.
- **Skill writes:** batched at natural stopping points, **silent by default** — transparency lives in the pane, not the chat; the one audible exception is a genuine contradiction surfaced by a write. Decisions are captured unprompted (feature requests usually hide one); every write response is a verdict (matched → merge, warnings → check canon, suspects → judge immediately via resolve_suspect). Both auto-invoked and manual (`/engram`). Default mode Relaxed.

---

## 6B. Operational decisions — storage · sync · lifecycle · safety

- **Storage:** per-repo `.engram/graph.db`, git-ignored; team sharing later via JSON export only. Pane reflects changes via SSE.
- **Lifecycle:** automatic flow prefers supersede (history preserved); hard delete is user-only and cascades edges; stale provisional nodes decay/archive (answers unbounded episodic growth).
- **Safety:** skill rule + backend redaction pass (regex + high-entropy) on title/body before write.
- **Bootstrap/portability:** empty start; JSON export/import ships in v1, embeddings stripped (regenerated on import) — the embedding-free dump *is* the git-shareable form.
- **Concurrency:** WAL + serialized writes; fine for local single-user.
- **Daemon:** one `engram-alpha serve` per repo, bound to its `.engram/`. Requests `--http-port` (default 8787) but **walks to the next free port** (up to +15) and records `{port, url, pid, db}` in **`.engram/daemon.json`**; clients resolve the URL from that file and verify via `/health` (which returns the served `db` path). Stale files are harmless — readers health-check before trusting.

---

## 7. Conflict handling

Staged: passive first, active later.

- **v1 (shipped):** contradictions live as `conflicts-with` edges, surfaced through retrieval (conflicting neighbors ride along on hits) and as a pane worklist. `add_note`/`update_node` return `warnings` when new text lands near conflicted/superseded nodes — the writer self-corrects in the same turn.
- **Phase 1 conflict scan (shipped v0.2.0):** detection is local and automatic (embedding cosine ≥ 0.85 + FTS overlap → `suspects` queue; write-time + 6-hourly sweep + pane "Scan now"), **judgment is cooperative** — the daemon never calls an LLM; Claude judges via `list_suspects`/`resolve_suspect` (conflict / replaces / dismiss) or the user judges in the pane. Shelling out to `claude -p` was rejected (PATH dependency, quota burn, no session context).
- **Later (planned):** proactive **mid-session push** via MCP channels — "this conflicts with an earlier decision" while Claude works. Behind a flag; opt-in.

---

## 8. Tech stack (locked: Rust + Vue, local-first)

The stable contract is the *interface* (local HTTP + MCP + SQLite).

- **Backend (Rust):** `rmcp` (MCP), `rusqlite` bundled + **FTS5** + `sqlite-vec`, `fastembed` (ONNX, bge-small class), `axum` (HTTP + SSE).
- **Frontend:** Vue 3.5 + TS + Vite + **Bun**; Pinia, vue-router, Tailwind 4, **Vue Flow**, vueuse, markdown-it + dompurify; ESLint (flat) + Stylelint. Renders the full graph (scaling deferred).
- **IDE wrappers (built):** VSCode extension (Webview, secondary sidebar) and JetBrains plugin (JCEF tool window, `dev.techtheist.engram`).
- **Distribution staging:** core loop as a plain local binary → **thin marketplace plugin** (skill + commands + config; the skill bootstraps a checksum-verified binary download from GitHub Releases after a one-time consent prompt) → full **`cargo-dist`** pipeline (installers must also install the chosen skill variant). GitHub is the single root of trust: Releases + `SHA256SUMS`, `install.sh` one-liner. All onboarding paths (JetBrains-only, VSCode-only, GitHub page, plugin-without-daemon) converge on binary→daemon→MCP→skill (later: the IDE plugins manage the daemon lifecycle themselves instead of setup cards); **no-plugin usage is first-class** (Claude Code runs everything; skill points users at the localhost pane).
- **Plugin publishing:** JetBrains `publishPlugin`; VSCode via `vsce` + Open VSX (`ovsx`) for Cursor/Windsurf — publish to both. Code-signing starts at checksums/minisign; OS signing only if Gatekeeper/SmartScreen friction bites.
- **Known caveat:** ONNX Runtime means "binary + one runtime lib per platform," not fully single-file.

---

## 9. Architecture (at a glance)

```
                 ┌─────────────────────────────────────────┐
                 │            Engram Core (Rust)            │
                 │  graph store (SQLite + FTS5 + vec)       │
                 │  RAG (hybrid search, local embeddings)   │
                 │  ── HTTP API ──┐   ┌── MCP server ───────┤
                 └────────────────┼───┼─────────────────────┘
                                  │   │
        ┌──────────────┬──────────┘   └──────────┬───────────────┐
        │              │                          │               │
   Vue frontend   Vue frontend              Claude Code      Claude Desktop
   (browser)      (Webview / JCEF)          (MCP)            (MCP)
```

---

## 10. Roadmap

**Phase 0 — Core loop** *(complete)*: Rust graph store per §4, `engram serve` — now `engram-alpha serve` — (HTTP + SSE + MCP), hybrid RAG + redaction, Vue Flow pane, JSON export/import, capture skill.

**Phase 1 — Quality + IDEs** *(complete — v0.2.0, 2026-07-06)*: conflict scan + suspects queue + Review drawer + health strip; decay pass (14-day TTL past stale-crossing, Claude-authored unapproved episodic/volatile only; similarity threshold tuned 0.75→**0.85** on the dogfood graph); node edit mode incl. type reclassification; VSCode + JetBrains wrappers (both user-tested in-IDE).

**Shipped since (v0.3.0 line, 2026-07-10)** — one line each; details in the graph:
- **Claude Code plugin** — repo self-hosts a marketplace (`.claude-plugin/marketplace.json` + `claude-plugin/`): relaxed-skill + brief-hook copies (verbatim, sync-tested), `/engram:setup` + `/engram:pane`; **deliberately no global MCP config** (would litter every repo with `.engram/` dirs) — per-repo wiring via `engram-alpha setup --mcp-only`.
- **Session-start brief hook** — `hooks/session-brief.sh`: daemon-first via `daemon.json` + `/health` DB check, CLI fallback, every failure exits 0 silently; registered via `SessionStart` (matcher `startup|clear|compact`); `engram setup` installs it. The same stdout-becomes-context contract holds on Codex/Gemini/OpenCode hooks (Phase 3 wiring).
- **Pane CRUD parity** — "+ New" create drawer, drag-to-connect with sentence-shaped verb dialog, edge retype/delete; **tags** end to end (column + FTS + `GET /tags` + editor/chips/filter + brief "Recent tags" line).
- **Audit journal** — per-mutation rows with binary-side context, pane Audit drawer, `audit` MCP tool; substrate for rollback/quarantine.
- **Verified code refs / drift** — sweep + `GET /drift` + `list_drift` + Review section + drifted badges.
- **`timeline`** — MCP tool + HTTP, replaces-chain history; `engram://node/{id}` MCP resource; brief format v2 (16k budget, ids on every record).
- **Skill overhaul** — value story, maintenance duties (close-the-loop, drift repair), example flows, unhappy-path protocol (empty search → say so; server down → surface, don't fall back to model memory; short keyword queries), across all variants.
- Four canvas layouts (Skyline default / Nebula / Archipelago / Orbit); SidePanel drawer shell; responsive pane folds for narrow IDE panels.

**Shipped since (v0.4.0 line, 2026-07-11)** — one line each; details in the graph:
- **Binary renamed `engram` → `engram-alpha`** — matches the product name so other users can find it; crate names, the MCP server key `engram`, `.engram/`, and the `/engram:` plugin namespace deliberately keep the short name (renaming them breaks existing wiring for zero discoverability gain). IDE plugins and the brief hook fall back to a pre-rename `engram` binary.
- **`engram-alpha doctor`** — store integrity (WAL / quick_check / FTS-vs-vec counts), embedding-model cache, daemon-vs-repo DB match via `/health`, and per-assistant wiring checks (`.mcp.json` binary/db validity, brief hook, gitignore, codex cwd risk). Exits non-zero on failures.
- **Codex desktop app covered** — the app (merged into the unified ChatGPT app 2026-07) shares `~/.codex/config.toml` + AGENTS.md discovery with the CLI, so `setup --cli codex` already serves it; added app-install detection, and setup/doctor warn that the app may launch MCP servers from an unexpected cwd (pin `cwd` / absolute `--db`). The app ignores project-local `.codex/config.toml` (openai/codex#13025).
- **Timeline pane view** — a History section in the node detail drawer renders the `replaces` chain oldest-first (dot timeline, current generation marked, retirement notes, click-to-jump); the fetch is gated on the node actually carrying a `replaces` edge.
- **JetBrains plugin zip ships its README**; both IDE-plugin READMEs refreshed.
- **Full-field retrieval index** — embeddings and FTS now cover tags + code_refs (embed composition v2 via PRAGMA user_version; guarded one-time re-embed on the first real-embedder open; FTS rebuild migration). A code-ref query like "policy.rs" reaches every node citing the file.
- **Bulk MCP tools** — `list_nodes` (full-fidelity paged read), `update_nodes` / `add_notes` (≤100 items, per-item results); see Appendix A.
- **System info pane view** — Settings → System info drawer rendering `GET /system`: binary version, daemon uptime/pid/repo, store health (integrity, WAL, vector/FTS coverage, embed composition), model cache, and per-assistant wiring with pre-rename flags; probes shared via `engram_core::harness`.

**Next (near-term):**
- **Bundle skill + `.mcp.json` bootstrap into the IDE plugins**; `runIde`/`verifyPlugin` for JetBrains.
- **MCP transport migration** to daemon-hosted streamable HTTP (current stdio `engram-alpha mcp` is a deliberate divergence; user owns this migration).
- **Session quarantine** — exclude a session's writes from retrieval without deleting them (journal substrate shipped; quarantined-sessions table + retrieval filter + pane toggle).
- **File-read match hook** (design TBD) — inject connected nodes when the assistant reads a file matching stored `code_refs`; needs a match endpoint + noise control (per-session dedupe, cap, trust filter).

**Phase 2 — active features:** mid-session conflict push (MCP channels), ambient capture → librarian, cross-platform release pipeline.

**Phase 3 — multi-harness memory:** first-class OpenCode / Codex CLI / Gemini CLI / Kilo CLI + Cursor/Windsurf. All speak MCP, so the backend is agnostic — the work is per-harness config bootstrap, capture-prompt equivalents, docs. Upgrades the pitch to **one shared local memory across different AI agents**. Multi-agent concurrency already handled (WAL + one daemon); verify with a smoke test.

**Later / maybe:** cross-project shared Principle layer; app-level DB encryption (deliberately far down); Obsidian export (typed edges map natively to wikilinks). **Out of scope permanently:** multi-user & repo sync — a future enterprise product, not Engram Alpha (user decision 2026-07-10).

---

## 11. Open questions

*Behavioral design resolved in §6A; remaining items are implementation-level.*

- **Skill wording:** keeps evolving as dogfooding (Aggressive) surfaces low-value nodes or ambiguous guidance.
- **Decay & promotion numbers:** implemented, untested — what counts as reconfirmation, TTLs.
- **Fuzzy re-match algorithm** for anchors after refactors (drift detection shipped; re-match still open).
- **Neighbor-cap / brief-quota tuning** as graphs grow.
- **Pane review UX:** writes are silent, so the pane is the transparency surface — Review drawer + audit journal shipped; timeline view pending.
- **Redaction coverage:** which patterns; false-positive handling.

---

## 12. Non-goals (v1)

- No companion/persona features — project memory, not a relationship.
- No custom UI inside the Claude Code chat panel (unsupported; not needed).
- No cross-project / global graph; no remote embeddings or cloud sync.
- No ambient hook *capture* in v1 — cooperative MCP first (the read-side brief hook shipped).

---

## 13. Development approach

**Dogfood from day one on Aggressive mode** — the first real user is us. **Minimalist product and code** — small surface area; clear naming and deps' public docs over inline comments.

---

## Appendix A — MCP surface (implemented)

Tools: `search` (hybrid, hits carry ≤5 1-hop neighbors, conflicts/replaces first), `brief` (budgeted session-start digest; inclusion refreshes decay clocks), `get_node`, `traverse`, `add_note` (same-type dupe pre-check → `{matched, created:false}`; `warnings` near conflicted/superseded nodes), `update_node` (same warnings), `link`/`unlink`/`update_edge` (mislink repair is Claude's to do), `approve_node`, `list_open`, `list_suspects`/`resolve_suspect`, `list_drift`, `timeline`, `audit`; bulk (v0.4.0, ≤100 items/call, per-item results — one bad item never blocks the rest): `list_nodes` (full-fidelity paged read with type/status/tag filters, archived excluded by default — the lossless read behind reviews and exports like a decisions.md; does not touch trust clocks), `update_nodes`, `add_notes` (each item runs add_note's dupe pre-check). Node hard-delete is user-only (pane/HTTP; Claude never calls it). Resources: `engram://node/{id}` (list = 25 newest + template). Exact contracts live in `crates/engram-mcp/src/lib.rs`.

## Appendix B — SQLite schema

Implemented in `crates/engram-core/src/schema.rs`: `nodes` + `edges` per §4 (ids are 12-char time-sortable base36, not UUIDs), `nodes_fts` (FTS5, incl. tags + code_refs), `vec_nodes` (sqlite-vec, 384-dim), plus `suspects` and the append-only `audit` journal. Forward migrations run on open.

## Appendix C — Repo layout

See CLAUDE.md "Where things go" — crates (`engram-core` / `engram-mcp` / `engram-http` / `engram-cli`), `frontend/`, `engram-vscode/`, `engram-jetbrains/`, `claude-plugin/` + `.claude-plugin/`, `hooks/`, `skills/engram/{aggressive,normal,relaxed}/`.
