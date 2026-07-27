# Does it notice the disagreement?

The retrieval half of this harness is written up in `README.md`; this is the
other half.

**This work changed the product.** As of 0.7.2 the NLI model is
`mobilebert-uncased-mnli` (27 MB), replacing `nli-deberta-v3-small` (172 MB),
and `check_claim` gained a confidence gate at **0.80** — both on the numbers
below. End to end that took false alarms from 80–86% to 38%, and it was then
checked against this repo's own graph, where the same gate takes them to 19%
and the new model is the only candidate of four that flags the one genuine
contradiction the project has ever recorded.

```sh
cargo run -p engram-eval --features fastembed -- --contradictions

# swap the model and re-run; the report names whatever loaded
ENGRAM_NLI_DIR=~/.cache/engram-nli-eval/mobilebert-mnli \
  cargo run -p engram-eval --features fastembed -- --contradictions

# and against a real graph's judged history (point it at a COPY —
# a running daemon owns the original)
cargo run -p engram-eval --features fastembed -- --real-graph /tmp/graph-copy.tepin
```

## Why this needs its own metric

Every published memory benchmark scores recall. None scores whether a memory
notices that two things it holds disagree. That is the layer Engram runs
locally, on a small cross-encoder, where the rest of the field spends an LLM
call — so if it is worth anything, it has to be measurable, and the measurement
did not exist.

**It scores the pipeline, not the model.** In `check_claim` the NLI model only
ever judges what retrieval handed it, so a claim whose target was never
retrieved is not a judgment the model got wrong — it is one the model never saw.
The report separates those, because one is fixed by better search and the other
by a better model, and confusing them wastes the effort.

**Catch rate alone is meaningless, and this is the whole trap.** A layer that
answers "contradiction" to everything scores 100%. Every number below is a pair:
what it caught, against what it cost. The cost side is measured on claims that
restate *another stored note verbatim* — a statement the graph literally
contains. Any contradiction verdict against one of those is a false alarm, and
every false alarm spends a human judgment on a pair that was never in conflict.

## The corpus

Generated restatements of stored notes, in the same invented-subject register as
the retrieval half: a contradicting version (same subject and parameter, altered
value), an agreeing version, and an unrelated one. 400 claims over a 600-fact
graph, five seeds. Grading needs no judge — the corpus knows which note each
claim restates.

There is a second corpus, small and real, that nobody assembled on purpose: every
pair a human has ever ruled on in **this repo's own graph**, plus every
`conflicts-with` edge in it. It answers the question the generated corpus cannot
— whether any of this survives contact with real project prose — and it is
reported in its own section below.

## What can even be tried

The loader wants `model.onnx`, `tokenizer.json`, and a `config.json` whose
`id2label` covers entailment / neutral / contradiction. That last requirement
disqualifies more of the field than size does.

The most-recommended small zero-shot models — `deberta-v3-xsmall-zeroshot-v1.1`,
`xtremedistil-l6-h256-zeroshot-v1.1` (13 MB!) — are **binary**:
`entailment` / `not_entailment`. They cannot express contradiction at all. For
this task "disagrees" and "is unrelated" are the two answers that matter most,
and a binary model collapses exactly that distinction. Size was never the
constraint; the label scheme is.

A sweep of ONNX zero-shot models by recency found nothing newer than the
DeBERTa-v3 family — as of July 2026 no ModernBERT-based NLI model ships an ONNX
export. So the candidate set is small:

| model | ONNX on disk | backbone | labels |
|---|---|---|---|
| `nli-deberta-v3-small` — *shipped until 0.7.2* | **172 MB** | 44M | 3-way |
| `nli-deberta-v3-xsmall` | 96 MB | 22M | 3-way |
| `distilbert-base-uncased-mnli` | 65 MB | 66M | 3-way |
| `mobilebert-uncased-mnli` — **ships from 0.7.2** | 27 MB | 25M | 3-way |

## Results

Same corpus, same embedder and reranker — only the NLI model swapped. Ungated:

| model | catch | false alarms | agreeing confirmed |
|---|---|---|---|
| `nli-deberta-v3-small` *(replaced)* | 97–99% | **80–86%** | 81–82% |
| `nli-deberta-v3-xsmall` | 96–99% | 68–80% | 80–82% |
| `distilbert-mnli` | 93–95% | 46–56% | 79–83% |
| **`mobilebert-mnli`** *(now default)* | **95–97%** | **57–62%** | 76–80% |

Five seeds each, except `deberta-v3-xsmall` at four. The first pass used two,
which was enough to rank them and — as the threshold section below shows — not
nearly enough to pick an operating point on the winner.

The first benchmark ran against `model_int8.onnx`, but the file the CLI downloads
is `model_quantized.onnx` — a **different file** for this model, 27 MB against
26. Rather than assume equivalence, the whole five-seed sweep was re-run on the
shipping file. The two agree within a point at every gate, and every number
quoted from here on is the shipping file's. Ship what you measured.

**Retrieval never missed once**, for any model, on any seed. Zero of the misses
were retrieval's fault. Better search cannot improve this layer at all; the
entire headroom is the model's.

### The gate is where the models actually separate

Requiring a minimum contradiction confidence before reporting one. A gate can
only ever drop reports, so both rates fall together — the question is whether
false alarms fall *faster*.

Mean catch / mean false alarms, five seeds each (`xsmall` four):

| gate | deberta-v3-small *(replaced)* | **mobilebert** *(ships)* | distilbert | deberta-v3-xsmall |
|---|---|---|---|---|
| 0.00 | 98 / 82 | 96 / 61 | 93 / 50 | 97 / 76 |
| 0.70 | 97 / 76 | 95 / 44 | 93 / 46 | 96 / 66 |
| **0.80** | 96 / 73 | **92 / 38** | 92 / 44 | 96 / 59 |
| 0.90 | 94 / 67 | 85 / 27 | 90 / 41 | 92 / 49 |
| 0.95 | 92 / 62 | 77 / 18 | 87 / 38 | 89 / 40 |
| 0.99 | 77 / 41 | 41 / 4 | 81 / 32 | 67 / 19 |

**The incumbent's curve is flat, and that is the whole case against it.** It
never gets below 62% false alarms at any usable threshold; reaching even 41%
costs 0.99 and a fifth of its catch. It is not *uncertain* about unrelated pairs
— it is confidently wrong about them, and confidence is the only thing a gate
can filter on.

**DistilBERT is the best-behaved and never gets good.** Its curve barely moves —
50% to 32% across the entire range — so it is quiet from the start and cannot be
made quieter.

**MobileBERT's curve is the only steep one.** 61% down to 38% by 0.80, 18% by
0.95, while catch holds. That steepness is what makes a threshold worth choosing
at all; it is also what makes the choice matter, which the next section is about.

### Choosing the threshold, on five seeds

Two seeds are enough to rank models and **not** enough to pick an operating
point. On five seeds of the file that actually ships, MobileBERT's curve reads
like this:

| gate | catch mean (worst) | catch spread | false alarms mean (worst) | gap | agreeing claims wrongly called conflicts |
|---|---|---|---|---|---|
| 0.00 | 96% (95%) | 2 pts | 61% (62%) | 35 | 7–13% |
| 0.50 | 96% (95%) | 1 pt | 56% (59%) | 40 | 6–11% |
| 0.70 | 95% (94%) | 1 pt | 44% (48%) | 50 | 2–6% |
| **0.80 — ships** | **92% (90%)** | **4 pts** | **38% (41%)** | **54** | **1–2%** |
| 0.90 | 85% (79%) | 11 pts | 27% (29%) | 58 | 0–2% |
| 0.95 | 77% (71%) | 13 pts | 18% (24%) | 59 | 0–1% |
| 0.99 | 41% (36%) | 12 pts | 4% (5%) | 38 | 0% |

**The gap column is not the criterion, and this table is why.** It keeps
improving as the gate tightens — 54 at 0.80, 58 at 0.90, 59 at 0.95 — right up
to a point where the layer catches barely seven contradictions in ten and swings
thirteen points between seeds. Optimising it alone lands on a gate that mostly
does not fire.

**Stability is the criterion.** Catch holds within 1–4 points of its mean up to
0.80 and then comes apart: 11 points of spread at 0.90, 13 at 0.95. 0.80 is the
last gate before the cliff.

Note the shape: false-alarm rates are stable across seeds at every gate; it is
**catch** that moves. So the cost side of this trade can be relied on and the
benefit side cannot — another reason to sit where catch is still flat.

**The last column is a second, independent argument for the same number.** An
agreeing restatement called a contradiction is the layer's worst output: not a
misfire at a stranger but an assertion of conflict against the very note the
claim was echoing. Ungated that happens to about one agreeing claim in ten. At
0.80 it is one or two in a hundred. The gate does not merely trade catch for
quiet — it removes the failure that actually damages the graph.

The harness deliberately opens the product's own gate to 0.0 before sweeping,
so it can see beneath the shipped floor. An instrument clamped by the value it
exists to choose can never revise it.

## On a real graph

Everything above is generated prose. This repo's own graph — 297 nodes, about a
month of dogfooding — carries 42 pairs a human has judged and one
`conflicts-with` edge, and `--real-graph` scores exactly those. The pairs come
pre-filtered by the similarity floor that guards the suspect queue, which makes
this the queue's own population rather than `check_claim`'s.

Two filters have to come off the raw count before it means anything. Three of the
42 were queued *because* an earlier model called them a contradiction — the exact
trait being scored. And the sweep skips any pair whose nodes are already linked,
before it ever calls the model, so a flagged pair that carries an edge today is
noise the product would never have produced. Both are reported, because the
difference between them is large:

| gate | all judged (n=40) | unbiased (n=37) | as the queue runs it (n=16) | the one real conflict |
|---|---|---|---|---|
| 0.00 | 65% | 62% | 62% | caught |
| 0.70 | 45% | 41% | 44% | caught |
| **0.80 — ships** | **32%** | **27%** | **19%** | **caught** |
| 0.90 | 20% | 19% | 12% | missed |
| 0.95 | 10% | 8% | 6% | missed |

Two things transfer and one does not.

**The false-alarm rate transfers, and it is better here than on generated prose.**
Ungated it is 62% against the generated corpus's 61% — mushier real notes are no
harder, which is the opposite of what was expected. Gated and with the sweep's own
linkage skip applied it is **19%**, against 38% on the generated corpus. Nine of
the thirteen pairs the model still flags at the shipped gate are already linked,
and the product drops all nine without asking anybody. That is the graph doing
work no sentence model can do: structure the user already recorded, spent as
precision.

**The threshold choice is corroborated independently.** MobileBERT flags the
single real conflict at 0.80 and loses it at 0.90 — the same place the five-seed
catch curve starts swinging. Two unrelated corpora put the cliff between the same
two numbers.

**The catch rate does not transfer, because there is nothing to transfer it
from.** In 297 nodes this project has contradicted itself on the record exactly
once. That is a fact about the workload as much as a limit of the corpus: the
positive class is genuinely rare, which is precisely why the cost side is what
this layer lives or dies on.

That rarity also sets a trap the same run walks into:

| model, same pairs | flagged @0.80, as the queue runs it | the one real conflict |
|---|---|---|
| `nli-deberta-v3-small` *(replaced)* | **19%** | missed at every gate |
| **`mobilebert-mnli`** *(ships)* | **19%** | **caught** |
| `deberta-v3-xsmall` | 38% | missed at every gate |
| `distilbert-mnli` | 44% | missed at every gate |

The incumbent ties for the quietest queue on this page. It is quiet because it
barely fires on real technical prose at all — including on the one pair that
genuinely conflicts, which it misses at *every* threshold, as do both DeBERTas
and DistilBERT. Read the middle column alone and the model being replaced looks
like a draw; read both and it is one of three that never does the job. A corpus
with a positive class of one can rank models by what they cost far more precisely
than by what they catch, and ranking on cost alone would have kept the wrong
model.

### The failure that is left

The pairs MobileBERT still flags are not random. Almost all of them are the same
shape — one note stating a plan, another stating that the plan shipped:

```
0.92  Build the machine-level hub daemon — one daemon owns every registered store
  vs  Hub daemon: one machine-level serve owns all project stores … supersedes
      one-daemon-per-repo at TepinDB cutover

0.92  v0.6.2 released — the machine core + tepindb 0.4.0
  vs  v0.6.2 scope: the machine core — single pane on a single port …

0.99  Verified code refs shipped (2026-07-10): drift scan, worklist, badges
  vs  Verified code refs: check that a node's code_refs still resolve
```

Same subject, same vocabulary, different tense. This is a sharper description of
the problem than the lexical-overlap hypothesis further down, and it points at a
structural fix rather than a better model — which is also why the linkage skip
matters so much: the first and third of those three are already linked in the
graph, so the sweep never raises them. The remaining noise is the same shape,
sitting on pairs nobody has linked yet. Recording the supersession silences it,
and that is maintenance the graph wants anyway.

## The other side: claims that agree

`check_claim` has a third bucket. Roughly a fifth of restatements that plainly
agree with a stored note are not confirmed, and that number sat on the page for a
while with nothing behind it. Splitting it the same way the contradiction rows
are split says what it actually is:

| agreeing claims (5 seeds, 133 each) | |
|---|---|
| confirmed | 76–80% |
| the note was never retrieved | **0, every seed** |
| retrieved, judged neutral | 8–14% |
| retrieved, judged a contradiction — ungated | 7–13% |
| retrieved, judged a contradiction — at the shipped gate | **1–2%** |

Two readings follow, and they point in opposite directions.

**The harmful half is already fixed.** Asserting *conflict* against a note the
claim agrees with is the worst thing this layer can do, and it was happening to
one agreeing claim in ten. The 0.80 gate takes it to one or two in a hundred.

**The rest is the model declining to certify, and it costs nothing.** With the
gate applied, everything else — the 8–14% judged neutral outright, plus the
gated contradictions — lands in `silent`: the model will not say the restatement
follows from the note, on pairs where the note states the answer verbatim. That
is a model limitation, not a pipeline one: retrieval found the note every single
time across five seeds, so there is nothing for better search to fix. And nothing
is lost when it happens. An unconfirmed node is still returned, carrying its raw
entailment and contradiction probabilities. The cost of the missing fifth is a
weaker label on a node that came back anyway.

0.7.2 sorts that bucket strongest-signal-first for the same reason, so the
near-misses sit at the top of it rather than in arrival order.

### Reading it

Every candidate beats what was shipping, and the one that wins is also the
smallest — 25 MB against 172 MB. The likely mechanism, stated as a hypothesis
rather than a finding: MNLI-trained models are known to learn a lexical-overlap
shortcut, and that shortcut is exactly what fires "contradiction" at two
unrelated notes sharing a component word. Whatever MobileBERT's distillation
discarded, it seems to have discarded some of that. This has not been tested and
is not the reason to switch — the numbers are. The real-graph half offers a
sharper version of the same idea: what survives the gate is overwhelmingly
plan-versus-shipped, one tense apart.

An earlier version of this work concluded that *no* confidence gate can fix the
false-alarm problem. That was true of the incumbent and **false as a general
claim**: the gate works fine on a better-calibrated model. The correction
matters, because it moves the fix from "wait for a better model" to "switch the
model and calibrate a threshold."

## Where this leaves the layer

The three numbers worth carrying away, all on the file that ships, at the gate
that ships:

| | |
|---|---|
| contradictions caught | **92%** (five seeds, worst 90%) |
| false alarms, generated prose | 38% |
| false alarms, this repo's real graph, as the queue runs it | **19%** |

The two false-alarm figures measure deliberately different things and the gap
between them is the product working. The generated corpus is adversarial by
construction: it asks about subjects nothing was ever written about, in a
register where every note looks like every other note, and it scores the model
alone. The real graph adds what the model does not have — a similarity floor, and
edges. Nine of the thirteen pairs the model still flags there are already linked,
and the sweep drops every one of them before the model is called.

That is the honest shape of this layer: a small cross-encoder that is wrong about
a fifth to a third of what it flags, wrapped in enough structure that the user
sees a fraction of it. Both halves are load-bearing. Neither is enough alone.

### What is still open

- **The catch rate rests entirely on generated prose.** In 297 nodes this project
  has recorded exactly one contradiction, so the real-graph corpus can validate
  the cost side precisely and the benefit side only anecdotally. MobileBERT
  catches that one case at the shipped gate and the other three candidates miss
  it at every gate — a fact, not a rate.
- **One graph, one project, one register.** The real-prose half is this repo's
  own memory: a Rust codebase's decision log, written by one person under one
  capture skill. It confirms that the generated numbers do not fall apart on real
  text; it cannot say what happens on a different kind of project.
- **The suspect queue's own gate is unchanged, on purpose.**
  `nli_sweep_min_confidence` sits at 0.80, inherited from the incumbent's
  calibration, and the real-graph curve is consistent with keeping it there — the
  next step tighter halves the false alarms and loses the one real conflict. But
  it was checked against sixteen pairs, which is enough to leave a value alone
  and not enough to move one.
- **False `supports` has never been measured**, which is why the gate is
  contradiction-only. A stored note wrongly reported as *agreeing* with a claim
  is a quieter failure than a false conflict, but it is not a harmless one, and
  no corpus here scores it.
