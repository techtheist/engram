# Getting started

Engram is one binary, `engram-alpha`, that runs entirely on your machine. It
serves the graph UI over localhost, speaks MCP to your AI assistants, and
keeps each repository's memory in a git-ignored `.engram/` folder inside that
repository.

## Install

From your project's root:

```sh
curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh
```

The installer downloads the binary for your platform (checksum-verified,
into `~/.local/bin`), wires the repository for the assistants it detects, and
git-ignores `.engram/`. Then:

```sh
engram-alpha serve
```

and open `http://127.0.0.1:8787` — or use the
[JetBrains plugin](https://plugins.jetbrains.com/plugin/32654-engram) or the
VS Code extension
([VS Marketplace](https://marketplace.visualstudio.com/items?itemName=techtheist.engram-alpha)
· [Open VSX](https://open-vsx.org/extension/techtheist/engram-alpha) for
VSCodium, Cursor, and Windsurf) instead of the browser.

`serve` is safe to run from anywhere, as many times as you like: one core
process serves every registered project on one port, and any further `serve`
simply points you at the running pane. In a git repository that has no graph
yet, it asks before creating one. See
[Multi-project memory](./multi-project.md) for how that works.

## Claude Code: install as a plugin

The plugin is the one-install path for Claude Code — it carries the capture
skill, the session-start brief hook, and the setup commands into every
project:

```
/plugin marketplace add techtheist/engram
/plugin install engram@engram
```

Then run `/engram:setup` once per repository you want remembered (it installs
the binary if missing, git-ignores `.engram/`, and registers the MCP server).
`/engram:pane` opens the graph UI. Details in
[`claude-plugin/`](../claude-plugin/).

## Wire any assistant

Setup lives in the binary. `engram-alpha setup` auto-detects which assistants
are installed and wires them; `--cli` picks explicitly (comma-separated, or
`all`), and `--skill relaxed|normal|aggressive` sets the
[capture intensity](./memory-model.md#capture-modes) for any assistant. The
installer forwards both flags:

```sh
curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh -s -- --cli codex,gemini --skill normal
# later, from any repo:
engram-alpha setup                          # auto-detect and wire
engram-alpha setup --cli kilo --skill aggressive
```

| `--cli` | MCP registration | Capture instructions |
|---|---|---|
| `claude` *(default)* | `.mcp.json` | `.claude/skills/engram/SKILL.md` (three intensities via `--skill`) |
| `codex` | `~/.codex/config.toml` (global — shared by the CLI **and** the Codex/ChatGPT desktop app; launch `codex` from the repo root, and for the app pin `cwd` or an absolute `--db` in the entry) | `AGENTS.md` |
| `gemini` | `.gemini/settings.json` | `GEMINI.md` |
| `opencode` | `opencode.json` | `AGENTS.md` |
| `kilo` | `kilo.json` | `AGENTS.md` |
| `antigravity` | `.agents/mcp_config.json` | `AGENTS.md` |
| `windsurf` | `${XDG_CONFIG_HOME:-~/.config}/devin/mcp_config.json` **and** `~/.devin/mcp_config.json` (global — Windsurf/Cascade reads one machine-wide config, but which file depends on the plugin generation, so setup writes the same db-less entry into both; the server follows your open workspace via MCP roots) | `AGENTS.md` |

Windsurf support is freshly added and still being field-tested — reports
welcome. One Windsurf-specific note: its JetBrains plugin spawns the MCP
server from `/` and its client never answers the roots request, so those
sessions carry no folder signal at all. They land on the **default agent
project** — set it in the pane under **Settings → System info → Default
agent project** so Cascade's memory goes to the project you're actually
working on; unset, such sessions bind the shared home graph. Every wired assistant reads and writes the same graph through the
same MCP server — one shared, local memory across your AI agents: a decision captured
by Claude is recalled by Codex. The `AGENTS.md`/`GEMINI.md` additions are a
marked, idempotent section; re-running the installer never duplicates them.

## Support matrix

What each harness actually gets. **Memory tools** is the MCP surface
(`brief`, `search`, capture — the same graph everywhere). **Injected brief**
means the session starts pre-briefed without the agent asking: a
SessionStart hook injects it (✓ auto = setup registers it; manual = the
harness supports it and [`hooks/session-brief.sh`](../hooks/session-brief.sh)
is portable, but you register it yourself). Harnesses without hooks still get
briefed — their `AGENTS.md` instructions teach the agent to call `brief`
first. **File-read recall** is the hook that surfaces matching memories when
the agent reads a file. **History recording** is the opt-in sealed session
transcript layer ([storage](./storage.md)) — ✓ means a harvest adapter reads
that harness's transcripts.

| Harness | Memory tools (MCP) | Injected brief | File-read recall | History recording |
|---|---|---|---|---|
| Claude Code | ✓ | ✓ auto (hook + plugin) | ✓ auto | ✓ |
| Codex CLI / app | ✓ | manual | — | ✓ |
| Gemini CLI | ✓ | manual | — | ✓ |
| OpenCode | ✓ | — | — | ✓ |
| Kilo Code | ✓ | — | — | ✓ * |
| Antigravity | ✓ | — | — | ✓ |
| Bob (IDE + Shell) | ✓ | — | — | ✓ |
| Windsurf / Cascade | ✓ ** | — | — | — *** |

\* Kilo's adapter is verified against fixture transcripts, not yet against a
live install — reports welcome.
\** Windsurf sessions carry no folder signal (see the note above) — set the
default agent project so they bind the right graph.
\*** Cascade stores its transcripts encrypted at rest with a key in the OS
keychain; there is nothing a local harvester can responsibly read. Knowledge
Cascade captures through the MCP tools is remembered like everyone else's —
only the raw dialogue layer is out of reach.

## Windows

Two supported paths — pick where your assistants live:

- Assistants inside **WSL2** → run the `install.sh` one-liner inside WSL. It
  installs the Linux binary; daemon, agents, and graph share the WSL
  filesystem.
- **Native Windows** assistants → PowerShell:

  ```powershell
  powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/techtheist/engram/main/install.ps1 | iex"
  ```

Don't mix the two: a Windows `engram-alpha.exe` and WSL-side agents see
different filesystems and will end up on different graphs.

macOS arm64, Linux x64, and Windows x64 binaries are on
[GitHub Releases](https://github.com/techtheist/engram/releases). Intel Macs
have no prebuilt binary (onnxruntime upstream dropped Intel-mac builds) —
build with `cargo install --path crates/engram-cli` from a checkout instead.
On Linux, building from source needs the D-Bus headers for the OS-keystore
integration (the history-sealing key): `sudo apt install libdbus-1-dev
pkg-config` or your distribution's equivalent.

Installer options: `--skill relaxed|normal|aggressive` (default relaxed),
`--bin-only` to skip repo wiring, `ENGRAM_VERSION=vX.Y.Z` to pin a version.

## Your first session

Open a wired repository with your assistant and just work. At session start
the assistant receives a compact brief of the graph's canon; as you make
decisions, it captures them silently; the pane shows the graph growing live.
When the session ends, open the Review drawer and look at what was written —
[Recall & capture](./recall-and-capture.md) explains the loop, and
[Trust & decay](./trust.md) explains why you can afford to let it run.

If the graph is empty, the assistant will offer a one-time seeding pass over
your existing README, plan documents, and recent history — accept it to start
with the project's standing knowledge instead of a blank canvas.

## Updating

```sh
engram-alpha update
```

checks the latest release, verifies its checksum, and swaps the binary in
place (a no-op when already current; `--version vX.Y.Z` pins). Re-running the
install one-liner does the same thing and is always safe — repo wiring is
idempotent. Coming from **v0.3.0 or older**, when the binary was named
`engram`: both paths work — `engram update` lands on the current version via
the v0.4.x transition assets, and the installer swaps the old binary and
re-points your MCP wiring automatically.

After an update, restart the daemon (`engram-alpha stop`, then `serve`) and
reconnect your assistant's MCP session (`/mcp` in Claude Code) so both run
the new binary.

You don't have to remember to check: `doctor` always compares the binary
against the newest published release, and the daemon quietly does the same at
most once per day (a single log line pointing at `engram-alpha update` —
nothing is ever installed automatically, and an unreachable network is
silently fine). Set `ENGRAM_UPDATE_CHECK=0` to keep the daemon fully offline.

## When something is off

```sh
engram-alpha doctor
```

checks the whole chain from your repository's root — store integrity, the
local models, the machine core's health, and every detected assistant's
wiring — and says exactly what to fix. It exits non-zero on real failures,
so it doubles as a pre-flight in scripts. `engram-alpha status` shows what
is running right now — core, models, projects, connected clients. More in
[Troubleshooting](./troubleshooting.md); how the processes fit together is
on [Runtime architecture](./runtime.md).
