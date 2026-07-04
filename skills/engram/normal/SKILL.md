---
name: engram
description: Read and write the project's durable reasoning memory (decisions, principles, cautions, problems, insights) through the Engram MCP tools. Recall relevant memory before non-trivial work; capture durable knowledge silently at natural stopping points. Normal variant — balanced capture.
---

# Engram — project memory (Normal)

Engram is a local, user-owned graph of *why things are the way they are* in this project: decisions and their reasons, gotchas that bit us, problems and how they were solved, and stable preferences. It is **not** a place for code structure or volatile implementation detail — the codebase already holds those.

**This is the Normal variant: balanced capture.** Save what a future session would genuinely want to know; skip noise.

Claude Code already has memory of its own (CLAUDE.md, auto-memory) — **don't mirror it**. Engram is additional: it holds the project's *reasoning* — decisions with reasons, conflicts, gotchas — not user preferences, session workflow, or code structure.

You interact with it through the `engram` MCP tools. Two jobs: **recall** (read before you act) and **capture** (write durable knowledge after you act).

## Recall — brief first, then search

- **At the start of a session**, call `brief` once: a compact digest of the canon — unresolved conflicts, open problems/intents, principles, decisions, cautions, recent changes. Read it before planning anything.
- Before any **non-trivial decision**, call `search` with a natural-language description of what you're about to do. Hits carry their **1-hop neighbors, `conflicts-with`/`replaces` first** — read those especially. If a prior Decision or Caution covers your situation, follow it or, if you're about to contradict it, surface that to the user.
- Use `get_node` / `traverse` to pull the reasoning around a hit (e.g. a Decision and the Principle it stands on).
- `list_open` shows the live worklist (open Problems and Intents) — check it when picking up work.

## Cold start — the graph is empty

When `brief` reports a cold start (empty graph), **offer the user a one-time
seeding pass** — this is the one capture that must not be silent. With their
go-ahead: read the project's existing canon (README, plan/design docs, recent
git history) and batch-capture the durable knowledge as provisional nodes —
key Decisions with their `because` reasons, stated Principles and conventions,
known Cautions, open Intents — attached to Anchors where several notes share a
subject. Seed the load-bearing canon; skip anything the code or docs already state verbatim. Point the user at the pane to review the seeded graph. If
they decline, don't ask again; capture knowledge as it emerges.

## Answering "why" — retell the reasoning chain

When the user asks *"why did we decide X?"* or *"why is it like this?"*: `search` the topic, then `traverse` the hit along `because`, `replaces`, and `answers` edges and retell the chain as a short narrative — the decision, its reason, what it replaced, and what problem drove it. Include dates when the history matters.

## Compiling docs from the graph

When the user asks for a decision log / `DECISIONS.md`: walk the current (non-superseded) Decisions with their `because` reasons (grouped by Anchor where it helps), render an ADR-style markdown file, and note supersessions inline. The graph stays personal; the compiled doc is the shareable artifact. Don't commit it unasked.

## Capture — what is worth a node

**Save:**
- **Decision** — choices made for a reason, including finer-grained ones when the reason isn't obvious from the code. The backbone of the graph.
- **Principle** — a stable preference / convention / taste.
- **Caution** — a gotcha, constraint, or spec-as-rule that will bite later.
- **Problem** + **Resolution** — when the problem was genuinely tricky or its solution non-obvious. Skip routine fixes.
- **Insight** — selectively: realizations you'd want back in a month.
- **Intent** — deferred work clearly worth carrying across sessions.

**Never save:**
- Secrets, credentials, tokens, PII — *ever*. (The backend also redacts, but you are the first line.)
- Volatile implementation detail (line numbers, transient state) unless the user explicitly asks.
- Mirrors of what code, git history, or CLAUDE.md already record.

## How to write

1. **Avoid duplicates — proportionally.** On a small graph, or right after you've already searched/recalled the area, write directly: `add_note` self-checks similarity and returns `{ matched, created: false }` instead of duping — then `update_node` the match. **Search first when the graph has grown large or the topic is plausibly already covered.**
2. **Pick the type** from the list above. Don't invent types — there are exactly 8 (the 7 above + **Anchor**).
3. **Title**: a short, declarative label. **Body**: the reasoning in 1–3 sentences — the *why*, not a transcript.
4. **Link it.** Edges must read as an English sentence: subject → verb → object. Use:
   - `because` — Decision/Caution **because** Principle (the reason).
   - `answers` — Resolution **answers** Problem.
   - `about` — any node **about** an Anchor (a code subject).
   - `builds-on` — Insight **builds-on** Insight.
   - `replaces` — Decision **replaces** Decision (supersession; the old one stays as history).
   - `conflicts-with` — when two nodes contradict. **High value — always create this** when you notice a contradiction.
   - `needs` — Intent **needs** Decision (a dependency/blocker).
   - If you can't complete the sentence with one of these verbs, don't link.
5. **Anchors** are free-text subjects ("auth flow", "the RAG layer"). Create/reuse one and attach nodes with `about` when several notes concern the same area. Optionally pass `code_refs` (responsibilities/paths, **never** line numbers).
6. **Read write results.** `add_note`/`update_node` may return `warnings`: your note landed near a node that is `in-active-conflict` or `superseded`. Check the flagged node — align with the canon, or record the contradiction deliberately with a `conflicts-with`/`replaces` edge.
7. **Repair mislinks.** A wrong edge (bad verb, wrong endpoints) is yours to fix: `unlink` deletes it; `update_edge` changes its status (`resolved`/`dismissed` for settled conflicts), note, or confidence.

## Durability — let it default

Usually let durability default from the type (Principle/Decision/Caution/Anchor → `stable`; Problem/Resolution/Insight → `episodic`; Intent → `volatile`). Override only with a reason. **Never** create `volatile` notes unless the user explicitly asks.

## Trust & staleness

- Nodes you create start **provisional** (lower confidence) and earn trust by being **reconfirmed** — `update_node` them in a later session because they're still relevant — or by user approval in the pane. Stale provisional nodes decay out.
- Practical effect: when `search` surfaces an existing node that's still correct, **`update_node` it** (even a small body refinement) instead of re-writing it — that reconfirmation promotes it and keeps it alive.

## The daemon & where the user sees memory

The graph UI is served by the local daemon — `engram serve`, one per repo, started in the repo root, default `http://127.0.0.1:8787`. If the default port is taken (another repo's daemon), the daemon takes the next free port and records the real one in `.engram/daemon.json` — **read that file first** when you need the URL. Your stdio MCP connection works without the daemon; it exists for the human.

- If the user asks **where to see the memory** ("where did you save that?", "show me the graph"): point them to their IDE's Engram panel, or the pane at `http://127.0.0.1:8787` (mind a custom `--http-port`).
- If the daemon isn't running (health check on that URL fails), **start it yourself**: run `engram serve --http-only` in the repo root as a background process, then share the URL.
- If the `engram` binary is missing entirely, don't improvise an install — point the user at the project's GitHub releases / README instructions.

## Timing & etiquette

- **Batch at natural stopping points** — task or sub-task done, end of turn. Never interrupt mid-flow to write.
- **Be silent.** Don't announce captures or narrate the graph in chat. The graph pane is the transparency surface. (You *may* mention a capture if the user explicitly asks what you saved.)
- A manual `/engram` invocation means the user wants an explicit "save this" or "recall X" right now — honor it directly.
