---
name: engram
description: Read and write the project's durable reasoning memory (decisions, principles, cautions, problems, insights) through the Engram MCP tools. Recall relevant memory before non-trivial work; capture decisions and knowledge silently at natural stopping points; keep the graph honest (judge suspects, close answered problems, repair drifted refs). Aggressive variant — maximum capture.
---

# Engram — project memory (Aggressive)

Engram is a local, user-owned graph of *why things are the way they are* in this project: decisions and their reasons, gotchas that bit us, problems and how they were solved, stable preferences. Not code structure, not implementation detail — the codebase already holds those.

**What good capture buys.** A session that recalls well starts where the last one stopped: settled decisions don't get relitigated, known rakes don't get stepped on twice, and "why is it like this?" gets a real answer instead of archaeology. A session that captures well pays that forward. And because the graph is a pane the user curates — not hidden plumbing — every node you write is something they will *see*. That's also the failure mode to respect: a graph that's noisy or wrong stops being trusted, and an untrusted graph stops being read. Quality of nodes, honesty of edges, and closed loops matter more than volume.

**This is the Aggressive variant: Engram is the spine of the project's decision history.** Every real decision, realization, and gotcha belongs in the graph. Err on the side of writing — trust decay prunes what never gets reused, so an unneeded node costs little, while a missing one costs a repeated mistake.

Claude Code already has memory of its own (CLAUDE.md, auto-memory) — **don't mirror it**. Engram is additional and holds the project's *reasoning graph* — the durable why that outlives any session or tool. In Aggressive mode, project decisions go to **Engram first**; leave user preferences and session workflow to Claude Code's memory, and code structure to the code.

You interact with it through the `engram` MCP tools. Three jobs: **recall** (read before you act), **capture** (write durable knowledge after you act), and **maintenance** (keep what's already there honest).

## Recall — brief first, then search

- **At the start of a session**, call `brief` once: a compact digest of the canon — unresolved conflicts, suspects to judge, recent changes, the open worklist, principles, decisions, cautions. Every record carries its node id; act on ids directly. **If the session already opens with an injected "# Engram brief"** (the session-start hook provides it), that IS the brief — read it and don't call the tool again.
- Before any **non-trivial decision**, call `search` with a natural-language description of what you're about to do. Hits carry their **1-hop neighbors, `conflicts-with`/`replaces` first** — read those especially. If a prior Decision or Caution covers your situation, follow it or, if you're about to contradict it, surface that to the user.
- Use `get_node` / `traverse` to pull the reasoning around a hit (e.g. a Decision and the Principle it stands on).
- For **history**: `timeline` walks a node's `replaces` chain oldest-first, each retired generation carrying the note that explains why it was replaced. `audit` pages the mutation journal — "what changed while I was away", "who wrote this".
- `list_open` shows the live worklist (open Problems and Intents) — check it when picking up work.

## Maintenance — keep the graph honest

- **Judge suspects early.** When the brief lists Suspected conflicts, resolve them with `resolve_suspect` before diving into work. The scan only finds look-alikes; you are the judge:
  - The two claims contradict → `conflict`.
  - The newer restates the older with fresher truth → `replaces` (archives the older).
  - They're **complementary** — most often a Resolution or Decision next to the Intent/Problem it *implements* → `dismiss`, then make sure the real relationship exists (`answers` edge) and the implemented item is closed.
- **Close the loop.** Whenever a Resolution `answers` a Problem or Intent, also set the answered node's `status` to `resolved` (`update_node`) — unless work genuinely remains. Open worklist items that are actually done pollute every future brief.
- **Repair drift.** `list_drift` names nodes whose path-shaped `code_refs` no longer exist — the code moved and the memory didn't. For each: fix the paths via `update_node`, and *re-read the claim* — if the code change invalidated the knowledge itself, supersede or `conflicts-with` it instead of just fixing the path.
- **Stale hits** (`stale: true`): trust has decayed — verify before relying. Still true → `update_node` refreshes it; wrong → supersede or `conflicts-with`.

## Cold start — the graph is empty

When `brief` reports a cold start (empty graph), **offer the user a one-time
seeding pass** — this is the one capture that must not be silent. With their
go-ahead: read the project's existing canon (README, plan/design docs, recent
git history) and batch-capture the durable knowledge as provisional nodes —
key Decisions with their `because` reasons, stated Principles and conventions,
known Cautions, open Intents — attached to Anchors where several notes share a
subject. Seed thoroughly — decisions, conventions, gotchas, open threads; trust decay will prune what never gets reused. Point the user at the pane to review the seeded graph. If
they decline, don't ask again; capture knowledge as it emerges.

**But first**: if the project plainly *should* have memory (an `.engram/` directory exists, the pane has nodes, or history says so), an empty brief means something is wrong — usually the wrong working directory or DB path. Check you're in the repo root and what `.engram/daemon.json` says before seeding; never seed a duplicate graph next to a real one.

## Answering "why" — retell the reasoning chain

When the user asks *"why did we decide X?"* or *"why is it like this?"*: `search` the topic, then follow `because` / `answers` edges (`get_node`, `traverse`) and — when the decision has history — `timeline` for the supersession chain. Retell it as a short narrative: the decision, its reason, what it replaced and why, and what problem drove it. Include dates when the history matters.

## Compiling docs from the graph

When the user asks for a decision log / `DECISIONS.md`: walk the current (non-superseded) Decisions with their `because` reasons (grouped by Anchor where it helps), render an ADR-style markdown file, and note supersessions inline. The graph stays personal; the compiled doc is the shareable artifact. Don't commit it unasked.

## Capture — what is worth a node

Capture **liberally**:
- **Decision** — *every* choice made for a reason, including fine-grained ones ("we picked X over Y because Z"). The backbone of the graph.
- **Principle** — any stable preference / convention / taste you observe or are told.
- **Caution** — gotchas, constraints, specs-as-rules — including **proactive** ones you foresee will bite.
- **Problem** + **Resolution** — anything non-trivial that went wrong and how it got solved.
- **Insight** — every non-obvious realization worth carrying forward.
- **Intent** — every TODO / deferred idea worth surviving the session.

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
   - `answers` — Resolution **answers** Problem (then close the Problem — see Maintenance).
   - `about` — any node **about** an Anchor. **Anchors only** — never point `about` at another node type.
   - `builds-on` — Insight **builds-on** Insight.
   - `replaces` — Decision **replaces** Decision (supersession; the old one stays as history). Put the *why of the change* in the edge note — `timeline` shows it later.
   - `conflicts-with` — when two nodes contradict. **High value — always create this** when you notice a contradiction.
   - `needs` — Intent **needs** Decision (a dependency/blocker).
   - If you can't complete the sentence with one of these verbs, don't link. An honestly unlinked node beats a forced edge.
5. **Anchors at write time.** Anchors are free-text subjects ("auth flow", "the RAG layer"). The moment a batch contains two or three notes on one subject, create/reuse the Anchor and attach them with `about` — anchors never accrue by themselves, and unanchored clusters are what makes the pane unreadable later. Optionally pass `code_refs` (repo-relative paths or responsibilities, **never** line numbers; path-shaped refs get drift-checked, so keep them real).
6. **Read write results.** `add_note`/`update_node` may return `warnings`: your note landed near a node that is `in-active-conflict` or `superseded`. Check the flagged node — align with the canon, or record the contradiction deliberately with a `conflicts-with`/`replaces` edge.
7. **Repair mislinks.** A wrong edge (bad verb, wrong endpoints) is yours to fix: `unlink` deletes it; `update_edge` changes its status (`resolved`/`dismissed` for settled conflicts), note, or confidence.

## Example flows — imitate these

**Recall before work.** User: "let's switch the pane to WebSockets."
→ `search("SSE websocket pane live updates")` → hit *"Source SSE from the Engine change-listener"* [Decision] with a `because` neighbor. Surface it: "There's a standing decision to use SSE (one-way, fits the shared daemon). Switching supersedes it — proceed?" Only after a yes: implement, then `add_note` the new Decision and `link` it `replaces` the old, edge note = why it changed.

**Capture at a stopping point.** A genuinely tricky bug just got fixed:
→ `add_note(Problem, "Audit rows attributed node updates to the creator session", body: what was wrong and how it stayed hidden)`
→ `add_note(Resolution, "audit_node stamps the acting session; the node's own session only marks its created row")`
→ `link(resolution, problem, answers)` → `update_node(problem, status: "resolved")` — the loop is closed.
Three nodes now concern the same subject? `add_note(Anchor, "Audit journal")` and `about`-link them.

**Judging a suspect pair.** Brief: *"Audit journal shipped" [Resolution] vs "Append-only audit journal" [Intent] (87%)*. They don't contradict — the Resolution implements the Intent → `dismiss`, then verify the `answers` edge exists and the Intent's status is `resolved`. Same scan, different pair: two Decisions stating opposite rules → `conflict`. A fresher restatement of an old claim → `replaces`.

**"Why is trust computed this way?"**
→ `search("trust decay")` → `timeline(hit.id)` → retell oldest-first: "Originally a daily decay sweep per durability class; replaced by read-time trust from three timestamps — the replaces note says a daemon can't be assumed to be running."

## Durability — let it default

Usually let durability default from the type (Principle/Decision/Caution/Anchor → `stable`; Problem/Resolution/Insight → `episodic`; Intent → `volatile`). Don't *override* durability to `volatile` on your own — types that default there (Intent) are the only volatile notes you create unasked.

## Trust & staleness

Trust is **computed from timestamps**, not stored: a never-surfaced node starts at 50% and fades over half a year; once retrieval surfaces it (search hits and the brief stamp `last_seen` automatically), it restarts at 60% on the same fade; an **approved** node restarts at 100% and fades slowly to 20% over a year. Below 30% a node is **stale** — search results and the brief mark it (`stale: true` / `STALE`).

- **`approve_node` is restricted**: call it ONLY on explicit user demand ("approve this", "yes that's still right") or after verifying the node's content word-by-word against current reality. Routine still-relevant signals are `update_node`, never approval.
- Practical effect: knowledge that keeps getting retrieved stays alive by itself; what never surfaces fades to stale and waits in the pane's review queue for the user.

## When something goes wrong

- **The engram tools vanish mid-session** (MCP server disconnected): never drop a capture silently. The daemon speaks the same language over HTTP — read `.engram/daemon.json` for the URL and `POST /nodes` with the same fields plus `"source": "claude"` and your session id. No daemon either? Summarize what you would have captured at the end of the turn and ask the user to reconnect (`/mcp`) — this is the one time memory work may be audible.
- **An id errors as not found**: ids come only from the brief, `search`, or `get_node` output — never guess, shorten, or reconstruct one.
- **`add_note` returns `{ matched, created: false }`**: that's the dupe guard, not an error — merge your content into the match with `update_node`.
- **The brief is empty but shouldn't be**: wrong cwd or DB path — see the cold-start guard above before writing anything.

## The daemon & where the user sees memory

The graph UI is served by the local daemon — `engram serve`, one per repo, started in the repo root, default `http://127.0.0.1:8787`. If the default port is taken (another repo's daemon), the daemon takes the next free port and records the real one in `.engram/daemon.json` — **read that file first** when you need the URL. Your stdio MCP connection works without the daemon; it exists for the human.

- If the user asks **where to see the memory** ("where did you save that?", "show me the graph"): point them to their IDE's Engram panel, or the pane at `http://127.0.0.1:8787` (mind a custom `--http-port`).
- If the daemon isn't running (health check on that URL fails), **start it yourself**: run `engram serve --http-only` in the repo root as a background process, then share the URL.
- If the `engram` binary is missing entirely, don't improvise an install — point the user at the project's GitHub releases / README instructions.

## Timing & etiquette

- **Batch at natural stopping points** — task or sub-task done, end of turn. Never interrupt mid-flow to write.
- **Be silent.** Don't announce captures or narrate the graph in chat. The graph pane is the transparency surface. (You *may* mention a capture if the user explicitly asks what you saved.)
- A manual `/engram` invocation means the user wants an explicit "save this" or "recall X" right now — honor it directly.

## Maintaining this skill

This skill is a living document and the project is dogfooding it. **When you notice it steering you wrong** — a rule that produced a low-value node, guidance that's ambiguous mid-task, a missing case — **edit this file** to fix it, and note what changed and why.
