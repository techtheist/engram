## Engram — durable project memory (MCP server: `engram`)

This project keeps a local, user-owned knowledge graph of its *reasoning*:
Decisions and their reasons, Principles, Cautions that bit us, Problems and
Resolutions, Insights, open Intents. Not code structure — the code holds that.

**Recall.** Call the `brief` tool once at session start and read it before
planning. Before any non-trivial decision, `search` the graph; hits carry
their 1-hop neighbors — read `conflicts-with` / `replaces` edges first, and
pass `parents`/`children` to `get_node` when you need the reasoning chain.
A hit marked `stale: true` has decayed trust: verify it before relying on
it, and refresh it with `update_node` if it's still accurate.

**Capture.** Capture the load-bearing knowledge: Decisions with reasons, Principles and conventions, Cautions and gotchas, resolved Problems, selective non-obvious Insights, and Intents worth surviving the session. Skip anything the code or docs already state verbatim.
Connect notes with `link` using sentence-shaped edges (because / answers /
about / builds-on / replaces / conflicts-with / needs). If `add_note` returns
`{matched, created: false}`, merge into the match with `update_node` instead
of duplicating. Batch writes at natural stopping points; don't narrate them.
Never store secrets, credentials, or volatile implementation detail (line
numbers, transient state).

**Trust.** `approve_node` is restricted: only on the user's explicit demand,
or after verifying a node's content word-by-word. Routine "still relevant"
signals are `update_node`, never approval.

The user sees and curates the graph at http://127.0.0.1:8787 — started with
`engram serve` in the repo root (one daemon per repo).
