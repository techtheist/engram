# Engram operator's guide — configuring a graph over HTTP

This is the manual for an assistant that has been asked to change how a
project's memory behaves. Everything the pane's Settings can do, the core's
HTTP API can do; the MCP tools deliberately cannot (they read and write
knowledge, never its shape). Read this once when a user asks for a change,
or once on a cold start when you review the settings with them.

## Ground rules

1. **Config is the user's.** Change it on their explicit ask or with their
   agreement after you proposed it. Never reshape an ontology, drop a field,
   wipe history, or swap a model on your own judgment.
2. **Read before you write.** `PUT /config` replaces the whole document:
   `GET /config`, edit the fields you mean to change, send it all back.
   A refusal is a teaching error — it says what is wrong; fix that, retry.
3. **Nothing is stranded.** A type that still has nodes, a verb that still
   has edges, and a field that still has values cannot be removed by a PUT.
   Use the rename routes to migrate, or retype first.
4. **Tell the user what changed**, in one sentence, after every write.
5. **After reshaping the ontology or fields**, reinstall the capture skill
   (`POST /skills/install`) so the assistant's own instructions match, and
   turn on the brief's ontology section so every fresh session learns the
   vocabulary (`brief.ontology.show = true`).

## Reaching the core

- Port: `.engram/daemon.json` in the repo (`port`), or the machine core's
  `~/.engram/daemon.json` (default `8787`). Base URL `http://127.0.0.1:<port>`.
  Loopback only, no auth.
- Project scoping: every graph route also exists as
  `/projects/<id-or-name>/<route>` — `GET /projects` lists them. Bare routes
  address the core's launch graph, which on the machine core is the home
  graph, so **always scope to the project** you mean.
- The pane shows the same settings under the gear menu; anything you change
  here is visible there immediately.

```sh
PORT=$(sed -n 's/.*"port"[: ]*\([0-9]*\).*/\1/p' .engram/daemon.json)
curl -s "http://127.0.0.1:${PORT}/projects/<name>/config"
```

## The config document (`GET /config`, `PUT /config`)

Top-level keys: `ontology`, `policy`, `brief`, `versioning`, `history`,
`fields`. Everything travels with the graph (export/import, migration).

### `ontology` — types and verbs

- `preset`: provenance only (`engram`, `research`, `minimal`, `general`,
  `custom`).
- `types[]`: `name` (1–32 chars), `hue` (0–359, the one color input),
  `thought` (the teaching line `describe_ontology` shows), `durability`
  (`stable` | `episodic` | `volatile`, the default for new nodes),
  `roles` {`worklist` (open/resolved lifecycle, never decays while open),
  `anchor` (code-subject node carrying `code_refs`), `rank_prior` (0–0.5,
  ranking only), `highlight`, `versioned` (stamped with the working
  version), `tombstone` (deletion marker; sits out the conflict scan)},
  `brief` {`show`, `cap`, `excerpt`} — this type's canon section in the brief.
- `verbs[]`: `name` (lowercase, hyphen-joined so "from verb to" reads as a
  sentence), `reads_as` (a worked example), `roles` {`supersession`,
  `contradiction`, `reason`, `answer`, `dependency`, `inherits`}.
  `inherits` (optional; unset = on for `dependency`/`reason` verbs and
  `builds-on`) means the source holds what the target holds: when a note
  contradicts or replaces the target, the source is queued for review with
  hint `inherited`. Exactly one verb must
  carry `supersession` and exactly one `contradiction` — these are the two
  edges that make the graph active. Move the role, never remove it.
- **Add a type or verb**: append to the array and PUT. **Rename**:
  `POST /config/rename-type {"from","to"}` / `POST /config/rename-verb`
  bulk-retype stored rows — a PUT with a changed name is refused as a drop.
  **Remove**: only when nothing uses it.
- **Apply a preset**: `GET /config/presets` returns `[{id, name,
  description, config}]`; `POST /config/presets/{id}` applies one — it
  replaces types and verbs only, and keeps the graph's policy (auto-tuned
  values included), brief, versioning, history and fields. On a graph with
  nodes it lands only when the type names line up, or after you retyped.

### `fields[]` — custom fields on notes

Each: `name` (lowercase snake_case, ≤32 chars, never a built-in node key),
`label`, `kind` (`text` | `number` | `bool` | `date` | `enum` | `url`),
`values` (the vocabulary when `kind` is `enum`), `required` (writes of
applicable types that omit it are refused), `applies_to` (type names; empty =
all), `indexed` (the value joins search — TepinDB graphs only),
`show_in_brief`. A `date` field is also a search clock (`date_field` on
search). Rename with `POST /config/rename-field {"from","to"}` — every stored
value moves. Assistants then pass `"fields": {"name": value}` on
`add_note`/`update_node`; `describe_ontology` lists the roster.

### `brief` — what the session-start digest contains

`total_chars` (budget, ~4 chars per token), `tags` {show, cap}, `conflicts`
{show; uncapped}, `suspects` {show, cap}, `recent` {show, cap, excerpt},
`open` {show, cap, excerpt}, `handoff` {show, cap, excerpt} (notes tagged
`handoff` get guaranteed first placement), `cycle` {show, cap, excerpt} (with
version tracking on: open work stamped with the working version, placed
first), `tasks` {show, cap, recent_done, idle_sessions} (the `## Tasks`
working list: active top-level titles up to `cap`, the last `recent_done`
done titles, and an idle mark after `idle_sessions` briefed sessions without
a change; 0 turns either off), `home_reserve` (chars kept for the home-graph
section), `ontology`
{show} — **teach this graph's types, verbs and custom fields at the top of
every brief**. Off in the shipped preset because the skill already teaches
the default ontology; turn it on for any customized ontology or when custom
fields exist.

Two shape switches:

- `bodies` (default `true`): entries carry a body excerpt after the title.
  `false` = titles only, every section's `excerpt` ignored. Turn it off when
  this graph's titles are already whole claims (long, sentence-shaped) — the
  same budget then holds more entries; keep it on when titles are short
  labels and the fact lives in the body. Read a few titles first
  (`GET /graph` returns every node — look at thirty live titles and their
  bodies) and say which you saw.
- `canon_order` (default `"endorsed"`): how each type's canon section picks
  entries. `"endorsed"` = pinned, then approved, then newest endorsement or
  capture. `"connected"` = pinned, then trust weighted by live edges — the
  notes other knowledge leans on lead. Prefer `connected` on a mature,
  well-linked graph whose canon sections show old or peripheral notes.

### `history` — session recording

`enabled` (master switch, **off by default**; turning it on is a user
decision: the harvester reads coding-assistant transcripts on this machine
and seals them with a key in the OS keystore — a keychain prompt on macOS),
`harnesses` {`claude_code`, `codex`, `gemini`, `opencode`, `kilo`,
`antigravity`, `bob`: per-harness on/off}, `exclude_paths` (absolute paths
never read), `search_fallthrough` (may history answer when curated memory is
silent), `recency_collapse` (0–1 cosine at which `order: "recent"` folds an
older restatement under a newer one). Status: `GET /history` → `{enabled,
open, search_fallthrough, stats}`. `DELETE /history` wipes the recording —
user-only, never call it unless the user asked for exactly that.

### `versioning` — the working version stamp

`enabled` turns tracking on; the current value is `GET /version` and
`PUT /version {"version": "0.9.8"}` (`null` clears). The `set_version` MCP
tool does the same and enables tracking on first use. Types with
`roles.versioned = false` (Principle, Anchor in the shipped set) are exempt.

### `policy` — trust, decay, and retrieval knobs

Trust: `trust_created`, `trust_confirmed`, `trust_approved`,
`trust_approved_floor`, `trust_floor`, `stale_trust`; windows in days:
`episodic_window_days`, `volatile_window_days`, `approved_window_days`,
`decay_ttl_days` (days below stale before the decay pass archives a
provisional node). Write-time judgment: `duplicate_similarity`,
`conflict_suspect_similarity`, `conflict_nli_gate` (`null` = off),
`warn_similarity`, `nli_sweep_min_confidence`,
`claim_contradiction_min_confidence`. Retrieval: `keyword_weight` (BM25 share
of relevance), `semantic_floor`, `search_min_score`, `search_relative_cut`,
`window_overfetch`, `rerank_trust_weight`, `rerank_vote_k` (`null` = the
reranker has the final word), `rerank_full_note`, `delivery_floor`,
`weak_evidence_top`, `knee_cliff` (`null` = off; **`0` is the harshest trim,
not off** — the config refuses it), `weak_line_quantile`,
`weak_line_probes`, `session_diversity_demote` (`0` = off),
`twin_trust_order` (cosine band for endorsed-first order among
near-identical hits — pinned, then higher trust, then the most recent
endorsement on the same rung; `null` = off), `auto_tune`
(let a mature graph refit its own conflict floor and weak line).

Caveats to say out loud: the defaults were measured on the eval bench and
absolute thresholds do not transfer between graphs of a different register;
change one knob at a time, on a stated reason, and prefer the pane's
plain-word rendering when the user wants to understand a number. The
similarity thresholds were calibrated on the default embedding model.

## Machine-level settings (the core, not one graph)

- `GET /settings` / `POST /settings {"default_agent_project": "<name|id|home>"}`
  — which project a session binds when its client gives no folder signal
  (`null`/`"home"` clears). Future sessions only.
- `GET /encryption` / `POST /encryption {"target": "graph"|"history",
  "enabled": true|false}` — at-rest sealing per store kind (graph off by
  default, history on). The POST starts a migration of the current project's
  store; poll the GET for `job` progress; other projects converge when next
  opened. Ask before flipping the graph switch: exports stay plaintext, and
  the key lives in the OS keystore.
- `GET /models` / `POST /models {"role": "embedding"|"reranker"|"nli",
  "preset": "<name>"}` or `{"role", "custom": {"name", "base_url",
  "model_file", "dim", "pooling"}}` — the GET lists roles, current
  selections and preset names. Reranker and NLI swaps apply instantly; an
  **embedding swap re-embeds every open graph and the call blocks until it
  is done** (minutes on a large graph). Warn the user, and that duplicate /
  conflict thresholds were calibrated on the default embedder.
- `GET /projects` / `POST /projects {"path": "/abs/repo"}` /
  `DELETE /projects/{id}` — the registry; a repo also registers itself when
  `engram-alpha serve` runs inside it. Home and `/` are never project roots.

## Tasks (the working list beside the graph)

Tasks are not knowledge. The only setting is `tasks.enabled` in the config
document (default `true`); `false` turns the feature off for that graph —
the brief drops its section and the task tools refuse with a message that
tells the assistant to work without them. Stored tasks are kept and come
back when it is turned on again. Change it only on the user's ask. Assistants use
the `add_tasks` / `update_task` / `list_tasks` MCP tools; the pane uses
`GET /tasks` (`?status=open|closed|all|<status>,…`, `parent`, `ref`),
`POST /tasks`, `PATCH /tasks/{id}` (`""` clears body, parent, wake, close),
`DELETE /tasks/{id}` (user-only hard delete; a group takes its tasks along)
and `POST /tasks/order {"ids": [...]}`. Never delete a task on your own —
close it (`done`) or drop it (`dropped`).

## Teaching surfaces

- `POST /skills/install {"variant": "relaxed"|"normal"|"aggressive"}`
  regenerates the capture skill in the repo from the live ontology (the
  canonical text when the graph runs the shipped set). Do it after every
  ontology or field change.
- The `describe_ontology` MCP tool renders the live vocabulary on demand;
  `brief.ontology.show` puts the same text at the top of every brief.

## Maintenance the user may ask for

`POST /conflicts/scan` (queue suspected conflicts now), `GET /conflicts/suspects`,
`POST /decay?dry_run=true` (preview what the decay pass would archive; omit
`dry_run` to run it), `GET /drift` (nodes whose `code_refs` no longer
resolve), `POST /audit/conflicts`, `POST /audit/duplicates`,
`POST /audit/answered`, `POST /audit/promotions`, `POST /audit/stale`
(checkup sweeps — nominations, never verdicts), `GET /export` / `POST /import`
(the whole graph as JSON, config included), `GET /crowding` (per-note
crowding and heat, plus the unreviewed subjects, each with a model-free
name), `GET /review`,
`GET /review/conflicts`, `GET /review/islands` and `POST /review/marks` (the
sleep queues — over MCP an agent reads them with `list_nodes` `view:
"review" | "conflicts" | "islands"` and marks with `update_node` `reviewed: true`;
the `engram-sleep` skill runs a sweep when the user asks or takes the
agent's suggestion). Hard delete, pinning and undo
(`POST /audit/{seq}/undo`, `POST /audit/sessions/{sid}/undo`, the audit
view's buttons) stay pane-only: point the user at the audit view rather than
calling them.

## Before a digest: review the settings with the user

An ingestion writes dozens of nodes into whatever ontology the graph has, so
the shape should be chosen first. `GET /config`, then ask: does the shipped
9-type set (decisions, principles, cautions, problems, resolutions,
insights, intents, anchors, tombstones) fit this project, or would a preset
(`research`, `minimal`, `general`) or a custom set fit better? Would custom
fields carry something the user keeps repeating in bodies — an owner, a
component, a ticket, an effective date? If anything is customized, set
`brief.ontology.show = true` and reinstall the skill before the first write,
so every session that follows speaks the graph's language.
