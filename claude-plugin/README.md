# Engram Alpha — Claude Code plugin

One install wires Claude Code for [Engram](https://github.com/techtheist/engram): the capture skill, the session-start brief hook, and the per-repo setup command.

```
/plugin marketplace add techtheist/engram
/plugin install engram@engram
```

Then, in each repository you want remembered: `/engram:setup` (installs the `engram` binary if needed, git-ignores `.engram/`, registers the MCP server). `/engram:pane` opens the graph UI.

## What's inside

| Piece | What it does |
|---|---|
| `skills/engram/` | The **relaxed** capture-skill variant (recommended default): recall before non-trivial work, capture durable knowledge silently, keep the graph honest. |
| `hooks/session-brief.sh` | SessionStart hook — injects the graph's brief so every session starts already briefed. Silent in repos without an `.engram/` graph, and defers to a repo-level registration (`engram-alpha setup`) so the brief never injects twice. |
| `skills/engram-digest/` | The **digest** skill — explicit, user-invoked ingestion of an existing project into the graph (offline `FIXME`/`TODO` scan + ontology-by-example authoring). Loaded only when invoked. |
| `commands/setup.md` | `/engram:setup` — per-repo wiring (binary → gitignore → `.mcp.json`). |
| `commands/pane.md` | `/engram:pane` — start the daemon if needed and hand over the pane URL. |
| `commands/digest.md` | `/engram:digest` — digest the current project (optionally a named subsystem) into memory nodes. |

The plugin deliberately ships **no global MCP server**: `engram-alpha mcp` binds a `.engram/graph.db` in whatever project it starts in, and a plugin-level server would create one in every repo you open. MCP registration stays per-repo via `/engram:setup`.

## Capture intensity

The bundled skill is the **relaxed** variant. For a fuller graph, install `normal` or `aggressive` at project level — `engram-alpha setup --cli claude --skill aggressive` — a project skill overrides the plugin's. The three variants are documented in [`skills/engram/`](../skills/engram/) at the repo root.

The skills and hook here are verbatim copies of `skills/engram/relaxed/SKILL.md`, `skills/engram/digest/SKILL.md`, and `hooks/session-brief.sh`; tests in `crates/engram-cli` fail if they drift.
