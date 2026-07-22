# Recall & capture

The loop Engram runs for your assistant: start every session briefed, recall
on demand mid-session, capture decisions silently as they happen, and return
a verdict — not a receipt — for every write.

## The session brief

A session-start hook injects a compact digest of the graph's canon —
suspected conflicts to judge, open problems and intents, principles,
decisions, cautions, and the tag vocabulary — so the assistant doesn't start
cold and doesn't have to remember to ask. A real (trimmed) brief from this
repository:

```markdown
# Engram brief
Recent tags (reuse before inventing new ones): phase-2, hooks, pane-crud, retrieval, tags

## Suspected conflicts — judge these
- "New pane drawers must go through SidePanel — a single-element scrolling drawer
  rubber-bands its content out of the frame on macOS" [Caution] vs "Pane side drawers
  unified into SidePanel.vue: one drawer per side, drag-resizable widths" [Decision] (88% similar)

## Open problems & intents
- Session quarantine: exclude a session's writes from retrieval without deleting them [Intent open]
- Phase 2: mid-session conflict push over MCP [Intent open]

## Cautions
- engram-alpha serve must start in the repo root — a relative --db silently creates a fresh DB anywhere else
```

You can read the same brief anytime in the pane (Memory Lens), or print it
with `engram-alpha brief`. When your projects share a
[home graph](./multi-project.md), its canon rides along in every brief.

## Search that carries its context

Mid-session recall is hybrid: keyword search and local vector search fused,
sharpened by a local cross-encoder reranker, ranked by
**relevance × [trust](./trust.md)**. Vector search works at two depths:
every node has a whole-node vector, and rich bodies additionally get one
vector per sentence-sized claim — so a query matching one point buried in a
detailed decision still finds it, and compact, information-dense nodes cost
nothing in reachability. One search action covers all of it: keywords, tags,
code refs, node vectors, claim vectors. Every hit carries its 1-hop neighbors —
**conflicts and supersessions first** — so contradicting a standing decision
is hard to do by accident: the contradiction arrives attached to the search
result that would have caused it.

## Writes come back as verdicts

Capture is batched and silent — no *"I've saved a note!"* chatter. But every
write the assistant makes is checked in the same turn, and the response is a
verdict it must act on:

- **Near-duplicate** — the note matches an existing node; the assistant
  merges into it instead of creating a twin. The special case is the
  **negated duplicate**: the "duplicate" actually says the opposite
  ("use X" vs "don't use X") — flagged distinctly, because blindly merging
  it would corrupt the canon; the right move is a `conflicts-with` edge.
- **Warning** — the note landed near conflicted or superseded knowledge;
  the assistant checks the canon before proceeding.
- **Suspects** — the write queued new look-alike pairs; the assistant judges
  them immediately (see [Conflicts & Checkup](./conflicts-and-checkup.md)),
  and a genuine contradiction is the one thing it surfaces to you out loud.
- **Missing code refs** — paths that don't resolve in the repository, caught
  at write time instead of at the next drift scan.

## Memory that tracks the code

Nodes can point at code (`code_refs`). When the code moves, the memory
doesn't get to pretend otherwise: drift scans check every path-shaped ref
against the repository, and a ref that no longer resolves badges its node as
**drifted** — in the pane, in the health strip, and in the assistant's
`list_drift` worklist. The contract is repair-or-retire: fix the path if the
knowledge still holds, supersede it if the refactor invalidated it.

## Subagents

Subagents share the MCP connection: they can search and read the brief, but
they start cold (no injected context) and their writes attribute to the
parent session. Recall flows down through the prompts you give them; capture
flows back up through their findings.
