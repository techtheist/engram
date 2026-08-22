# Runtime architecture

Engram is one binary, but at runtime it is one heavy process and any number
of deliberately light ones. This page is the map: which processes exist and
why, how they find each other and talk, and what happens between the first
`serve` and the final `stop`. Everything binds `127.0.0.1` — nothing ever
listens on an external interface.

## The processes

```
 assistant (stdio MCP)      assistant (stdio MCP)          browser / IDE
         │                          │                            │
  engram-alpha mcp           engram-alpha mcp                    │
   (bridge, light)            (bridge, light)                    │
         │ streamable HTTP          │ streamable HTTP            │ HTTP + SSE
         └──────────┬───────────────┘                            │
                    ▼                                            ▼
       ┌──────────────────────────────────────────────────────────────┐
       │              the machine core · 127.0.0.1:8787               │
       │  every store lock · the local models · the pane · MCP · SSE  │
       └──────────────────────────────────────────────────────────────┘
                    ▲                                ▲
     engram-alpha serve                 status / doctor / stop / hooks
     (ensure core + register repo,      (one-shot REST clients)
      print pane URL, exit)
```

**The machine core** is the one process per machine that holds everything
heavy: every open store (and, on the [TepinDB backend](./storage.md), their
exclusive locks), the three [local models](./models.md), the pane's web
server, and every MCP session. It always holds the **home graph** — that is
what its `/health` advertises — and opens registered project stores lazily on
first access (a pane switch, a bridge connecting). You never start it by
hand: `serve` and `mcp` spawn it detached when the machine has none, with its
output going to `~/.engram/core.log`. It runs as `engram-alpha core`, an
internal subcommand hidden from the help listing, and it never exits on its
own — only `engram-alpha stop` (or you) ends it.

**`engram-alpha serve`** is a launcher, not a server. It makes sure the core
is running (spawning it if needed), registers the current repository with it,
refreshes the repo-local `.engram/daemon.json` advertisement, prints the pane
URL, and exits — the core keeps serving after the command returns. Run it
from anywhere, as often as you like; concurrent runs converge on exactly one
core.

**`engram-alpha mcp`** is the stdio MCP server your assistant launches — and
it is *always a bridge*: it never opens a store itself, on any backend. It
proxies the client's stdio session to the core's MCP endpoint over streamable
HTTP, holds a census lease so the core can name it, and exits on its own when
the client disconnects or the core goes away. If no core answers it starts
one; if the core can't start, the session fails with a clear error in
`.engram/mcp.log` rather than silently opening the store in-process.

**Everything else is a thin client.** `status`, `doctor`, `stop`, the
session-brief hook, `brief`, `export`, and `import` are one-shot REST calls
against the core (with a direct-open fallback only when no daemon owns the
store). The IDE plugins and the browser just point at the pane URL.

## How processes find each other

Discovery is two JSON files plus a health check — no process-name matching,
no port scanning:

- **`~/.engram/daemon.json`** — the machine core's advertisement (port, pid,
  version, home-graph path), written when the core binds its port and removed
  by its shutdown. Every client reads it and verifies the port over
  `GET /health` before trusting it, so a stale file is harmless.
- **`<repo>/.engram/daemon.json`** — a repo-local pointer at the same core,
  written by `serve`. This is what the IDE plugins and the Claude Code plugin
  scrape the pane port from. The core removes these on shutdown too.
- **`~/.engram/registry.json`** — the machine's
  [project registry](./multi-project.md): which repositories have graphs.
  `serve`, `mcp`, and `setup` register; the core opens registered stores
  lazily.

## How they communicate

- **MCP (streamable HTTP).** Bridges forward each stdio session to
  `/projects/{id}/mcp`, which binds the session to that project; `/mcp` is
  the same endpoint for the home graph. A direct streamable-HTTP MCP client
  can skip the bridge and connect to these URLs itself.
- **REST + SSE.** The pane, `status`, `doctor`, the hooks, and `stop` use the
  core's HTTP API; live graph updates stream over SSE.
- **Census leases.** Each bridge registers itself with the core
  (`POST /clients` → a lease: pid, kind, project root, the MCP client's own
  name from its `initialize` — `claude-code`, `mcp-go`, … — and
  connected-since), renews the lease on its existing 15-second heartbeat,
  and deregisters on clean exit. A lease that stops renewing expires after
  45 seconds — a crashed client disappears from the census within three
  missed beats. The census is pure observability: a failed registration
  never blocks bridging. It is exactly what `engram-alpha status` and the
  pane's **Settings → System** process list render — including which client
  bound where, and via what.

## Which project an MCP session serves

A bridge binds its session to a project in this order (each rung is logged,
so `mcp.log` always says which one bound the session):

1. **An explicit `--db`** pins the project; the bridge is a verbatim
   passthrough.
2. **The client's MCP roots** (no `--db`): after the handshake the bridge
   asks for `roots/list` and binds to the first `file://` root. On a
   `roots/list_changed` notification it re-resolves and rebinds — one
   long-lived, db-less config entry follows the client across project
   switches. This is what makes single-global-config clients like Windsurf
   work.
3. **The bridge's working directory**, when the client doesn't advertise
   roots (or advertises them and never answers), as long as it can host a
   project.
4. **The default agent project** — a machine-level setting for sessions with
   no folder signal at all (an IDE that spawns the bridge from `/` and never
   answers `roots/list`). Set it in the pane's **Settings → System info →
   Default agent project**, or over the core's loopback-only
   `GET/POST /settings` (`~/.engram/settings.json` underneath). The bridge
   reads it at bind time: changing it affects future sessions, never ones
   already connected. Unset, the ladder continues to
5. **The home graph** — the session binds the core's `/mcp` endpoint rather
   than dying. A later `roots/list_changed` naming a real workspace still
   rebinds away normally.

**The agent can rebind itself: `set_project`.** When a client advertises
roots but never answers them (Windsurf and Devin CLI in the field) and has
no usable working directory either, the agent is the only party that knows
the real workspace. The `set_project` tool rebinds the running session to a
registered project — name, id, or any absolute path inside its root — and
returns that project's brief in the same call. Sessions stranded on rungs 4
or 5 see a one-line hint atop their brief pointing at it. One line in the
client's rules file (e.g. `AGENTS.md`) makes it automatic:

> At the start of a session, call engram's `set_project` with the absolute
> path of the workspace, then follow the brief it returns.

`set_project` is session-scoped: it refuses unregistered paths (listing the
known projects — register a repo by running `engram-alpha serve` in it once)
and never touches the default-agent-project setting. The pane's Processes
census shows each live session's current binding under **Sessions**.

## Lifecycle

**First run.** The first `serve` — or the first assistant session, via its
bridge — spawns the core detached. Model provisioning can dominate a first
run, so the spawner waits up to 180 seconds for a healthy advertisement;
progress and failures land in `~/.engram/core.log`.

**Convergence.** Any number of `serve`, `mcp`, and plugin launches converge
on one core. If two core starts race for the port, the loser recognizes the
winner by its `/health` — it converges only with a daemon advertising *this
user's home graph*; a foreign engram daemon (another user on the machine, a
test sandbox) is just a taken port to walk past.

**Idle unload.** When the core has had zero connected bridges, no counted
HTTP activity, and no model use for 15 minutes, it drops all three ONNX model
sessions — measured, that takes a working core from ~845 MB resident to
~150 MB (the OS reclaims the memory in stages), while the core itself stays
resident and instantly reachable. The next demand from any path — a search, a
background sweep, a model swap — reloads lazily and exactly once: 0.1–0.5 s
measured. A connected-but-idle assistant keeps the models resident by design.
Health polls, `status` reads, and lease pings are deliberately exempt from
the activity clock, so *watching* the core never keeps it warm.
`ENGRAM_IDLE_UNLOAD_SECS` tunes the window; `0` disables the unload.

**Stop.** `engram-alpha stop` asks the core to shut down over a
loopback-only `POST /shutdown`: MCP sessions close (bridges exit immediately
instead of timing out), every engine is drained so any store operation that
had started has committed, every store lock is released, the daemon files are
removed, and the process exits. If the core doesn't answer, `stop` falls back
to a health-verified PID kill. Legacy per-repo daemons from pre-0.8.8
binaries are discovered and stopped the old way.

**Deprecated: `serve --http-only`.** The pre-0.8.8 foreground shape. It
still works — it ensures the core exactly like plain `serve` and then stays
in the foreground while the core is healthy (back-compat for process
supervisors), exiting when the core dies — but plain `serve` is the current
form.

## Watching it run

`engram-alpha status` reads the core's `/system` and `/projects` live: core
pid, version, uptime, and port; model residency (`loaded` or unloaded-idle,
and since when); every registered project with whether the core currently
holds its store open; and every connected client with pid, project folder,
and connection age. `--json` emits the same for scripts. The pane shows the
same census in **Settings → System → Processes**.
