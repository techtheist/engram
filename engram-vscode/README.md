# Engram — VS Code extension

[![Build](https://github.com/techtheist/engram/actions/workflows/vscode.yml/badge.svg)](https://github.com/techtheist/engram/actions/workflows/vscode.yml)
[![VS Marketplace](https://vsmarketplacebadges.dev/version/techtheist.engram-alpha.svg?label=VS%20Marketplace)](https://marketplace.visualstudio.com/items?itemName=techtheist.engram-alpha)
[![Installs](https://vsmarketplacebadges.dev/installs-short/techtheist.engram-alpha.svg)](https://marketplace.visualstudio.com/items?itemName=techtheist.engram-alpha)
[![Open VSX](https://img.shields.io/open-vsx/v/techtheist/engram-alpha?label=Open%20VSX)](https://open-vsx.org/extension/techtheist/engram-alpha)
[![Downloads](https://img.shields.io/open-vsx/dt/techtheist/engram-alpha)](https://open-vsx.org/extension/techtheist/engram-alpha)

Embeds the [Engram](https://github.com/techtheist/engram) graph pane in VS Code.
Engram is a durable, graph-based long-term memory for AI coding assistants — the
*reasoning/decision* layer (why we chose this, what bit us, what's still open),
shown as a graph you can see, edit, and own.

The extension hosts the Engram pane (the Vue app, **bundled into the extension**)
in a Webview and talks to the local **`engram serve`** daemon — the same daemon
your AI assistant reads from and writes to over MCP. Decisions, cautions,
problems, and insights surface and update live as you work.

<!-- Absolute URL on purpose: this README is also the Marketplace listing,
     where repo-relative paths outside the extension folder don't resolve. -->
![Engram pane in VS Code's secondary sidebar: the memory graph updates live while the assistant works](https://raw.githubusercontent.com/techtheist/engram/main/.screenshots/engram-alpha-vscode.png)

## Requirements

- **The Engram backend.** From your project's root:

  ```sh
  curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh
  ```

  This installs the `engram` binary (checksum-verified, into `~/.local/bin`),
  writes the workspace `.mcp.json` for Claude Code, and installs the capture
  skill. On Windows, run the same command inside WSL2 — it installs the native
  `engram.exe`. The pane and status bar connect to the daemon at
  `http://127.0.0.1:8787` (configurable via `engram.daemonUrl`).

## Use

1. Run `engram serve` in your project.
2. Open the **Engram** view from the activity bar (graph icon), or run
   **Engram: Open Graph in Editor** for a center-tab view.
3. The **status bar** shows daemon connectivity; if the daemon is down the pane
   shows a Retry overlay and reconnects on its own once it's up.

### MCP for Claude Code

The install script above already writes `.mcp.json`. If you skipped it (or
installed the binary another way), run **Engram: Configure MCP for Claude Code**
to add an `engram` server to the workspace `.mcp.json` (it merges, never
clobbers other servers):

```json
{ "mcpServers": { "engram": { "command": "engram", "args": ["mcp", "--db", ".engram/graph.db"] } } }
```

Restart Claude Code to pick it up. Requires the `engram` binary on your PATH.

## Build

The pane is the repo's `frontend/` build, copied in at package time.

```sh
npm install
npm run build:pane   # builds ../frontend (needs Bun)
npm run build        # copies the pane in + bundles the extension (esbuild)
npm run package      # -> engram-<version>.vsix
```

Install the `.vsix` via *Extensions view → ⋯ → Install from VSIX…*.

## Scope (first pass)

Pane + MCP config + daemon **detect-and-guide** (no spawning — matches the
JetBrains plugin). Auto-starting the daemon and bundling the capture skill are
follow-ups.
