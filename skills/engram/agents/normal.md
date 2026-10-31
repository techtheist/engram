## Engram — durable project memory (MCP server: `engram`)

This project keeps a local, user-owned graph of its *reasoning*: Decisions and
their reasons, Principles, Cautions, Problems and Resolutions, Insights, open
Intents, Anchors that group them. Not code structure — the code holds that.
The user sees and curates every node in the pane.

**Recall.** Read the brief at session start — an injected "# Engram brief" if
the session opened with one, otherwise call `brief` once. Its first line names
the graph: if it is not this workspace, call `brief` with `project` set to the
workspace's ABSOLUTE path. That call rebinds the session — later writes go
there; if a reply warns your writes moved and you only meant to read, go back
with the call it names (for read-only looks, pass `project` to `search` /
`list_open` / `get_node`). A write refused as "bound … by fallback" means:
bind, then retry. Before any non-trivial decision, `search`: respect its
`confidence` verdict and each hit's `standing`, read `conflicts-with` /
`replaces` neighbors first, and reformulate (name the thing, use the note's
words, scope in time) before concluding the graph is silent.

**Capture.** Capture the load-bearing knowledge: Decisions with reasons, Principles and conventions, Cautions and gotchas, resolved Problems, selective non-obvious Insights, and Intents worth surviving the session. Skip anything the code or docs already state verbatim.
Types: exactly the graph's set (default 9: Decision, Principle, Caution,
Problem, Resolution, Insight, Intent, Anchor, Tombstone — a record of a
deliberate removal; don't resurrect it); `describe_ontology` if the brief
teaches a custom one. Title = the claim; body = the why in 1–3 sentences.
Shapes: `add_note {"type": "Decision", "title": "...", "body": "..."}`;
`add_notes {"notes": [...]}` items carry NO links — `link {"from": "<id>",
"to": "<id>", "type": "because"}` the returned ids in a second pass, with
sentence-shaped verbs (because / answers / about → Anchors only / builds-on /
replaces / conflicts-with / needs). Every write
reply is a verdict: `matched` → `update_node` the match; `suspects` → judge
each now with `resolve_suspect` (conflict | replaces | dismiss) and tell the
user about a genuine contradiction; `warnings` → read the flagged canon
(`tombstoned` = deliberately removed: don't re-add). Write silently at natural
stopping points; never store secrets, credentials or line numbers.

**Maintain.** Judge the brief's suspects first. A Resolution that answers a
Problem/Intent → set that node resolved. Several notes saying one thing →
`merge_nodes`; one subject restated all over, or the user finds memory noisy →
suggest a sleep (the engram-sleep skill), run it on a yes. `list_drift` → fix moved paths and re-check the claim. Stale
hits: verify; still true → `update_node`. `approve_node` only on the user's
explicit word. Settings (ontology, fields, history, knobs, models) change only
on the user's ask, over the core's HTTP API — read `GET /guide` first.

**Tasks.** Your working list lives beside the graph (never searched); the
brief's `## Tasks` shows the active titles. Use it when work will span sessions: one task per piece
of work (a feature = a group, `add_tasks` items with `"parent": "$1"`), steps
in the body; `update_task` with `progress` when you stop, `status` done /
delayed / dropped as things move. `close` / `wake` rules take a version, a
date or `"node:<id>"`. When one settles something, capture it as a note.
Resume from the brief's `## Tasks` (`list_tasks` reads bodies; `search`
never returns tasks). An Intent is the plan worth remembering, tasks are the
doing — they `refs` it. A status or order the user set on the board stands.
If a task tool says tasks are turned off, work without them there.

The pane is at http://127.0.0.1:8787 (real port in `.engram/daemon.json`);
`engram-alpha serve` in the repo root starts the core and exits. If the tools
disconnect, don't drop captures — `POST /nodes` on the core with
`"source": "claude"`, or ask the user to reconnect.
