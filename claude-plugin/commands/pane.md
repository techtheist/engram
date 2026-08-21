---
description: Open the Engram graph pane — make sure the machine core is running and share the URL.
allowed-tools: Bash
---

Get the Engram graph pane in front of the user.

1. If `.engram/daemon.json` exists in the repo root, read its `port` and check `http://127.0.0.1:<port>/health`. If healthy, the pane is already up — one machine core serves every project (its health reports the home graph; that's normal, the pane's project switcher has this repo).
2. Otherwise run, from the repository root:
   ```sh
   engram-alpha serve
   ```
   It makes sure the machine core is running (starting it detached if needed), registers this repo with it, prints the pane URL, and exits on its own — no backgrounding needed. Then re-read `.engram/daemon.json` for the real port (the default 8787 may have been taken).
3. Tell the user the URL (`http://127.0.0.1:<port>`) — or, if they use the JetBrains plugin / VS Code extension, that the same pane lives in their IDE's Engram panel.

If the `engram-alpha` binary is missing, run `/engram:setup` first instead of improvising an install. If this repo has no `.engram/` graph at all, say so — the pane of an unwired repo would be an empty graph.
