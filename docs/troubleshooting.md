# Troubleshooting

Two commands solve most situations:

```sh
engram-alpha doctor   # diagnose: store, models, daemon, assistant wiring
engram-alpha stop     # stop the core and every engram process, cleanly
```

`doctor` checks the whole chain from your repository's root — store
integrity, cached models, whether the running daemon actually serves *this*
repo, and every detected assistant's wiring — and says exactly what to fix.
It exits non-zero on real failures, so it works as a pre-flight in scripts.

`stop` discovers every advertised engram daemon from their daemon files,
health-checks each before terminating it, and cleans up stale files. Bridge
processes exit on their own when the core goes. Use it before updates and
whenever a repair needs exclusive access to a store.

## Common situations

**The pane shows a different project than expected.** One core serves all
your projects on one port — use the project switcher in the top bar. Running
`serve` from a repo registers it with the core and prints the pane URL.

**The assistant's MCP tools stopped responding after an update or restart.**
The assistant's MCP session was connected to the old daemon process.
Reconnect it (Claude Code: `/mcp`) — the new session bridges to the running
core automatically.

**`database_locked` on a `.tepin` store.** The store has exactly one owning
process — normally the core, which serves reads to everyone else. If an
external tool reports this, either it predates sidecar discovery (use
`npx tepindb`, which discovers it automatically) or no core is running and
two processes raced; `engram-alpha stop` then `serve` resets the state.
Details in [Storage & TepinDB](./storage.md#the-single-owner-model).

**Search quality is degraded / "reranker unavailable" in the logs.** A model
layer failed to load — usually a first run that happened offline. The daemon
runs without it (that's by design); it provisions itself on the next online
start. `doctor` reports which models are cached, and the System panel shows
which layers are active.

**A node's code refs are flagged as drifted.** The code moved. Fix the path
if the knowledge still holds, supersede the node if the refactor invalidated
it — the pane badges drifted nodes and the assistant sees them in its
`list_drift` worklist. Drift never lowers trust on its own.

**The graph in the pane looks empty in a repo that has memory.** Almost
always a daemon serving a different database than the repo expects — a
classic cause is starting `serve` with a relative `--db` from the wrong
directory. `doctor` catches exactly this mismatch; plain `serve` from the
repository root is always safe.

**Two graphs after switching between WSL and native Windows.** A Windows
`engram-alpha.exe` and WSL-side agents see different filesystems. Pick one
side for binary + assistants + repo and stay there
([details](./getting-started.md#windows)).

**Something was captured that shouldn't have been.** Open Review, find it,
delete it (hard-delete is yours alone) — or edit it into shape; a deliberate
edit also re-validates trust. The audit log shows exactly which session
wrote what, with before/after values.

## Where things live

| Path | What it is |
|---|---|
| `<repo>/.engram/graph.db` / `graph.tepin` | The repository's graph (git-ignored) |
| `<repo>/.engram/daemon.json` | A repo-launched daemon's advertisement (port, pid) |
| `~/.engram/registry.json` | The machine's project registry |
| `~/.engram/daemon.json` | The machine core's advertisement |
| `~/.engram/home.db` | The shared home graph |
| `~/.engram/models.json` | Your model selection (absent = defaults) |
| `~/.cache/engram/<model>/` | Downloaded model files |

All of it is plain JSON or database files; stale daemon files are harmless —
every reader health-checks before trusting one.

Still stuck? Open an issue with the output of `engram-alpha doctor` — it's
designed to be exactly the report a maintainer needs.
