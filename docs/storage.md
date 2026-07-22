# Storage & TepinDB

Your memory is a file inside your repository — `.engram/graph.db` (SQLite)
or `.engram/graph.tepin` ([TepinDB](https://github.com/tepindb/tepindb)) —
git-ignored, portable, and yours. No cloud, no accounts; the canonical
interchange format is JSON export/import, not a binary blob.

## Two backends, one behavior

Both backends implement the same storage contract and are covered by the
same test battery, so search ranking, trust, conflicts, and every feature
behave identically on either. Which one a repository uses is decided by
which file exists — every command, hook, and MCP wiring picks the right one
automatically.

- **SQLite** (`graph.db`) — the original backend: one database file with
  full-text and vector indexes alongside. Existing repositories stay on it
  untouched until you migrate.
- **TepinDB** (`graph.tepin`) — a single self-describing file holding
  documents, keyword index, and vectors together. New graphs are created on
  TepinDB by default.

The roadmap is a staged sunset: today migration is opt-in; a later 0.7
release migrates by default (with the SQLite file kept as backup); 0.8 drops
the SQLite backend for new writes while still reading old files to migrate
stragglers.

## Migrating a repository

```sh
engram-alpha migrate
```

run from the repository root:

- moves nodes and edges through the canonical JSON form, regenerating
  embeddings locally,
- carries the suspect queue and the **full audit journal** over verbatim —
  judged-conflict history and provenance survive,
- verifies counts before declaring success,
- and **never touches `graph.db`** — it stays behind as your backup. Rolling
  back is deleting `graph.tepin`.

After migrating, restart the daemon (`engram-alpha stop`, then `serve`) and
reconnect your assistant's MCP session so everything picks up the new file.

## Why TepinDB

A `.tepin` file describes itself: run `head` on it — or point any agent at
it — and it explains what it is and how to work with it. That makes your
memory legible outside Engram:

```sh
npx tepindb inspect .engram/graph.tepin
```

prints a report of every collection — nodes, edges, suspects, the audit
journal — with counts and purposes, and

```sh
npx tepindb query .engram/graph.tepin nodes '{"type": "Principle"}'
```

queries it with MongoDB-style filters. **This works while Engram is
running**: the daemon owns the file and serves reads to other processes
through TepinDB's in-driver sidecar, so `npx tepindb` — or any other tool —
can inspect a live store without stopping anything. (Semantic search from
the slim `npx` client is not wired yet; keyword queries and document reads
are.)

## The single-owner model

A `.tepin` file has exactly one owning process at a time. In Engram that
owner is the [machine core](./multi-project.md): the session brief,
`doctor`, and your assistant's MCP server all read through it over
localhost, transparently — you never manage this. The visible consequences
are good ones: one process, one pane, and MCP writes appearing live on the
pane's event stream.

If you see `database_locked` from an external tool, the store is owned and
that tool predates sidecar discovery — stop the core (`engram-alpha stop`)
or use `npx tepindb`, which discovers the sidecar automatically.

## Export, import, backups

```sh
engram-alpha export --out graph.json     # the whole graph as portable JSON
engram-alpha import graph.json           # upsert by id; idempotent
```

Exports carry no embeddings (they're regenerated on import) and no computed
trust (it's a function of time), which makes them stable, diffable, and the
right thing to commit to a private backup repo if you want history. Import
into either backend — the JSON form is how graphs move between them.
