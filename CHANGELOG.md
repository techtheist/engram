# Changelog

Release notes for Engram Alpha. Each release's section below becomes the
body of its GitHub Release (draft-release.yml lifts it automatically).

## v0.8.1 — the knee and the phantom probes

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
