# The memory model

Engram stores *reasoning*, not code structure: why things were decided, what
went wrong, what to watch out for. The model is deliberately small — nine
node types, seven edge verbs, three durability classes — so that both you and
an AI assistant can hold all of it in mind.

## The nine node types

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
| **Tombstone** | A record that knowledge was deliberately removed — *"we dropped the vendor SDK, don't resurrect it"*. Not the same as superseded: a superseded note has a successor to follow; a tombstoned one must not come back |

The Problem → Resolution pair is where memory pays off most visibly: the
second time your assistant meets a flaky build step or a library quirk, the
graph already holds the fix from last time, and it applies it instead of
rediscovering it.

Your assistant never invents a tenth type — a write can only use a type the
graph declares. These nine are the shipped default, and they're what most
projects should keep; if a note doesn't fit one of them, that's usually a
sign it doesn't belong in reasoning memory. If your project genuinely wants a
different vocabulary, the whole set is yours to reshape — see
[Customization](./customization.md).

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

Like the node types, these seven are the default set. What the engine
actually keys on is a verb's *role* — which one supersedes, which one
contradicts — not its name, so the vocabulary is
[yours to rename or replace](./customization.md) while the behavior holds.

## Durability

Every node declares how long it should matter, and
[trust](./trust.md) treats each class differently:

- **stable** — decisions, principles, cautions: holds trust indefinitely,
  loses it only to contradicting evidence
- **episodic** — context that ages out over roughly six months
- **volatile** — short-lived notes that fade in about a month

Open Problems and Intents never fade while open — a worklist is not
archaeology.

## Who wrote a note

Every note records its `source`: `user` for a person (the pane, the CLI)
and `agent` for an AI assistant writing over MCP. A `user` note starts out
approved; an `agent` note waits in the Review drawer, and an unapproved
episodic or volatile one can decay out. Before 0.11.0 the agent value was
`claude` whatever the assistant (issue #18); older graphs and exports still
carry it, and it reads as `agent`.

Which assistant wrote a note is not recorded yet: a note keeps its
`session_id`, and the pane's System panel lists which clients bound to the
graph. Naming the agent per note would add a field to every note, so it waits
for a real need.

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

## Sleep: consolidating a crowded graph

A graph that is used grows piles: one subject restated across versions, an
investigation captured as a dozen findings. Each pile costs twice — search
delivers near-identical notes where one would do, and nobody can review
thousands of notes by hand. **Sleep** (0.11.0) consolidates after the fact,
without a new structure:

- **Crowding** is measured from the vectors the graph already stores: for
  each note, how many others sit inside this graph's *close band* — the
  cosine only its own top 1% of note pairs reach (relative, because absolute
  similarity levels differ between graphs). No model runs; nothing about
  search ranking changes.
- **Subjects** are what gets reviewed: notes joined by chains of close
  pairs, at least 2% of the graph's notes (never fewer than 5). At the band
  a graph's subjects touch and chain into one blob, so a group larger than
  four times the minimum is re-cut at a stricter cosine, branch by branch,
  until its pieces fit. On this repo's 562-note graph the band chained 409
  notes together; the adaptive cut found six subjects of 12–45 notes
  (retrieval and eval, the process model, tombstones, releases, the TepinDB
  move, LongMemEval).
- **Names** come without a model: class TF-IDF over the members' titles
  and tags — the terms most of a subject's notes share and the rest of the
  graph rarely uses, two or three of them (*retrieval · oblique recall ·
  NLI*, *TepinDB · multi-project · hub*). A tag and the same words in titles
  count as one term, and a phrase replaces the words it holds.
- **The review queue** ranks subjects by how much they are worth
  consolidating: size × (1 + ln versions spanned) × how settled they are
  (members from past versions, older than two weeks). An agent reads it
  with `list_nodes` `view: "review"`, one subject per page, or one note's
  neighbourhood with `around: <id>`; the pane shows it as Checkup →
  *Crowded subjects*, subject chips in the Review panel, and the graph
  view's heat glow.
- **Two more queues** ride along: **conflicts** (`view: "conflicts"` —
  standing `conflicts-with` edges and pending suspects), settled first, and
  **islands** (`view: "islands"` — notes no edge and no tag reaches, which
  only a lucky text search finds), each listed with its nearest notes so it
  can be linked, tagged, merged or buried. The brief stays out of it: the
  capture skills carry the cue to offer a sleep.
- **The `engram-sleep` skill** (`/engram-sleep [subject]`; `/engram:sleep`
  with the plugin) runs on request. The agent suggests it — one line, run on
  a yes — when search keeps returning restatements of one subject or the
  user finds memory noisy. It works one
  subject at a time: write a state-of note for settled history (linked
  `builds-on` what stays, `replaces` only what it fully covers), merge
  restatements (`merge_nodes` — the victims stay archived behind
  `replaces`), link what superseded or answered what, and mark the notes left
  standing **reviewed**. A subject stays hidden while more than half its
  notes carry a mark (a changed note loses its mark), so it comes back once new
  or changed notes outnumber the reviewed ones; a mark never moves trust. Every write of a sweep
  can be undone at once from the pane's Audit panel ("Undo session").
