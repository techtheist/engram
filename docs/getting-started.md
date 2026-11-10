# Getting started

Engram is one binary, `engram-alpha`, that runs entirely on your machine. It
serves the graph UI over localhost, speaks MCP to your AI assistants, and
keeps each repository's memory in a git-ignored `.engram/` folder inside that
repository.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh
```

The installer downloads the binary for your platform (checksum-verified,
into `~/.local/bin`) — and deliberately nothing more: it never writes a
config unasked, so it is safe to run from anywhere, your home directory
included. Wiring is your own explicit step, from the project you mean:

```sh
cd your-project
engram-alpha setup     # auto-detects your installed assistants and wires them
engram-alpha serve
```

(`serve` also reminds you: any assistant it detects that isn't wired to the
repo yet gets a ready-to-paste `engram-alpha setup --cli …` line.)

Then open `http://127.0.0.1:8787` — or use the
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

The plugin brings the MCP server with it. Run `/engram:setup` once per
repository you want remembered (it installs the binary if missing, git-ignores
and creates `.engram/`), then `/mcp` → reconnect `engram`. The server binds
only repositories wired this way and never creates a graph on its own; in any
other folder it reads the home graph and refuses project writes until you wire
it. `/engram:pane` opens the graph UI. Details in
[`claude-plugin/`](../claude-plugin/).

## Wire any assistant

Setup lives in the binary. `engram-alpha setup` auto-detects which assistants
are installed and wires them; `--cli` picks explicitly (comma-separated, or
`all`), and `--skill relaxed|normal|aggressive` sets the
[capture intensity](./memory-model.md#capture-modes) for any assistant. The
installer forwards both flags — passing `--cli` there is the one way to have
the install wire anything (it opts the current directory in):

```sh
curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh -s -- --cli codex,gemini --skill normal
# the usual path — from any repo:
engram-alpha setup                          # auto-detect and wire
engram-alpha setup --cli kilo --skill aggressive
```

| `--cli` | MCP registration | Capture instructions |
|---|---|---|
| `claude` *(default)* | `.mcp.json` | `.claude/skills/engram/SKILL.md` (three intensities via `--skill`) + the `SessionStart` brief and file-read-match hooks in `.claude/hooks/`, registered in `.claude/settings.json` |
| `codex` | `~/.codex/config.toml` (global — shared by the CLI **and** the Codex/ChatGPT desktop app; launch `codex` from the repo root, and for the app pin `cwd` or an absolute `--db` in the entry) | `AGENTS.md` + `.codex/skills/` + a `SessionStart` brief hook in `.codex/hooks.json` (Codex trust-gates project hooks — run `/hooks` inside codex once to approve it) |
| `gemini` | `.gemini/settings.json` | `GEMINI.md` |
| `opencode` | `opencode.json` | `AGENTS.md` |
| `kilo` | `kilo.json` | `AGENTS.md` |
| `antigravity` | `.agents/mcp_config.json` | `AGENTS.md` |
| `bob` | `.bob/mcp.json` (project-level, explicit `--db` — Bob IDE and BobShell share it; to wire globally add the entry to `~/.bob/mcp.json` or `~/.bob/mcp_settings.json` by hand) | `AGENTS.md` + a `SessionStart` brief hook in `.bob/settings.json` (Bob IDE 2.0.2+ / BobShell 2.0.1+ run lifecycle hooks; older Bobs still get briefed through `AGENTS.md`) |
| `windsurf` | `${XDG_CONFIG_HOME:-~/.config}/devin/mcp_config.json` **and** `~/.devin/mcp_config.json` (global — Windsurf/Cascade reads one machine-wide config, but which file depends on the plugin generation, so setup writes the same db-less entry into both; the server follows your open workspace via MCP roots) | `AGENTS.md` + `.windsurf/rules/engram.md` (an `always_on` rule that binds the session first — Windsurf has no hooks) + `.windsurf/skills/` |
| `devin` | `.devin/mcp_config.local.json` (project-local, highest-precedence tier, explicit `--db`; git-ignored — it carries personal absolute paths) | `AGENTS.md` + `.devin/skills/` + a `SessionStart` hook in `.devin/hooks.v1.json` that injects the brief |
| `cursor` | `.cursor/mcp.json` (project-level, explicit `--db`; git-ignored when setup creates it — it carries personal absolute paths; an existing team file is never rewritten, the entry is printed for you to add) | `AGENTS.md` + `.cursor/skills/` (skipped when `.claude/skills/engram` exists — Cursor reads that too) + a `sessionStart` hook in `.cursor/hooks.json` that injects the brief (merged into an existing hooks file) |

**Skills and instruction blocks update with the binary.** Every skill setup
installs, and the engram section between the `<!-- engram:begin … -->`
markers in `AGENTS.md` / `GEMINI.md`, carries a small stamp: the version that
wrote it, its variant, and a fingerprint of the text. When you update
`engram-alpha`, the machine core rewrites the copies that are still exactly
as installed, keeping your variant, the first time an assistant session binds
each project (since 0.11.1; `serve` and `setup` do the same in their repo). A
copy you edited no longer matches its fingerprint and is left alone (`doctor`
lists it; delete it and run setup to take the new text). Your own text around
the markers is never touched, and symlinked skills are never followed.

Copies **git tracks** are not rewritten automatically, so a session never
leaves your working tree dirty: the brief and `doctor` name them, and
`engram-alpha setup --refresh` updates them when you choose to (a change to
commit). Hooks and rules (`.claude/settings.json`, `.windsurf/rules/`,
`.cursor/hooks.json`, …) are never updated automatically; re-run
`engram-alpha setup` to rewire them.

**Setup remembers.** `setup` records the binary version, the assistants it
wired, and the skill variant in `.engram/setup.json` (git-ignored, like the
graph). Re-running `setup` without `--cli` or `--skill` reuses them, `doctor`
checks the recorded assistants instead of guessing from what is installed,
`engram-alpha status` shows each project's setup under it, and
`engram-alpha status --setup` lists only the projects with something due
(it reads the registry, so no core is needed). After `engram-alpha update`
swaps the binary, it asks the new binary for the same list, judged against
the new release: skill copies the core will update on the next session,
tracked copies waiting for `setup --refresh`, and projects whose hooks and
rules predate a release that changed them (`engram-alpha setup` there
rewires them).

Windsurf support is freshly added and still being field-tested — reports
welcome. One Windsurf-specific note: its JetBrains plugin spawns the MCP
server from `/` or your home directory and its client never answers the
roots request, so those sessions carry no folder signal at all. They land on
the **default agent project** — set it in the pane under **Settings → System
info → Default agent project** so Cascade's memory goes to the project you're
actually working on; unset, such sessions bind the shared home graph
**read-only**: writes are refused with an error naming the rebind call until
the agent binds the session (since 0.9.8 — nothing lands in the wrong graph
silently). Running `engram-alpha setup --cli windsurf` inside a repo also
writes an `always_on` rule (`.windsurf/rules/engram.md`) whose first
instruction makes Cascade rebind the session itself — `brief` with `project`
set to the workspace path — and verify the brief's first line after every
resume or IDE restart (each spawns a new session, back on the fallback), so
multi-project work lands on the right graph without touching the default. Every wired assistant reads and writes the same graph through the
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
| Codex CLI / app | ✓ | ✓ auto (hook — trust once via `/hooks`) | — | ✓ |
| Gemini CLI | ✓ | manual | — | ✓ |
| OpenCode | ✓ | — | — | ✓ |
| Kilo Code | ✓ | — | — | ✓ * |
| Antigravity | ✓ | — | — | ✓ |
| Bob (IDE + Shell) | ✓ | ✓ auto (hook — IDE 2.0.2+ / Shell 2.0.1+) | — | ✓ |
| Windsurf / Cascade | ✓ ** | — | — | — *** |
| Devin CLI | ✓ | ✓ auto (hook) | — | — |
| Cursor | ✓ | ✓ auto (hook) | — | — |

\* Kilo's adapter is verified against fixture transcripts, not yet against a
live install — reports welcome.
\** Windsurf sessions carry no folder signal (see the note above) — the
generated rule makes Cascade bind the session itself, an unbound session
refuses writes, or set the default agent project.
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
`--bin-only` (accepted for compatibility — binary-only is now the default),
`ENGRAM_VERSION=vX.Y.Z` to pin a version.

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
install one-liner does the same thing and is always safe — it touches
nothing but the binary. Coming from **v0.3.0 or older**, when the binary was
named `engram`: both paths work — `engram update` lands on the current version
via the v0.4.x transition assets, the installer removes the old binary, and
re-running `engram-alpha setup` re-points your MCP wiring (idempotent).

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
