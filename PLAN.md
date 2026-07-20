# Engram Alpha — Plan & Spec

> **Name (locked 2026-07-06).** The product is **"Engram Alpha"** — *Alpha is part of the name, not a version tag*; plain "engram" is contested four ways in the AI-memory space (see §1A). The binary is **`engram-alpha`** (renamed from `engram` in v0.4.0 so shell completions and docs match the product); this repo and the JetBrains plugin keep the short name; the **VSCode extension publishes as `engram-alpha`**.

A graph-based, durable, **inspectable long-term project memory** for AI coding assistants (Claude Code first). Engram sits beside your assistant as a memory it can read from and write to — and that you can see, edit, and own. The graph is the product surface, not hidden plumbing.

> **History note (2026-07-10):** shipped work is summarized here in one line each — the detailed rationale and post-mortems live in the Engram graph itself (`engram-alpha serve` → pane, or the `search`/`timeline` MCP tools) and in CLAUDE.md's Status block.

---

## 0. Locked decisions

| Decision | Choice |
|---|---|
| **Goal** | Open-source / portfolio; optimize for docs, DX, shareability |
| **Name** | **Engram Alpha** ("Alpha" part of the name, not a version) |
| **License** | MIT |
| **Memory scope** | **Per-repo only** (v1); cross-project layer now planned — **§7C** (federation + home graph, rides the TepinDB migration) |
| **Build first** | Browser standalone (doubles as Claude Desktop support); IDE wrappers later |
| **Embeddings** | **Local-only** — `fastembed`/ONNX; no remote, no keys |
| **Backend language** | **Rust** — `rmcp`, `rusqlite` (bundled), `sqlite-vec`, `fastembed` |
| **Capture (v1)** | **Cooperative (MCP) first**; ambient hooks later |
| **Conflict handling (v1)** | Passive, surfaced via RAG; mid-session push is a Phase-2 feature |
| **Storage** | `.engram/graph.db` in repo, **git-ignored**; share via JSON export, never the binary DB |
| **Live updates** | SSE (axum → pane) |
| **Deletion** | Supersede auto; **hard-delete user-only** (cascades edges) |
| **Backend run (v1)** | `engram-alpha serve` — one local daemon per repo (HTTP + MCP + SSE); **§7C plans the hub-daemon successor** (one per user), shipping feature-first on SQLite, before TepinDB |
| **Bootstrap / export** | Empty start; JSON export in v1 (embeddings stripped, regenerated on import) |
| **Secrets** | Skill rule + backend redaction pass (regex + high-entropy) |
| **Validation** | **Dogfood on Aggressive mode** — we're the first user |
| **Engineering style** | Minimalist; clear names + deps' public docs over inline comments |
| **Merge identity** | Claude searches, then updates; `add_note` self-checks same-type dupes |
| **Concurrency** | SQLite WAL, serialized writes, last-write-wins |
| **MCP transport** | Streamable HTTP from the daemon (planned; **current divergence:** stdio `engram-alpha mcp`, see CLAUDE.md) |
| **Session id** | From the MCP transport; daemon mints a fallback if omitted |
| **Distribution** | Local binary first; thin marketplace plugin + skill-driven checksum-verified fetch; `cargo-dist` pipeline later |

---

## 1. Vision

When you work with an AI assistant on a long-lived project, the valuable context — *why* you chose something, *what* bit you last time, *how* a gnarly bug got solved — evaporates between sessions and overflows any context window. Engram captures that as a **structured, durable, user-owned graph** the assistant consults and contributes to over the life of the project.

### What makes it different (the wedge)
1. **An *active* graph, not a passive note pile** — `replaces` and `conflicts-with` edges surface staleness and contradictions. Almost nobody does this.
2. **Durability-aware memory** — every node knows if it's stable, episodic, or volatile; this is what stops the graph rotting (the core failure mode of every memory tool).
3. **Graph-first UI** — the user sees and edits the canon directly in their own pane, not buried in a chat.
4. **Local-first and portable** — a local SQLite graph you own; one backend serves any MCP client.

---

## 1A. Competitive positioning (mid-2026 scan)

The space splits into two layers; Engram sits in the under-served one:

- **Code-structure graphs** (Graphify ~58K★, CodeGraph, …) auto-derive architecture from the codebase. Regenerable, stale-with-the-code. **Complementary, not competition** — one could later feed Engram's Anchors.
- **Reasoning / decision memory** — the durable "why / what was decided / what bit us" layer. Closest concept: **Cairn** (reasoning graph + contradictions via hooks/MCP) — no UI, ~3★. The MCP "memory visualizers" have graph UIs but are passive viewers, not curated reasoning memory.

**Validated wedge:** the combination nobody ships — durable reasoning memory **+** an editable graph pane embedded in the IDE **+** conflict/supersede surfacing **+** local-first. JetBrains has zero memory-graph plugins.

**Name collisions:** four other "engram" AI-memory projects — most notably **Gentleman-Programming/engram** (4.9K★, Go; the most direct competitor *and* proof of demand, but flat FTS5 observations: no embeddings, no ontology, no trust/decay, no graph UI) — hence the product name **Engram Alpha**. The traction there softens the "demand unproven" caveat: appetite for coding-agent memory is real; unproven is only the graph-pane wedge.

---

## 2. What it is

A **local core service** plus thin clients: a Rust backend owning graph + RAG (HTTP API for the pane, MCP for assistants), a Vue single-page graph pane (browser standalone first, embedded in VSCode Webview / JetBrains JCEF), and shipped capture skills. One backend, many faces.

---

## 3. Integration with Claude Code

Three channels, all shipped or planned: **MCP server** (primary, v1 — Claude reads/writes the graph, guided by a shipped skill), **hooks** (session-start brief shipped; file-read match later), **SDK/CLI** (pane-launched sessions; later). There is no API to render custom UI inside Claude Code's own panel — fine, Engram has its own pane. v1 is cooperative: Claude *chooses* to save.

---

## 4. Graph ontology

The schema's organizing principle is **durability**, not just type. Names are abstract and "thought-shaped" so the assistant reaches for them naturally.

### 4.1 Node types (7 + 1 anchor)

| Node | Durability | The thought it captures |
|---|---|---|
| **Principle** | stable | "this is how I like things / what I value" |
| **Decision** | stable, supersedable | "we chose this, for a reason" (ADR-like) |
| **Caution** | stable | "watch out — this bites" (constraints, gotchas, specs-as-rules) |
| **Problem** | episodic | "this was hard / went wrong" (the micro bug-tracker) |
| **Resolution** | episodic | "here's how it got solved" |
| **Insight** | episodic | "I realized something non-obvious" |
| **Intent** | volatile | "do this later" (TODO / deferred work) |
| **Anchor** | semi-stable | "what this is *about*" — a code subject, by **semantic identity, never line numbers** |

**Durability classes:** **stable** persists until explicitly `replaces`-superseded; **episodic** ages but is never "wrong"; **volatile** has a short TTL and is not stored unless explicitly asked. No separate `Concept`/`Spec` node — that material folds into Caution/Principle/Decision. Keeping the set at 8 is deliberate; do not add types.

### 4.2 Node properties

`id` (12-char time-sortable base36), `type`, `title`, `body`, `durability`, `source` (user|claude — trust signal), `session_id`, `created_at`, `valid_from/valid_until` (supersession), `status` (open|resolved|obsolete for Problem/Intent), `embedding`, `code_refs` (loose semantic refs), `tags`, plus the trust fields `last_seen` (retrieval observability — trust never reads it), `confirmed_at` (last deliberate act — the unapproved anchor), `approved_at`, `demoted_at` (when contradicting evidence landed), and `trust_override` (user pin) — trust is **computed at read time** from these (stored confidence was removed in v0.1.15; trust v2 landed in v0.4.2).

### 4.3 Edge types

**Principle: a triple reads as a plain English sentence** — the assistant completes a sentence, not a foreign key.

| Edge | Reads as |
|---|---|
| **about** | *Insight* **about** *Anchor* |
| **because** | *Decision* **because** *Principle* |
| **answers** | *Resolution* **answers** *Problem* |
| **builds-on** | *Insight* **builds-on** *Insight* |
| **replaces** | *Decision* **replaces** *Decision* (old kept, marked) |
| **conflicts-with** | *Insight* **conflicts-with** *Decision* |
| **needs** | *Intent* **needs** *Decision* |

**No generic `relates_to`** — if no real verb completes the sentence, the link isn't worth making. `replaces` and `conflicts-with` are the high-value edges: they make the graph *active*. Edges carry `type`, directional `from/to`, `source`, `note`, timestamps, validity, and `status` (active|resolved|dismissed) — which makes `conflicts-with`/`needs` a live worklist.

---

## 5. Write-policy modes

A user-facing knob for **when the assistant writes**: **Relaxed** (durable, high-value only — recommended default), **Normal** (+ Cautions, selective Insights), **Aggressive** (+ Intents, all Insights — maximum capture; what this repo dogfoods). Ships as three self-contained `SKILL.md` variants in `skills/engram/{aggressive,normal,relaxed}/`; the user installs exactly one, and can always reclassify nodes in the pane — the graph people trust is one they can correct.

---

## 6. The librarian (quality layer)

The hardest problem: **deciding what's worth a node.** In v1 Claude is the sole gatekeeper via the skill (no separate librarian LLM pass); dedupe/classify happen inline. An async librarian becomes essential when ambient hooks land. On contradiction: record a `conflicts-with` edge, never overwrite.

---

## 6A. Resolved behaviors — capture · anchors · retrieval · skill

All implemented; summary of the resolved design:

- **Capture:** worth-a-node is Claude's judgment via the skill — no objective gate. Duplicates → merge into the existing node. Search-before-write is a recommendation that matters once the graph is big; the safety net is `add_note`'s built-in same-type similarity pre-check (`{matched, created:false}` on a near-dupe → `update_node` instead).
- **Trust & decay (v2, 2026-07-12 — shaped by the first two external feedbacks):** two principles fix which signals may move trust. *Time doesn't validate*: **stable** knowledge holds its trust flat until a judged `conflicts-with` against the older claim stamps `demoted_at` and starts the ramp — and withdrawing the evidence (edge resolved/dismissed/retyped/deleted) withdraws the demotion; drift is surfaced for review but deliberately never demotes (the scan runs on every pane load against an environment-dependent root — a wrong cwd must not be able to mass-bury the graph); episodic fades over ~6 months, volatile over ~1 month; open Problems/Intents never fade while open. *Exposure doesn't validate*: retrieval (search hits, brief inclusion) stamps `last_seen` for observability only — trust anchors on `confirmed_at`, refreshed solely by deliberate acts (`update_node`, "Confirm still true", approval), so a wrong-but-attractive note can't keep itself alive by being findable ("retrieval must not certify its own outputs"). Anchors: created 50% → confirmed 60% → approved 100%; deliberate updates and approvals clear demotion (repair = re-validation). **Pins** (`trust_override`, user-only, pane) lock constant trust: never decay, never auto-archive, evidence can't silently demote them — contradictions against a pin surface loudly for review instead. Claude nodes start **provisional**; stale unapproved unpinned episodic/volatile nodes decay out; user nodes are trusted from the start. Type weighting lives in *ranking* (a small trust-boost prior for Principle/Caution/Decision/Insight) and in type→default-durability, never as a third decay axis — durability is the one decay knob, so "why is trust this number" always has a one-sentence answer (the pane renders it on every card). Migration to v2 backfilled `confirmed_at = last_seen` once, so no node's trust moved at upgrade; the semantics changed only going forward.
- **Anchors:** free-text responsibility label + optional `code_refs` file binding; flexible granularity (Claude decides); auto-created/reused; on refactor, fuzzy re-match and flag if unsure.
- **Retrieval:** session-start `brief` (budgeted digest: conflicts, open work, principles, decisions, cautions, recent adds) + on-demand `search`. Hits carry **matches + ≤5 1-hop neighbors, conflicts/replaces first**; ranking is hybrid (FTS5 OR-recall + cosine) × trust boost — trust modulates, never dominates.
- **Skill writes:** batched at natural stopping points, **silent by default** — transparency lives in the pane, not the chat; the one audible exception is a genuine contradiction surfaced by a write. Decisions are captured unprompted (feature requests usually hide one); every write response is a verdict (matched → merge, warnings → check canon, suspects → judge immediately via resolve_suspect). Both auto-invoked and manual (`/engram`). Default mode Relaxed.

---

## 6B. Operational decisions — storage · sync · lifecycle · safety

- **Storage:** per-repo `.engram/graph.db`, git-ignored; team sharing later via JSON export only. Pane reflects changes via SSE.
- **Lifecycle:** automatic flow prefers supersede (history preserved); hard delete is user-only and cascades edges; stale provisional nodes decay/archive (answers unbounded episodic growth).
- **Safety:** skill rule + backend redaction pass (regex + high-entropy) on title/body before write.
- **Bootstrap/portability:** empty start; JSON export/import ships in v1, embeddings stripped (regenerated on import) — the embedding-free dump *is* the git-shareable form.
- **Concurrency:** WAL + serialized writes; fine for local single-user.
- **Daemon:** one `engram-alpha serve` per repo, bound to its `.engram/`. Requests `--http-port` (default 8787) but **walks to the next free port** (up to +15) and records `{port, url, pid, db}` in **`.engram/daemon.json`**; clients resolve the URL from that file and verify via `/health` (which returns the served `db` path). Stale files are harmless — readers health-check before trusting.

---

## 7. Conflict handling

Staged: passive first, active later.

- **v1 (shipped):** contradictions live as `conflicts-with` edges, surfaced through retrieval (conflicting neighbors ride along on hits) and as a pane worklist. `add_note`/`update_node` return `warnings` when new text lands near conflicted/superseded nodes — the writer self-corrects in the same turn.
- **Phase 1 conflict scan (shipped v0.2.0):** detection is local and automatic (embedding cosine ≥ 0.88 — raised from 0.85 in v0.5.0 — + FTS overlap → `suspects` queue; write-time + 6-hourly sweep + pane "Scan now"), **judgment is cooperative** — the daemon never calls an LLM; Claude judges via `list_suspects`/`resolve_suspect` (conflict / replaces / dismiss) or the user judges in the pane. Shelling out to `claude -p` was rejected (PATH dependency, quota burn, no session context).
- **Later (planned):** proactive **mid-session push** via MCP channels — "this conflicts with an earlier decision" while Claude works. Behind a flag; opt-in.

---

## 7A. The local cortex (decided 2026-07-13; reranker + NLI logic layer shipped v0.5.0)

The daemon grows a stack of small local ONNX models — every layer converts an LLM judgment into offline, zero-token compute. Competitors (Mem0, Letta, Zep) run LLM judges for all of these operations; **nobody in the category ships a local consistency classifier**. The daemon-never-calls-an-LLM rule (§7) holds throughout.

**Four layers, one question each:**

| Layer | Question | Model | Status |
|---|---|---|---|
| Recall | what might be relevant? | `bge-small-en-v1.5` (34 MB) | shipped v0.1 |
| Precision | which of these actually is? | `jina-reranker-v1-turbo-en` cross-encoder (38M) | **shipped v0.5.0** — search over-fetches 3×, reranks title+snippet, trust still modulates (relevance dominates, trust breaks ties); optional: absent model degrades to hybrid order |
| Logic | do these two claims agree, disagree, duplicate? | runtime `Xenova/nli-deberta-v3-small` (quantized ONNX, ~35 MB, max compatibility); `finecat-nli-m` (100M, ModernBERT-base distillation) stays the eval-side benchmark (no ONNX export) | **shipped v0.5.0** — `nli.rs` (FastNli), suspects carry NLI hints, `check_claim` buckets supports/contradicts/silent; eval harness `scripts/nli-eval.py` |
| Decomposition | what are the atomic claims in this text? | rule-based splitter first; `wtpsplit/SaT` (~30 MB) when quality demands | planned |

**Governing principle — models don't validate** (third rule after "time doesn't validate" and "exposure doesn't validate", §6A): no model verdict ever moves a trust field or archives a node. Models NOMINATE (order the suspects queue, rank merge/split candidates, suggest edge verbs, annotate claims); JUDGMENTS move trust (a human or the assistant creates the edge, approves the operation, clicks the verdict). Model outputs render as telemetry chips, like `last_seen` — visible, never load-bearing.

**Features this unlocks (build order):**
1. **NLI conflict triage** — classify scan candidates (contradiction / mutual-entailment / neutral), order the suspects queue by contradiction probability, show suggested-verdict chips in Review and the brief. **Co-reference caveat (dogfooded 2026-07-13):** MNLI-class models presuppose that both sentences describe the same situation — below the ~0.85 similarity band, unrelated same-shaped titles read as *confident* contradictions (140 junk pairs at a 0.8 gate on the dogfood graph, canonical MNLI pairs meanwhile perfect). So hints run only at the standing suspect floor (raised 0.85→0.88 on 2026-07-13 after the judged-suspects table showed a 96% false-positive rate — same-genre vocabulary inflates cosine into the 0.85–0.87 shelf); reaching the paraphrased-contradiction band below it waits for a domain-calibrated model from the judged-suspects corpus. The strongest live validation of "models don't validate" yet.
2. **`check_claim` / verify-against-canon** — MCP tool + pane Audit panel: split input into claims (decomposition layer), NLI each against top-k retrieved nodes → `{supports, contradicts, silent}` with node ids. NLI beats grounding detectors here because it distinguishes "the canon disagrees" (a conflict) from "the canon doesn't know" (a gap worth capturing).
3. **Audit panel** — one-click canned passes instead of a blank query box: find hidden conflicts (lowered threshold + NLI), find duplicates (mutual entailment), find bloated nodes (body claims cluster into >1 group), suggest missing links (edge-verb templates over the sentence-shaped ontology — "does A entail 'this exists because of B'?"), check open problems (does any node entail an answer?), structural hygiene (no ML: decisions without a `because`, islands). Every button emits *proposals* into the existing review flow.
4. **Offline split/merge** — extractive, no generation: split = segment body → cluster claim embeddings → NLI-verify clusters are distinct → parts titled by their most central sentence verbatim, original archived behind fan-out `replaces`, edges re-attached by endpoint similarity; merge = canonical keeps its body + appends only non-entailed sentences from the other, edges re-pointed, loser archived. Fidelity over fluency — every word traceable; pane preview + approve; pinned nodes never auto-proposed. Split parts don't inherit approval (it vouched for the whole).
5. **The NLI eval loop is self-improving:** every judged suspect (conflict/replaces/dismiss) is a labeled NLI example on real project prose — `scripts/nli-eval.py` scores candidate models on it, and accumulated verdicts become fine-tune data later. Model quality grows with product usage, locally.

**Deferred organs:** GLiNER-small (zero-shot entities → tag/Anchor/`about` suggestions); a SetFit-style memory-worthiness head on bge embeddings — the librarian's ambient-capture gate (§6), trained on our own graphs (captured nodes = positives). Explicitly not: a GNN (per-repo graphs are hundreds of nodes; neighborhood-as-text + NLI covers it legibly).

## 7B. Digestion — project ingestion (shipped 2026-07-13, v0.5.1)

*Status:* implemented and verified by a first live digest run. Tier 1 shipped as `engram_core::digest::scan` (the `ignore` crate walks gitignore-aware with `require_git(false)`, skips `log`/`logs`/`tmp`/`temp` dirs, binary/oversized/unreadable files are counted skips — never errors; caps: 1 MB/file, 500 candidates with `truncated` reported, 200-char redacted texts) behind `POST /digest/scan` (runs off the engine lock; root derived from the DB path like `/drift`). Tier 2 shipped as the `engram-digest` skill (`skills/engram/digest/SKILL.md`, no variants) with the 8 worked examples; plugin ships a verbatim copy + `/engram:digest` (sync-tested), `engram-alpha setup` installs it alongside the capture variant, and this repo symlinks it via `.claude/skills/engram-digest`. Ingest writes are marked `session_id: digest-<date>` (the reviewable-batch guardrail; full session quarantine remains a separate §10 item).

Engram still **bootstraps empty** (§6A locked). Digestion is a **user-invoked, opt-in** way to seed a graph from an existing project — never automatic, never on install. It does **not** violate empty-start; it is an explicit "ingest this project" action. Two tiers, deliberately separate, because fully-offline *rich* ingestion is not achievable with our micromodels: they embed / rank / classify / segment but **do not generate**, so authoring a well-typed, sentence-shaped node (title, `because`, durability) needs the assistant's LLM. This is the §7A **models-nominate / assistant-and-human-judge** split applied to bootstrapping — the offline stack nominates, the LLM authors, the user curates.

**Tier 1 — offline code scan (the foundation; build first, keep it small).**
- Scope for v1 is **only `FIXME` / `TODO` markers in code** → candidate `Intent` (TODO) / `Problem` (FIXME) nodes. Nothing broader yet (no Anchors-from-dirs, no ADR mining, no README extraction) — the user cut that: too much noise for a first cut.
- **Must be `.gitignore`-aware** — ignore everything gitignored (node_modules and the thousands of transient paths). This is the load-bearing complexity of tier 1. Also treat `/log/`-style dirs as trash (drop by default; "useful with intelligence" is a later, LLM-side concern, not tier 1).
- **Robustness is a hard requirement:** a malformed/huge/binary/oddly-encoded file, a permission error, a symlink loop — none may crash the daemon or CLI. Rust's `Result` plumbing should make this natural: skip the offending file, log, continue. A file-walk flaw must never take down the app.
- **No new MCP tool for this.** Expose it as an **HTTP endpoint** (like the `/audit/*` buttons) that the digest skill calls; it doesn't warrant its own MCP surface. Not a standalone CLI command either — it lives behind the skill.

**Tier 2 — the digest skill (separate skill file; the real value).**
- A **distinct skill**, not part of the always-on `engram` capture skill — invoked **explicitly** ("ingest/digest this project", `/engram:digest`). Rationale: skill bodies load on demand (progressive disclosure); a heavyweight, occasional ingestion doc must **not** sit in every session's context and burn tokens. Keeping it out of the always-on path is the point.
- **Default target = the current branch's working-tree snapshot** — the code and files that exist *now*. Code is the primary material; git history is only *supporting* (the "why" behind a change), not a deep archaeological crawl of thousands of commits. Startup ingestion = digest the current codebase, not the repo's whole past.
- **Teach the ontology by worked examples — one per node type, each also teaching a different feature** (Principle→durability+`because`; Decision→the `Decision because Principle` triple; Caution→extract the *why*+tags; Problem→open status; Resolution→`answers`+supersession; Insight→`builds-on`; Intent→volatile deferred work; Anchor→code_refs+drift+`about`). Doubles as canonical ontology documentation.
- **Examples must be copy-paste-safe.** Assume a weak model: on skill call the agent plans, searches the examples, and (if weak) copies them with minimal edits. The examples must be written so that copy-paste-with-minimal-changes still yields *correct, well-formed* nodes — leave no room to produce garbage by imitation.
- **Every insert runs the usual write-time checks** — the standard `add_note` same-type dedup, `warnings`, and `suspects` verdicts apply to ingestion exactly as to normal capture; batch inserts (`add_notes`/`update_nodes`) too. No bypass just because we're bulk-filling. The examples must *force* these checks, since ingestion's job is to fill the whole ontology at once.

**Guardrails (carry into implementation):**
- **Trust contamination** — ingested nodes start **provisional/low-trust** like any Claude-authored node and earn trust by later reconfirmation; a bulk ingest must never become instant high-trust canon. Digestion should mark its writes as **one session** so a bad ingest is reviewable/reversible — **this is the killer use case for the existing session-quarantine intent; ship them together.**
- **Redaction** — git history and old docs contain secrets; the existing secret/PII redaction pass must run on every ingested item.
- **Noise bias** — ingestion must lean hard toward the *reasoning* types (Decision / Principle / Insight / Caution) and away from volatile implementation trivia. Engram is **not** a code-structure graph (§1A) — quality over coverage.
- **Idempotency** — re-running must not duplicate; same-type dedup covers most, a stored marker (e.g. last-scanned state) makes re-runs incremental.

---

## 7C. Multi-project memory — federation + home graph (planned 2026-07-20; SHIPPED in v0.6.0 the same day, including the step-5 TepinDB cutover)

*Status:* ALL FIVE steps below are **shipped in v0.6.0** (2026-07-20). Steps 1–3 first, on SQLite: `registry.rs` + `hub.rs` in engram-core (153 workspace tests incl. a federation end-to-end), `serve`/`mcp`/`setup` self-register, the HTTP API is fully project-scoped (`/projects` + a pre-routing rewrite of `/projects/{sel}/…`, per-project SSE), every MCP tool takes `project` (+ a `list_projects` tool), the home graph + brief section + promotion nominations work, and the pane has the switcher / add-by-path / registry view / promote-to-home. One model runtime serves every open store (`Arc` sharing). The formal `Store` trait extraction was deliberately deferred to the TepinDB driver work — with one implementor it would be ~45 delegation stubs around a boundary that still leaks `conn()` into `/system`; the seam that multi-store actually needed (engine factory + shared models) is in.

*Scope guard:* one user, one machine. Multi-user & repo sync stay permanently out (§10). Single-user multi-machine is later/maybe at most, via the JSON-export-through-a-user-repo pattern — not planned here.

**Principle: access + promotion, not replication.** If any project can read the others' graphs, nothing needs syncing — no copies, no divergence, no merge-conflict machinery (the problem class that made team sync an enterprise-product decision). The only knowledge that moves between graphs is a deliberate, user-approved **promotion** into the home graph.

**Pieces:**
- **Global registry — first-class and obvious.** `~/.engram/registry.json`: every `serve`/`setup`/`mcp` start registers `{name, repo_root, db_path, last_seen}`. Inspectable via CLI (`engram-alpha projects`) and the pane. Stale entries are harmless — health-check-before-trust, same as `daemon.json`.
- **Scoped read access.** Reads accept the `project` param (see *Addressing surface* below). Cross-project hits carry provenance (project name) plus a **locality prior** — a small rank penalty so local canon wins ties. `brief` stays project-local except a capped home-graph section.
- **No cross-project edges** (v1 of this layer). Edges stay intra-graph — URI-style endpoints break "reads as a sentence" (§4.3); cross-project relatedness is a search result, not an edge.
- **Home graph.** `~/.engram/home.db` — a normal Engram graph for knowledge that was never project-scoped: user preferences, global principles, cross-cutting cautions. Included in every project's brief. Written only by explicit gesture ("remember this globally") or promotion.
- **Promotion flow.** The same Principle/Caution near-duplicated across ≥2 project graphs is a **nomination** surfaced in Review; the user approves; project copies keep provenance links back to the promoted node. Scans nominate, the human moves knowledge (§7A principle).

**Addressing surface (decided 2026-07-20):**
- **MCP:** most tools grow an optional `project` param — omitted/null = the current project (the store this MCP entry is wired to); a name/id = that project (reads *and* writes — capturing into a sibling repo's graph is allowed); `all` = every registered project, **reads only**. A write addressed to `all` is refused: fanning one write into N graphs is replication and mints N future dupes — the shared write target is the home graph, reserved name `home`.
- **HTTP:** project id in the URL — the hub serves `/projects` (register-by-path, list, health) and project-scoped routes `/projects/{id}/…` for everything else, incl. SSE at `/projects/{id}/events`. Legacy unscoped routes alias to the launch project during the transition so existing panes/plugins keep working.
- **Pane — deliberately simple:** a project switcher in the topbar + "add project by path" (validates/creates `.engram/`, registers it); registry view under Settings → System.
- **Identity:** the registry assigns each project a stable id + unique slug name (dir basename, deduped); MCP accepts name or id, URLs use the id. `home` and `all` are reserved names.

**Topology: hub daemon (decided 2026-07-20; supersedes one-daemon-per-repo — ships pre-TepinDB, see sequencing).** redb — TepinDB's storage core — holds an exclusive per-process file lock, so per-repo daemons can never cross-attach each other's stores post-migration; federation forces the hub. One machine-level `engram-alpha serve` owns all registered project stores plus the home graph: **one models runtime** instead of N (matches TepinDB's "one binary owns the models"), the pane gets a project switcher, MCP resolves the project from cwd (dovetails with the streamable-HTTP transport migration). **Per-repo storage is unchanged** — the store file stays in the repo, git-ignored, portable; the hub opens files, it doesn't own data. Per-project touchpoints (brief hook, project MCP entry) become **thin, minimal clients** that resolve the hub via the registry and **lazily start it** when absent — no always-on requirement.

**Sequencing (feature-first, decided 2026-07-20 — the hub ships on SQLite; TepinDB swaps in behind the trait):** everything above the `Store` trait — registry, `project` addressing, provenance, locality prior, home graph, promotion — is storage-agnostic application logic, so nothing meaningful is built twice *as long as federation is hub-shaped from day one* (what dies at redb cutover is attach-based per-repo federation, not the feature). Feature-first also unstacks the risk — the topology change and the storage change land separately, each debuggable alone — turns the federation test suite into the migration's regression gate, and hands TepinDB v1 a precise spec: the trait, with a working reference implementation, instead of guessed requirements.
1. **Storage trait seam** in engram-core (`Store` trait over the rusqlite impl). *Done 2026-07-20: `trait Store` in store.rs (primitives + provided pure-Rust composites — hybrid fusion/traverse/neighbors/decay — so backends share one behavior), `SqliteStore` reference impl, the `conn()` leaks sealed behind `stats()`/`health()`, Engine on `Box<dyn Store>`, both-backends conformance battery in the core tests.*
2. **Hub daemon + registry + `project`-scoped MCP/HTTP on SQLite** (provenance + locality prior; thin lazy clients). *First dogfood milestone: from this repo, reach the TepinDB repo's graph — and write into it.*
3. **Home graph + promotion nominations** in Review.
4. **TepinDB v1** matures in parallel (sibling repo); Engram's `Store` trait is its requirements spec.
5. **Cutover = pure driver swap** behind the trait; JSON export/import (embeddings stripped, regenerated) is the vehicle — already the canonical portable form (§6B); the federation suite gates the swap. *Done 2026-07-20, shipped in v0.6.0: `store_tepin.rs` implements the trait on tepindb's primitives tier (git rev-pinned, default-features off — Engram brings its own embedder; nodes/edges/suspects/audit/meta collections, manual one-vector-per-node, BM25 + raw KNN under the shared fusion, driver-side sentinel snippets, batch-atomic cascades; empty-collection reads mapped to empty). `engram-alpha migrate` moves a repo: export/import for the graph, suspects + audit journal verbatim (oldest-first so the import's own row lands last), counts verified, graph.db untouched as the backup; `resolve_db_path` makes a `graph.tepin` sibling win over `graph.db` everywhere, so hooks/.mcp.json/registry/IDE wiring survive unchanged. KEY GOTCHA: tepin pins one (model_id, dim) per file with no unpin — `reset_vectors` on the tepin backend is a whole-file rebuild (docs copied, vectors dropped). Verified by migrating the real dogfood graph (identical briefs before/after). The live cutover first hit redb's exclusive per-process lock (brief/doctor/stdio-MCP could no longer co-open the store beside the daemon, the coexistence SQLite WAL provided) — resolved the same day by the thin-client work below; **this repo now lives on graph.tepin.***

---

## 8. Tech stack (locked: Rust + Vue, local-first)

The stable contract is the *interface* (local HTTP + MCP + SQLite).

- **Backend (Rust):** `rmcp` (MCP), `rusqlite` bundled + **FTS5** + `sqlite-vec`, `fastembed` (ONNX: bge-small embeddings + jina-turbo reranker; the §7A cortex grows here), `axum` (HTTP + SSE).
- **Frontend:** Vue 3.5 + TS + Vite + **Bun**; Pinia, vue-router, Tailwind 4, **Vue Flow**, vueuse, markdown-it + dompurify; ESLint (flat) + Stylelint. Renders the full graph (scaling deferred).
- **IDE wrappers (built):** VSCode extension (Webview, secondary sidebar) and JetBrains plugin (JCEF tool window, `dev.techtheist.engram`).
- **Distribution staging:** core loop as a plain local binary → **thin marketplace plugin** (skill + commands + config; the skill bootstraps a checksum-verified binary download from GitHub Releases after a one-time consent prompt) → full **`cargo-dist`** pipeline (installers must also install the chosen skill variant). GitHub is the single root of trust: Releases + `SHA256SUMS`, `install.sh` one-liner. All onboarding paths (JetBrains-only, VSCode-only, GitHub page, plugin-without-daemon) converge on binary→daemon→MCP→skill (later: the IDE plugins manage the daemon lifecycle themselves instead of setup cards); **no-plugin usage is first-class** (Claude Code runs everything; skill points users at the localhost pane).
- **Plugin publishing:** JetBrains `publishPlugin`; VSCode via `vsce` + Open VSX (`ovsx`) for Cursor/Windsurf — publish to both. Code-signing starts at checksums/minisign; OS signing only if Gatekeeper/SmartScreen friction bites.
- **Known caveat:** ONNX Runtime means "binary + one runtime lib per platform," not fully single-file.

---

## 9. Architecture (at a glance)

```
                 ┌─────────────────────────────────────────┐
                 │            Engram Core (Rust)            │
                 │  graph store (SQLite + FTS5 + vec)       │
                 │  RAG (hybrid search, local embeddings)   │
                 │  ── HTTP API ──┐   ┌── MCP server ───────┤
                 └────────────────┼───┼─────────────────────┘
                                  │   │
        ┌──────────────┬──────────┘   └──────────┬───────────────┐
        │              │                          │               │
   Vue frontend   Vue frontend              Claude Code      Claude Desktop
   (browser)      (Webview / JCEF)          (MCP)            (MCP)
```

---

## 10. Roadmap

**Phase 0 — Core loop** *(complete)*: Rust graph store per §4, `engram serve` — now `engram-alpha serve` — (HTTP + SSE + MCP), hybrid RAG + redaction, Vue Flow pane, JSON export/import, capture skill.

**Phase 1 — Quality + IDEs** *(complete — v0.2.0, 2026-07-06)*: conflict scan + suspects queue + Review drawer + health strip; decay pass (14-day TTL past stale-crossing, Claude-authored unapproved episodic/volatile only; similarity threshold tuned 0.75→**0.85** on the dogfood graph); node edit mode incl. type reclassification; VSCode + JetBrains wrappers (both user-tested in-IDE).

**Shipped since (v0.3.0 line, 2026-07-10)** — one line each; details in the graph:
- **Claude Code plugin** — repo self-hosts a marketplace (`.claude-plugin/marketplace.json` + `claude-plugin/`): relaxed-skill + brief-hook copies (verbatim, sync-tested), `/engram:setup` + `/engram:pane`; **deliberately no global MCP config** (would litter every repo with `.engram/` dirs) — per-repo wiring via `engram-alpha setup --mcp-only`.
- **Session-start brief hook** — `hooks/session-brief.sh`: daemon-first via `daemon.json` + `/health` DB check, CLI fallback, every failure exits 0 silently; registered via `SessionStart` (matcher `startup|clear|compact`); `engram setup` installs it. The same stdout-becomes-context contract holds on Codex/Gemini/OpenCode hooks (Phase 3 wiring).
- **Pane CRUD parity** — "+ New" create drawer, drag-to-connect with sentence-shaped verb dialog, edge retype/delete; **tags** end to end (column + FTS + `GET /tags` + editor/chips/filter + brief "Recent tags" line).
- **Audit journal** — per-mutation rows with binary-side context, pane Audit drawer, `audit` MCP tool; substrate for rollback/quarantine.
- **Verified code refs / drift** — sweep + `GET /drift` + `list_drift` + Review section + drifted badges.
- **`timeline`** — MCP tool + HTTP, replaces-chain history; `engram://node/{id}` MCP resource; brief format v2 (16k budget, ids on every record).
- **Skill overhaul** — value story, maintenance duties (close-the-loop, drift repair), example flows, unhappy-path protocol (empty search → say so; server down → surface, don't fall back to model memory; short keyword queries), across all variants.
- Four canvas layouts (Skyline default / Nebula / Archipelago / Orbit); SidePanel drawer shell; responsive pane folds for narrow IDE panels.

**Shipped since (v0.4.0 line, 2026-07-11)** — one line each; details in the graph:
- **Binary renamed `engram` → `engram-alpha`** — matches the product name so other users can find it; crate names, the MCP server key `engram`, `.engram/`, and the `/engram:` plugin namespace deliberately keep the short name (renaming them breaks existing wiring for zero discoverability gain). IDE plugins and the brief hook fall back to a pre-rename `engram` binary.
- **`engram-alpha doctor`** — store integrity (WAL / quick_check / FTS-vs-vec counts), embedding-model cache, daemon-vs-repo DB match via `/health`, and per-assistant wiring checks (`.mcp.json` binary/db validity, brief hook, gitignore, codex cwd risk). Exits non-zero on failures.
- **Codex desktop app covered** — the app (merged into the unified ChatGPT app 2026-07) shares `~/.codex/config.toml` + AGENTS.md discovery with the CLI, so `setup --cli codex` already serves it; added app-install detection, and setup/doctor warn that the app may launch MCP servers from an unexpected cwd (pin `cwd` / absolute `--db`). The app ignores project-local `.codex/config.toml` (openai/codex#13025).
- **Timeline pane view** — a History section in the node detail drawer renders the `replaces` chain oldest-first (dot timeline, current generation marked, retirement notes, click-to-jump); the fetch is gated on the node actually carrying a `replaces` edge.
- **JetBrains plugin zip ships its README**; both IDE-plugin READMEs refreshed.
- **Full-field retrieval index** — embeddings and FTS now cover tags + code_refs (embed composition v2 via PRAGMA user_version; guarded one-time re-embed on the first real-embedder open; FTS rebuild migration). A code-ref query like "policy.rs" reaches every node citing the file.
- **Bulk MCP tools** — `list_nodes` (full-fidelity paged read), `update_nodes` / `add_notes` (≤100 items, per-item results); see Appendix A.
- **System info pane view** — Settings → System info drawer rendering `GET /system`: binary version, daemon uptime/pid/repo, store health (integrity, WAL, vector/FTS coverage, embed composition), model cache, and per-assistant wiring with pre-rename flags; probes shared via `engram_core::harness`.

**Next (near-term):**
- **Local cortex phases (§7A):** Phase-A NLI eval on the dogfood corpus → conflict triage → `check_claim`/verify + Audit panel → edge-verb suggestion → offline split/merge.
- **Bundle skill + `.mcp.json` bootstrap into the IDE plugins**; `runIde`/`verifyPlugin` for JetBrains.
- **MCP transport migration — DONE 2026-07-20 (user-directed, unblocking the live tepin cutover):** the daemon mounts rmcp's `StreamableHttpService` at `/mcp` (stateful — one session = one `Engram` instance over the shared hub, so per-session audit attribution survives; MCP writes now reach the pane's SSE feed). `engram-alpha mcp` stays the wired command everywhere but became a thin client: healthy daemon owning the db (daemon.json + /health match) → verbatim stdio↔HTTP passthrough (an rmcp `Service<RoleServer>` forwarding to a `Peer<RoleClient>`, `get_info` mirroring the daemon); tepin store with no daemon → spawn `serve --http-only` detached, wait for health, bridge; otherwise direct file open (sqlite WAL coexistence unchanged). `brief` rides GET /brief and `doctor` reads /system the same way, both with direct-open fallback. No wiring file changed anywhere. e2e: engram-mcp `transport_tests` (axum-hosted /mcp + direct client + duplex-hosted bridge sharing state) and a live handshake/tool-call probe through the deployed daemon.
- **Security hardening pre-release (SECURITY.md is the tracked list, 2026-07-20):** CORS origin allowlist for the localhost API (today: permissive — any browser page can call the daemon; needs the IDE webview origins retested in-IDE), per-file SHA-256 pinning for cortex model downloads (binary self-update already verifies; tepin's fetcher is the precedent). App-level store encryption is explicitly **post-this-release** (user decision 2026-07-20; disk encryption is the interim answer).
- **Session quarantine** — exclude a session's writes from retrieval without deleting them (journal substrate shipped; quarantined-sessions table + retrieval filter + pane toggle).
- **File-read match hook** (design TBD) — inject connected nodes when the assistant reads a file matching stored `code_refs`; needs a match endpoint + noise control (per-session dedupe, cap, trust filter).
- **Multi-project memory — §7C:** all five steps done (hub + federation on SQLite 2026-07-20, then the `Store` trait + TepinDB driver + `engram-alpha migrate` cutover the same day). Remaining: propose the API asks upstream (empty-collection reads as empty, an embedder-pin reset); one machine-level hub daemon before a SECOND daily repo migrates (a sibling tepin store can only be federated by whichever single daemon opens it first).

**Phase 2 — active features:** mid-session conflict push (MCP channels), ambient capture → librarian, cross-platform release pipeline.

**Phase 3 — multi-harness memory:** first-class OpenCode / Codex CLI / Gemini CLI / Kilo CLI + Cursor/Windsurf. All speak MCP, so the backend is agnostic — the work is per-harness config bootstrap, capture-prompt equivalents, docs. Upgrades the pitch to **one shared local memory across different AI agents**. Multi-agent concurrency already handled (WAL + one daemon); verify with a smoke test.

**Shipped 2026-07-20 (was Later/maybe): user-selectable cortex models.** `cortex.rs` (machine-level `~/.engram/models.json`, presets + custom-by-URL specs), `GET/POST /models` (the `ModelAdmin` trait lives in engram-http, the CLI implements it — curl provisioning into `~/.cache/engram/<name>/` stays the CLI's job), live hot-swap (RwLock'd model set + push into every hub-open engine), and the `ensure_embed_model` guard (store records `EmbedModelId`; a mismatch = reset vectors to the new width + full re-embed + stamp; skipped entirely under fake embeddings). Pane: Settings → System → "Choose models" with per-role dropdowns, custom URL entry and the embedding-swap warning. Still open: the 0.90/0.88/0.85 cosine thresholds are bge-small-calibrated — swapping the embedding model un-calibrates suspect/dupe quality until re-tuned against the judged-suspects corpus (warned in the UI; recalibration story still deferred).

**Later / maybe:** app-level DB encryption (deliberately far down); Obsidian export (typed edges map natively to wikilinks). **Out of scope permanently:** multi-user & repo sync — a future enterprise product, not Engram Alpha (user decision 2026-07-10).

---

## 11. Open questions

*Behavioral design resolved in §6A; remaining items are implementation-level.*

- **Skill wording:** keeps evolving as dogfooding (Aggressive) surfaces low-value nodes or ambiguous guidance.
- **Decay & promotion numbers:** trust v2 settled *what counts* (deliberate acts confirm; evidence demotes; exposure never counts) — the window/floor constants remain dogfood-tunable. "Successful downstream use" as a third confirmation signal waits for the file-read match hook.
- **Fuzzy re-match algorithm** for anchors after refactors (drift detection shipped; re-match still open).
- **Neighbor-cap / brief-quota tuning** as graphs grow.
- **Pane review UX:** writes are silent, so the pane is the transparency surface — Review drawer + audit journal shipped; timeline view pending.
- **Redaction coverage:** which patterns; false-positive handling.

---

## 12. Non-goals (v1)

- No companion/persona features — project memory, not a relationship.
- No custom UI inside the Claude Code chat panel (unsupported; not needed).
- No cross-project / global graph *in v1* (now planned — §7C); no remote embeddings or cloud sync.
- No ambient hook *capture* in v1 — cooperative MCP first (the read-side brief hook shipped).

---

## 13. Development approach

**Dogfood from day one on Aggressive mode** — the first real user is us. **Minimalist product and code** — small surface area; clear naming and deps' public docs over inline comments.

---

## Appendix A — MCP surface (implemented)

Tools: `search` (hybrid, hits carry ≤5 1-hop neighbors, conflicts/replaces first; stamps `last_seen` for observability only — retrieval never refreshes trust), `brief` (budgeted session-start digest; pinned nodes marked PINNED), `get_node`, `traverse`, `add_note` (same-type dupe pre-check → `{matched, created:false}`; `warnings` near conflicted/superseded nodes), `update_node` (same warnings; a deliberate update stamps `confirmed_at` and clears demotion), `link`/`unlink`/`update_edge` (mislink repair is Claude's to do), `approve_node`, `list_open`, `list_suspects`/`resolve_suspect`, `list_drift`, `timeline`, `audit`; bulk (v0.4.0, ≤100 items/call, per-item results — one bad item never blocks the rest): `list_nodes` (full-fidelity paged read with type/status/tag/pinned filters, archived excluded by default — the lossless read behind reviews and exports like a decisions.md; does not touch trust clocks), `update_nodes`, `add_notes` (each item runs add_note's dupe pre-check). Pinning/unpinning and hard delete are user-only (pane/HTTP; Claude has no tool for either). Node hard-delete is user-only (pane/HTTP; Claude never calls it). Resources: `engram://node/{id}` (list = 25 newest + template). Exact contracts live in `crates/engram-mcp/src/lib.rs`.

## Appendix B — SQLite schema

Implemented in `crates/engram-core/src/schema.rs`: `nodes` + `edges` per §4 (ids are 12-char time-sortable base36, not UUIDs), `nodes_fts` (FTS5, incl. tags + code_refs), `vec_nodes` (sqlite-vec, 384-dim), plus `suspects` and the append-only `audit` journal. Forward migrations run on open.

## Appendix C — Repo layout

See CLAUDE.md "Where things go" — crates (`engram-core` / `engram-mcp` / `engram-http` / `engram-cli`), `frontend/`, `engram-vscode/`, `engram-jetbrains/`, `claude-plugin/` + `.claude-plugin/`, `hooks/`, `skills/engram/{aggressive,normal,relaxed}/`.
