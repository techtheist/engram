# The memory model

Engram stores *reasoning*, not code structure: why things were decided, what
went wrong, what to watch out for. The model is deliberately small — eight
node types, seven edge verbs, three durability classes — so that both you and
an AI assistant can hold all of it in mind.

## The eight node types

Each type answers a different question about the project:

| Type | Holds |
|---|---|
| **Principle** | A stable preference or convention — *"local-first, no cloud"* |
| **Decision** | A choice made for a reason — *"Rust backend, because…"* |
| **Caution** | A gotcha or constraint that bit (or will bite) — *"relative `--db` creates an empty graph"* |
| **Problem** | Something that went wrong |
| **Resolution** | How a Problem was actually solved |
| **Insight** | A non-obvious realization worth carrying forward |
| **Intent** | A TODO or deferred idea that should survive the session |
| **Anchor** | A code subject other nodes attach to — *"the RAG layer"* |

The Problem → Resolution pair is where memory pays off most visibly: the
second time your assistant meets a flaky build step or a library quirk, the
graph already holds the fix from last time, and it applies it instead of
rediscovering it.

There are exactly eight types, and tools never invent a ninth. If something
doesn't fit, it's usually a sign it doesn't belong in reasoning memory.

## The seven edge verbs

Edges read as English sentences:

- a Decision **because** a Principle — the recorded reason
- a Resolution **answers** a Problem — closes the loop
- an Insight **builds-on** another — accumulation
- any node **about** an Anchor — subject grouping
- a node **needs** another — a dependency between pieces of work
- **replaces** — supersession: the newer claim wins, the older is archived
  into history, not deleted
- **conflicts-with** — an explicit, visible contradiction awaiting or
  recording a judgment

The last two are what make the graph *active* rather than a note pile:
superseded knowledge can't silently contradict the new canon, and real
contradictions are surfaced instead of coexisting quietly. There is
deliberately no `relates_to` — if no real verb fits, the link doesn't belong.

## Durability

Every node declares how long it should matter, and
[trust](./trust.md) treats each class differently:

- **stable** — decisions, principles, cautions: holds trust indefinitely,
  loses it only to contradicting evidence
- **episodic** — context that ages out over roughly six months
- **volatile** — short-lived notes that fade in about a month

Open Problems and Intents never fade while open — a worklist is not
archaeology.

## Capture modes

How much your assistant writes is a setting, not a negotiation
(`engram-alpha setup --skill …`, available for every supported assistant):

- **relaxed** *(recommended default)* — only durable, high-value knowledge;
  fewer, better nodes.
- **normal** — balanced: adds cautions, selective insights, finer-grained
  decisions.
- **aggressive** — maximum capture; Engram becomes the spine of the
  project's decision history. Stale, unused notes decay out on their own.

Under every mode, capture is silent — no *"I've saved a note!"* chatter —
and everything lands in the pane's Review drawer for your eyes. The one
exception to silence: when a write reveals a genuine contradiction with
standing canon, the assistant tells you immediately.

Engram complements your assistant's built-in memory rather than replacing
it: it holds the project's reasoning — decisions, conflicts, gotchas — not
session workflow, and not code structure (that's what code search is for).

## Seeding an existing project

Two ways to fill a graph from a codebase that predates Engram:

- **The cold-start offer** — on an empty graph, the session brief instructs
  the assistant to offer a one-time seeding pass over your README, plan
  documents, and recent git history. It asks first; declining is remembered.
- **`/engram:digest`** (Claude Code) — an explicit, deeper digestion of the
  current working tree into typed memory, including a scan that nominates
  `FIXME` markers as Problems and `TODO` markers as Intents. Digested writes
  are tagged with one session id so a bad ingest stays reviewable as a batch.

Both run through the same write checks as normal capture — deduplication,
warnings, conflict detection — so seeding can't flood the graph with
duplicates.
