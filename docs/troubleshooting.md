# Troubleshooting

Three commands solve most situations:

```sh
engram-alpha status   # what's running: core, models, projects, clients
engram-alpha doctor   # diagnose: store, models, core health, assistant wiring
engram-alpha stop     # stop the core and every engram process, cleanly
```

`status` reads the [machine core](./runtime.md) live: its pid, version, and
uptime, whether the models are loaded or idle-unloaded, every registered
project with whether the core holds its store open, and every connected MCP
client with its pid and project folder. `--json` emits the same for scripts.

`doctor` checks the whole chain from your repository's root — store
integrity, cached models, core health, and every detected assistant's wiring
— and says exactly what to fix. It finds the machine core through
`~/.engram/daemon.json` regardless of where you run it, and when the core
holds this repo's store open it asks the core instead of touching the file —
a store held by a healthy core is reported as exactly that, never as a lock
failure. It exits non-zero on real failures, so it works as a pre-flight in
scripts.

`stop` runs an orchestrated shutdown over the core's API: MCP sessions
close (bridges wait 90 seconds for a core to come back, then exit — a
`stop` followed by `serve` keeps assistants connected), in-flight store
operations commit, every
store lock is released, and the daemon files are cleaned up — with a
health-verified PID kill as the fallback when the core doesn't answer. Use
it before updates and whenever a repair needs exclusive access to a store.

## Common situations

**The pane shows a different project than expected.** One core serves all
your projects on one port — use the project switcher in the top bar. Running
`serve` from a repo registers it with the core and prints the pane URL.

**A write fails with "bound to the home graph by fallback".** The MCP client
answered no roots and the bridge was launched from a directory that can't
host a project (`/` or your home directory — the Windsurf JetBrains plugin
does both), so the session sits in the home graph without anyone having
chosen it, and engram refuses to write there silently. The agent fixes it
in one call: `brief` with `project` set to the workspace's absolute path
(or `"home"` to work in the user-level graph on purpose), then retries the
write. It happens again after every resumed conversation, IDE restart, or
MCP reload — each spawns a new session — which is exactly why it is an
error and not a note. Setting a default agent project in the pane
(Settings → System info) makes such sessions writable from the start, at
the cost of every root-less session landing there. Details in
[Runtime](./runtime.md#which-project-an-mcp-session-serves).

**The assistant's MCP tools stopped responding after an update or restart.**
Since 0.10.4 a bridge whose core restarts reconnects to the new one on its
own (within 90 seconds) and tells the assistant to reload the tool list.
The bridge itself stays the binary that was running, so after an update a
reconnect (Claude Code: `/mcp`) still picks up the new bridge. A bridge
that found no core for 90 seconds has exited; reconnect it the same way.

**"MCP connection failed" while the core was running.** Up to 0.10.3 a
bridge treated one ping that took over 10 seconds as a dead core and exited
— a core busy with a contradiction sweep could disconnect every assistant
on the machine. Since 0.10.4 the bridge checks the core's `/health` before
deciding: a busy core keeps its sessions. Both sides leave a trace now.
`<repo>/.engram/mcp.log` names every slow or missed ping, a likely machine
sleep, each reconnect, and why a bridge exited. `~/.engram/core.log` logs
"the core's runtime has been unresponsive for N s" when the core itself
stalls, and how long each librarian sweep took.

**A new release won't start ("GLIBC_2.43 not found" or similar).** Since
0.10.4, `engram-alpha update` and both install scripts start the new binary
once with `--version` before replacing anything, so a binary that can't run
on your machine is refused and the working one stays. On an older version,
or to go back to a release that worked, pin it:

```sh
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | ENGRAM_VERSION=v0.10.2 sh
# with a working engram-alpha still installed
engram-alpha update --version v0.10.2
```

```powershell
# Windows
$env:ENGRAM_VERSION = 'v0.10.2'; irm https://raw.githubusercontent.com/techtheist/engram/main/install.ps1 | iex
```

Linux release binaries are built on Ubuntu 24.04 and need glibc 2.38 or
newer (Ubuntu 24.04, Debian 13, Fedora 39 and later). Issue #17 was 0.10.3,
built by mistake on a newer runner that required glibc 2.43.

**`database_locked` on a `.tepin` store.** The store has exactly one owning
process — normally the core, which serves reads to everyone else. If an
external tool reports this, either it predates sidecar discovery (use
`npx tepindb`, which discovers it automatically) or no core is running and
two processes raced; `engram-alpha stop` then `serve` resets the state.
Details in [Storage & TepinDB](./storage.md#the-single-owner-model).

**MCP tools fail inside an IDE and there's nothing to look at.** The stdio
MCP server logs to `<repo>/.engram/mcp.log` (IDE clients usually swallow its
stderr) — startup, the daemon it bridges to, and any fatal error land there.
`RUST_LOG=debug` (or `trace`) in the server's `env` raises the detail on both
stderr and the file; `ENGRAM_MCP_LOG=0` turns the file off.

**Corporate proxy: MCP dies with an HTML error page in `mcp.log`.** A proxy
configured via `HTTP_PROXY`/`HTTPS_PROXY` was intercepting the bridge's
loopback connection to the core. Since 0.8.5 engram's own bridge ignores
proxies entirely, so this fixes itself on update. On older versions — or for
*other* local tools that follow the proxy env — exclude loopback explicitly:
`NO_PROXY=127.0.0.1,localhost`. `doctor` warns when a proxy is configured
without that exclusion.

**Search quality is degraded / "reranker unavailable" in the logs.** A model
layer failed to load — usually a first run that happened offline. The daemon
runs without it (that's by design); it provisions itself on the next online
start. `doctor` reports which models are cached, and the System panel shows
which layers are active.

**A node's code refs are flagged as drifted.** The code moved. Fix the path
if the knowledge still holds, supersede the node if the refactor invalidated
it — the pane badges drifted nodes and the assistant sees them in its
`list_drift` worklist. Drift never lowers trust on its own.

**The graph in the pane looks empty in a repo that has memory.** Usually the
pane is on a different project — one core serves all of them; check the
top-bar switcher — or the repo was never registered with the core (a classic
cause is `serve` with a relative `--db` from the wrong directory, which
registers the wrong path). `engram-alpha status` lists what the core
actually has; plain `serve` from the repository root registers the right
graph and is always safe.

**Two graphs after switching between WSL and native Windows.** A Windows
`engram-alpha.exe` and WSL-side agents see different filesystems. Pick one
side for binary + assistants + repo and stay there
([details](./getting-started.md#windows)).

**`serve` or `doctor` says `.engram/` isn't git-ignored, but it is.**
Fixed in 0.10.3. Both ask git (`git check-ignore`), so any rule that
already covers the folder counts: an anchored `/.engram/`, `.engram/**`, a
nested `.gitignore`, `.git/info/exclude`, or your global excludes file.
Nothing is appended to a tracked `.gitignore` then. Outside a git
repository they read the folder's own `.gitignore` and accept the same
shapes. On 0.10.2 and earlier, remove the extra `.engram/` line after you
update.

**`engram-alpha stop` reported a failure, but the core did stop.** Fixed
in 0.10.3. The core answers `POST /shutdown` and then exits, and on macOS
the connection could be reset before the reply was fully read, so the
answer was lost. `stop` now keeps a complete reply even when the socket
resets right after it.

**Something was captured that shouldn't have been.** Open Review, find it,
delete it (hard-delete is yours alone; the Feed's **Delete** is the
quick way, with or without a Tombstone) — or edit it into shape; a deliberate
edit also re-validates trust. The audit log shows exactly which session
wrote what, with before/after values.

## Where things live

| Path | What it is |
|---|---|
| `<repo>/.engram/graph.db` / `graph.tepin` | The repository's graph (git-ignored) |
| `<repo>/.engram/daemon.json` | The repo's pointer at the machine core (port, pid) — written by `serve`, scraped by the plugins |
| `<repo>/.engram/mcp.log` | The stdio MCP bridge's log (append; `ENGRAM_MCP_LOG=0` disables) |
| `~/.engram/core.log` | Output of the auto-spawned machine core — the first place to look when a core won't come up |
| `~/.engram/registry.json` | The machine's project registry |
| `~/.engram/daemon.json` | The machine core's advertisement |
| `~/.engram/home.db` / `home.tepin` | The shared home graph |
| `~/.engram/models.json` | Your model selection (absent = defaults) |
| `~/.engram/update-check.json` | The daemon's once-a-day update-check stamp (`ENGRAM_UPDATE_CHECK=0` disables the check) |
| `~/.cache/engram/<model>/` | Downloaded model files |

All of it is plain JSON or database files; stale daemon files are harmless —
every reader health-checks before trusting one.

Still stuck? Open an issue with the output of `engram-alpha doctor` — it's
designed to be exactly the report a maintainer needs.
