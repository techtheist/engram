# Changelog

Release notes for Engram Alpha. Each release's section below becomes the
body of its GitHub Release (draft-release.yml lifts it automatically).

## v0.10.3

### Deleting from the Feed, with or without a Tombstone

- **The Feed's Delete works in VS Code again (issue #14).** It asked for
  confirmation with the browser's own pop-up, which VS Code's webview
  silently blocks, so the button looked dead there. It now opens a dialog
  in the pane: the note and how many connections go with it, then **Just
  delete** (nothing left behind) or **Leave a Tombstone**, with an
  optional note on why and the removed text kept by default, so the
  assistant doesn't learn it again. The dialog remembers your last choice;
  ⌘/Ctrl+Enter confirms, Esc cancels.
- **No browser pop-ups anywhere in the pane.** Deleting a recorded session,
  wiping session history and switching the ontology preset ask in the same
  kind of dialog, so they work in every IDE too. Destructive questions start
  with Cancel focused.

### Fixes

- **Every project's session brief carries the home graph again, and 3
  seconds sooner.** The core runs on the home graph, but building a
  project's brief opened the home graph a second time, waited 3 seconds for
  a lock the core itself held, and quietly left the home section out.
  Measured on this project's graph: the brief your assistant gets at
  session start went from 3.2 s to 0.35 s. Searching or checking a claim
  across `all` projects paid the same 3 seconds and skipped the home graph;
  both are fixed.
- **`serve` and `doctor` recognize an `.engram/` folder that's already
  ignored (issue #12).** They used to look only for an exact `.engram/`
  line, so an anchored `/.engram/` read as missing: `doctor` warned, and
  `serve` appended a duplicate line to your tracked `.gitignore` on every
  start. Both now ask git, so any rule that ignores the folder counts,
  including nested `.gitignore` files, `.git/info/exclude` and your global
  excludes file. Outside a git repository they read `.gitignore` and
  understand anchored entries. `setup` does the same for the config files
  it ignores. On older versions, remove the extra line after updating.
- **System info names the project you picked (issue #13).** The panel
  describes the core, which runs on the home graph, so its store details
  always showed home. With another project selected in the switcher, the
  Graph store section now shows that project's folder and database file.
- **`engram-alpha stop` no longer reports a failure after a clean stop.**
  The core answers the shutdown request and exits; on macOS the connection
  could close before the answer was fully read, so `stop` threw away a
  complete reply. It now keeps it.
- **Card text wraps the same on the GPU canvas as in the browser.** The
  canvas only broke lines at spaces, while the browser also breaks after a
  hyphen, so a card like "write-verdict protocol" could wrap differently
  depending on which renderer drew it.

### Docs and tests

- The pane guide covers the 5,000-note canvas, the selection highlight,
  the Feed's delete dialog and what System info shows for each project;
  troubleshooting has entries for the `.gitignore` and `stop` fixes.
- New unit tests for the `.gitignore` check, the home graph's place in
  briefs and searches, selection focus, line breaking, code refs, audit
  labels and the confirm dialog. A test that failed about 2 runs in 5
  (it hit the `stop` issue above) passes reliably now.

## v0.10.2

### See a note's neighborhood on the graph

- **Selecting a note highlights its connections.** Edges touching the
  selected note are drawn twice as wide, the next ring of connections a bit
  wider than usual and the third ring slightly wider, and they sit above
  the other edges. Every other edge and its label fades back, so you can
  follow what a note connects to at a glance. The change animates in a
  quarter of a second, and on a 5,000-note graph a selection costs about
  2 ms, as before.

### The Feed reads by depth

- **The current card stands out.** Cards fade by how far they sit from the
  card you're reading: the next ones to 70%, then 55%, then 40%. The fade
  follows the focus as you scroll, and it animates. On the Engram Purple
  theme, cards two or more notes away also get a light blur; the IDE
  themes only fade.
- **A selection outline travels with you.** The current card carries the
  same outline as a selected card on the graph, and it glides from card to
  card as you scroll.
- **Feed cards are solid**, the same color as cards on the graph. The
  frosted glass stays on the floating toolbar and action bar.

### Big graphs stay smooth everywhere

We ran a scripted session on a 5,000-note test graph at 1920×1080 on a
Retina screen: zooming from the whole graph to 100% in four steps, with
clicks, drags, scrolling and minimap jumps at each step. Every step now
holds 60 frames per second with no pauses. Fixed along the way:

- **No freeze after jumping around with the minimap.** Minimap jumps at
  low zoom filled the canvas's card image memory with small pictures.
  Zoomed in to 100% afterwards, cards kept swapping each other out every
  frame: 8 to 12 frames per second, blurry cards, until you reloaded the
  pane.
- **Minimap jumps are smooth.** While the view moves fast, cards that come
  into view are drawn at half resolution and sharpen as soon as it slows.
  Uploading full-size card pictures made each jump stutter.
- **Dragging no longer stalls.** Every drag made the canvas measure all
  of its cards again at the start and at the end, about 150 ms each on a
  big graph, with cards briefly redrawn at half resolution.
- **Dragging a card is smooth on big graphs.** Each mouse move used to look
  up every edge's color and style one by one.
- **The first click no longer freezes the canvas.** Opening a note's
  details loads a bold font for its title, and the canvas re-measured
  every card when any font loaded, about 200 ms on a big graph. It now
  only does so for fonts its cards use.

### Smaller fixes

- **A new connection no longer rearranges the graph.** Connecting two cards
  re-ran the layout, and the result landed a moment later, often while you
  were dragging one of them. Cards now stay where they are until you switch
  layout, or notes are added or removed; the next layout includes the new
  connections.
- **Connection dots stay put when a card lifts.** A hovered card rises a
  few pixels; its connection dots now stay where the edges end.
- **In the IDE Light theme, a hovered card stays white** and just lifts.
  It used to turn gray.

## v0.10.1

### The graph canvas runs on the GPU

- **Big graphs stay smooth.** The canvas is now drawn with WebGL2 instead
  of one web page element per note. On a 1,000-note test graph with 1,500
  connections, panning went from 53 ms to 2.5 ms per frame at the median,
  and zooming from 38 ms, with stalls of several seconds, to about 9 ms
  per frame, with no stalls.
- **It looks the same.** Cards keep their colors, type stripe, shadows,
  badges and text. Card text is still drawn by the browser's own text
  engine, in your IDE's font when the pane runs inside an IDE. The card
  under your pointer and the selected card are real HTML on top of the
  canvas, so hover expansion, links and the selection outline work as
  before. The Engram Purple theme's frosted glass now covers every card,
  the minimap and the controls; the IDE themes stay flat.
- **Zoomed far out, card text turns into simple lines.** Below readable
  size, cards show their title and body as bars, which keeps zooming smooth
  on big graphs.
- **The layout no longer freezes the pane.** It runs in the background, and
  only reruns when notes or connections are added or removed. Before, every
  update and every card you dragged re-laid out the whole graph: seconds
  each on a large, connected graph.
- **Five thousand notes work.** The Skyline layout now splits one big,
  densely linked cluster into its communities before arranging it. A
  1,000-note test graph lays out in 0.2 s instead of 5 s, on a field about
  a tenth the size. At 5,000 notes it takes 0.6 s, where it never
  finished before.
- **Zoom out until the whole graph fits.** The zoom-out limit is now the
  zoom that fits your graph, so a big graph can be seen at once.
- **Fewer redraws.** Once zoomed out far enough that text turns into
  lines, the moving dashes on conflict edges stop and every edge is drawn
  as a still line, so an idle canvas draws nothing. Zoomed in, only the
  edges near the screen are drawn, and far-off curves use fewer segments.
- **No more hitch every few seconds on big graphs.** The pane checks for
  changes every 3 seconds, and each check used to rebuild every card
  because trust is recomputed on every read: about 270 ms at 5,000 notes.
  Unchanged notes are now kept as they are.
- **Double-click or double-tap to zoom in** (Shift+double-click zooms out),
  on desktop and on phones.
- **Without a GPU, nothing changes.** If WebGL2 isn't available on a real
  GPU, or the GPU gives out mid-session, the pane switches to the previous
  canvas on its own.

### The Feed keeps up with big graphs

- **Switching screens and scrolling stay fast.** The Feed now only keeps
  the cards near your screen on the page, and still scrolls the full length
  of the feed. On the same 1,000-note graph, opening the Feed went from a
  160 ms stall to about 10 ms, fast scrolling from 120 ms per frame to 8 ms,
  jumping to the end from 137 ms to 9 ms, and following a connection to a
  far card from 100 ms frames to 9 ms.
- **Jumps land exactly where they did.** Every card is measured once in
  the background at its real size, so a jump lands centered at any
  distance, and the card you're reading never moves while measuring runs.
  A long jump skips most of the way and glides the last stretch.
- **A very fast scroll shows placeholder cards**, the same size and color
  with gray lines for text, which fill in a frame or two later.
- **Your browser's find (Ctrl+F) only sees the cards near your screen.**
  The pane's search still finds every note.

### Smaller fixes

- **Selecting a note on the canvas no longer stutters on big graphs.** The
  hidden Feed screen used to redraw all of its cards on every selection;
  it now waits until you open it.
- **In IDE themes, a hovered card no longer shows the canvas through it.**
  The card keeps its color and just lifts. Before, it took the IDE's hover
  highlight, which is often see-through.
- **Card text spacing.** Cards now use proportional figures, so hyphens and
  numbers sit the way they do in the rest of the pane, and the canvas and
  the HTML card wrap text at the same words.
- **Tasks and History have their own background again in Safari.** Safari
  still showed the hidden graph canvas behind them, as a black or frozen
  frame (menus left ghosts on it).
- **The selection outline stays visible when zoomed far out.** It now holds
  about 2 screen pixels at any zoom below 10%. At the new far zoom it had
  thinned to under one pixel.

## v0.10.0

### Tasks: a working list beside the graph

- **Your assistant now keeps a working list, and the next session picks it
  up.** Tasks are what the assistant is doing, what it parked and what it
  still owes. They live next to the memory graph, not in it: tasks never
  show up in search, never raise conflicts and never count as knowledge.
- **The brief opens with the active tasks**, titles only (up to 15). A
  feature with several pieces shows as one line with its progress and the
  next step. Work carried over from an older version is marked, and so is
  a task nobody touched for five sessions. Three recently finished tasks
  ride along.
- **Five statuses and one level of groups.** Active, delayed, backlog,
  done and dropped. Done and dropped tasks are kept, never deleted. A group
  closes itself when its last task closes, and reopens if one of them does.
- **Tasks can close or wake on their own.** A rule is a version
  ("0.10.0", or ">0.9.12" for once the project moves past it), a date
  ("2026-10-20", "in 3 days") or a note ("node:<id>", when that note is
  resolved). A task titled like a release step ("publish 0.9.12") closes
  itself when the working version moves on. Each automatic change says why.
- **Tasks point at memory.** A task can reference the notes it is about;
  the note's drawer lists the open tasks that reference it. When a task
  finishes and settled something, the assistant is asked to save that as a
  note.
- **A Tasks screen in the pane**, between Feed and History: columns by
  status, drag to reorder or move, groups with their tasks inside, a drawer
  for the plan, a progress note, rules and references. Only you can delete a
  task. The brief's task section is set in Settings → Graph settings → Tasks.
- **The brief names the working version once.** It is on the brief's
  first line; the notes under it carry a version only when it is an older
  one, so the current cycle no longer repeats it on every line.
- **For assistants:** three new MCP tools, `add_tasks`, `update_task` and
  `list_tasks`. `set_version` reports the tasks its version change closed or
  woke. The skills teach one task per piece of work, not per step.
- Tasks travel with exports, imports and the SQLite-to-TepinDB migration,
  and are sealed at rest when encryption is on.
- **Tasks can be turned off** per graph in Settings → Graph settings →
  Tasks. They are on by default. Off removes the Tasks screen and the brief
  section, and the assistant's task tools answer that tasks are off and
  what to do instead. Your stored tasks are kept for when you turn them
  back on.

### A faster, more consistent pane

- **Switching screens is much faster on big graphs.** Screens you have
  visited stay ready in the background, and the others are prepared while
  the pane is idle. On a 640-note graph, opening the feed went from about
  0.8 s to under 0.2 s, and returning to the graph from up to 0.75 s to
  under 0.4 s.
- **Screens slide.** Switching between Graph, Feed, Tasks and History moves
  the new screen in from the side while the old one fades out; the top bar
  stays put. With reduced motion turned on in your system, screens just
  crossfade. A screen that isn't ready yet shows a light outline first.
- **One look everywhere.** Every panel now uses the same buttons, fields,
  badges, section headings and notes, so they match in every theme. A node
  and a task open in the same kind of card.
- **A themed date picker** replaces the browser's own, the narrow top bar
  folds the screen switch into one compact control, text boxes grow with
  their text instead of showing a resize handle.
- **New and Filter follow the screen.** On Tasks, New opens a new task and
  Filter narrows the board by version, who created a task, and flags such
  as carried over or idle; on History, Filter narrows the sessions by
  assistant and version. Everywhere else they work on memories as before.
- **Dragging works inside the IDEs.** The task board drags with the
  pointer instead of the browser's drag-and-drop, which the JetBrains tool
  window never finishes (a card greyed out and stayed that way). Escape
  cancels a drag, and it works on touchscreens too. In a narrow IDE pane the
  board's column switch no longer collapses to a sliver. Pressing a card
  never selects its text, and nothing on the page is selected while a card
  is being dragged.
- **On phones the pane no longer zooms.** Pinching, double-tapping and
  tapping into a text box leave the page at its size; the graph canvas
  still zooms on its own.
- **The History screen opens sessions faster.** Listing sessions and
  opening one used to read every recorded message, about 0.1 s per request
  on a graph with ~4,700 recorded messages, growing with every session. They
  now read only what they show: on a sealed test store, listing went from
  46 ms to 0.6 ms and opening a session from 43 ms to 0.8 ms. Existing
  stores are indexed once, the first time they open.

### Browser demo

- **Memory search works in the demo.** The search box runs a keyword
  match over the demo graph, with highlighted snippets like the real
  daemon's. The demo has no embedding models, so it doesn't rank by
  meaning.

## v0.9.12

### Skills that are shorter and keep themselves current

- **The capture skills are about a third of their former length** (from
  ~4,900 to ~1,500 words per variant), and the instruction blocks written
  into `AGENTS.md` / `GEMINI.md` are ~40% shorter. Every rule that changes
  what the assistant does is kept; detail the MCP tools already describe is
  no longer repeated, so each session starts with less to read.
- **Installed skills and instruction blocks update with the binary.** Each
  copy setup writes now carries a stamp: the version, the variant, and a
  fingerprint of the text. After an update, `serve` and `setup` rewrite the
  copies that are still exactly as installed, keeping their variant. A copy
  you edited is left as it is. Copies from earlier releases are recognised
  by matching a text Engram shipped byte for byte, so an edited older copy
  is kept too. Your own text around the `engram:begin` / `engram:end`
  markers is never touched, and symlinked skills are never followed.
- **`doctor` has a skills section** listing each installed copy as current,
  due for an update, or edited and kept.

### Cursor

- **`engram-alpha setup --cli cursor`** wires a repository for Cursor: the
  project MCP config `.cursor/mcp.json`, the skills under `.cursor/skills`
  (skipped when `.claude/skills/engram` exists, which Cursor also reads),
  and a `sessionStart` hook in `.cursor/hooks.json` that puts the brief into
  each new session. An existing `mcp.json` or hooks file is merged into or
  left with a printed snippet, never overwritten. Cursor joins detection,
  `doctor` and the wiring hint `serve` prints.

### Safer project switching

- **Reading another project's brief warns before your writes move.**
  `brief(project: …)` rebinds the session, so a look at a sibling project
  used to send the next write there silently. The reply now leads with which
  graph receives writes from here on and the exact call that goes back.
  Binding a fresh session to its own project stays a one-line confirmation.

### Presets keep your settings

- **Applying an ontology preset replaces the types and verbs only.** The
  graph's policy (including the values auto-tune fitted to it), brief
  settings, version tracking, session history settings and custom fields
  are kept. A new `POST /config/presets/{id}` applies a preset; `PUT /config`
  is unchanged.

### Documentation

- The README says plainly when Engram helps and when a notes file is enough,
  and its introduction and feature list are written in simpler words.

## v0.9.11

### The pane matches your IDE

- **Inside VS Code or a JetBrains IDE, the pane takes the IDE's own theme**:
  backgrounds, text, borders, accent, focus ring, hover tint, corner radii
  and fonts, and it follows a theme switch live. VS Code supplies the theme
  through the variables every webview receives; the JetBrains plugin reads
  its look-and-feel and editor scheme through stable public API and pushes
  them into the pane on load and on every theme or scheme change. Shades a
  theme does not name are derived from the ones it does, so community themes
  land coherent too. Text sizes and spacing stay the pane's own.
- **Match IDE is the default inside an IDE.** It appears first in the theme
  menu there and nowhere else; a choice made in the menu is remembered.
- **Controls follow the host:** buttons and toggles take the IDE's button
  corner instead of a pill, labels use the full foreground, text on the
  accent uses the IDE's button text color, the settings gear is a flat
  toolbar icon, and hover tints are drawn over a control's own surface.
- **Three static themes:** Engram Purple, IDE Light and IDE Dark. The former
  JetBrains Dark and Light skins migrate to IDE Dark and Light.

### Checkup sweeps that finish

- **"Find duplicates" and "Find hidden conflicts" make progress across
  runs.** Each pair is judged once per run, and a pair judged without being
  queued is remembered until either note changes or the judge model or its
  threshold does. Each run spends its budget on pairs no run has read; on a
  600-note graph "Find duplicates" now completes in five runs instead of
  re-reading the same 300 pairs forever.
- **An empty sweep says why:** how many pairs it judged and how many close
  pairs were already linked, already raised, archived, or judged on an
  earlier run.
- **"Find hidden conflicts" reaches below the look-alike threshold** through
  the same title check the write path uses (both titles name one subject,
  share a claim word, and read as contradicting). Replayed on two real
  graphs before shipping, it queued no false alarms.

### Search verdicts that read "strong" again

- **The weak line is calibrated on probes that cannot answer themselves.**
  A calibration probe built from a real sentence is now asked of the graph
  without the note it came from. On long-prose graphs that note used to
  answer its own probe, which pinned the line at its ceiling and made
  nearly every search read "weak". The post-tune ladder is unchanged at 500
  and 1500 notes and keeps a false-positive rate of 0.00 at 100.

### Steadier writes and first runs

- **A checked write embeds with the engine released.** The vectors for the
  duplicate check and for storage are computed before the lock is taken and
  reused under it only when the text and model are unchanged. At 1500 notes,
  readers waiting on the graph went from about half to a fifth of the time
  writes ran, the longest wait from 355 to 157 ms, and a write got faster
  (472 to 435 ms) because its text is no longer embedded twice.
- **The MCP bridge answers while its core is starting.** A first run
  downloads about 450 MB of local models before the core listens. After a
  15-second grace (`ENGRAM_BRIDGE_GRACE_SECS`), the bridge lists the tools
  itself and answers a tool call with an error saying the core is still
  starting, which models are ready, and how much of the rest has arrived;
  nothing runs or is written. Clients with short timeouts no longer read the
  wait as a hang.

### Smaller things

- **Docs:** undo from the audit log is reflected in the trust guide and the
  feature list, with a new screenshot of the session-undo preview; the
  first-run model sizes are corrected (about 130, 150 and 172 MB); the pane
  guide describes Match IDE.
- **The browser demo previews undo** the way the core words it; applying an
  undo still needs the local daemon.
- **Eval:** `engram-eval --writes N` times checked writes under a reader
  waiting for the engine; `--sweep-replay` also runs both Checkup sweeps to
  completion on a copy of a real graph.

## v0.9.10

### Answers that say how far they can be trusted

- **Every search hit carries a `standing`**, the graph's own verdict on how
  far it can be relied on: `tombstone`, `superseded` (naming its
  replacement), `contested` (naming the other side of an open conflict),
  `stale`, `canon` (pinned, approved or written by you), `confirmed`, or
  `unverified`. It is read from the note's full edge lists, so no conflict
  hides behind the neighbour cap. The assistant weighs a hit by its standing,
  not just its rank: every capture skill and the MCP search description teach
  it. A pinned note that was later replaced now says so.
- **The pane shows the same verdict.** Search results that need a second look
  get a chip, and the node drawer gets a standing line that links to the note
  that replaces or contests it (`GET /nodes/{id}/standing`).

### Endorsements decide between near-identical notes

- **The one you endorsed ranks first.** When two delivered hits are near
  twins (cosine 0.90 or more), the pinned one goes first, then the more
  trusted one, and between two you endorsed alike, the one endorsed most
  recently. Nothing that isn't a twin moves. Before this, a fresher capture
  date beat every endorsement, and pin against approve was a coin flip. On the
  new `engram-eval --authority` bench, endorsed-first rises from 0.54 to 1.00
  with the control unchanged; recall on the post-tune ladder is identical at
  100, 500 and 1500 notes. Graph settings → *endorsed twins ≥*
  (`policy.twin_trust_order`, `null` = off).

### Undo from the audit log

- **Open an audit row and undo it**: the row, its whole operation, or
  everything that session wrote. Every undo shows a dry-run preview you
  confirm in place, is journaled itself, and can be undone again. An entity
  changed since is skipped and named, not overwritten. Undoing a created
  note removes it without a tombstone; undoing an edit restores the before
  snapshot; a delete comes back with its links. Rows written before this
  release are undoable too. Undo is user-only, in the pane and over HTTP
  (`POST /audit/{seq}/undo`, `/audit/sessions/{sid}/undo`, `?dry_run`),
  never over MCP.
- **Audit badges line up.** The badge holds the verb (CREATED, STARTED,
  SERVED); a long action's subject (MCP session, history, brief) moves to
  the line below. Only the display changed: the journal and the API keep
  the original action names.

### Steadier under load

- **A write no longer holds the core while the contradiction judge runs.**
  Duplicate, suspect and canon pairs are gathered under the lock, judged
  with it released, then re-checked and queued. On a slow machine or with
  Laya selected, other sessions stop waiting behind one write. Each write
  also reads its neighbourhood once instead of four times.
- **An open MCP session follows a wiped or replaced store** instead of
  writing to the one it captured at start.
- **The weak line is reproducible.** Its calibration probes were sampled in
  id order, and ids are minted from the clock, so the same graph fitted a
  slightly different line on every run. They are sampled by content now.

### Smaller things

- **`doctor` understands the Claude Code plugin:** a plugin-wired repo with
  no `.mcp.json` entry is healthy, and an entry beside the plugin warns that
  the tools load twice.
- **Graph settings:** the verb card shows each verb's `inherits` role
  (`inherits · auto` when derived); the brief's canon-order select spans its
  row.
- **Docs:** what redaction covers and how to check it yourself
  (recall-and-capture), the fields it does not scrub (SECURITY.md), and
  what to do when a pinned note is wrong (trust).
- **CI** runs every action on its Node 24 major; the VS Code extension
  publishes through the `ovsx` and `vsce` CLIs.

## v0.9.9

### Contradictions travel along edges

- **A contradiction now reaches the notes that depend on it.** When a new
  note contradicts one your graph already holds, or you link it `replaces` /
  `conflicts-with`, every note that builds on, needs, or holds because of the
  old one is queued for review against the new one, hinted `inherited`.
  This catches the transitive case ("the harbor ingester mirrors the
  sibling's settings" + "the sibling now retries 19 times") that no sentence
  model can see, because the two notes name different subjects. On the
  generated bench it lifts separation from 0.75 to 0.82 AUROC with no extra
  false alarms; replayed on this repo's own graph it queues nothing new.
- **Verbs carry an `inherits` role.** On by default for `builds-on`,
  `needs` and `because`; a graph's ontology can switch it per verb. The pane
  marks `inherited` suspects in their own color, and every capture skill and
  agent instruction block teaches how to judge them.

### A second logic model

- **Laya is selectable as the contradiction judge.** Two new choices in
  Settings → System → Choose models: Laya English int4 (262 MB) and Laya
  multilingual int8 (873 MB, for notes in other languages). The
  DeBERTa-tasksource model stays the default: Laya catches compound
  contradictions far better, but it flags more pairs on real notes and costs
  ~15× more per pair. MobileBERT and nli-deberta-v3-small left the list (an
  existing selection keeps working). The Laya exports are published at
  huggingface.co/techtheist/laya-onnx.
- **Each model in the picker says what it trades.**

### The brief opens with the current cycle

- With version tracking on, the brief's first section lists the open work
  stamped with the working version. Graphs without version tracking get the
  brief they always got.
- **Two new brief settings** (Graph settings → Brief composition, or
  `brief.bodies` / `brief.canon_order` in the config): *note bodies* off
  makes every entry its title alone — worth it when titles are already whole
  claims; *canon order* `connected` ranks each type's section by trust
  weighted by how many live edges a note has, so the knowledge others lean
  on leads. Defaults keep today's brief.

### The Claude Code plugin brings its own MCP server

- **Install the plugin and the `engram` tools are there** — no per-repo
  `.mcp.json` entry any more. The plugin's server is a new bridge mode,
  `engram-alpha mcp --wired-only`: it binds repositories that are already
  Engram projects (a `.engram/` directory, or any folder inside a registered
  project) and never turns a folder into one. Anywhere else it reads the home
  graph and refuses project writes with the fix spelled out (`/engram:setup`).
- `/engram:setup` now marks the repo (`.engram/` + git-ignore) instead of
  writing `.mcp.json`; with the plugin installed, `engram-alpha setup --cli
  claude` skips the project entry and warns about an old one, which would
  load every engram tool twice.
- The launcher finds `engram-alpha` off Claude Code's PATH and refuses a
  binary older than 0.9.9.

### A sweep no longer freezes the core

- The session-start validation, the six-hourly sweep and the pane's *Scan
  now* held the graph's lock for the whole conflict scan. With a slow logic
  model (Laya takes about a minute on a 580-note graph) every request on that
  graph queued behind it until the core stopped answering entirely and MCP
  sessions dropped with "the engram core went away". The scan now takes the
  lock one note at a time and steps aside between notes; *Scan now* runs off
  the async workers.

### Tools take numbers as text

- Every numeric MCP argument (`limit`, `depth`, `max_chars`, `window`,
  `turn`, `confidence`, …) now accepts a numeric string too — `"limit":
  "15"` works the same as `15`. Some agents quote every argument and had
  their calls refused.

### Eval

- `engram-eval --shapes`: fifteen contradiction shapes from KnowledgeDrift v2
  plus chain shapes, judged on titles, on title + body + related notes, and
  with the propagation rule. Receipts under `eval/results/2026-09-24-*`.

## v0.9.8

### Unbound sessions refuse writes

- **A session that fell back to the home graph no longer writes there
  silently.** Some MCP clients advertise roots and never answer them, and
  launch the server from a directory that can't host a project (the
  Windsurf JetBrains plugin does both, from `/` or the home directory).
  Such a session lands in the home graph by fallback — and, as issue #11
  reports, a resumed conversation or an IDE restart spawns a new session
  that lands there again, whatever the conversation remembers binding
  earlier, so captures went to the wrong graph for hours with no signal.
  Now the bridge tells the core which rung of the binding ladder bound the
  session, and a session bound to the home graph *by fallback* refuses
  every write addressed to "this project" with a teaching error: which
  client, why, the one call that binds the session (`brief` with `project`
  set to the workspace's absolute path, or `"home"` to work there on
  purpose), and the registered projects to pick from. Reads keep working
  so the agent can find its workspace; a write aimed at an explicit
  `project` still passes; the default-agent-project rung, a user's own
  setting, stays writable.
- **Every write verdict names the graph it landed in.** `add_note`,
  `update_node`, `link`, `merge_nodes`, `resolve_suspect` and the rest
  carry `project` on their reply — a few tokens that make a misplaced
  write visible on any client, not only the one shape the refusal
  catches. A session on the default-agent-project rung says so once, on
  its first write (`binding_note`), then stays quiet.
- **The brief's first line names the graph.** `# Engram brief — project
  'x' (/path)` or `# Engram brief — the home graph (user-level, no
  project)`, so "am I still bound to the right project?" after a resume
  is one line to read; the fallback hint above it now says writes are
  refused and names the client.
- **The home directory is never a project.** The Windsurf JetBrains
  plugin launches the bridge from `~`, which is writable, so the
  working-directory rung registered it as a project named after the user
  with a second graph under `~/.engram/` beside the home graph — the
  wrong-graph shape in a different coat. `/` and the home directory now
  fall through to the next rung, and the registry refuses them outright.
- **The assistant can configure the graph, on your ask.** Everything the
  pane's Settings can do, the core's HTTP API always could — but no
  assistant knew. `GET /guide` on the core now serves the operator's
  manual: the config document and its rules (whole-document `PUT /config`,
  nothing in use is ever dropped, rename routes migrate data), history
  recording and its harness switches, custom fields, presets and the
  ontology's roles, the brief's composition, every policy knob with the
  caveats that matter, version tracking, model swaps, encryption, the
  default agent project, the project registry, the checkup sweeps, and
  what to review with the user before a digest. The MCP instructions, all
  three skill variants, the `AGENTS.md` block and the cold-start brief
  point there: read it when the user asks to change how memory behaves,
  and once on a cold start to choose the shape — a preset, a custom
  ontology, custom fields, the brief's ontology section — before the
  first notes land. Config stays the user's decision; the assistant
  proposes and executes on their word.
- **The generated Windsurf rule verifies the binding.** It now tells
  Cascade to read the brief's first line after every `brief`, to expect a
  new session after any resume, IDE restart, or MCP reload, and that a
  refused write means "rebind, then retry" — the same line rides the MCP
  instructions and every skill variant. The session census on `/system`
  shows each session's client and binding rung.

## v0.9.7

### The icon arrives

- **Engram Alpha has an icon.** A purple box, three bars (the "E") cut on
  a diagonal by a lighter face (the "A"). The pane's top bar shows it as
  drawn on the brand theme; the IDE skins draw the boxless mark inside a
  chip in their own accent color, the bars black and slightly translucent,
  the face white. System info opens on the 256px icon, the black-box
  variant on light IDE themes. The browser favicon is the boxless mark,
  its face following the tab bar's color scheme, and the page title reads
  "Engram Alpha - AI Memory". The README carries the 1024px render under
  its badges.
- **The plugins wear it too.** The JetBrains tool-window icon (16 and
  20px, light and dark) and the VS Code view icon are the monochrome mark
  on a padded viewBox; both marketplace icons are rendered from the same
  1024px source. Both plugins now call themselves **Engram Alpha**
  everywhere they show a name — plugin listing, tool window, editor tab,
  sidebar container, settings section, status bar. The JetBrains tool
  window's id changed with it, so its saved position resets once after
  the update.
- **The gear menu reaches its end on phones.** Its height was capped
  against `100vh`, which on mobile browsers includes the space under the
  toolbar, so the menu's tail sat off-screen and could not be scrolled to
  (iOS Chrome). The cap now uses the dynamic viewport height.
- **Screenshots regenerated** from the demo build with the new pane; the
  three full-pane shots (standalone, VS Code, JetBrains) retaken by hand
  from a real graph.

## v0.9.6

### The bench spoke lowercase, and the queue learned to listen

- **The title-contradiction path reads lowercase subjects.** 0.9.5's
  guard took capitalised tokens as the only evidence of a subject, so on a
  graph whose subjects are plain words ("amber harbor lease broker") it
  never nominated anything — KnowledgeDrift v2's shared-vocabulary worlds
  dropped tier-1 contradiction recall from 0.94 to 0.04 and drift from 91%
  to 47%. Names are now read from a title's first clause, and when neither
  title carries one the subject is the longest run of content words both
  first clauses share, refused when a differing word qualifies it on
  either side ("hazel bayou assay station" is not "wheat dale assay
  station"). On that unnamed path the logic layer reads the titles twice,
  whole and cut to their first clause, and the stronger contradiction
  counts. Replayed on two real graphs before shipping (`engram-eval
  --sweep-replay`, below): zero new nominations on either, and of the
  dogfood graph's 87 human-dismissed sub-floor pairs the rule would queue
  3 where 0.9.5 queued 4. Measured on the KnowledgeDrift v2 ladder (its
  repository's `results/v2/`), against the 0.9.5 rows on the same worlds:
  contradiction 46% → **70%** (tier-1 recall 0.04 → 0.79 over three
  seeds; the `historical` trap now costs a 0.05 false alarm), drift 44% →
  **91%** on every seed at 500 and 48% → 93% at 1500; score 739 → **816
  (813–818)** at 500 and 718 → **801** at 1500, success 70% → 73% and 66%
  → 69%. Retrieval, currency, rationale, temporal and the token columns
  are unchanged to the digit; the v1 world holds (85% → 86%, 511 → 518,
  its deletion family 90% → 100%, everything else within a point).
- **Re-adding a buried note word for word always warns.** The write path
  now matches a new title against every live tombstone's victim title
  beside the vector neighbourhood, so a `tombstoned` warning no longer
  depends on where the marker's vector landed: KnowledgeDrift's
  `resurrection_warned` 0.81 → 1.00 at 100 notes. A deeper vector scan
  was tried first and rejected — it warned rewrites of purged notes about
  other subjects' markers. The rung at 500 then showed the other way a
  resurrection slips through: the write was absorbed as a near-duplicate
  of an *unrelated* same-type note at 0.90 cosine (shared boilerplate
  bodies), so no warning ever ran. **The duplicate match now refuses a
  candidate whose title demonstrably names a different thing** — names on
  both sides with none shared, or a shared component qualified by a
  differing word ("hazel quarry edge cache" is not "wheat geyser edge
  cache"); undecidable titles keep matching on the vectors.
- **`engram-eval --sweep-replay PATH`** replays the conflict sweep on a
  copy of a real graph and prints every pair the shipped nomination rule
  would newly queue, plus how the current title guard reads the graph's
  own dismissed history — the check the 0.9.4 caution demanded before any
  change to what enters the suspect queue, as one command.
- **Two things measured and left alone.** The calibrated "likely not in
  memory" line hedges 40% of answerable questions on KnowledgeDrift v2,
  but a transcript of the whole world shows oblique answers and natural
  nulls scoring the same on the top hit (q50 0.54 vs 0.64): no line
  separates them, and every lower line costs more false answers than it
  buys signal. The line stays; `ENGRAM_WEAK_LINE_DEBUG=1` now prints both
  probe families of the fit. And the crossed phrasing (no content word
  shared with the note) is a cross-encoder ceiling, not a fusion setting:
  keyword weight 0.15 → 0 moves it 0.08 → 0.10 at 100 notes, the vote and
  the semantic floor move nothing.
- **KnowledgeDrift left the tree.** The in-tree `knowledgedrift` crate,
  its adapters and write-up are gone; `eval/knowledgedrift/README.md` is
  now a pointer carrying the v2 ladder table, and the benchmark, its
  frozen worlds, adapters and every receipt live in
  [their own repository](https://github.com/techtheist/knowledgedrift),
  where engram-core is a rev-pinned dependency behind `--features arms`.

## v0.9.5

### The bench fixed the product, then became a product

- **KnowledgeDrift** (`eval/knowledgedrift/`): an offline, judge-free
  benchmark for AI memory in software development — one seeded, invented
  project poured through a ten-operation protocol, then questioned,
  re-decided, contradicted, deleted and asked again across eight task
  families (retrieval with 10% stale-sibling pollution, abstention,
  currency and lineage, a three-tier contradiction ladder with negatives,
  drift, deletion honesty and resurrection, rationale, temporal). Success
  = passed over every task posed (a family a system cannot attempt counts
  as failed), beside a macro composite and an attention-multiplied score.
  Six in-process arms and two external adapters (Mem0, LangMem) on the
  official 500/1500 ladder: engram 85% / 80% (score 511 / 459), LangMem
  63% / 57% (51 / 52), Mem0 58% / 55% (45 / 47), grep 53% / 51%; three
  seeds at 500 put every arm within about a point; `--pollution-shape
  twin|late` shows no ranker separates a hint-less stale twin
  (`stale_above` 0.64) while the drift queue notices 98% of them.
  `eval/knowledgedrift/README.md` is the internals, `WRITEUP.md` the
  reading, and the benchmark now lives as its own repository —
  [KnowledgeDrift v1](https://github.com/techtheist/knowledgedrift) — with
  the worlds frozen, the grader dependency-free, and adapters and results
  accepted by pull request.
- **Contradictions queue on their titles.** The suspect scan had one way
  in: two notes at or above the similarity floor. Similarity cannot tell a
  contradiction from an agreeing restatement, so a note that flatly
  contradicted another in different words never reached the judge. Below
  the floor the logic layer now reads the two bare titles, and a confident
  contradiction between titles that name the same subject and share the
  claim's own words queues too (`policy.conflict_nli_gate`, default 0.80,
  `null` = off; the pane's Graph settings carry it as "title contradiction",
  0 = off). Measured on KnowledgeDrift at 500 notes: the contradiction
  family 58% → 75% (tier-1 recall 0.06 → 0.94), drifted siblings noticed
  53% → 91%, false alarms only on "this is history" traps — and, re-measured
  against the 56 false alarms a first, looser version of the rule raised on
  this repository's own graph in one sweep, 4 of 56. Auto-tune's similarity
  dial ignores what this path raised.

## v0.9.4

### Hits name their role, code refs are yours, and the bench grew three ways

- **Search hits carry the tombstone role.** A hit that plays the graph's
  tombstone role now carries `tombstone: true`, on REST as well as MCP, so
  "Removed: X" is never mistaken for a memory of X — whatever the type is
  called in a custom ontology. Absent when false.
- **A zero knee cliff is refused.** `policy.knee_cliff: 0` was accepted and
  meant the harshest trim, not off. The config refuses it and names `null`
  as the off switch.
- **Code refs are editable in the pane.** One ref per line in the node
  drawer and the create panel; an empty field clears them. Checkup lists
  notes with drifted refs and opens them.
- **ForgetEval, per family and with a "no".** The judge-free forgetting
  benchmark (arXiv:2606.15903) runs against engram over HTTP and is
  reported per family with two readers — the tombstone marker is meant to
  stay findable, so decay and amnesia read 0% role-blind and 100% / 92%
  role-aware, the rest at 99–100%. Three never-inscribed control probes per
  case add what the benchmark lacks: engram declines all 3,000 where the
  reference answers all, and its scores separate real from never-inscribed
  at 0.80–0.90. `eval/forgeteval/README.md` has the honest reading.
- **The ladder learns history and collisions.** `--history` puts every note
  in a session with a turn index and scores an answer reachable through a
  delivered session-mate (`reach@5`): worth +0.07 at depth five for rag and
  engram alike at 1,500 notes, a tie at a ninth of the tokens.
  `--collision R` mints same-subject colliders; at 0.5 both retrievers lose
  about three points, false positives unchanged. Both flags are
  byte-identical when off, and 0.9.4 reproduces 0.9.0 to the digit.
- **The rake test** (`eval/rake/`): does an agent *act* on what memory
  says? Three arms, three tasks, executable oracles on the diff; phase 2
  separates the arms only when the knowledge is memory-only.
- Every receipt in `eval/results/` now carries its run date.

## v0.9.3

### The pane fits any width

- **Six widths on one page.** The pane was re-checked at 157, 214, 318, 542,
  613 and 794 px — smartphones, and IDE side panels dragged thin. Under
  250 px the view switch alone outgrew the top bar and pushed the burger
  *and the gear* — the only path to Settings — off the edge; the bar now
  wraps (icons on row one, switch on row two) and every screen starts below
  it. Segments tighten under 300 px, the minimap yields to the zoom controls
  under 260 px, history's harness chips stop breaking mid-word, and the
  health strip wraps to two rows instead of running under the minimap.
- **Swipe to close.** On touch screens a horizontal swipe on a drawer's
  header toward its own edge dismisses it (left drawer ←, right drawer →).
  Mouse and pen are ignored on purpose; taps still reach the header's
  buttons.
- **Graph settings no longer crashes the pane** when a policy knob is
  missing from the config the core sent (the demo shipped without
  `session_diversity_demote`; a pane newer than its core is the same
  pairing). The number input renders the floor instead of throwing, and the
  half-rendered drawer that could not be closed is gone with it.
- **Two boot and navigation glitches.** "No memory yet" flashed over a
  graph that was merely still loading; and switching Feed → History carried
  the feed's centered card along as a detail drawer over the session lanes.
- **Undefined tokens fixed.** Three inputs (tombstone reason, custom-field
  control, built-in-fields reference) referenced `--border` and
  `--bg-primary`, which no theme defines, and rendered borderless and
  transparent. Hard-coded reds and an overlay literal now read theme tokens;
  dead fallbacks and two unreachable rules were removed. The breakpoint
  table and the z-index inventory now live at the top of `app.css`.
- **Demo badge per screen, CSS only.** Graph keeps it clear of the minimap;
  feed and history sit it in the corner; a narrow feed rides it just above
  the action bar with CSS anchor positioning, falling back to the corner
  where anchors are unsupported or no card is centered.

### Docs

- **README rewritten lighter**: the "why a graph" section is shared verbatim
  with the VS Code listing, *Measured, not promised* moves below the feature
  tour and shrinks to six bullets, and *Stack* names the three stacks — the
  Rust core on TepinDB, the Vue pane, the IDE hosts. The JetBrains README's
  Windows line now shows the PowerShell installer.

## v0.9.2

### Tombstones guard the write path

- **A `tombstoned` write warning.** A Tombstone was findable but guarded
  nothing: it is active (so never `superseded`), it sits out the conflict
  scan (so never `in-active-conflict`), and the near-duplicate match is
  same-type, so a Decision re-deriving a tombstoned Decision sailed through.
  Every write — `add_note`, `add_notes`, `update_node`, `merge_nodes` — now
  reports any tombstone-role node within the warning similarity, across
  types, as `{reason: "tombstoned", note: <why it was removed>}`. The write
  is never blocked: models nominate, people judge. Superseded means *a
  successor exists, follow it*; tombstoned means *re-adding this is the
  error* — the server instructions, tool descriptions, and all three skill
  variants teach the difference.
- **The delete mint carries the buried content.** The Tombstone a hard
  delete leaves used to embed only the victim's title plus your reason, so a
  paraphrase of the removed *content* landed nowhere near it. It now carries
  the victim's body (`**Removed text:**`), tags, and code refs by default;
  the pane's delete confirm gains **Keep the removed text** (untick = the
  purge shape, identity and reason only; `keep_text=false` on
  `DELETE /nodes/{id}`). The victim's `about` edges move onto the Tombstone
  — same edge id, the connection moved — so the removal stays attached to
  the code subject it happened under. Every other edge cascades as before.
- **Search names its tombstone hits.** A `search` reply with a Tombstone
  among its hits carries a `tombstone_note`, so a compact scan never reads
  "Removed: X" as a memory of X.
- **`check_claim` gains a `retracted` bucket.** Tombstone-role hits are
  sorted out by role before the NLI runs ("Removed: X" against "X" is not a
  pair the model reads reliably) and reported as *retracted*: a person
  deliberately removed this — don't act on the claim. The pane's claim check
  renders the group; the write-time canon check skips tombstones since the
  warning already covers them.
- **Tombstones queue no suspects.** The write-time suspect recorder now
  skips tombstone-role nodes on both sides, as the sweep always did — a
  Tombstone resembles its victim by design, and a suspect between them
  invited a `replaces` verdict that would have archived the marker.
- **The assistant can bury, traceably.** No new tool: `add_note` a
  Tombstone titled `Removed: <title>` with the why, then `link` it
  `replaces` the victim — the victim is archived behind the marker and a
  later resurrection warns on both records. Hard delete stays user-only.
  Docs corrected where they implied the delete mint links its victim (it
  cannot — the victim is gone; the identity lives in the body).

### Demo, screenshots, marketplace

- **The demo caught up.** Its delete mints a Tombstone the way the daemon
  does (reason, kept text, `about` edges rehomed), the seed gains a second
  Tombstone — a scroll-mode prototype the assistant buried — that the
  canned claim check files under *retracted*, and the System panel reports
  0.9.2.
- **Screenshots regenerated** from the demo, plus a new one of the delete
  confirm with its Tombstone options (17 automated shots now).
- **VS Code README rewritten for the Marketplace listing**: what Engram is
  and why a graph, what the extension adds, a four-step setup including
  Windows, and the MCP snippet corrected — the extension's *Configure MCP*
  command wrote `--db .engram/graph.db`, a path that has not existed since
  the TepinDB cutover; it now writes the same db-less `["mcp"]` entry
  `engram-alpha setup` does, and the bridge binds the project's
  `graph.tepin` itself.

## v0.9.1

### The pane stops looking foreign

- **Every dropdown is ours now.** All ten native `<select>` elements in the
  pane are replaced by a `SelectMenu` component — a styled trigger with a
  body-teleported listbox, keyboard navigable, flipping upward when it runs
  out of room below. The native popup is unstyleable and looked like a
  different application on Windows; nothing about the pane's chrome should
  depend on which OS is rendering it.
- **The ontology type card holds its shape at narrow widths.** The
  Durability / Rank prior row wraps instead of pushing the stepper out of
  the card, and the brief **cap** and **excerpt** steppers moved onto a row
  of their own so they never trail the end of a wrapped line of role
  toggles.

### Bob runs the brief

- **`setup --cli bob` installs a lifecycle hook.** Bob IDE 2.0.2 and
  BobShell 2.0.1 run Claude-Code-shaped lifecycle hooks out of
  `.bob/settings.json`, and a SessionStart hook's plain stdout becomes model
  context — no envelope, unlike Devin. Setup now writes the portable brief
  script under `.bob/hooks/` and registers it (`sh`-prefixed,
  workspace-relative, explicit 30s timeout), so a Bob session opens with the
  project's canon already in hand. A settings file Engram didn't write is
  never edited: the snippet is printed for you to paste, as with every other
  harness. `docs/getting-started.md` gains the row and the support matrix
  the upgrade.

### Documentation screenshots regenerate themselves

- **`scripts/screenshots.sh`.** The images under `.screenshots/` are now
  produced by driving the browser demo with headless Chromium:
  `frontend/scripts/shots/shots.mjs` is the manifest (one entry per PNG —
  viewport, seeded theme and layout, the click sequence that stages it, and
  what to clip), `run.mjs` the driver. Reproducibility rests on a frozen
  clock (the demo resolves its relative ages at load), layouts that are
  deterministic by construction, and motion switched off before the shutter.
  Nine shots regenerate and seven are new — node detail, the ontology type
  card, custom fields, the Memory Lens brief, the System panel, the history
  screen, and a claim check with its receipts — while the standalone,
  VS Code and JetBrains shots stay hand-made. The feature shots now show the
  demo's invented Lantern graph instead of this repository's own working
  notes: every image in the docs is one a reader can reproduce at
  <https://techtheist.github.io/engram/demo/>.
- **The demo caught up with 0.9.0.** Staging the shots showed the demo
  lagging the product: its graph config declared no custom fields (it now
  ships an `impact` enum and a `tracker` url, with values on three notes),
  no note carried the Tombstone type, and the System fixture still reported
  0.8.1. The Pages demo now demonstrates what 0.9.0 shipped.

### Fixes and hardening

- **Calendar arithmetic can't wrap.** Month and year shifts in `timespec`
  are checked: an absurd count (`"600000 months ago"`) reads as "not a
  time" and errors, where it could previously produce a wrapped instant and
  silently search the wrong window.
- **The seal key file is born 0600.** It is created with its permissions
  rather than `chmod`'d after the fact, closing the window where a
  freshly-written key sat readable.
- **A documentation sweep against the code.** Thirteen fixes across the
  README, `SECURITY.md`, `docs/{getting-started,storage,multi-project}.md`
  and the plugin README: `graph.db` as the default store (it is
  `graph.tepin`), `home.db` → `home.tepin`, the installer described as
  wiring repositories (it stops after fetching the binary since 0.9.0), the
  eight node types (nine since Tombstone), stale agent rosters, the CORS
  allowlist listed as a gap when it shipped, and the plugin README's binary
  name. `docs/pane.md` also gains a section for the session-history screen,
  the one pane screen that had none.

## v0.9.0

### Deletion leaves a trace

- **Tombstones (new `tombstone` type role).** Knowledge that is deliberately
  removed can now be *recorded as removed* instead of vanishing: the default
  ontology gains a ninth type, **Tombstone** ("deliberately removed — don't
  resurrect it"), the research preset gains **Retraction**, and any custom
  type can carry the role. Tombstone-role types sit out the conflict scan
  and answer candidacy, never decay (stable), and a tombstone `replaces` its
  victim through the ordinary supersession machinery when the victim still
  exists.
- **Hard delete can mint one.** The pane's delete now offers "leave a
  Tombstone" (on by default, with an optional reason) —
  `DELETE /nodes/{id}?tombstone=true&reason=…` records the victim's type,
  title, and id in a tombstone before the cascade, so a future session
  can't innocently re-learn what a person deliberately killed. Plain delete
  is unchanged, and graphs whose ontology declares no tombstone type keep
  the old behavior exactly.
- **A fourth preset: General.** An open experimental set for saving almost
  anything — Fact, Note, Idea, Task, Reference, Tombstone — designed to
  pair with 0.9.0's custom fields.

### Custom fields

- **Your own first-class values on every note.** A graph can now declare
  custom fields — `text` / `number` / `bool` / `date` / `enum` / `url`,
  required or optional, scoped to some types or all — and every note
  carries them in a `fields` map beside `tags` and `created_at`.
  Definitions live in the graph config (pane **Settings → Graph settings →
  Custom fields**, or `PUT /config`); values are validated at the engine
  write boundary, so the pane, HTTP, and MCP all get the same contract.
  Reserved built-in names (`title`, `created_at`, …) can never be shadowed.
- **The assistant teaches itself.** `describe_ontology` lists the graph's
  field definitions with kinds and enum vocabularies; `add_note`/
  `update_node` accept a `fields` object (update MERGES — `null` deletes a
  key); and a refused write answers with a teaching error naming the full
  roster and exact call shape, so an agent that has never seen the graph
  self-corrects in one step.
- **Optional search integration.** A field flagged `indexed` joins the
  note's embedding and the keyword channel (tepin-backed graphs); the
  indexed set is fingerprinted, so changing it re-indexes and re-embeds
  exactly once. `show_in_brief` renders `name: value` on brief lines.
- **Bitemporal search.** `date_field` on `search` (and `GET /search`) aims
  the existing `after`/`before` grammar at a date-kind custom field — the
  EVENT clock — instead of `created_at`, the capture clock. A single field
  (`"event_date"`) matches values inside the window; a `"from..to"` pair
  (`"effective_from..effective_to"`) matches nodes whose validity span
  overlaps it, with absent ends reading as unbounded. Built for historic
  imports, where everything is captured today but happened over years; an
  unknown or non-date field, or a clock without a window, is a loud error,
  never a dropped filter.
- **Renames carry data.** `POST /config/rename-field` (and the pane's
  rename gesture) moves every stored value with the definition, like
  renaming a type.

### At-rest encryption, two switches

- **Encrypt graph / Encrypt history** (System panel): field-level
  zstd→XChaCha20-Poly1305 over titles, bodies, tags, code refs, custom
  field values, edge notes, and the audit journal's before/after images,
  under the machine key in your OS keystore. Graph defaults **off** (a
  sealed graph costs `npx tepindb` inspectability), history defaults **on**
  — formalizing what the daemon has done since 0.8.4. Flipping a switch
  migrates the whole store with a progress loader
  (`POST /encryption` + polled `GET /encryption`); a migration killed
  halfway is crash-safe and resumes at the next open.
- **The store self-describes.** Each store records its actual encryption
  state in its OWN meta (`plaintext | sealing | sealed | unsealing`), so
  the daemon can never desync from the `.tepin` file; the settings file
  only carries your *intent*, and every store converges at open. The old
  daemon-internal history opt-in (`enable_history_sealing`) is gone —
  legacy sealed history stores are recognized and finished automatically.
- **BM25 is identical either way — and history finally has it.** The
  keyword index is now a *blind index*: one synthetic `_kw` token stream
  per node, identity tokens on a plaintext store and keyed HMAC-SHA256
  digests on a sealed one. Term frequencies and document lengths — all
  BM25 reads — are preserved exactly (a golden test asserts bit-identical
  rankings), while a sealed index holds no vocabulary. The same channel
  gives the history layer its first keyword search, fused with vector
  candidates using the curated layer's own weights.
- **Honest scope, unchanged:** embedding vectors stay open (documented
  inversion risk — gist, not text), exports are plaintext by intent, and
  redaction still runs before anything reaches either store. SECURITY.md
  carries the full rewritten two-layer table.
- **The receipt:** the full eval ladder (10 → 1500 notes, real embeddings)
  re-measured on the `_kw` keyword path reproduces the 0.8.10 note-register
  baselines **to the digit** at every rung (100: R@5 0.94 / oblique 0.82 /
  198 tok; 1500: R@5 0.78 / oblique 0.36 / 297 tok) —
  `eval/results/0.9.0-ladder-run.log` — and the post-tune stack re-measured
  on 0.9.0 keeps the honest-FP story (FP 0.00 at 100, 0.02 at 1500; recall
  and attention rows match the 0.8.10 posttune receipt) —
  `eval/results/posttune-0.9.0-100-1500.log`. The one honest delta: at 1500
  the self-calibrated weak line fits 0.878 where 0.8.10 fits 0.898 on the
  same day and corpus (honest FP 0.01 → 0.02, i.e. 4 → 7 of 375 controls) —
  the single `_kw` stream normalizes BM25 slightly differently than
  per-field scoring, deep candidates shuffle, and the dial re-fits as
  designed; top-rank metrics are untouched at every rung. Sealed-state
  parity is asserted separately by the bit-identical-BM25 golden test.

### Fixes and hardening

- **The installer stops wiring things unasked.** `install.sh` / `install.ps1`
  used to run auto-detect setup right after fetching the binary — and the
  one-liner is often pasted in `$HOME`, scattering project files and
  touching global assistant configs nobody asked for. Both installers now
  install the binary and stop; passing `--cli`/`-Cli` remains the explicit
  opt-in. The onboarding nudge moved to where the context is right:
  `serve` names exactly what it found and hands you the command —
  *"found codex, gemini — installed but not connected to Engram in this
  repo; wire them: `engram-alpha setup --cli codex,gemini`"*.
- **Windows: config writers escape paths.** `setup` interpolated the binary
  path raw into hand-rolled TOML and JSON — on Windows,
  `command = "C:\Users\…"` is an invalid escape and Codex refused its whole
  `config.toml` (field report). Every writer and printed snippet (codex
  TOML, `.mcp.json`-shaped files, opencode/kilo, devin, bob) now emits
  paths through real string encoders, with a Windows-path round-trip test
  over each shape.
- **A required custom field can no longer veto a hard delete.** The
  tombstone a delete mints is engine-authored: it skips required-field
  enforcement (values, when present, are still validated) — a
  required-on-all-types field used to make "delete with tombstone" error
  out. An *authored* Tombstone note still owes its required fields.
- **Test depth on the seams.** ~20 new tests where 0.9.0 meets the old
  machinery: write verdicts, suspects, brief, merge, timeline, redaction,
  and delete-with-tombstone over a *sealed* store; indexed fields and the
  bitemporal clock through the blind index; desired-encryption reconcile at
  open plus keyless reads (placeholders, never plaintext, never an error);
  imports into a sealed store; pre-0.9 export and config shapes; the
  version handshake's unsuccessful branches (unparsable/dev versions never
  fight, corrupt or stale `daemon.json` never blocks serve) as unit tests
  and two new e2e journeys.

## v0.8.13

### Locks that actually release — and write tools a small model can drive

- **A deleted store no longer haunts the core (issue #8).** The core's engine
  cache is now identity-checked: every cached open re-stats its backing file
  (device + inode), and a store that was deleted or replaced on disk —
  a wiped `.engram/` — is evicted and reopened fresh instead of being served
  from the orphaned inode forever. Deleting a project's `.engram/` and
  running `serve` now creates a fresh graph without recycling the core.
- **`stop` means stopped.** `engram-alpha stop` now waits for the core
  *process* to exit (PID-verified), not just for `/health` to go silent —
  the store locks release at process exit, and the old health-only check
  could report success while the core still held every lock behind a wedged
  engine. Escalation is orchestrated `/shutdown` → SIGTERM → SIGKILL (both
  backends are crash-safe), with a warning if the port is still occupied
  afterwards. The core's own exit path was reordered to match: engines drain
  (bounded, 10s) *before* the listener closes, so health-silence now means
  "about to exit", never "draining indefinitely".
- **Version handshake: an update reaches the core on the next touch.** Every
  `serve`, bridge bind/rebind, and `core` start now compares the running
  core's advertised version against its own binary. An older core is retired
  (orchestrated stop) and respawned from the newer binary; a newer core is
  converged on, never downgraded. This closes the issue #8 tail where a
  client relaunching its bridges after an update could resurrect — or keep
  converging on — a core from the old binary, serving stale code and stale
  tool descriptions indefinitely. `bind_or_converge` also probes a taken
  port three times before walking past it, so a busy core is not mistaken
  for a foreign process (which used to open a second core over the same
  home graph).
- **The write tools teach their own shapes (issue #9).** `add_notes` /
  `update_nodes` items are now inlined in the wire schema — previously they
  collapsed to a bare `$ref` a small model never chased, hiding every field
  name and the required list. The two required `type` fields (node type,
  link verb) now carry their full vocabulary in the schema itself, unknown
  fields on write tools are rejected loudly (an inline `links` array used to
  be silently dropped), and the server instructions were cut from 4.1KB of
  mechanism essays to the contract: the 8 node types, the 7 verbs, and the
  exact call shapes, including "add_notes carries no links — link the
  returned ids in a second pass".
- **Skills show real calls.** The capture skills' examples were positional
  pseudo-syntax (`link(a, b, answers)`) that small models imitated verbatim
  into invalid calls; they now show exact keyword JSON, name the 8 types in
  one line, and include a worked `add_notes` + second-pass `link` example.
  The digest skill's "batch into add_notes… link as you go" contradiction is
  resolved the same way, it now stops early on a near-empty tree instead of
  inflating, and an empty graph's cold-start brief appends the graph's
  actual ontology — the one channel that reaches a model with no skill
  loaded. The Devin/Codex/Windsurf `AGENTS.md` block gained the full type
  roster (Anchor was missing), `describe_ontology`, and the same call
  shapes.

## v0.8.12

### serve means it — and Codex sessions start pre-briefed

- **`serve` eagerly opens the store it registers (issue #8).** Registration
  alone left a fresh repo's `.engram/graph.tepin` uncreated until something
  touched the project — so SessionStart hooks that gate on the store file
  existing injected nothing, silently. After registering, `serve` now makes
  one loopback `GET /brief?project=<root>` so the core opens (and thereby
  creates) the store immediately, verifies the file is on disk, and prints
  `store ready: <path>` — or a warning when it isn't. The brief hook also
  gained defense in depth: a repo is recognized on its `.engram/daemon.json`
  alone (registered but not yet materialized — the daemon query creates the
  store on demand), while the CLI direct-open fallback stays gated on an
  existing store so the hook can never birth a fresh graph itself.
- **The wiring footer confirms before it nags.** `serve` now prints
  `engram wired for: …` by scanning the full adapter roster (wiring is
  file evidence, not a PATH probe — a repo wired for Devin CLI used to read
  as unwired when the binary wasn't detectable), and the "run
  `engram-alpha setup`" nudge only covers assistants that are detected AND
  unwired.
- **Codex CLI: skills + an auto-injected brief.** `setup --cli codex` now
  also installs the capture + digest skills to `.codex/skills/` and a
  `SessionStart` brief hook (`.codex/hooks.json` + scripts under
  `.codex/hooks/`). Codex injects hook context only via the
  `hookSpecificOutput` JSON envelope — the same contract as Devin CLI — so
  both share one wrapper (`hooks/envelope-session-brief.sh`), which now also
  resolves the repo root via `git rev-parse` since codex sessions can start
  in a subdirectory. Codex trust-gates project hooks per definition hash:
  run `/hooks` inside codex once to approve the engram hook (setup says so).
  Foreign `hooks.json` files get the merge snippet printed, never rewritten;
  `--mcp-only` skips all of it.

## v0.8.11

### One tool binds and briefs — and Windsurf and Devin CLI get first-class wiring

- **`brief` absorbs `set_project` (24 tools, was 25).** Passing `project` to
  `brief` — a name, id, or any absolute path inside a registered root — now
  REBINDS the session to that project and returns its brief in one call,
  exactly what 0.8.9's separate `set_project` did. One tool means one
  instruction: the field report on issue #4 showed the injected "call
  `brief` first" guidance overruling a workspace rule that said
  `set_project` first — with the merge there is no wrong first call left to
  make. The server instructions, the `brief` description, and the fallback
  hint atop a fallback-bound session's brief all teach the same gesture; a
  refused selector (unknowns are listed with the known roster) leaves the
  binding untouched.
- **Windsurf project wiring (issue #7).** `setup --cli windsurf` now also
  writes `.windsurf/rules/engram.md` — an `always_on` rule whose critical
  first instruction is the brief-rebind (Windsurf has no session hooks and
  answers no MCP roots) — and installs the capture + digest skills under
  `.windsurf/skills/`, both honoring `--skill` and skipped by `--mcp-only`.
  The global config merge and the `AGENTS.md` block are unchanged.
- **Devin CLI adapter (issue #8).** `setup --cli devin` wires the repo
  three ways: `.devin/mcp_config.local.json` (Devin's highest-precedence,
  per-project tier — explicit `--db`, and git-ignored since it carries
  personal absolute paths), skills under `.devin/skills/`, and a
  `SessionStart` hook registered in `.devin/hooks.v1.json` that injects the
  brief. Devin accepts injected context only as a `hookSpecificOutput`
  JSON envelope — plain stdout is dropped — so the hook wraps the portable
  brief script's markdown in that envelope (escaping verified by test).
  `devin` joins auto-detection (PATH probe) and doctor's wiring report.

## v0.8.10

### Search mixes sessions — and tool replies stop speaking in epochs

- **Session-diverse delivery, on by default.** When more strong candidates
  survive the calibrated floor than fit the result list, each additional hit
  from a session already holding a slot is demoted a few rank positions at
  the final cut — so one session's restatements stop crowding out
  complementary evidence captured in other sessions, on both `scope:
  "memory"` and `scope:"history"`. The demotion is rank-based on purpose
  (absolute score thresholds don't transfer between graphs; relative
  mechanisms do), it changes selection and order only — scores, the floor,
  the knee and the confidence verdict still read the cross-encoder's
  calibrated scale — and a graph with one session (or none recorded) is
  byte-identical to before. Per-graph knob: `policy.session_diversity_demote`
  (0 disables); the default is the value measured recall-free by the new
  bench. Search hits now carry `session_id`, so provenance is visible where
  the diversity acts.
- **MCP tool replies are tidied for the model reading them.** Every reply is
  per-session context on every client (the 0.8.9 tool-description lesson,
  applied to payloads): unix timestamps now render as ISO instants
  (`"2026-08-22T14:36:43Z"` — paste one straight back into `after`/
  `before`), and `null` fields, empty arrays and empty objects are pruned
  recursively. MCP replies only — the pane, the HTTP API and exports keep
  raw shapes.
- **The eval workbench learned sessions.** `--sessions` benches the
  diversity knob: multi-session subjects (one complementary aspect per
  session plus same-session recaps) asked as one aggregation question,
  session coverage as the gain and the regular single-gold questions
  re-asked under the same knob as the price. `--longmemeval` gains
  `--lme-turns N` — a per-question ingestion budget (~50 turns instead of
  the full ~494) that always keeps the labelled answer sessions, loudly
  marks its receipts, and turns a 16-hour CPU run into a tuning loop — plus
  a `multi-cov` column: how much of a multi-session question's evidence
  spread the delivered list covers. `--floor` now also prices the dial-three
  candidate (delivery floor fitted per graph from the phantom-probe score
  body) beside the observed-distribution grid, and `--rerank-full` reaches
  the LongMemEval arm. The delivery-floor bake-off those tools ran ended
  with **neither attack shipping**: full-note reranker input wins the note
  register uncut (+0.07 oblique at 1500) but collapses on the chat register
  (R@5 0.96 → 0.78) — register-fragile, it stays a per-graph knob — and the
  dial-three q25 fit is recall-free at ≤500 notes but not at 1500 and
  misfires under full-note input, so the auto-tune dial waits for a
  chat-register fit. Receipts in `eval/results/`, story in the eval README's
  0.8.10 section.

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
