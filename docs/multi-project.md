# Multi-project memory

Every repository keeps its own graph — and since v0.6 they're aware of each
other, through one core process per machine. The design principle is
**access, not replication**: any project can read the others' graphs, so
nothing needs syncing, and one insight lives in exactly one graph.

## One core, one pane

`engram-alpha serve` is safe to run from anywhere, as many times as you
like:

- If a core is already running, `serve` registers your current repository
  with it, prints the pane URL, and exits. Ten concurrent `serve` runs
  converge on exactly one core process on one port — never a second pane.
- From a repository with a graph, `serve` becomes the core with that project
  current.
- From a git repository *without* a graph, it asks before creating one —
  nothing is initialized silently. (You can also add any folder later from
  the pane's project picker.)
- From your home directory or any non-git folder, it runs the core over the
  **home graph** — useful when you just want the pane.

The pane's top-bar switcher moves between projects; every project's live
updates stream to the same UI.

`engram-alpha stop` shuts down the core and every satellite process in one
gesture — used before updates, or whenever a repair needs exclusive access
to the stores.

## The registry

Each `serve`, `mcp`, or `setup` run registers its repository in
`~/.engram/registry.json` — plain JSON, inspectable with `cat`. The registry
is how projects find each other and how the pane's project list is built.
Removing an entry (pane: Settings → System → *forget*) never touches the
project's graph; it only forgets the address.

## The home graph

`~/.engram/home.db` is a normal Engram graph for knowledge that was never
project-scoped: your global principles, standing preferences, cross-cutting
cautions. Its canon rides along in every project's session brief. Tell your
assistant *"remember this globally"* and it lands there.

When the same Principle or Caution shows up in several project graphs, a
Checkup pass nominates it for **promotion** into the home graph — the copy
moves up, the projects keep provenance links, and as everywhere in Engram:
the scan nominates, you approve.

## What the assistant sees

An assistant session is bound to the repository it runs in — its searches
and writes go to that project's graph by default. Most MCP tools also take
an optional `project`:

- omit it → the current repository
- a project name → that project's graph, reads **and** writes — an insight
  about a sibling project belongs in *that* project's memory
- `home` → the shared home graph
- `all` → read across everything (search and claim checks only). Foreign
  hits carry provenance and rank under a locality prior, so the local canon
  wins ties. Writes to `all` are refused by design — fanning one write into
  N graphs would mint N future duplicates.

## How it works underneath

The core process is the sole owner of every open store; assistants connect
through lightweight bridges that die with the core (no orphan processes),
and the session brief and `doctor` read through it the same way. This
matters most on the [TepinDB backend](./storage.md), whose single-file
stores allow exactly one owning process — the core *is* that process, for
all of them.
