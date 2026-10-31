# Engram Alpha — Claude Code plugin

One install wires Claude Code for [Engram](https://github.com/techtheist/engram): the MCP server, the capture skill, the session-start brief hook, and the per-repo setup command.

```
/plugin marketplace add techtheist/engram
/plugin install engram@engram
```

Then, in each repository you want remembered: `/engram:setup` (installs the `engram-alpha` binary if needed, git-ignores and creates `.engram/`), then `/mcp` → reconnect `engram`. `/engram:pane` opens the graph UI.

## What's inside

| Piece | What it does |
|---|---|
| `.mcp.json` + `bin/engram-mcp.sh` | The `engram` MCP server, in every project: an `engram-alpha mcp --wired-only` bridge. It binds repositories that are already Engram projects (a `.engram/` directory, or a folder inside a registered project) and never creates one; elsewhere it reads the home graph and refuses project writes, pointing at `/engram:setup`. The launcher finds the binary off Claude Code's PATH (`~/.local/bin`, `~/.cargo/bin`, Homebrew) and refuses a binary older than 0.9.9. |
| `skills/engram/` | The **relaxed** capture-skill variant (recommended default): recall before non-trivial work, capture durable knowledge silently, keep the graph honest. |
| `hooks/session-brief.sh` | SessionStart hook — injects the graph's brief so every session starts already briefed. Silent in repos without an `.engram/` graph, and defers to a repo-level registration (`engram-alpha setup`) so the brief never injects twice. |
| `skills/engram-digest/` | The **digest** skill — explicit, user-invoked ingestion of an existing project into the graph (offline `FIXME`/`TODO` scan + ontology-by-example authoring). Loaded only when invoked. |
| `commands/setup.md` | `/engram:setup` — per-repo wiring (binary → gitignore → `.engram/`). |
| `commands/pane.md` | `/engram:pane` — make sure the machine core is running and hand over the pane URL. |
| `commands/digest.md` | `/engram:digest` — digest the current project (optionally a named subsystem) into memory nodes. |
| `skills/engram-sleep/` | The **sleep** skill — on-demand consolidation of a crowded graph: work its subjects (piles of close notes), write state-of notes, merge restatements, link supersessions, mark the rest reviewed. Loaded only when invoked. |
| `commands/sleep.md` | `/engram:sleep` — run a sleep sweep over the review queue, or over one subject. |

Since 0.9.9 the plugin ships the MCP server itself. Earlier versions left it out because a plain `engram-alpha mcp` turns the folder it starts in into a project — a plugin-level server would have grown a graph in every repo you opened. The `--wired-only` bridge closes that: unwired folders stay untouched. A repository wired before 0.9.9 may still carry an `engram` entry in its `.mcp.json`; remove it, or Claude Code loads the engram tools twice (`engram-alpha setup --cli claude` says so when it finds one).

## Capture intensity

The bundled skill is the **relaxed** variant. For a fuller graph, install `normal` or `aggressive` at project level — `engram-alpha setup --cli claude --skill aggressive` — a project skill overrides the plugin's. The three variants are documented in [`skills/engram/`](../skills/engram/) at the repo root.

The skills and hook here are verbatim copies of `skills/engram/relaxed/SKILL.md`, `skills/engram/digest/SKILL.md`, `skills/engram/sleep/SKILL.md`, and `hooks/session-brief.sh`; tests in `crates/engram-cli` fail if they drift.
