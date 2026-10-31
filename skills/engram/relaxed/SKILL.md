---
name: engram
description: Read and write the project's durable reasoning memory (decisions, principles, cautions, problems, insights) through the Engram MCP tools. Recall relevant memory before non-trivial work; capture only durable, high-value knowledge silently at natural stopping points; keep the graph honest (judge suspects, close answered problems, repair drifted refs). Relaxed variant — the recommended default.
---

# Engram — project memory (Relaxed)

Engram is a local, user-owned graph of *why this project is the way it is*: decisions and their reasons, gotchas, problems and their fixes, conventions. Not code structure — the code holds that. The user sees every node in the pane, so quality and honest edges matter more than volume: a noisy graph stops being trusted, and an untrusted graph stops being read.

**Relaxed: capture only durable, high-value, non-obvious knowledge.** When in doubt, fewer and better nodes. Don't mirror Claude Code's own memory (CLAUDE.md, auto-memory): Engram holds project reasoning, not user preferences, workflow or code structure.

## Recall

- **Session start:** read the brief. If the session opened with an injected "# Engram brief", that IS it; otherwise call `brief` once. Its first line names the graph — if that is not this workspace, call `brief(project: "<workspace absolute path>")`.
- **Before any non-trivial decision, `search`.** Read the `confidence` verdict (`strong` act / `weak` verify first / `none` say the graph is silent) and each hit's `standing` (`contested` and `tombstone` are said out loud; `superseded` → read the replacement). If a Decision or Caution covers what you're about to do, follow it or raise the contradiction with the user before acting.
- **On `weak`/`none`, reformulate before concluding absence:** name the thing (entity-first), use the words a note would use, or scope in time (`after: "last week"`, `during_version`). Pass time expressions as written; the daemon resolves them.
- Start `detail: "compact"` when scanning; `full` only for hits you'll act on. `get_node` / `traverse` pull the reasoning around a hit; `timeline` walks a supersession chain; `audit` shows who changed what; `list_open` is the worklist; `list_nodes` is the lossless read for reviews and exports.
- **"Why is it like this?"** → search, follow `because`/`answers` edges and `timeline`, retell as a short dated narrative.
- **Session history** (if the user turned recording on): a weak verdict may carry a `history` section — leads, not knowledge. `expand_history(session, turn)` before relying on one; capture what matters as a proper note. `list_sessions` answers "what was I doing last Tuesday".

## Capture — what is worth a node

**Save (sparingly):**
- **Decision** — a choice made for a reason ("X because Y"). The backbone.
- **Principle** — a stable preference, convention or taste.
- **Caution** — a gotcha or constraint that will clearly bite later.
- **Problem** + **Resolution** — only when the problem was genuinely hard; skip routine fixes.
- **Insight** — rarely: a realization you'd regret losing.
- **Intent** — only when the user asks to remember deferred work.
- **Tombstone** — only when the user deliberately removes something with no replacement.
- **Anchor** — a subject several notes share.

**Decisions are never opt-in, and they hide in feature requests:** "add a login page" is a feature; *sessions in httpOnly cookies, not localStorage* is the Decision made while building it. At every stopping point ask: *what did I just choose, and why?* That holds in Relaxed mode too — restraint applies to everything else, never to decisions.

**Never save** secrets, credentials, tokens or PII; line numbers or transient state; or what code, git history or CLAUDE.md already record, or one-off chatter with no lasting value.

## How to write

1. **Title** = the claim in one declarative sentence. **Body** = the why in 1–3 sentences. Use exactly the graph's types — default 9: `Decision`, `Principle`, `Caution`, `Problem`, `Resolution`, `Insight`, `Intent`, `Anchor`, `Tombstone`; if the brief teaches a custom ontology or a write is refused for an unknown type, `describe_ontology` and use that. Custom fields go in `"fields": {...}` when the graph declares them; refusals name the roster.
2. **Batch** with `add_notes`, then **link** the returned ids. Calls are keyword JSON; notes never carry links:
   `add_notes {"notes": [{"type": "Decision", "title": "…", "body": "…"}]}` → `link {"from": "<id>", "to": "<id>", "type": "because"}`.
   Edges read as a sentence, from → verb → to:
   `because` (Decision → its reason) · `answers` (Resolution → Problem/Intent) · `about` (any → **Anchor only**) · `builds-on` · `replaces` (newer → older; edge note = why it changed) · `conflicts-with` (always create it when you see a contradiction) · `needs` (Intent → blocker). No verb fits → don't link.
3. Two or three notes on one subject → create or reuse an **Anchor** and `about`-link them. `code_refs` are repo-relative paths, never line numbers.
4. **The write response is a verdict — act on it in the same turn:**
   - `matched, created: false` → a near-duplicate exists: `update_node` it, never re-add.
   - `suspects` → judge each now with `resolve_suspect`: contradict → `conflict` **and tell the user in one line**; yours is the fresher truth → `replaces`; fine together → `dismiss` (then add the real edge if one fits).
   - `warnings` → you landed near contested or superseded canon: read it, align or record the disagreement. `tombstoned` → a person removed this on purpose: don't re-add it; tell the user.
   - `canon` → `supports`: link the support; `contradicts`: read it, `conflicts-with` if real, tell the user.
5. Wrong edge? `unlink`, or `update_edge` to settle it.

## Keep the graph honest

- **Judge the brief's suspects first**, same rules as above. A Resolution next to the Intent it implements → `dismiss`, ensure the `answers` edge, close the Intent. Hint `inherited` → the contradiction flows through an edge; if the other note is now wrong too, conflict or fix it.
- **Close the loop:** a Resolution that `answers` a Problem/Intent → set that node `resolved`.
- **Merge** several notes with the same knowledge via `merge_nodes` (you write the merged body).
- **Bury, don't delete:** when the user kills something on purpose with no successor, add a Tombstone `Removed: <title>` with the why, then link it `replaces` → the victim. Hard delete is user-only.
- **Too crowded:** search keeps returning restatements of one subject, or the user finds memory noisy → suggest a sleep in one line (`/engram-sleep [subject]`); run it only on a yes.
- **Drift:** `list_drift` → fix moved paths, and re-read the claim — if the code change made it false, supersede it.
- **Stale hits:** verify; still true → `update_node` (that re-confirms it); wrong → supersede or conflict.
- **Version:** notes are stamped automatically; when a release is cut or a cycle opens, `set_version`.
- **Handoff:** something the next session must read before anything else → an open Intent tagged `handoff`; resolve it when acted on. Work in progress belongs in tasks.

## Tasks — your working list

Tasks live **beside** the graph: never searched, never knowledge. The brief's `## Tasks` lists the active titles, so the next session knows what you were doing. **Use them when the user asks you to plan, or when work clearly spans sessions** — not for a single-session fix.
- **Resume from the brief:** its `## Tasks` section is where the last session stopped — `list_tasks` reads the body (plan, progress lines) of the one you pick up. `search` never returns tasks; `list_tasks` with `ref` finds the tasks about a note.
- **Task or Intent?** An Intent is durable reasoning worth remembering ("we will do X, because Y" — a cycle plan, a deferred idea); a task is the doing. Plan as an Intent, track the work as tasks that `refs` it.
- **One task per piece of work, not per step** — steps go in the body as a checklist. A feature with several pieces is one group: `add_tasks {"tasks": [{"title": "Feature X"}, {"title": "Its store part", "parent": "$1"}]}`. Groups are one level deep.
- **Keep it current:** stopping mid-way → `update_task` with `progress` (where you stopped, what's next); finished → `status: "done"`; blocked → `delayed`; abandoned → `dropped`. A `matched` verdict means the task already exists — update it.
- **Rules close or wake a task on their own:** `close` / `wake` take a version (`"0.10.0"`, `">0.9.12"`), a date (`"in 3 days"`) or `"node:<id>"`. A release-shaped title ("publish 0.9.12") closes itself once the version moves past it.
- **The user edits the board too.** A status, order or rule they set stands — don't undo it; if it blocks you, say so. Task writes are silent, like notes.
- **Done settles something?** Capture it as a note and add the id to the task's `refs`. Tasks point at notes; notes never point back.
- The brief marks a task `idle` or `↻ from <version>`: finish it, park it, or drop it — don't let the list rot.
- **Tasks can be turned off per graph.** If a task tool answers that they are off, don't keep a list there — work without one (a `handoff` Intent carries what the next session must see first).

## Trust

Trust is computed from deliberate acts, never from being read. `approve_node` only on the user's explicit word, or after verifying the note word-by-word. Pins are user-only — if a pinned note looks wrong, tell the user. `check_claim("<one sentence>")` asks the canon before you act on an assumption: `contradicts` → surface it; `retracted` → a Tombstone forbids it.

## Projects, settings, subagents

- **`project`** on most tools reaches another graph: knowledge about a sibling goes in **its** graph; `home` holds user-level canon ("remember this everywhere"); `all` reads across graphs (attribute foreign hits). Edges never cross graphs.
- **`brief(project: …)` rebinds the session** — later writes go there. If its reply warns that writes moved and you only meant to read, go back at once with the call it names; for read-only looks, pass `project` to `search` / `list_open` / `get_node` instead. A write refused as "bound … by fallback" → bind with `brief(project: <workspace path>)`, then retry. Every write verdict names its project.
- **Settings** (history recording, ontology, presets, fields, knobs, models) change only on the user's ask, over the core's HTTP API: read `GET /guide` first (port in `.engram/daemon.json`). Never on your own judgment.
- **Subagents** share the MCP tools but not the brief or this skill: pass them the relevant node ids, and have them *return* findings for you to capture. A subagent that must write gets the verdict rules in its prompt.

## Cold start, failures, etiquette

- **Empty graph:** offer the user a one-time seeding pass (the one capture that is not silent). With a yes, read README, design docs and recent git history and batch-capture decisions with reasons, principles, cautions and open intents — seed conservatively: a dozen durable, still-true nodes beat fifty mirrored doc lines. If `.engram/` exists or the project plainly had memory, an empty brief means a wrong cwd or db — check `.engram/daemon.json` before seeding; never seed a duplicate graph.
- **Tools vanished mid-session:** don't drop captures — `POST /nodes` to the core (URL in `.engram/daemon.json`, `"source": "claude"`), or list what you'd have saved and ask the user to `/mcp` reconnect.
- **Core not running:** run `engram-alpha serve` in the repo root (it exits on its own), then share the pane URL (default `http://127.0.0.1:8787`). Binary missing → point to the README; don't improvise an install.
- Ids come only from tool output — never guess or shorten one.
- **Write silently, at natural stopping points.** Speak only for the seeding offer and genuine contradictions (or when asked what you saved). A manual `/engram` means act now.
