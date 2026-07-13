---
name: engram-digest
description: Seed or top up this project's Engram graph from the existing codebase — an explicit, user-invoked digestion of the current working tree into typed memory nodes (decisions, principles, cautions, problems, insights, intents). Use when the user says "digest this project", "ingest the codebase into memory", "seed the graph", or invokes /engram:digest. Not for routine capture during work — the engram skill covers that.
---

# Engram digest — ingest an existing project

Digestion turns a codebase that predates its memory graph into a first graph. It is **explicit and opt-in**: it runs only when the user asks, never automatically. One digestion is a bounded pass, audibly reported at the end — the opposite of the engram skill's silent trickle capture.

**The target is the current working tree** — the code and docs that exist *now* on this branch. Git history is supporting material only (the *why* behind what you see), never a deep crawl of old commits. And Engram is **reasoning memory, not a code map**: digest the decisions, constraints, and gotchas the code embodies — not its structure, which the code itself already records. Quality over coverage; a good first digest is dozens of well-linked nodes, not hundreds of thin ones.

## Procedure

**0. Preflight.** The `engram` MCP tools must be available (if not, the repo isn't wired — run `engram-alpha setup`, or the plugin's `/engram:setup`, and stop). Agree the scope with the user if they gave one ("just the backend"); default is the whole tree. Choose the ingest session id now: `digest-<YYYY-MM-DD>` — **every write in this pass carries it** (see Session marking).

**1. Recall first.** Call `brief` (or read the injected one). If the graph already has nodes, digestion tops up: `search` each area before writing about it, and lean on the dupe guard — never re-state what the graph already holds.

**2. Offline marker scan (tier 1).** The daemon scans the tree for `FIXME`/`TODO` markers, gitignore-aware:

```sh
PORT=$(sed -n 's/.*"port"[: ]*\([0-9]*\).*/\1/p' .engram/daemon.json)
curl -s -X POST "http://127.0.0.1:${PORT}/digest/scan"
```

(No `.engram/daemon.json` or the health check fails → start it first: `engram-alpha serve --http-only` from the repo root, backgrounded. A 404 from `/digest/scan` means an older binary — `engram-alpha update`, restart the daemon, retry once.)

The response is nominations, not nodes: `candidates` with `marker`, `suggested_type` (FIXME → Problem, TODO → Intent), redacted `text`, `file`, `line`. **You are the judge.** For each candidate: skip vendored/generated/dead markers and trivial notes-to-self; when the text is too thin to write a real title, read the surrounding code first; then write the survivors — Problems get `status: "open"`, Intents stay volatile, `code_refs` = the candidate's `file` (path only, never the line number). `truncated: true` means the cap cut the walk short — digest what you have, then run the scan again.

**3. Read the reasoning surfaces (tier 2 — the real material).** README, design/plan docs and ADRs if present, top-level configs and manifests (lockfiles and CI reveal locked tool choices), the heads of the main modules. Use `git log --oneline -30` and targeted `git log --follow <file>` only to recover the *why* behind something you found in the tree. From this, extract: choices made for a reason (Decisions), stated or evident conventions (Principles), constraints and gotchas (Cautions), non-obvious realizations (Insights), unfinished threads (Intents), known open issues (Problems).

**4. Author nodes by the worked examples below.** Batch related notes into `add_notes` calls (≤100 items, per-item verdicts). Link as you go — an unlinked pile is not a graph.

**5. Act on every verdict.** Digestion gets **no bulk bypass**: each write's response carries the same duties as normal capture. `{matched, created: false}` → merge into the match with `update_node`, never re-add. `warnings` → read the flagged node before proceeding. `suspects` → judge each with `resolve_suspect` *now* (contradiction → `conflict` and tell the user; your note is the fresher truth → `replaces`; fine together → `dismiss`). `missing_code_refs` → fix or drop those paths. Leaving verdicts unhandled is how an ingest rots the graph on day one.

**6. Report.** Digestion is audible: summarize what was ingested (counts by type, the anchors created, anything contradictory found), remind the user everything is provisional and reviewable in the pane (`/engram:pane` or `http://127.0.0.1:<port>`), and that the audit log filters by this ingest's session id.

## Session marking

Pass `session_id: "digest-<YYYY-MM-DD>"` explicitly on **every** `add_note` / `add_notes` item. The whole ingest then reads as one batch in the audit journal — reviewable, attributable, and revocable as a unit if it turns out bad. One digestion, one session id; a re-run on another day gets its own.

Ingested nodes start **provisional at 50% trust** like any Claude-authored node and earn trust only through later reconfirmation or user approval. Never `approve_node` your own ingest, and never present digested content as settled canon.

## The eight node types — worked examples

One example per type; each also teaches one feature of the graph. **The shapes are load-bearing**: copy the example, replace the content with your project's fact, and keep everything else — type names, edge verbs, durabilities, and statuses are exactly as written here (there are only 8 types and 7 verbs; inventing others fails the write).

**Principle** — a stable conviction the project keeps honoring. Teaches: `durability`, and that principles are what `because` edges point at.

```json
add_note {"type": "Principle", "title": "Prefer boring, inspectable storage over clever caching",
  "body": "Every past cache layer here got stale-data bugs; plain reads from the source of truth were always fast enough. Optimize only with a measured need.",
  "durability": "stable", "session_id": "digest-2026-07-13"}
```

**Decision** — a choice made for a reason. Teaches: the core triple — *Decision `because` Principle* — link every decision to its reason when the reason is in the graph.

```json
add_note {"type": "Decision", "title": "Sessions live in httpOnly cookies, not localStorage",
  "body": "Chosen over localStorage tokens: XSS cannot read httpOnly cookies, and the CSRF cost is covered by same-site=strict.",
  "session_id": "digest-2026-07-13"}
link {"from": "<decision-id>", "to": "<principle-id>", "type": "because"}
```

**Caution** — a constraint or gotcha that bites. Teaches: extract the *why* into the body (a rule without its reason gets relitigated), and `tags` for the slice it belongs to.

```json
add_note {"type": "Caution", "title": "The payment webhook redelivers without idempotency keys",
  "body": "The provider retries on any non-200 for 24h. Every handler must be replay-safe or invoices double-post — bit us in production twice.",
  "tags": ["payments", "webhooks"], "session_id": "digest-2026-07-13"}
```

**Problem** — something known to be wrong, not yet solved. Teaches: open `status` — open Problems are the live worklist and never decay while open. This is what a judged FIXME candidate becomes.

```json
add_note {"type": "Problem", "title": "Invoice import drops rows on duplicate external ids",
  "body": "From FIXME in the importer: the upsert silently ignores conflicts instead of merging. Affects re-imports after provider outages.",
  "status": "open", "code_refs": ["src/billing/import.py"], "session_id": "digest-2026-07-13"}
```

**Resolution** — how a problem actually got solved. Teaches: `answers` + closing the loop, and supersession: when the tree shows the *current* shape and history shows an older approach it replaced, capture the current one and link it `replaces` the old node with the why in the edge note.

```json
add_note {"type": "Resolution", "title": "Imports are keyed by (provider, external_id) with explicit merge on conflict",
  "body": "Replaced the silent ignore with a merge that keeps the newest line items; re-imports are now idempotent.",
  "session_id": "digest-2026-07-13"}
link {"from": "<resolution-id>", "to": "<problem-id>", "type": "answers"}
update_node {"id": "<problem-id>", "status": "resolved"}
```

**Insight** — a non-obvious realization worth carrying forward. Teaches: `builds-on` chains insight to the insight it extends.

```json
add_note {"type": "Insight", "title": "The importer is single-threaded by design — ordering is the contract",
  "body": "Line items must apply in provider order; a parallel importer passed every unit test and still corrupted running balances.",
  "session_id": "digest-2026-07-13"}
link {"from": "<insight-id>", "to": "<earlier-insight-id>", "type": "builds-on"}
```

**Intent** — deferred work worth surviving the session. Teaches: volatile durability (the Intent default — let it default; unresolved month-old maybes should fade). This is what a judged TODO candidate becomes.

```json
add_note {"type": "Intent", "title": "Add backoff with jitter to the export job's retries",
  "body": "From TODO in the exporter: fixed 1s retries hammer the API during provider incidents.",
  "code_refs": ["src/export/job.ts"], "session_id": "digest-2026-07-13"}
```

**Anchor** — a free-text subject that clusters related nodes. Teaches: `about` (the only edge that points at Anchors, and Anchors are its only valid target) and `code_refs` — repo-relative paths that get drift-checked when code moves, so keep them real and never include line numbers. Create an Anchor the moment three digested nodes share a subject.

```json
add_note {"type": "Anchor", "title": "billing pipeline",
  "code_refs": ["src/billing/"], "session_id": "digest-2026-07-13"}
link {"from": "<problem-id>", "to": "<anchor-id>", "type": "about"}
link {"from": "<decision-id>", "to": "<anchor-id>", "type": "about"}
```

If no edge verb completes an honest English sentence between two nodes, leave them unlinked — a forced edge is worse than none.

## Guardrails

- **Bias hard toward reasoning types.** Decisions, Principles, Cautions, Insights are the payload; marker-derived Problems/Intents are the floor, not the ceiling. Never digest code structure, file inventories, or volatile implementation trivia.
- **No secrets, ever.** Old docs and history are where keys leak. The backend scrubs every field and the scan redacts marker text, but you are the first line — if a fact can't be stated without its credential, it isn't memory.
- **Idempotency.** Re-running a digest must not duplicate: the dupe guard catches near-identical restatements, and step 1's search-before-write covers the rest. On a re-run, prefer `update_node` (which also reconfirms) over new nodes.
- **Don't inflate.** If the tree offers little reasoning to digest (a fresh scaffold, a mirror of vendored code), say so and stop — an honest small graph beats a padded one.
