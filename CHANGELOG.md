# Changelog

Release notes for Engram Alpha. Each release's section below becomes the
body of its GitHub Release (draft-release.yml lifts it automatically).

## v0.8.9

### The agent binds itself — and the graph grades its own referee

- **`set_project`: a session can now rebind itself to the right project
  (#4).** Some clients advertise the MCP roots capability but answer
  `roots/list` empty (Windsurf and the Devin CLI, per field reports), so the
  binding ladder can't see the workspace and the session lands on a fallback
  graph. The new tool closes that gap from the agent's side: call it with a
  project name, id, or any absolute path inside a registered root, and *this*
  session rebinds and gets that project's brief back in the same reply — no
  global setting is touched, and other sessions keep their bindings. Unknown
  selectors are refused with the roster of registered projects, so a typo
  can't silently land in the wrong graph. When a session binds to the home
  graph or the default-agent-project rung of the ladder, its brief now opens
  with a one-line hint naming the tool; the skills teach the pattern, and a
  single `AGENTS.md` line — *"at the start of a session, call engram's
  `set_project` with the absolute path of the workspace, then follow the
  brief it returns"* — makes it automatic for roots-silent clients.
  [Runtime architecture](docs/runtime.md) documents where it sits in the
  binding ladder.
- **MCP sessions join the process census.** The pane's **Settings → System →
  Processes** (and `GET /system`) now list every live MCP session alongside
  the client leases: which project it's bound to, since when, and when it
  last acted — so "which graph is my assistant actually writing into?" has a
  visible answer. Sessions age out of the list after 30 idle minutes; the
  section hides itself against an older core.
- **Every MCP tool description cut to its contract.** Tool descriptions are
  context each session pays, on every client, before its first thought. All
  of them were rewritten to roughly half their former length: the
  load-bearing lines stay (verdict duties, safety rules — the parts an agent
  must not skip), the details moved into per-argument docs where they're read
  on use.
- **The Checkup panel scores the referee: an NLI scoreboard.** Engram's
  conflict scan runs on "models nominate, people judge" — and the judging
  history can now grade the nominator. Over every judged suspect pair that
  carried an NLI hint, the new block reports how often the hint matched the
  verdict: hits, false alarms, misses, correct passes, and the agreement rate
  (`GET /conflicts/agreement`). On our own dogfood graph the answer is 67.5%
  over 40 judged pairs — mostly correct passes, with the model over-flagging
  contradictions. That's the local cortex measured on your graph, not
  promised from a benchmark.

### Fixed

- **Cross-project reads no longer skip live TepinDB stores as "db
  missing".** The registry records the store path each project was
  *registered* with — which may still say `graph.db` while the store on disk
  migrated to `graph.tepin` long ago. Cross-project surfaces (the pane's
  promotion scan, `search` with `project: "all"`, the projects listing) read
  that recorded path literally and skipped healthy projects. Every registry
  reader now resolves the recorded path exactly the way opening it does
  (a `.tepin` sibling wins over the recorded `.db`), so stale registry
  entries keep working forever — no manual edits needed.

## v0.8.8

### One heavy core, everything else light

- **The process model is now deliberate: one machine core, N light access
  points.** The core (an internal `engram-alpha core` process, spawned
  detached for you) holds everything heavy — every store lock, the three
  local models, the pane, and every MCP session — and never exits on its
  own. `serve` became a launcher: it ensures the core is running, registers
  your repo with it, prints the pane URL, and exits; plugins and scripts that
  call it keep working unchanged. `engram-alpha mcp` is now *always* a light
  bridge: the last direct-open fallback (which could silently turn one
  assistant session into a second 600 MB process holding your store) is
  removed on every backend — if no core can be reached or started, the
  session fails with a clear error in `.engram/mcp.log` instead of going
  heavy. The new [Runtime architecture](docs/runtime.md) page documents the
  whole model — processes, discovery, transport, lifecycle (#3).
- **`engram-alpha status` shows what's running** (#5): core pid, version,
  uptime, and port; model residency (loaded, or unloaded-idle and since
  when); every registered project with whether the core holds its store
  open; and every connected MCP client with pid, project folder, and
  connection age — all read live from the core's API, `--json` for scripts.
  Underneath it is a client census: every bridge holds a lease it renews on
  its existing 15-second heartbeat, so a crashed client disappears from the
  list within 45 seconds and a clean exit removes it immediately. The pane
  shows the same census in **Settings → System → Processes** — hidden
  automatically against an older core.
- **`engram-alpha stop` is an orchestrated shutdown, not a kill.** It asks
  the core over a loopback-only `POST /shutdown`: MCP sessions close
  (bridges exit right away instead of waiting out a heartbeat), in-flight
  store operations commit, every store lock is released, and the daemon
  files are removed — then reports how many clients were released. SIGTERM
  and ctrl-c run the same exit; the health-verified PID kill remains as the
  fallback for an unresponsive or older core.
- **An idle core gives its memory back.** After 15 minutes with zero
  connected bridges, no HTTP activity, and no model use, the core drops all
  three ONNX sessions — measured: ~845 MB resident down to ~150 MB (the OS
  reclaims in stages), with the core itself staying resident and instantly
  reachable. The next demand from any path reloads lazily, exactly once:
  0.1–0.5 s measured, not seconds. A connected-but-idle assistant keeps
  models resident by design; health polls, `status` reads, and lease pings
  are exempt from the activity clock, so watching the core never keeps it
  warm — and a background sweep in flight is never cut mid-work.
  `ENGRAM_IDLE_UNLOAD_SECS` tunes the window, `0` disables.
- **`mcp` binds its project by the client's MCP roots — `--db` is now
  optional** (#4). Without it, the bridge binds the session to the first
  `file://` root the client advertises (falling back to its working
  directory) and rebinds on `roots/list_changed`, so one global, db-less
  config entry follows the client across project switches. That unlocks
  **Windsurf**: `setup --cli windsurf` writes the machine-wide entry into
  BOTH global configs — `${XDG_CONFIG_HOME:-~/.config}/devin/mcp_config.json`
  (what the JetBrains plugin generation reads) and
  `~/.devin/mcp_config.json` (the older generation) — inserting into an
  existing config without touching other servers, plus the `AGENTS.md`
  capture block. `claude` setups now write db-less entries too, making
  `.mcp.json` portable across checkouts; an explicit `--db` still pins the
  project exactly as before.
- **A roots bridge survives hostile launches** (field-tested against the
  Windsurf JetBrains plugin). A client that advertises MCP roots but never
  answers `roots/list` no longer strands the session — the 5s timeout falls
  back to the launch directory and held requests are answered. A launch cwd
  that can't host a project (the plugin spawns the server from `/`) binds
  the session to the machine core's **home graph** instead of dying, with
  the census lease on the home root; a later `roots/list_changed` naming a
  real workspace rebinds away from home. And when binding fails for good,
  the bridge answers outstanding requests with the real error and exits
  immediately — previously tokio's stdin read kept the exiting process
  alive as a silent zombie until the client killed it (~60s of nothing).
  When the launch cwd is unwritable, `mcp.log` tees into `~/.engram/`
  instead of vanishing.
- **A "default agent project" for sessions with no folder signal at all.**
  Some clients can't say where they're working — Windsurf's JetBrains plugin
  spawns its bridges from `/` and never answers the roots request — and
  "correctly degraded to the home graph" is still the wrong graph when the
  agent is plainly working on one specific project. The new machine-level
  setting slots in as the pre-home rung of the binding ladder: explicit
  `--db` → client roots → usable cwd → **default agent project** → home.
  Set it in the pane (**Settings → System info → Default agent project**, a
  dropdown of home + your registered projects), or over the core's
  loopback-only `GET/POST /settings`; it lives in `~/.engram/settings.json`,
  survives restarts, validates on write (the project must exist), and is
  read at bind time — changing it points *future* sessions, never rebinds
  connected ones. Each session logs which rung bound it.
- **The census now says which MCP client bound where.** Every lease carries
  the client's own name from its `initialize` handshake (`claude-code`,
  `mcp-go`, …), shown in `engram-alpha status`, `/system`, and the pane's
  Processes list — so "which client is writing into which graph, and via
  what" is one glance, not an archaeology session. Purely additive: older
  bridges simply show without a name.

- **A project's folder is a selector.** Anywhere a `project` argument is
  accepted — every MCP tool, and now `GET /brief?project=…` over HTTP — you
  can pass the project's directory instead of its name or id, and any path
  *inside* the repo resolves to it (the longest registered root wins, so a
  project nested in another still picks itself). Callers that hold a folder
  and not a name — a SessionStart hook with `$CLAUDE_PROJECT_DIR`, a bridge
  with its working directory — ask directly instead of mapping it first.
  `GET /brief?project=…` renders the brief a session bound there receives
  (that project, plus the home-graph section and the roster), while
  `/projects/{id}/brief` keeps its existing meaning: that graph alone.

### Fixed

- **The session brief follows the session's project again.** `brief` with no
  `project` argument, and the `current` flag in `list_projects`, were read
  from the *hub's* current project rather than the one the session is bound
  to. While the daemon launched inside the repo it served, those were the
  same graph; once the machine core became a dedicated home-rooted process,
  every bound session was briefed on the **home** graph — usually the
  "cold start, the graph is empty" text — and told it was sitting in
  `home`, no matter which project its bridge had correctly bound. Both now
  render the session's own project (an explicit `project` argument behaves
  exactly as before), and the brief's home section and "other project
  graphs" roster are computed relative to it.
- **The SessionStart brief hook stopped injecting nothing.** Its daemon
  check only accepted a daemon whose `/health` advertised *this repo's*
  store, which the home-rooted core never does, and its fallback gave up
  silently when `engram-alpha` wasn't on the hook's login-less `PATH` — so
  sessions started unbriefed, indistinguishable from an empty graph. The
  hook now resolves the repo to a project id and asks the core for the
  scoped brief (a read — it never registers anything), and the fallback
  probes `~/.cargo/bin`, `~/.local/bin`, `/usr/local/bin` and
  `/opt/homebrew/bin` before giving up.
- **`doctor` no longer mis-reports a healthy setup** (#6). It finds the
  machine core via `~/.engram/daemon.json` regardless of the directory it
  runs from (the old check only read the repo-local file, so an auto-spawned
  core looked like "a daemon serving a different db"), and it asks the core
  whether it holds this repo's store *before* touching the file — a store
  held open by a healthy core is reported as exactly that, instead of a
  failed direct open. The comparison is by the store a path resolves to, not
  the string: a registry that says `graph.db` matches the `graph.tepin` the
  core actually holds.
- **A core starting on a busy port only converges with its own daemon.** The
  old check treated any engram `/health` answer as "another serve won the
  race"; a foreign daemon — another user on the machine, a test sandbox —
  could absorb the start and leave it coreless. The probe now compares the
  advertised store against this user's home graph; anything else is just a
  taken port to walk past.
- **A freshly spawned core answering its first requests slowly no longer
  fails the session.** Early boot can block the core for a few seconds, so
  one unlucky single-shot probe used to kill an MCP bind that would have
  succeeded moments later. Both bridge resolution paths now retry for up to
  20 seconds after a spawn.

## v0.8.7

### Search learns time

- **`search` is now time-scoped, in one grammar across both memory layers.**
  `after` / `before` take a day (`2026-08-14`), an ISO instant, or a relative
  expression the daemon resolves for you — `today`, `yesterday`, `last week`,
  `last 3 days`, `2 hours ago`, `a month ago`, `this year`. `during_version`
  ("0.8.4") scopes to when that working version was current, resolved from the
  version switches your graph already recorded, and combines with explicit
  bounds by narrowing. The assistant never does date arithmetic: it passes the
  phrase, one clock resolves it. Month and year shifts are calendar-correct,
  so "3 months ago" lands on a real date rather than 90 fixed days. The same
  arguments work on `scope: "memory"` and `scope: "history"`, on the HTTP API
  (`GET /search`), and `list_sessions` takes `after`/`before` too — which
  answers "what was I working on last Tuesday" with no search hit at all.
- **`order` reads results by time instead of by score.** `chronological`
  (oldest first) for how something developed; `recent` (newest first) for the
  current value of something that changed. Ordering is applied after every
  cut, so a time-ordered read returns exactly the set — and carries exactly
  the confidence verdict — its relevance-ordered twin would.
- **Under `order: "recent"`, repeated statements fold under their newest
  form.** History has no supersession chain to follow (nobody curates a
  transcript), so restatements of the same thing collapse into one hit with
  the older ones nested as `prior`. Nothing is dropped — folding is shape, not
  a cut — and the similarity it folds on is a per-graph setting.
- **A scoped question gets a scoped answer, or an error.** The window filters
  before the reranker and both calibrated cuts, so a "strong" verdict means
  the best answer *inside* the window cleared the line. An unreadable date, an
  unknown version or a backwards window is an error that names the problem and
  teaches the grammar — never a silently dropped filter, which would answer an
  unscoped question while looking scoped.
- **Scoping in time makes search *better*, measured.** A new bench
  (`engram-eval --window`, three seeds, receipts in `eval/results/`) asks every
  question twice — once unscoped, once inside a window containing its answer.
  On a 2100-note graph a 30-day window lifts mean recall@5 from 0.74 to 0.94,
  and **oblique recall — questions that never name their subject — from 0.26 to
  0.83**: the window deletes distractors such a query has no way to
  discriminate. Time is a precise, model-free filter the retrieval stack
  otherwise has no access to. The same run retuned the windowed candidate pool
  (`policy.window_overfetch` 8 → 2): depth past 2 was identical on recall at
  both window widths while costing wall-clock, so the deepening that shipped
  earlier in this cycle was mostly waste.
- **The skill teaches reformulation.** On a `weak` or `none` verdict the
  assistant now tries two or three angles — entity-first, paraphrased into the
  graph's vocabulary, date-anchored — before concluding a memory isn't there.
  One phrasing is one probe.

### Fixed

- **A large `limit` beside a time window returned nothing at all.** sqlite-vec
  refuses a KNN `k` above 4096 rather than capping it, and the vector search
  quadruples `k` internally to dedupe claim chunks — so the windowed candidate
  pool sailed past the ceiling for any `limit` of 11 or more, and the error
  emptied the whole search. Worse, an empty result is indistinguishable from
  the "the graph is silent" verdict, so a backend refusal was presenting
  itself as a statement about your graph's contents. The vector search now
  clamps its own ask. Found by the new `--window` bench, which is also why it
  now fails loudly on a search error instead of scoring it as a zero.
- **The redaction backstop stopped eating technical identifiers.** Entropy is
  now measured per separator-delimited segment instead of over a whole token,
  so model slugs (`cross-encoder/nli-deberta-v3-small`), target triples
  (`x86_64-unknown-linux-gnu`) and long URLs survive, while structureless
  credential material still dies — real secrets have no dictionary-shaped
  parts. Every named pattern (PEM, AWS, JWT, GitHub, Slack, OpenAI-style,
  `key = value`) is untouched; those are what actually catch secrets. Notes
  masked by the old rule are not recoverable — the original was never stored —
  so this fixes the mechanism, not the past. SECURITY.md now spells out the
  two-layer split behind it: the curated graph is redacted but deliberately
  *not* encrypted (it exists to be inspected), while session history is
  redacted *and* encrypted (nobody reviews a transcript).

### Changed

- **History ingestion reacts to the filesystem.** A transcript write now pulls
  the next harvest forward instead of waiting out the 60-second interval, so a
  note captured seconds ago can already find the exchange it was born in. The
  timed sweep remains the guarantee: no watcher, an unwatchable directory or a
  missed event all degrade to exactly the previous behaviour.
- Search hits carry `created_at`, so a result can be dated or time-ordered
  without a second read.

## v0.8.6

- **JetBrains plugin works on 2026.1 again** (#1). Installing on IDEA 2026.1.x
  disabled the plugin with "Module engram.frontend is not enabled because
  dependency intellij.platform.ui.jcef is not available": that module dep is
  required on 2026.2+ (JCEF moved into a separate bundled plugin there) but
  unresolvable on a 2026.1 classic desktop runtime — the name only exists for
  the split-mode loader, which is also why `runIde` (split mode) and the plugin
  verifier never caught it. One descriptor can't serve both lines, so each
  release now ships two Marketplace artifacts under the same plugin id:
  `X.Y.Z-261` (2026.1-only, without the dep) and `X.Y.Z` (2026.2+, with it) —
  your IDE gets the matching one automatically.
- **Engram now tells you when a newer release is out.** `doctor` gained a
  `version` section that always asks GitHub Releases whether the binary is
  behind and warns with the exact `engram-alpha update` to run — an
  unreachable network is an informational note, never a failure. The daemon
  does the same check in the background: at most once per 24h across
  restarts (stamped in `~/.engram/update-check.json`), off the startup path
  so `serve` never waits on the network, one log line when something newer
  exists, silence otherwise — including on any curl failure; offline stays
  fully supported. Nothing is ever installed automatically, the query is the
  same system-curl `releases/latest` call self-update always made (still no
  bundled HTTP client), and `ENGRAM_UPDATE_CHECK=0` switches the daemon-side
  check off.
- **A narrow pane sheds its header chrome sooner.** The bare-minimum layout —
  connection badge plus the actions burger — now kicks in at 500px instead of
  400px, so a docked IDE pane in that range stops overflowing its header.

## v0.8.5

### Bugfix release — first field reports (thank you!)

Locking, corporate proxies, and self-consistent configs — everything found
by the first users running Engram outside its home machine.

- **A tepin store can no longer be locked away from its core.** The stdio
  MCP server's last-resort fallback used to open the store directly when no
  core answered; an IDE-launched session could then hold the repo's graph —
  and, through lazy multi-project federation, the machine-wide home graph —
  hostage for hours, while the pane showed a fresh empty graph and
  `/projects/home/graph` returned `database_locked`. The fallback now
  refuses tepin stores outright (SQLite stays fine — WAL coexists), the
  direct hub's project factory refuses tepin satellites, and the auto-started
  core gets a real chance to come up: the spawn no longer aborts when
  `.engram/` doesn't exist yet (a fresh repo hit exactly that), and the wait
  accommodates first-run model provisioning (180s, was 60s). `doctor` learned
  the flip side: a store the machine core holds open is *healthy*-locked, not
  a failure.
- **Corporate proxies can't intercept the MCP bridge anymore** (#2). The
  bridge talks to the core on 127.0.0.1 but its HTTP client honored
  `HTTP(S)_PROXY`, so a corporate proxy answered instead — with an HTML error
  page. The bridge client now opts out of proxies entirely; `NO_PROXY`
  workarounds are no longer needed.
- **MCP failures are finally visible** (#2). The stdio MCP server logs to
  `.engram/mcp.log` (IDE clients swallow stderr) — startup, the bridge
  target, and any fatal error. `RUST_LOG=debug|trace` raises detail,
  `ENGRAM_MCP_LOG=0` turns the file off. `doctor` also warns when a proxy
  is configured without a `NO_PROXY` loopback exclusion, and the
  troubleshooting guide gained a proxy section.
- **`setup` and `doctor` agree about the store path again.** `setup` wrote
  `--db …/graph.db` into agent configs while the store is `graph.tepin`,
  and `doctor` then failed its own generated config ("--db points at …,
  not this repo's graph"). `setup` now writes the resolved store path, and
  `doctor` compares the store a path *opens* rather than the string, so
  existing configs pass without re-running setup.

## v0.8.4

### The history layer — your coding sessions become memory's footnotes

The daemon can now record coding-assistant conversations into a hidden,
**encrypted** per-project history layer, cross-linked to curated memory and
searchable only as a labeled fall-through behind it. Curated memory stays the
answer; history is its footnotes and its escape hatch. Recording is
**opt-in** — the history view explains what turning it on does and offers
the switch.

- **The harvester** tails transcripts on a 60-second sweep
  (`ENGRAM_HARVEST_INTERVAL`), first tick at startup — months of existing
  sessions appear retroactively. Seven adapters, one per harness of the main
  agent roster: **Claude Code** (JSONL, the richest), **Codex CLI** (streamed
  line-by-line — 700 MB rollouts never slurped), **Gemini CLI**, **opencode**,
  **Kilo Code** (the VS Code extension's task storage), **Antigravity**
  (the CLI's brain transcripts, format verified live), **IBM Bob** (one
  SQLite serves the IDE and BobShell, so one toggle covers both; format
  verified live against a Bob IDE install). Only
  user text and assistant prose survive: tool traffic, thinking blocks,
  subagent sidechains and harness scaffolding are dropped at parse level,
  and every message keeps a `raw_ref` back into the original transcript.
  Routing follows the cwd each harness recorded (longest registered root
  wins); sessions of unregistered projects are skipped, not hoarded.
- **A sibling store, not a filter.** History lives in
  `.engram/history.tepin` beside the curated graph — search, brief, drift,
  decay, suspects and the graph pane structurally cannot see it. Session and
  Message nodes (a chat ontology, outside the default 8 types) chain with
  `next`, carry no trust and never decay: they're records, not knowledge.
- **Sealed at rest.** Message and session text is zstd-compressed then
  encrypted (XChaCha20-Poly1305) under a per-machine key in the OS keystore
  (file fallback for headless; `ENGRAM_KEYRING=off`). Stored text measures
  ~0.25× its raw size; decrypting a full candidate set costs ~0.2 ms.
  Structure, timestamps and embedding vectors stay open — the threat model
  is stated honestly in SECURITY.md (protects copied files, backups, other
  users; not same-user malware).
- **Sectioned search, never blended.** `search` grew
  `scope: auto | memory | history`. On `auto`, history is queried only when
  the calibrated verdict says the answer is likely not in curated memory,
  and appears as its own labeled section — snippets plus handles. The section
  is gated on its top hit clearing the calibrated delivery floor: an
  all-noise section vanishes, a section with any real match keeps its full
  candidate list (measured against per-hit trimming, which cost 0.09 oblique
  dialogue recall for the same noise reduction). The new `expand_history`
  tool returns the surrounding exchange, and `list_sessions` browses the
  recordings (newest first, per-harness filter) when no search hit points
  the way; the model decides how much raw dialogue to spend context on. Measured before shipping (`--cascade`, the
  new bench): the router fires on 97–99% of dialogue-only questions and
  end-to-end recall of facts that exist only in conversation is 0.83–0.84
  at sizes 100/500.
- **born-in provenance, both directions.** Notes captured over MCP link to
  the exchange they were born in (resolved by the harvester's parking lot —
  closest preceding assistant message, alive sessions preferred). Search
  hits and `get_node` carry a `born_in` handle; `expand_history` returns the
  reverse — `notes`, every curated note born during that session — and the
  node drawer's Session field jumps into the history view centered on the
  birth exchange.
- **The pane owns every knob.** A third **History** view (session lanes; the
  conversation zigzags user-left / assistant-right down a time axis), a
  Settings section with the group switches (record sessions / history in
  search), per-harness toggles, an ignored-paths editor and the search
  fall-through knob, per-session delete (which also excludes that transcript
  from re-indexing), wholesale delete (`history.tepin` removed, cursors
  reset), and live stats. Recording is **off by default**: the sealing key
  lives in the OS keystore (a keychain prompt on macOS), so the layer starts
  only on the user's own gesture — from the history view or settings.

## v0.8.3

### The default embedder is pinned to fp32 — the shipped weights now match the measured ones

- `cortex::presets` pinned `bge-small-en-v1.5` to **`onnx/model.onnx`**
  (fp32), explicitly, the way the default reranker already was — instead of
  inheriting the quantized `onnx/model_quantized.onnx` default.
- **Why:** every number in `eval/results/` was produced by the fp32 weights.
  The quantized default arrived later, and `provision()` skips a model
  directory that already holds a `model.onnx`, so no machine that had ever
  run Engram picked the int8 file up. The shipped default and the measured
  default had quietly diverged — and this project gates retrieval changes on
  that bench, so they cannot be allowed to disagree.
- **Not a claim that int8 is worse.** It would take roughly 100 MB off the
  daemon and may well be fine; it is a retrieval change, so it belongs behind
  a `--ladder` run rather than behind a default nobody measured. The other
  embedding presets (`bge-base-en-v1.5`, `all-MiniLM-L6-v2`) are untouched
  and stay quantized.
- **Caveat, stated plainly:** this aligns *fresh* installs. Anyone who
  provisioned during v0.8.2 already has the int8 file on disk, and
  `provision()` will not replace it — they keep running quantized weights
  until that directory is cleared.

### The daemon gets lighter and faster at once — batch width was the culprit

- **Inference batches are capped at 2** (`engram_core::onnx`), instead of
  inheriting fastembed's default of 256. The daemon's footprint had been
  climbing through normal use and never coming back down, well past what the
  three models' weights account for; the batch width turned out to be the
  whole of it. Narrowing it removes **almost all of that growth** — a
  workload of searches, a `check_claim` and a conflict scan now barely moves
  the daemon off its startup footprint, where before it added hundreds of
  megabytes and kept them for the life of the process.
- It is **faster**, not a trade. Padding is `BatchLongest`, so a wide batch
  pads every short text out to the longest one in it and pays full attention
  cost on the padding — and Engram's notes vary a lot in length. On the eval
  bench, the narrow batch cut wall-clock by **~40%** and CPU time by more than
  half, reproducibly; on the two-size gate run, ~16% end to end.
- **Retrieval is untouched, and that is checked rather than asserted.** The
  gate ran the bench at sizes 100 and 500 under the old and new batch widths:
  **every headline metric is identical on all seven arms** — recall, focus,
  noise, oblique recall, false-positive rate, tokens per query. Of 878 values
  compared, 14 move, and all of them are float summation order (a different
  batch shape blocks the kernels differently): mean scores agree to about
  eight decimal places instead of exactly, and one brief came out two tokens
  shorter downstream of that. Numerically equivalent, not bit-identical —
  stated plainly so a future receipt diff is not mistaken for a regression.
- Three other suspects were measured and **rejected**: ONNX Runtime's arena
  allocator (no effect on either axis once the batch is narrow — left off as
  cheap insurance), per-session thread pools (worth ~20 MB, cost ~20% of
  wall-clock, so the default stays wide), and the allocator holding freed
  pages (disproved outright — a `malloc_zone_pressure_relief` timer reclaimed
  nothing, because the heap is genuinely live).
- New `scripts/mem-probe.sh` runs a daemon against an isolated copy of a
  graph and reports its footprint across startup, search, NLI and idle — the
  harness these numbers come from, so the next person can re-measure instead
  of trusting this entry.
- New escape hatches for anyone wanting a different trade:
  `ENGRAM_ONNX_BATCH`, `ENGRAM_ONNX_THREADS` (cap inference threads — gives
  up retrieval latency so a background daemon does not saturate every core),
  and `ENGRAM_ONNX_ARENA=1`.

## v0.8.2

### merge_nodes — consolidation becomes one atomic verb

- **New MCP tool `merge_nodes`** (`Engine::merge_nodes`): merge several notes
  stating the same knowledge into one survivor. Tags and code_refs union onto
  the survivor, the victims' **live edges rehome onto it** — deduped by
  (verb, endpoint, direction), self-loops and edges internal to the merged
  set stay behind, and incoming `replaces` edges never move (they are the
  victim's own story) — and each victim is archived behind a `replaces` edge
  so its generation stays traversable. A rehomed edge keeps its id and
  timestamps; a rehomed `conflicts-with` re-runs demotion reconciliation, so
  the survivor genuinely inherits a live conflict. Supersession, not
  deletion: nothing is destroyed, every step lands in the audit journal, and
  the victims get a new `merged` audit action. Pinned victims are refused
  for the assistant (surfaced instead) and archived explicitly for a
  user-sourced merge — the same contract as the pane's replaces verdict.
  The response is the usual write verdict (`warnings` / `suspects` /
  `canon`), with warnings about the victims themselves filtered out.
  Previously the guidance was "merge via `update_node`" plus hand-written
  `replaces` links, which stranded the victims' edges on archived nodes.

### External corpus — the harness stops grading only its own homework

- **LongMemEval adapter** (`engram-eval --longmemeval s|oracle`): the first
  corpus the harness runs that we did not generate. The dataset (Wu et al.,
  MIT) is downloaded on demand into `eval/data/` (gitignored), verified
  against a **pinned SHA-256**, and cached — the repo carries only the
  loader and the digests. Ingestion is deliberately **as-is** (one note per
  chat turn, verbatim — Engram ships no extractor, so the unflattering
  register is the honest one) and grading is **retrieval**: a hit counts
  when a note from a labelled evidence session is delivered — the full
  population, deterministic, no LLM judge anywhere. The `_abs` questions
  (deliberately unanswerable) are scored under the calibrated "likely not
  in memory" verdict: a warned answer is honest, only an unwarned one is a
  false positive. `--lme-limit` caps a smoke run and is loudly labelled.
  Measured, full population (`eval/results/longmemeval-s-full.json`):
  engram ties rag's R@1 (0.91) within 0.02 R@5 at **208 vs 2,654 delivered
  tokens per query**, and the 30 never-answerable questions produce **0
  unwarned answers** under the auto-tuned line (28 warned, 2 empty) — every
  arm without a verdict layer confidently answers all 30.
- **The chat ontology, defined as data** (`--lme-ontology chat`, the
  default): the LongMemEval stores run under a two-type per-graph config —
  user `statement` (small rank prior: the first-party source wins ties) over
  assistant `reply` (no prior, muted) — the one distinction an as-is
  ingester can make honestly without a classifier. Same engine, not a line
  of engine code changed: per-graph GraphConfig doing what it was built for
  on a register the stock software ontology was never meant to fit. Notes
  are stamped with their session's real date (`created_at` — the knowledge's
  original date), so recency reads the conversation timeline.
  `--lme-ontology default` runs the stock set for comparison.
- **GPU embeddings for the LongMemEval run** (`--lme-embedder ollama`,
  `--lme-workers`): a live profile put 99.5% of the run's wall-clock in the
  CPU embedder, so the harness — and only the harness, the daemon is
  untouched — can serve the same `bge-small-en-v1.5` weights (GGUF F16, CLS
  pooling, L2-normalised) from a local Ollama or llama-server endpoint,
  auto-detected, with parallel question workers feeding it. Grades are
  identical to the shipped CPU path to the digit on every arm; the only
  thing that changes is the clock: ~119 s/question → ~13, a 16-hour full
  run in under two.

### Supersession chains — what `replaces` buys, measured

- **The chain bench** (`engram-eval --chains`): ADR-shaped history in the
  generated corpus — N generations of the same decision, each `replaces`-ing
  the last, retired generations backdated a month apart. Three questions the
  regular suite structurally cannot ask: does the **live head** come back
  when the subject is asked about; does a **retired generation** ever arrive
  beside it (pollution — on the superseded store it is 0 by construction,
  and a **flat ablation** with no supersession edges shows what it would be
  without the mechanism); and does **retired mean retired** — every retired
  generation must be absent from search even when queried with its own title
  verbatim, while staying reachable through the `replaces` chain and
  fetchable by id. Chains live on their own corpus field so the regular
  suite's no-state-mutating-edges invariant stays intact.
- First measured run (200 facts + 20 chains × 3 generations, bge-small +
  jina reranker): superseded store **R@1 0.75 / pollution 0.00 /
  head-first 1.00**; flat ablation R@1 0.50 / **pollution 0.88** /
  head-first 0.59. All mechanism checks perfect — history reachable by link
  1.00, retired searchable by its own verbatim title 0.00, retired
  fetchable-by-id (archived) 1.00. The "a reversed decision keeps polluting
  the corpus" failure mode is what the flat ablation shows; supersession is
  what removes it, by construction, at +0.25 R@1.
- **Baselines beside both external benches.** The chain bench also scores
  rag, grep, the curated file, and the whole file over the same chain
  questions — stacks with no supersession concept, for which every
  generation is just another record (a file holding the whole history can
  never deliver an unambiguous current answer). The LongMemEval runner
  gained the same three flat-data arms beside engram and rag, and the run
  has its own page — `eval/LONGMEMEVAL.md` — including why the offline
  retrieval-graded protocol is deliberately not a LongMemEval *score*, and
  a reserved section for the future official-suite online half.

### Corpus enrichment and the delivery-budget question

- **Every slot-vocabulary pool grew 12 → 25 entries** (130 new hand-written
  wording/paraphrase pairs across the ten template pools), taking the unique
  slot space from 2,304 to 10,000 combinations per kind. At 1,500 notes each
  vocabulary entry now repeats ~12× instead of ~25×: a more diverse, less
  template-shaped crowd. The word-disjointness invariants that make the
  oblique column meaningful are test-enforced and held through the change.
  **Comparability note:** results measured before and after the enrichment
  are different corpora — the README tables refresh wholesale with the next
  `--series` run, never row by row.
- **`--budget`** (research bench): the delivery-budget sweep — rag and three
  engram configurations (shipped; open pool = every pre-rank cut off; open +
  no delivery trims) each at result budgets 10/15/20/30, asking whether
  engram's order-of-magnitude token headroom can buy the weighted headline
  from pure vectors, and which knob pays for it. Groundwork for an optional
  user-selectable recall profile (token economy vs extended recall).

### NLI model swap — contradictions stop firing at strangers

- **The default NLI model is now `deberta-v3-small-tasksource-nli`**
  (multi-task DeBERTa-v3-small, our quantized ONNX export, 172 MB),
  replacing `mobilebert-uncased-mnli`. MNLI-only models presuppose that two
  sentences co-refer, so unrelated notes in the same register scored as
  confident contradictions ("Engram Alpha is written in Rust" vs "TepinDB is
  published on crates.io": c=0.99, straight past the gate). Measured on the
  `eval/CONTRADICTIONS.md` harness, three seeds at the reference shape:
  false alarms at the shipped 0.80 gate drop 38% → 18–29% with catch at
  94–96% (100% ungated), agree-alarms 6–13% → 0–1% ungated, and on this
  repo's real graph the unbiased queue noise goes 28% → **0%**. MobileBERT
  and the 0.7.2 DeBERTa stay selectable in the pane; explicit selections
  keep resolving.

This release's research cycle asked two questions at 100–2000 notes — can focus
survive scale, and can a false-positive rate of 1.00 be brought down without
cutting a single result — and this release ships the three answers the
tricks bench measured (`eval/README.md` has the full tables).

### Transplant probes — the register gap closed at scale

- **The weak line now calibrates in the graph's own voice, and honest FP
  lands at 0.02 at 1500 notes (0.00 at 100), from 0.84/0.32.** The
  weak-line fit mints two probe families and takes the max of their
  quantiles: the existing question templates over borrowed vocabulary, and
  **transplants** — real sentences from real notes with their two most
  distinctive words swapped for coinages (Inverse Cloze Task inverted,
  ACL 2019). Transplants score exactly like the loudest real noise
  register, so the line finally clears the crowd ceiling that templated
  probes under-read. Price, stated plainly: the "likely not in memory"
  hedge now leads ~45% of lowest-confidence answerable replies (answers
  still delivered, recall/focus unchanged); `policy.weak_line_quantile`
  is the per-graph softness knob.
- **Auto-tune is damped and glitch-bounded.** Every dial now travels half
  the distance from its current value to the fresh fit per pass
  (`policy::AUTO_TUNE_DAMPING = 0.5`) and the *damped* target is
  hard-clamped into the dial's band — one noisy fit can no longer teleport
  a threshold, a corrupt value recovers on the next pass, and consistent
  fits still converge in a few session boundaries. The journal shows both:
  `weak line 0.850 -> 0.663 (fit 0.476, damped)`.
- **The negative results are receipts too** (`eval/results/`,
  `--qpp` mode is new): every per-query unanswerability signal measured
  blind at 1500 — score-shape features, pool-bottom z, random-background
  z/Gumbel, embedding coherence, local-crowd shoulder (AUC 0.60–0.68) —
  the crowd fakes the curve's shape, not just its scale. The knee buffer
  and full-note reranker input also died measured deaths. The eval README
  gained the **evolution table**: every generation, what changed, what it
  bought, refuted branches included.

### Knee-mode delivery trim

- **Delivery now cuts at the score cliff, not just a fixed depth.** After
  the fixed floor, the delivered list is trimmed at the largest relative
  drop in its score curve when that drop is at least `policy.knee_cliff`
  (default **0.25**; `null` opts out) — a simplified Tail-Aware Adaptive-k
  (arXiv:2606.11907). Measured recall-free at every size from 100 to 2000
  notes (recall@5 and oblique recall within 0.01) while focus rises 3–4.5×
  and delivered tokens fall 35–50% — and unlike the fixed floor, the cliff
  sharpens as the graph grows, so the gain holds at scale.

### The calibrated "likely not in memory" verdict

- **A `weak` search verdict now says what it means: this likely isn't in
  memory.** The reply leads with that recommendation while the nearest
  candidates are still delivered, never cut — a label, not a barrier. On the
  bench this turns the honest false-positive rate on never-written questions
  from **1.00 into 0.08–0.12** at every measured size, at the price of a
  verify-first note on the lowest-confidence quarter-to-half of real
  questions.
- The skills (all three variants + plugin copy) and the search tool teach
  the new verdict wording.

### Auto-tune's second dial: the weak line

- **The weak-evidence line is now calibrated per graph.** The measured
  correct line runs 0.56 → 0.81 from 100 to 2000 notes, so the fixed 0.85
  default was only ever right for big graphs. At session boundaries (past 50
  notes, reranker loaded) auto-tune mints deterministic phantom probes —
  questions about coined subjects that cannot exist in any graph — and fits
  `policy.weak_evidence_top` to the `policy.weak_line_quantile` (default
  **q90**) of what they still score, split-conformal style. One
  `policy.auto_tune` button governs both dials; every move lands in one
  journaled `auto_tuned` row.

### Pane

- Graph settings → Calibrated delivery grew the knee-trim toggle with its
  cliff stepper and the weak-line quantile, with the plain-word explanations
  rendered from the live values (carried from earlier in this cycle: the
  calibrated-delivery settings block itself).
- Side-pane drawers open at a uniform 42 rem default width, both sides.
- New favicon, and the GitHub Pages site root now serves it too (browsers
  ask the root for `/favicon.ico`; only `/demo/` carried one).

### Measured end to end, and the field lesson

- **New `--posttune` eval mode** measures the shipped stack — knee on, weak
  line auto-calibrated, FP under the recommendation regime — as one
  engram-only pass per size; the eval README's baseline tables now carry
  **engram (pre-tune)** and **engram (post-tune)** rows at 100 and 1500
  notes. Post-tune at 1500: 315 tok/query (−40%), focus 0.10→0.44, recall
  −0.01. At 100: 192 tok/query, focus 0.20→0.64, honest FP 0.96→0.32.
- **Phantom probes speak the graph's own language.** The first live fit
  showed cross-encoder score scales are register-dependent (a real graph's
  noise ceiling sat at ~0.20 where the eval corpus's sat at 0.77), so the
  weak-line probes now borrow vocabulary from the graph's own note titles —
  the coined subject keeps them unanswerable, the borrowed words keep them
  in register — and the fitted line clamps floor-relative
  (`delivery_floor` + 0.03), never to an absolute. Known open edge: at 1500
  notes the shipped calibration still under-reads the crowd ceiling
  (end-to-end FP 0.84 vs the 0.12 in-register ceiling) — named as the next
  cycle's problem in the eval README.
- PLAN.md retired: merged into a slim CLAUDE.md keyword index — the memory
  graph is the plan of record (PLAN §-references in code comments point at
  git history).

## v0.8.0 — measured, not promised

From this release on, retrieval behavior is an output of the benchmark, not
an opinion. The eval harness grew a gradation ladder, attention metrics, and
a delivery-floor sweep — and the product's new defaults cite those runs.

### Calibrated delivery

- **Weak tail hits are trimmed before delivery.** Post-rerank hits under
  `policy.delivery_floor` (default **0.22** — the top of the measured free
  zone at 100/500/1500 notes) are not returned. Recall is unchanged at every
  measured size; delivered tokens drop up to 67% on young graphs (−22% at
  100 notes, quiet by 1500, where everything reaching the top ten already
  clears the floor).
- **Every search reply carries a confidence verdict.** `strong` — the top
  hit cleared `policy.weak_evidence_top` (default **0.85**, from the
  control-arm separation analysis); `weak` — treat hits as leads to verify,
  not answers; `none` — the graph is silent, and the assistant is told to
  say so instead of inventing a memory. A label rather than a hard decline,
  because the floor sweep priced hard abstention out: declining 62–73% of
  never-written questions costs 38–45% of real answers. The capture skills
  teach the verdict in all three variants.
- Verdicts and the trim apply only under a loaded reranker — no judgment is
  issued from an uncalibrated score scale.

### Auto-tune: a mature graph calibrates itself

Past **200 notes** and **20 judged suspect pairs** (at least 3 per side), the
session-boundary validation refits `conflict_suspect_similarity` from the
graph's own judgment history — every dismissal is a labeled false positive,
every confirmation a labeled true one. Balanced-accuracy fit, clamped to
[0.85, `duplicate_similarity`), applied only when it moves the floor
meaningfully, journaled as an `auto_tuned` row. `policy.auto_tune: false`
opts a graph out; smaller graphs keep the benchmark-calibrated defaults.
This automates, per graph, the hand retune that produced the shipped 0.88.

### The measuring stick grew (eval/)

- **The ladder**: `--ladder` runs total graph sizes 10→1500 with **every
  stored fact questioned** (no untested distractors, no type-mix thinning)
  and the curated-file baseline at 3k *and* 30k tokens per size — the
  where-does-a-maintained-file-lose question now has measured crossovers
  (3k: overtaken at 100 notes; 30k: wins to ~200, overtaken at 500).
  `--series` runs the whole battery in one command.
- **Attention metrics**: `focus` (share of delivered tokens that were the
  answer) and `noise` (share of delivered records that were not — a miss
  counts in full, honest silence counts zero) sit beside recall in every
  table. A full-context dump scores recall 1.00 and focus 0.004 on the same
  run.
- **`--floor`**: the delivery-floor sweep that produced the new defaults.
- **Corpus v2**: 16 components across three registers (software, laboratory,
  abstract) and 12-wide slot pools — no result is tuned to one genre's
  vocabulary. Measured side effect worth knowing: half the published oblique
  decay was component crowding, not the retriever.
- **The tuning quest, closed honestly**: no configuration of any arm exceeds
  0.97 R@5 / 0.92 oblique at 100 notes — pure vectors included; the ceiling
  is the embedding model's. With bge-base the shipped stack reaches full
  rag parity at a seventh of rag's tokens.

### Fixes the instrument caught

- **Id collisions under bulk writes**: ~1,600 same-second mints had ~2%
  birthday odds on the 5-char random tail; the tail now advances from a
  per-process random seed — same-process collisions impossible, id shape
  unchanged.
- The eval report labeled its NLI leg with the retired model's name after
  the 0.7.2 mobilebert swap (numbers were right, the label lied); it now
  reads the runtime's identity.
- 768-dim embedders (bge-base) were never actually runnable in the eval —
  the in-memory store now re-dims to the embedder before the first write.

### Storage

TepinDB remains the default: new graphs are born `.tepin` (since 0.6.2) and
any `graph.db` still on SQLite migrates itself on first open with real
embeddings (since 0.7.0), leaving the SQLite file behind as the backup. 0.8.0
makes that the stated contract rather than a transition note.

## v0.7.3 — retired means retired

### Superseded knowledge leaves the canon

A `replaces` edge has always meant *the newer claim wins, the older is
archived into history*. Only the conflict-verdict path actually did it — a
`replaces` link written by the assistant, or a verb retyped in the pane, left
the superseded note sitting in retrieval, on the canvas, and in the review
queue, competing with the claim that replaced it.

Now every live `replaces` edge retires the node it replaces, wherever it came
from. Retired notes keep their place in the successor's **History** section
and nothing is deleted. Two deliberate exceptions: pinned nodes are never
auto-archived (the pane's replaces verdict still overrides a pin, because a
human unsays a human's pin), and withdrawing the edge never un-archives —
`valid_until` is also set by the decay pass and by you. Graphs written before
this rule heal themselves: the session-boundary validation sweeps up whatever
a `replaces` edge left behind.

The pane follows: **archived notes are hidden by default**, one *Show
archived* click away in the filter menu.

### Feed

- **Review lens** opens on the weakest trust first, at the top of the list.
- **↑ / ↓** in the toolbar jump to the ends of the feed.
- Cards show their **code refs** (struck through where the file is gone) and
  say **stale** once, as the badge the Review drawer uses; the card date
  spells out creation and last retrieval on hover.
- A short card at either end of the feed can be focused again — the center
  line now clamps to the first and last card, which the feed's padding could
  never let them reach.
- Switching to the feed closes the detail drawer — the feed acts on the card
  at its center, and **Edit** is what opens the full form.

### Starting from empty

An empty graph shows one card (not two), on either screen. It now points at
the gesture that actually exists — `/engram:digest`, or *"digest this
project"* — and carries an **Ontology** picker: an empty graph is the one
moment a preset can be applied with nothing to retype, so the choice lives
where you meet it.

### Switching projects starts clean

Pane state that describes one graph — the feed's back trail, search hits,
Checkup reports, the audit page, filters and selection — is reset on a
project switch instead of being carried into the next graph.

## v0.7.2 — measured, not guessed

Engram grew an eval harness, pointed it at itself, and two layers lost. The
NLI model was replaced and search was retuned — both on numbers, both
reproducible from `eval/`.

### Search finds more of what you meant

Two knobs moved together, and only together:

- **Keyword weight 0.5 → 0.15.** A question that never names its subject
  scores zero on the keyword channel by construction, so a high weight caps
  how relevant it can ever be — while a distractor sharing one common word
  scores on both channels. 
- **The reranker votes instead of deciding.** Its ordering is folded into the
  retrieval ordering by a reciprocal-rank vote, so a result two independent
  channels ranked highly can't be buried by one confident cross-encoder
  mistake.

| | before | after |
|---|---|---|
| recall, questions that name their subject | 1.00 | 1.00 |
| recall, questions that describe it instead | 0.18 | **0.32** |
| weighted recall | 0.916 | **0.929** |
| tokens per query | 525 | 521 |

Nothing costs more. The gain is entirely on questions phrased the way people
actually ask them months later — *"which thing did we pick because the
storage layer closes a chunk at a fixed cadence?"* — where the subject's name
is the one word you've forgotten.

Measured alone, each knob looks dead: at the old keyword weight the vote is
actively *harmful*, because the right answer is dropped from the candidate
set before the reranker is ever called. Their doc comments say to change them
together or not at all.

Search tuning now lives in `PolicyConfig` (`keyword_weight`, `semantic_floor`,
`search_min_score`, `search_relative_cut`, `rerank_trust_weight`,
`rerank_vote_k`) instead of compile-time constants, so a graph can be swept
rather than argued about. Existing graphs pick up the new defaults; anything
you set explicitly is untouched.

### MobileBERT replaces DeBERTa-v3-small for NLI

Engram detects contradictions locally, with a small cross-encoder, where most
agent-memory tools spend an LLM call. That layer had never been scored end to
end — only the model in isolation, on sentence pairs, which is the wrong unit:
in `check_claim` the model only ever judges what retrieval hands it.

The new benchmark (`eval/CONTRADICTIONS.md`, `--contradictions`) scores the
whole path, and reports what the layer *catches* against what it *costs*. That
second number is the one that mattered: a layer answering "contradiction" to
everything scores a perfect catch rate. Measured on claims that restate a
stored note verbatim — statements the graph literally contains — the old model
called **80-86% of them contradictions**.

`Xenova/mobilebert-uncased-mnli` is now the default:

| | catch | false alarms | ONNX |
|---|---|---|---|
| `nli-deberta-v3-small` (old) | 97-99% | 80-86% | 172 MB |
| `mobilebert-uncased-mnli` (new) | 95-97% | **57-62%** | **27 MB** |

Five seeds, same corpus and retrieval, only the model swapped. Two points of
catch for twenty-three points of false alarms, from a model a seventh of the
size. Retrieval never missed once in any run — the entire headroom was the
model's.

**Nothing to do on upgrade.** The NLI layer is stateless, so there is no data
migration: the new model downloads on first `serve`/`mcp` (27 MB, one time).
The old model stays selectable under Settings → Choose models, and an existing
explicit selection keeps working. If you never picked one, you get the new
default. Suspects already queued keep the hints the old model gave them —
those only ever affected queue ordering.

Also corrected: `nli.rs` had long claimed the shipped model was "~34 MB". It
was 172 MB. Every candidate tested was smaller than what shipped.

### `check_claim` gained a confidence gate

A contradiction the model is not confident about is now reported as **silence**
rather than as a conflict. The raw probabilities still ride along on the
verdict, so nothing is hidden — only unasserted.

This gate did not exist before. Its sibling, the write-time conflict sweep, has
held a similarity floor and a confidence gate for a year precisely because
MNLI-class models call unrelated same-shaped titles confident contradictions.
`check_claim` had neither and judged whatever the top-8 retrieval returned.

The threshold is **0.80**, chosen on five seeds for stability rather than for
the best headline:

| gate | catch (worst seed) | spread | false alarms | agreeing claims called conflicts |
|---|---|---|---|---|
| 0.00 | 96% (95%) | 2 pts | 61% | 7–13% |
| 0.70 | 95% (94%) | 1 pt | 44% | 2–6% |
| **0.80** | **92% (90%)** | **4 pts** | **38%** | **1–2%** |
| 0.90 | 85% (79%) | 11 pts | 27% | 0–2% |
| 0.95 | 77% (71%) | 13 pts | 18% | 0–1% |

Tighter gates keep scoring a better catch-minus-false-alarms gap all the way to
0.95, where catch falls to seven in ten and swings thirteen points between
seeds — so the gap is not the criterion. 0.80 is the last gate before catch
comes apart.

The last column is the second reason for it. Asserting *conflict* against a note
a claim plainly **agrees** with is the worst thing this layer can do, and it was
happening to one agreeing claim in ten. The gate takes that to one or two in a
hundred.

Tunable per graph as `policy.claim_contradiction_min_confidence`. It is
deliberately contradiction-only: false `supports` has never been measured, and
gating it on the same number would be guessing.

**End to end**, against what 0.7.1 shipped: false alarms **80–86% → 38%**, catch
**98–99% → 92%**.

### Checked against a real graph

New `--real-graph` eval mode scores the suspect queue against every pair a human
has actually ruled on in a live graph, plus every `conflicts-with` edge. Run
against this repo's own memory (297 nodes, 42 judged pairs), it says three
things the synthetic corpus could not:

- **Real prose is not harder.** Ungated false alarms are 62% on real notes
  against 61% on generated ones. The worry that mushier multi-paragraph notes
  would be worse turned out to be wrong.
- **In the product, the rate is 19%, not 38%.** The queue skips pairs whose
  nodes are already linked, before the model is called — and nine of the
  thirteen pairs the model still flags at the shipped gate carry an edge
  already. Structure the user recorded, spent as precision.
- **The new model is the only candidate that catches the one real
  contradiction** in this project's recorded history — at 0.80, and not at
  0.90. Both DeBERTas and DistilBERT miss it at every threshold. The incumbent's
  quiet queue was quiet because it barely fires on real technical prose at all.

`check_claim`'s `silent` bucket is now sorted strongest-signal-first, so the
claims the model came closest to ruling on sit at the top of it.

### Still open

19–38% false alarms is better, not good. The catch rate rests entirely on
generated prose: in 297 nodes this project has recorded exactly one
contradiction, so real data validates the cost side precisely and the benefit
side anecdotally. One graph, one project, one register. See
`eval/CONTRADICTIONS.md`.

## v0.7.1 — the timeline release

The graph gets a second screen: your memory as a story you can scroll.

### The timeline feed

The new **Graph / Feed** toggle in the topbar switches the pane to a
vertical feed of node cards — neighbors peeking above and below, the
centered card in focus.

- **Two lenses.** *Timeline* shows everything in chronological order, with
  version markers wherever the project's working version moved on. *Review*
  shows only what needs a human eye (provisional / stale / drifted).
- **Read without leaving.** Every card renders its full markdown clipped to
  a fixed preview; the card you stop on opens to its whole body, however
  long. Card positions never shift under you — far-away jump targets always
  land. `j`/`k` and the arrow keys navigate.
- **Judge without leaving.** A bottom action bar carries the side drawer's
  controls for the centered card: Approve / Still true / Pin / Edit /
  Delete.
- **Traverse the graph.** Click any edge chip and the feed jumps to that
  node — even one the current lens filters out — and **← back** walks the
  trail home (session-scoped, forty hops).
- **One "current node" across screens.** Select a node on the canvas and
  the feed opens centered on it; leave the feed and the canvas selects and
  centers the card you were reading.

### MCP

- `set_version` now auto-enables version tracking when it was off — asking
  for a version *is* opting in; the reply says so explicitly. Clearing a
  version never toggles tracking.

### Pane polish

- **New control kit** — the Graph settings drawer (and checkboxes/radios
  app-wide) trade native inputs for a hue rail, steppers, segmented
  controls, and toggle chips.
- **Topbar consistency** — brand chip, ⌘K / Ctrl-K search shortcut with a
  keycap hint, and the Graph/Feed switch, all in one glass row.
- **Accent spines** — node/card accent strips are now background gradients
  that follow rounded corners cleanly, with a per-theme wash
  (full-strength in Engram Purple, subtle in the IDE themes).
- **Zoom-gated glass** — canvas card blur switches off below ~0.7 zoom
  (with hysteresis), keeping big-graph panning smooth; archived cards went
  grayscale instead of translucent.
- **Dynamic-center drawers** — the left and right drawers now push each
  other instead of clipping at the midline.
- **Responsive fixes** — the search bar hides under 530 px and the brand
  chip / project switcher under 400 px (IDE side panels); the burger
  dropdown became opaque, which also fixes drawers opened from it
  positioning off-screen (a CSS containing-block trap: `backdrop-filter`
  on an ancestor hijacks `position: fixed` descendants).
- The search bar is opaque in the feed view — Chromium's `backdrop-filter`
  cannot sample content inside a composited scroller, so glass there read
  as a bug. The graph view keeps its blur.
- The graph-health strip is graph-view-only now.

### Docs

- New [timeline feed](./docs/pane.md#the-timeline-feed) section in the pane
  guide, README updates, and refreshed screenshots.
- This file: release notes live in `CHANGELOG.md` from now on, and the
  release workflow publishes each version's section as the GitHub Release
  body.
