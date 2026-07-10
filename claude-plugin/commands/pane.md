---
description: Open the Engram graph pane — start the local daemon if it is not already running and share the URL.
allowed-tools: Bash
---

Get the Engram graph pane in front of the user.

1. If `.engram/daemon.json` exists in the repo root, read its `port` and check `http://127.0.0.1:<port>/health`. If healthy **and** the reported DB path is this repo's `.engram/graph.db`, the pane is already up.
2. Otherwise start the daemon yourself — from the repository root, as a background process:
   ```sh
   engram serve --http-only
   ```
   then re-read `.engram/daemon.json` for the real port (the default 8787 may be taken by another repo's daemon).
3. Tell the user the URL (`http://127.0.0.1:<port>`) — or, if they use the JetBrains plugin / VS Code extension, that the same pane lives in their IDE's Engram panel.

If the `engram` binary is missing, run `/engram:setup` first instead of improvising an install. If this repo has no `.engram/` graph at all, say so — the pane of an unwired repo would be an empty graph.
