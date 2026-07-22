# Engram Alpha — documentation

Everything here is for users: what each part does, how to use it, and what to
do when something is off. Start with [Getting started](./getting-started.md)
if Engram isn't installed yet.

| Page | What it covers |
|---|---|
| [Getting started](./getting-started.md) | Install, wire your assistants, first session, updating |
| [The memory model](./memory-model.md) | The eight node types, seven edge verbs, durability, capture modes |
| [Trust & decay](./trust.md) | How trust is computed, what moves it, pins, stale knowledge |
| [The pane](./pane.md) | The graph UI: layouts, tags and filters, editing, review, audit, history |
| [Recall & capture](./recall-and-capture.md) | The session brief, search, silent writes and their verdicts, code refs |
| [Conflicts & Checkup](./conflicts-and-checkup.md) | Suspected conflicts, judgments, claim checks, graph sweeps |
| [Multi-project memory](./multi-project.md) | The machine core, the registry, the home graph, promotion |
| [Storage & TepinDB](./storage.md) | The SQLite default, migrating to a `.tepin` file, the `npx tepindb` flow |
| [Local models](./models.md) | The cortex (embeddings, reranker, NLI), choosing models, staying offline |
| [Troubleshooting](./troubleshooting.md) | `doctor`, `stop`, daemons and ports, locked stores, common fixes |

Security posture — threat model, measures in place, known gaps — lives in
[`SECURITY.md`](../SECURITY.md) at the repository root. The full technical
spec and roadmap is [`PLAN.md`](../PLAN.md).
