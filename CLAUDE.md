# Engram — context for Claude Code

**What this is:** a graph-based, durable, inspectable long-term project memory for AI coding assistants (Claude Code first). Local-first, user-owned, graph-first UI. The **reasoning/decision memory** layer (why/decided/what-bit-us) — not a code-structure graph; the wedge is the editable IDE-embedded graph pane + conflict surfacing + local. The line since 0.8.0: *the most powerful and feature-rich inspectable graph long-term memory for software development with AI agents, based on reproducible research.*

**THE GRAPH IS THE MEMORY OF RECORD — this file is only a keyword index.** The session brief is auto-injected; `search` engram before any non-trivial work and follow the write-verdict protocol. Details, rationale, gotchas, and the full history live in this repo's graph (`.engram/graph.tepin`, daemon on 8787) and the eval workbench graph (`eval/.engram`, project `eval`, scientific ontology Claim/Method/Question/Finding/Source/Task). PLAN.md was retired 2026-08-04 (merged here); `PLAN §…` references in code comments point at git history.

## Hard rules (locked — don't relitigate without reason)
- Open-source, MIT; optimize for docs & DX. Product name **"Engram Alpha"**, binary `engram-alpha` (repo/plugin namespaces stay `engram`; JetBrains package `dev.techtheist.engram` — don't rename).
- **Rust** backend (rmcp, rusqlite bundled, sqlite-vec, fastembed, tepindb) — no Node runtime dep. **Vue 3.5 + TS + Vite + Bun**, Pinia, Tailwind 4, Vue Flow; `bun run lint` / `lint:style` in `frontend/`.
- Embeddings/models **local-only**; **no LLM in the daemon, ever** (encoder-only mechanisms); models nominate, people judge.
- **Retrieval changes cite a measured `eval/` run or they don't ship** (since 0.8.0).
- **8 node types / 7 sentence-shaped verbs** on the default ontology; no new types, no `relates_to`; durability governs staleness; high-value edges are `replaces`/`conflicts-with`. Per-graph ontology/policy config exists since 0.7.0, but THIS repo's graph stays on the default ontology permanently.
- **Hard delete is user-only** (pane); MCP deliberately has no delete/register/pin tools. Writes are silent; transparency is the pane. One workspace version for every crate, stamped from the tag; `claude-plugin/.claude-plugin/plugin.json` must match (test-enforced).
- Multi-user & repo sync: **out of scope permanently** (future enterprise product). Dogfood on the **aggressive** skill variant (relaxed is the user default). Eval ladder max **1500 notes**.

## Where things go
`crates/engram-core` (engine, Store trait + sqlite/tepin drivers, policy, config, cortex) · `crates/engram-mcp` (rmcp tools) · `crates/engram-http` (axum API + embedded pane) · `crates/engram-cli` (`engram-alpha`: serve/mcp/doctor/setup/migrate/stop) · `frontend/` · `eval/` (engram-eval bench) · `engram-jetbrains/`, `engram-vscode/` · `skills/engram/{aggressive,normal,relaxed}` + `claude-plugin/` (verbatim copies, sync-tested).

## Workflows & sharp edges
- **Pane/daemon rebuild: `scripts/deploy-pane.sh` only** (never hand-chain build/install/restart); after any redeploy, `/mcp` reconnect — live stdio sessions keep the OLD binary and stale tool descriptions.
- **Release:** CHANGELOG section (one `## v<version>` per release) → push → `gh workflow run draft-release.yml -f version=X.Y.Z` → publish the draft. Bump the graph's working version (`set_version`) when a cycle OPENS, not at release.
- **Eval:** `cargo run -p engram-eval --features fastembed -- --series|--ladder|--sizes N|--tricks|--posttune|--floor|--bench` — receipts in `eval/results/`; `--distractors 0` = question-everything mode.
- Search-before-write; every write response is a verdict (matched → merge, suspects → judge now, warnings → check canon).

## Chronicle (slim — search the graph with these keywords for the full story)
- **v0.8.2** (2026-08-04, committed, UNRELEASED — more coming): *knee trim* (TAA-k, `knee_cliff`), *phantom-probe weak line* (auto-tune dial two, graph-vocabulary probes, floor-relative clamp), *"likely not in memory" recommendation verdict* (never cuts), `--posttune` end-to-end mode, pre/post-tune rows in eval README. Field lesson: *register-dependent score scales* — absolute thresholds don't transfer between graphs, relative mechanisms do.
- **v0.8.0** (2026-08-03, RELEASED): *measured not promised* — attention metrics (focus/noise/FP), gradation ladder, floor sweep; *calibrated delivery* (`delivery_floor`, strong/weak/none verdict); *auto-tune* dial one (conflict floor from judged history). 0.8.1 pane cycle folded in unreleased.
- **v0.7.3** (2026-07-28): *retired means retired* — supersession retires from any edge path; archived hidden by default; per-graph pane state; cold-start + preset picker.
- **v0.7.2** (2026-07-27): *eval harness born* (invented-subject corpus, arms, ladder); search retune (keyword 0.15 + reranker VOTES); mobilebert NLI swap; check_claim confidence gate; real-graph suspect eval.
- **v0.7.1** (2026-07-25): *timeline release* — feed view + lenses, graph↔feed sync, control kit, `set_version` auto-enable, CHANGELOG convention born.
- **v0.7.0** (2026-07-23): *customization* — per-graph GraphConfig (ontology/policy/brief), redactor UI + presets, generated skills, version tracking, handoff tag, file-read match hook, session-boundary validation, tepin auto-migrate.
- **v0.6.x** (2026-07-20..22): *Store trait + TepinDB cutover*, model selection + hot-swap, MCP streamable-HTTP transport, machine hub + convergent serve + per-project MCP, multi-project federation + home graph, conflict push, claim search, CORS allowlist, docs/ split.
- **v0.5.x** (2026-07-13): *local cortex* — reranker precision layer, NLI logic layer + co-reference caveat, Checkup panel, digest skill.
- **v0.4.x** (2026-07-11/12): *rename to engram-alpha*, doctor, bulk MCP tools, full-field retrieval index, **trust v2** (time/exposure don't validate; confirmed_at; pin; demotion via judged conflicts), write-verdict protocol.
- **v0.3.0** (2026-07-10): plugin + marketplace, audit journal, session-brief hook, drift scan, timeline, four layouts, skill overhaul.
- **Phase 0–1** (2026-07-02..06): core graph + hybrid RAG + pane CRUD, conflict scan + suspects, decay pass, brief.

## Now / next (keywords)
Latest published release **v0.8.0**; v0.8.2 committed and unreleased on main; daemon 8787 over `.engram/graph.tepin`. Open threads in the graphs: *delivery floor as auto-tune dial three* (fixed 0.22 empties oblique prose-register queries — bench first), *probe-register gap at scale* (shipped FP 0.84 @ 1500 vs 0.12 ceiling), *Provence-class sentence pruning*, *online eval half + competitor arms*, *residual oblique misses*, session quarantine, downstream-use confirmation, SHA-256 model pinning, Cursor/Windsurf wiring, app-level encryption (far), plugin-directory publishing (user action).
