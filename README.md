# Engram

> Durable, inspectable long-term project memory for AI coding assistants. Local-first, user-owned, graph-first.

Engram gives your AI coding assistant a memory that survives across sessions: a structured graph of your project's **principles, decisions, cautions, problems, resolutions, insights, and intents** — that the assistant reads from and writes to, and that **you can see, edit, and own**.

![The Engram pane: hybrid search over the graph, a review queue for approving what the assistant saved, and the full reasoning trail behind every node](.screenshots/engram-alpha-standalone.png)

<details>
<summary><b>Inside JetBrains IDEs</b> — the same pane as a tool window, live next to your assistant <i>(click to expand)</i></summary>
<br>

![Engram tool window in IntelliJ IDEA: the graph updates live while Claude Code works in the terminal below](.screenshots/engram-alpha-jetbrains.png)
</details>

<details>
<summary><b>Inside VS Code</b> — the pane in the secondary sidebar, watching the assistant work <i>(click to expand)</i></summary>
<br>

![Engram pane in VS Code's secondary sidebar: the memory graph fills in while the assistant explains the project](.screenshots/engram-alpha-vscode.png)
</details>

Unlike a flat note pile, Engram's graph is *active*: it tracks when knowledge is **superseded** (`replaces`) and when it **conflicts** (`conflicts-with`), and it knows which facts are durable vs. likely to go stale — so it doesn't rot.

The payoff shows up the second time something goes wrong. When your assistant gets stuck on a problem it has fought before — a flaky build step, a library quirk, a config trap — the graph already holds the artifacts from last time: the **Problem**, the **Resolution** that answered it, and the **Caution** that would have prevented it. Instead of rediscovering the fix from scratch, the assistant recalls it and applies it.

- **Local-first** — your memory is a local SQLite graph. No cloud, no keys.
- **Portable** — one backend serves Claude Code, Claude Desktop, and a browser UI via MCP + HTTP.
- **Graph-first** — the graph is the product surface, not hidden plumbing.

**Status:** early development. See [`PLAN.md`](./PLAN.md) for the full spec and roadmap.

## Capture modes

How much your assistant writes to the graph is set by which **skill variant** you install — see [`skills/engram/`](./skills/engram/):

- **relaxed** *(recommended default)* — only durable, high-value knowledge; fewer, better nodes.
- **normal** — balanced: adds cautions, selective insights, finer-grained decisions.
- **aggressive** — maximum capture; Engram becomes the spine of the project's decision history (stale, unused nodes decay out).

Engram complements Claude Code's built-in memory rather than replacing it: it holds the project's *reasoning* — decisions, conflicts, gotchas — not session workflow or code structure.

## Stack
Rust core (`rmcp`, `rusqlite`, `sqlite-vec`, `fastembed`) · Vue 3 + TypeScript + Vue Flow · JetBrains (JCEF) & VSCode (Webview) wrappers · MIT licensed.

## License
[MIT](./LICENSE)
