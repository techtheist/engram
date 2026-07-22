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

Every wired assistant reads and writes the same graph through the same MCP
server — one shared, local memory across your AI agents: a decision captured
by Claude is recalled by Codex. The `AGENTS.md`/`GEMINI.md` additions are a
marked, idempotent section; re-running the installer never duplicates them.

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

## When something is off

```sh
engram-alpha doctor
```

checks the whole chain from your repository's root — store integrity, the
local models, whether the running daemon actually serves *this* repo, and
every detected assistant's wiring — and says exactly what to fix. It exits
non-zero on real failures, so it doubles as a pre-flight in scripts. More in
[Troubleshooting](./troubleshooting.md).
