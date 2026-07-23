# Customization

Engram ships with an opinionated memory model — eight node types, seven edge
verbs, tuned trust and decay numbers. From v0.7.0 onward, none of it is
fixed. Everything the engine treats as *meaning* is per-graph configuration,
stored inside the graph itself and edited in one place: **Settings → Graph
settings** in the pane.

Most people never need to touch this — the defaults are the product. But a
research log, a design system, or a solo notebook may want a different
vocabulary, and every project may want to nudge how aggressively memory
fades. This page is that door.

## The ontology redactor

Open **Settings (the gear) → Graph settings** to reach the redactor. It edits
three things — the ontology, the policy numbers, and the brief — as one
document, with a **Save** that validates the whole thing before it takes
effect. Nothing changes until you save; **Revert** discards the draft.

### Node types

Each type is a card. You control:

- **Name** — what the assistant writes (`Decision`, `Claim`, `Rule`, …).
- **Hue** — a single 0–360 slider. Every other color is *derived* from it:
  the light-theme and dark-theme accents, and the muted variant for types
  that shouldn't shout. One input keeps every scheme coherent by
  construction — you can't pick a color that clashes with a theme.
- **Thought** — the one-line "what this type captures", shown when the
  assistant asks the graph to describe itself.
- **Durability** — how fast this type's knowledge ages
  ([stable / episodic / volatile](./trust.md)).
- **Roles** — the flags the *engine* keys on, never the name (see below).
- **Brief section** — whether this type gets its own section in the session
  brief, and how much of it.

### Edge verbs

Verbs are cards too: the verb name, a worked example ("Decision because
Principle"), and role flags. A triple must always read as an English
sentence, so verb names stay lowercase and hyphen-joined.

### Roles, not names

This is the idea that makes customization safe. The engine never looks at the
string `"conflicts-with"` — it looks at which verb carries the
**contradiction** role. Rename it to `refutes`, and every behavior follows:
the conflict scan, the trust demotion, the red animated edge on the canvas.
The roles you can assign:

| Role | On a type | |
|---|---|---|
| **worklist** | open/resolved lifecycle; lives in the brief's worklist, never decays while open | Problem, Intent |
| **anchor** | a code subject; carries `code_refs`, sits out the conflict scan, renders muted | Anchor |
| **highlight** | may be accented on the canvas (off ⇒ always muted) | most types |
| **rank prior** | a small ranking nudge in search (never touches trust) | Principle, Caution |

| Role | On a verb | |
|---|---|---|
| **supersession** | creating it archives the older endpoint and chains history | replaces |
| **contradiction** | a judged one demotes trust and feeds the conflict queue | conflicts-with |
| **reason** | the "because" edge; its absence is what the structural checkup flags | because |
| **answer** | closes a worklist item | answers |
| **dependency** | a live blocker | needs |

Two invariants hold across **any** configuration, and Save enforces them:
edges stay sentence-shaped, and **exactly one** verb carries supersession and
**exactly one** carries contradiction. Those two edges are what make the
graph active rather than a note pile, so they can be *moved* to another verb
but never removed.

### Renaming carries your data

Renaming a type or verb from its card isn't a cosmetic relabel — it is the
**migration gesture**. Rename `Decision` to `Choice` and every stored
Decision becomes a Choice in the same step; the card shows how many nodes
will follow. (A plain Save can't drop a type that still holds nodes — it
tells you to rename them into another type first, so an edit can never
strand knowledge.)

## Presets

The redactor's preset shelf swaps the whole ontology at once. Three ship:

- **Engram** — the default 8-type product-building set. This is what every
  graph is born with.
- **Research** — for investigation-shaped work: Claim, Method, Question,
  Finding, Source, Task, with `refutes` carrying the contradiction role.
- **Minimal** — three types (Rule, Note, Todo) for a graph that wants almost
  no ceremony.

Applying a preset replaces types, verbs, policy, and brief settings together.
On a graph that already holds nodes it only lands cleanly when the type names
line up (or after you've retyped), for the same no-stranding reason as above.

## Tuning trust, decay, and thresholds

The **Trust & decay policy** section exposes the fourteen numbers behind
[the trust model](./trust.md): the starting trust for a fresh note, the
confirmed and approved anchors, how many days episodic and volatile
knowledge take to fade, when a note reads as stale, how long it then waits
before the decay pass archives it, and the similarity thresholds that decide
when a write is a duplicate, a suspected conflict, or a near-miss worth a
warning.

Every knob renders a plain-word explanation from the *live* values, so you're
never editing a raw number in the dark — change the episodic window and the
sentence under it re-reads to match. The defaults were dogfooded on this
project's own graph; treat them as a good starting point, not a ceiling.

## Composing the brief

The **Brief composition** section controls what the session-start digest
includes and how big it gets: the character budget, which sections appear
(tags, conflicts, suspects, recent, open work), and their caps. Two switches
worth knowing:

- **Teach ontology** — prepend a description of this graph's types and verbs
  to every brief. Off by default (the assistant already knows the shipped
  ontology); turn it on for a heavily customized graph so a fresh session
  learns the vocabulary immediately.
- Per-type **brief section** (on each type card) — which types get their own
  canon section, and how many entries.

## Version tracking

Optional, per-graph. Turn on **Track versions** and set a current working
version — anything free-form: `v0.7.0`, `26.7.23`, `sprint-14`. From then on,
every new note of a version-bound type is stamped with it, so the graph shows
*when* each piece of knowledge was captured, and the brief announces the
current version at the top.

Which types carry the stamp is a per-type role (`versioned`): in the shipped
ontology, Principles and Anchors are exempt — a value or a code subject
transcends any single release. Your assistant moves the version with the
`set_version` tool when the project bumps; the switch history lives in the
[audit journal](./pane.md).

## Handoff notes

Sometimes the most important thing in the graph is a message to the *next*
session: an unfinished cutover, a "start here", a warning. Tag any open
worklist note `handoff` and it gets guaranteed top placement in the next
brief — "read first" — never crowded out by the budget. Once the note is
acted on, resolving it stops the reminder; a forgotten one fades on its own.
It's reactive memory without a new node type.

## Teaching the assistant your ontology

Config is a **user gesture** — the redactor and the HTTP API only; there are
deliberately no MCP tools that let the assistant reshape your ontology, the
same way pinning and hard-delete are yours alone. But the assistant still
needs to *know* the shape:

- The `describe_ontology` MCP tool (and the optional brief section above) tell
  it this graph's live types, verbs, and roles on demand.
- From the redactor's **Assistant skill** section you can (re)install the
  Claude Code capture skill. On a customized graph the skill is *generated
  from your ontology* — it teaches your actual type and verb names, your
  durability defaults, and your tuned trust numbers in plain words. Reinstall
  after reshaping the ontology so the skill and the graph stay in step.

## It travels with the graph

The whole configuration lives in the graph's own storage, so it follows the
data everywhere: it survives the [migration to a `.tepin` file](./storage.md),
it rides along inside a [JSON export](./storage.md) and is restored on import,
and each project in a [multi-project setup](./multi-project.md) keeps its own.
A graph that never customizes carries no config document at all and behaves
exactly like the shipped defaults — customization is strictly opt-in, all the
way down.
