# The pane

The graph is the product surface, not hidden plumbing. The pane renders the
whole graph, updates live over SSE while your assistant works, and is where
every review and repair gesture lives. It runs in the browser at
`http://127.0.0.1:8787`, inside JetBrains IDEs (tool window or editor tab),
and in VS Code's secondary sidebar.

## Four layouts

One shape can't serve every question, so the canvas ships four:

| **Skyline** — layered left→right, packed rows | **Nebula** — one force-directed cloud |
|---|---|
| ![Skyline layout](../.screenshots/layout-skyline-example.png) | ![Nebula layout](../.screenshots/layout-nebula-example.png) |
| **Archipelago** — community islands, physics inside | **Orbit** — hubs with satellites in rings |
| ![Archipelago layout](../.screenshots/layout-archipelago-example.png) | ![Orbit layout](../.screenshots/layout-orbit-example.png) |

Skyline reads like a history, Nebula shows what clusters, Archipelago
separates concerns into islands, Orbit puts the load-bearing nodes in the
middle of their neighborhoods. Themes match where you work (Engram Purple,
JetBrains dark/light, VS Code dark/light), a click-to-center minimap handles
big graphs, and a health strip keeps the counts that matter — suspected
conflicts, stale nodes, provisional writes — in the corner of your eye.

## Tags and filters

Nodes carry free-form tags, settable by you in the pane or by the assistant
on request (*"tag everything about the auth rewrite"*).

<img src="../.screenshots/engram-alpha-filter-and-tags-feature.png" width="170" alt="The filter menu: type chips, the project's tag vocabulary, and status / trust / durability filters">

The filter menu turns the graph into slices: one click on a tag chip and the
canvas shows only that concern. Combine tags with type, status
(`open`/`resolved`/`obsolete`), trust (`pinned`/`provisional`/`trusted`/
`stale`), and durability filters for views like *"open problems in the
retrieval layer"* or *"every unreviewed decision from phase 2"*.

Tags are also how you and the assistant stay on the same page: the session
brief lists the project's tag vocabulary, the assistant reuses it when
capturing, and you filter by it when reviewing.

## Edit everything by hand

The graph is yours, not a read-only visualization of what the AI did.

<img src="../.screenshots/engram-alpha-add-memory-feature.png" width="198" alt="The New memory dialog">

- **Create** nodes from the **+ New** drawer — type, title, markdown body,
  durability, tags.
- **Link** by dragging from one node's handle to another; a dialog asks
  which of the seven verbs the connection means. If no verb fits, there is
  no edge to create.
- **Edit, retype, re-anchor** any node in place; retype or delete edges from
  the node's connection list.
- **Hard-delete is user-only** by design: the assistant can supersede
  knowledge, but only you can destroy it.

## The Review drawer

Capture is silent; Review is where it becomes accountable.

<img src="../.screenshots/engram-alpha-review-feature.png" width="243" alt="The Review drawer: a suspected-conflict pair awaiting a verdict, above the approval queue">

Everything recently added, everything awaiting review with its computed
trust, one-click **Approve** for what you vouch for — and above the queue,
the conflict worklist: suspected look-alike pairs awaiting your
**Conflict / Replaces / Dismiss** verdict (see
[Conflicts & Checkup](./conflicts-and-checkup.md)).

## Every change on the record

An append-only audit journal records every node and edge mutation — created,
updated, approved, archived — with before/after values, which session did
it, over which transport, and what the daemon knew at the time.

<img src="../.screenshots/engram-alpha-audit-log-feature.png" width="240" alt="The Audit log with expanded field-level records">

When you come back from vacation to a graph that looks different, *"what
changed and who wrote this"* has an exact answer.

## History at the knowledge level

Any node in a `replaces` chain shows a **History** section in its detail
drawer: every generation on a timeline, oldest first, the current one
marked, each retired generation carrying the note that explains why it was
replaced — one click to jump to any of them. The assistant gets the same
chain through the `timeline` tool: *"how did the auth decision evolve"* is
one call, with dates.

## Settings → System

The System panel is the daemon's self-report: binary version and uptime,
store backend and integrity, the loaded
[local models](./models.md) with their on-disk paths and the
**Choose models** selector, the machine
[project registry](./multi-project.md), and per-assistant wiring status.
