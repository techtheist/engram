# Trust & decay

Memory tools die from noise: after a few months, a note pile contains enough
stale and contradictory material that you stop trusting any of it. Engram's
answer is a computed trust value on every node — derived live from the
node's timestamps, so no background process has to run for the graph to stay
honest.

Two rules decide what may move trust.

## Time doesn't validate

Whether trust fades at all depends on the node's
[durability](./memory-model.md#durability):

- **Stable** knowledge (decisions, principles, cautions) **holds its trust
  flat forever**. A production constraint that only matters during one
  migration a year later is exactly as trusted on that day as when you wrote
  it. It loses trust only to *evidence*: a judged `conflicts-with` edge
  against a newer claim starts a decay ramp. Editing or approving the node
  clears the demotion — repair is re-validation — and so does withdrawing
  the conflict itself (dismiss it, resolve it, or delete the edge).
- **Episodic** notes fade over about six months; **volatile** ones over
  about a month.
- Open Problems and Intents never fade while open.
- Drifted code refs are surfaced for review but never demote a node on their
  own: the drift scan depends on where the daemon runs, and a wrong working
  directory must not be able to bury your graph.

## Exposure doesn't validate

Retrieval — a search hit, inclusion in the session brief — records a
`last seen` timestamp for observability, but never refreshes trust. Being
findable proves a note was findable, not that it's true. Trust anchors move
only on deliberate acts:

| Act | Trust |
|---|---|
| Just written | **50%** |
| **Confirmed** — any deliberate edit, or one click of *Confirm still true* in the pane | restarts at **60%** |
| **Approved** — by you in the pane, or by the assistant only on your explicit demand | restarts at **100%**; on stable knowledge, holds there until evidence says otherwise |
| **Pinned** — 📌 in the pane, yours alone | locked at 100% (or any constant you set) |

Pinned nodes never decay, never auto-archive, and contradicting evidence can
only *flag* them for your review — nothing silently demotes a pin. They're
badged on the canvas and filterable as their own slice.

The same rule binds the local models: no model verdict — NLI, similarity,
reranker — ever moves a trust value. Models nominate candidates for the
review queues; only human or assistant *judgments* change anything.

## Stale, and what happens to it

Below **30%** a node is **stale**: badged in the pane, flagged to the
assistant in search results and the brief, and queued in Review for your
verdict — refresh it, supersede it, or delete it. A decay pass archives
long-stale scratch that nobody ever approved (assistant-written, episodic or
volatile only). Nothing is ever silently removed, superseded knowledge stays
readable as history, and hard-delete remains yours alone.

Every card in the pane explains its own number in plain words — *"Confirmed
3 months ago, holding at 60%: stable knowledge does not decay with time"* —
so *why is trust this?* never needs a manual.

## How search uses trust

Results rank by **relevance × trust**, with a small recency bonus, so fresh,
living knowledge wins near-ties against stale look-alikes — but an
irrelevant, well-trusted node can never outrank an actual match, because
trust multiplies relevance instead of adding to it.

The net effect: capture can be liberal, because a wrong-but-attractive note
can't keep itself alive by being retrieved — it fades, or dies of a judged
conflict — while the rare true constraint survives its quiet year untouched.
