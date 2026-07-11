---
description: Wire this repository to Engram — install the binary if missing, git-ignore the local graph, register the MCP server.
allowed-tools: Bash
---

Wire the current repository to Engram. The plugin already provides the capture skill and the session-brief hook globally, so the only per-repo work is the binary and the MCP registration — never install a project-level skill or hook here.

1. **Binary.** Check `command -v engram-alpha`. If missing, ask the user once for consent to install, then run:
   ```sh
   curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh -s -- --bin-only
   ```
   (`--bin-only` matters: the installer's default repo wiring would duplicate what this plugin ships.) If they decline, stop and point them at https://github.com/techtheist/engram#install.

2. **Wire the repo.** From the repository root:
   ```sh
   engram-alpha setup --cli claude --mcp-only
   ```
   This git-ignores `.engram/` and writes the `engram` MCP server into `.mcp.json` (or prints the snippet if a foreign `.mcp.json` exists — apply it, `.mcp.json` holds machine-absolute paths, so keep it out of version control).

3. **Connect.** Tell the user to restart the session (or approve the new MCP server via `/mcp`) so the `engram` tools appear. The next session opens pre-briefed via the plugin's SessionStart hook.

4. **Cold start.** If this created a brand-new graph, mention that once connected, the skill offers a one-time seeding pass from the project's existing docs/history — and that `/engram:pane` opens the graph UI.

If anything fails, report the exact error — never pretend the repo is wired.
