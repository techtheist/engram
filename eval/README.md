# Does it come back?

An offline harness for the only question that matters about a memory layer:
**when something was written down, does it come back — and what did that cost?**

```sh
cargo test -p engram-eval                                      # fast, no models, what CI runs
cargo run -p engram-eval --features fastembed -- --series      # everything below, one command
cargo run -p engram-eval --features fastembed -- --ladder      # the gradation ladder only
cargo run -p engram-eval --features fastembed -- --sizes 500   # the headline table
cargo run -p engram-eval --features fastembed -- --bench       # the strategy grid
cargo run -p engram-eval -- --sizes 50 --sample                # read what it generates
```

Everything below was produced by these commands. Nothing was measured by
watching an agent use the tool.

---

## Result

At 1500 notes shaped like real ones — median 748-character bodies, ~1,590
edges — asked **4,500 questions** with known answers (every stored fact is
questioned since the ladder rework) plus 375 about subjects never written,
on the shipped stack including calibrated delivery:

| arm | standing | tok/query | focus | noise | R@1 | R@5 | lex | para | oblique | FP |
|---|---|---|---|---|---|---|---|---|---|---|
| chance | 0 | 2511 | 0.10 | 1.00 | 0.00 | 0.00 | 0.01 | 0.00 | 0.00 | 1.00 |
| grep | 0 | 2741 | 0.10 | 0.93 | 0.66 | 0.68 | 1.00 | 0.99 | 0.04 | 1.00 |
| rag (pure vectors) | 0 | 2673 | 0.10 | 0.91 | 0.66 | 0.80 | 1.00 | 0.94 | 0.47 | 1.00 |
| **engram** | 3062 | **528** | 0.10 | 0.91 | **0.69** | 0.80 | 1.00 | **1.00** | 0.39 | 1.00 |
| **engram + graph** | 3062 | **528** | 0.10 | 0.91 | 0.69 | **0.81** | 1.00 | 1.00 | 0.42 | 1.00 |
| curated-file 3k | 2928 | 2928 | 0.03 | 1.00 | 0.02 | 0.02 | 0.02 | 0.02 | 0.02 | 1.00 |
| curated-file 30k | 29966 | 29966 | 0.00 | 1.00 | 0.25 | 0.25 | 0.25 | 0.25 | 0.25 | 1.00 |
| whole-file | 377260 | 377260 | 0.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |

Weighted for how often each phrasing actually occurs, the ranking is
**engram 0.94, rag 0.92, grep 0.90** — engram now beats pure vectors on the
headline, not just ties it.

Three of these columns exist because recall alone can be gamed, and they are
the honest part of the table. **focus** — of the tokens delivered, what share
was the answer; everything else is spent attention. **noise** — of the records
delivered, what share was *not* the answer, counting a miss as all-noise and
an empty return as zero, so declining honestly beats guessing. **FP** — asked
about something never written, how often the arm answered anyway.

Four things follow.

**Engram delivers the most recall per token, by about 5×.** It returns a title
and a matched snippet; the flat-file arms return whole records. 528 tokens per
query against rag's 2,673 at the same recall@5 — the one gap on the table that
is an order of magnitude.

**The perfect-recall rows are the cautionary ones.** `whole-file` and the 30k
curated file score 1.00 on recall with the answer at under 1% of the delivered
text and noise at 1.00 — the measured form of *present is not readable*. Recall
and focus have to be read as a pair, and no other memory benchmark publishes
the second number.

**Nothing on this table declines at 1500 notes — but the product now can.**
Every arm answers never-written questions here (FP 1.00): the score populations
overlap too much for a hard floor, which the `--floor` sweep priced exactly. So
the shipped stack trims weak tail hits (at 100 notes: −22% tokens, focus +54%,
FP 0.96, recall identical — the effect is strongest where graphs are small) and
labels every search reply `strong` / `weak` / `none`, so the assistant is told
when the graph is silent instead of being handed the nearest-looking thing.

**Reading the whole file is not expensive — it is impossible.** 377,260 tokens
against a 3,062-token brief; the strategy works, works, then does not exist.
And a session is cheapest on engram from the second question onward: the brief
plus 528/query crosses rag's 2,673/query at ~1.4 questions.

On questions that name what they are looking for — which is most of them —
Engram is at **1.00 lexical and 1.00 paraphrase**, against rag's 1.00 and 0.94,
with the best **R@1** of any retrieving arm: the right answer is more often the
*first* thing returned, not merely somewhere in the list.

### The other end of the ladder

The attention story is scale-dependent, so here is the same table at **100
notes** — the young-project size where a memory layer earns or loses its
keep. At scale, everything that survives to the top ten already scores above
the delivery floor, so the trim goes quiet (focus 0.12 at 300 notes, 0.11 at
500, 0.10 at 1500); on a young graph the tail is genuinely weak and the floor
works hardest:

| arm | standing | tok/query | focus | noise | R@1 | R@5 | lex | para | oblique | FP |
|---|---|---|---|---|---|---|---|---|---|---|
| chance | 0 | 2536 | 0.10 | 0.99 | 0.01 | 0.06 | 0.08 | 0.05 | 0.05 | 1.00 |
| grep | 0 | 2652 | 0.10 | 0.91 | 0.67 | 0.82 | 1.00 | 1.00 | 0.47 | 1.00 |
| rag (pure vectors) | 0 | 2269 | 0.12 | 0.88 | 0.82 | 0.97 | 1.00 | 1.00 | 0.92 | 1.00 |
| **engram** | 3041 | **373** | **0.20** | **0.80** | 0.79 | 0.96 | 1.00 | 1.00 | 0.89 | **0.96** |
| **engram + graph** | 3041 | **373** | **0.20** | **0.80** | 0.79 | 0.97 | 1.00 | 1.00 | 0.91 | **0.96** |
| curated-file 3k | 2929 | 2929 | 0.03 | 0.99 | 0.36 | 0.36 | 0.36 | 0.36 | 0.36 | 1.00 |
| curated-file 30k | 8403 | 8403 | 0.01 | 0.99 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| whole-file | 25179 | 25179 | 0.01 | 0.99 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |

Three numbers on this table exist nowhere else in the field. **Focus 0.20 at
373 tokens/query** — a sixth of rag's bill with two-thirds more of it being
the answer, roughly 6× the focus per token. **FP 0.96** — the only
false-positive rate below 1.00 this harness has ever measured on any arm:
4% of never-written questions get an honest empty result and a `none`
verdict instead of confident noise (the rest get a `weak` label the model is
told to verify). And the 30k curated file's recall-1.00 column sits next to
its own price: 8,403 standing tokens every session with the answer at 1% of
the text — at exactly the size where a diligent file still *can* hold
everything, holding everything is already the expensive way to remember.

---

## The baseline that actually competes

The honest comparison is not "no memory", and it is not a dump of every note
ever written. It is **a well-maintained `CLAUDE.md`** — pruned to stay readable,
always in context — plus the occasional prompt to shorten it. That objection
came from a reviewer of this project, and it is the strongest one available, so
`curated-file` is that baseline.

Its curation rule is deliberately **blind to the questions**: durable types
first, an unbiased tie-break within a type, each entry trimmed, filled to a
token budget. A human pruning a file has no idea what will be asked next, and
letting the arm peek would be inventing a baseline nobody has.

What it cannot do is hold everything, and that is the whole measurement —
recall@5 by **total graph size**, every fact questioned, measured rather than
extrapolated (the ladder, 2026-08-03):

| graph | curated 3k (held) | curated 30k (held) | grep | rag | **engram** |
|---|---|---|---|---|---|
| 10 | 1.00 (all) | 1.00 (all) | 0.97 | 1.00 | 1.00 |
| 100 | 0.36 (36) | 1.00 (all) | 0.82 | 0.97 | 0.96 |
| 200 | 0.18 (36) | 1.00 (all) | 0.77 | 0.95 | 0.93 |
| 500 | 0.07 (37) | 0.72 (358) | 0.70 | 0.88 | 0.86 |
| 1000 | 0.04 (36) | 0.37 (368) | 0.68 | 0.83 | 0.82 |
| 1500 | 0.02 (36) | 0.25 (368) | 0.68 | 0.80 | 0.80 |

Three things fall out, and the crossovers are now numbers instead of a line's
extrapolation.

**A 3,000-token file is whole only below ~40 notes** — that is where its
budget caps out — and it is overtaken by retrieval at 100, where it holds 36
facts and answers 0.36 against retrieval's 0.96.

**A 30,000-token file genuinely wins while everything fits.** Up to ~200 notes
it holds the entire graph and beats every retrieving arm on recall — the honest
point in the file's favour — then falls off the capacity cliff: overtaken at
500, down to 0.25 by 1500. What that win costs is attention: 30,000 standing
tokens in context every session, with the answer at ~0.5% of the delivered
text, against Engram's ~370 tokens per query with the answer at ~20%.

**At every size, curated recall equals its held fraction to the rounding
digit.** A static file does not care how the question is phrased — if the text
is present, it is present. It loses on *capacity*, never on retrieval quality,
because it does no retrieval.

The practical reading survives, with numbers on it: below ~40 notes a
maintained markdown file is a perfectly reasonable memory and this project is
not needed; an unusually diligent one stays reasonable to ~200–300; everything
Engram claims is about what happens after that.

### The ladder

`--ladder` measures exactly where that crossing happens instead of
extrapolating it: total graph sizes 10 → 1500 under one seed, with the curated
file scored at **3,000 and 30,000 tokens at every size**, and a closing table
naming the first size at which each budget falls behind retrieval.

It also changes what gets asked. The sized runs above question a tested third
of the graph and thin those questions to an assumed type mix; the ladder
questions **every fact it stores** — no untested distractors, no thinning.
Noise per question is the same either way (every fact has its own invented
subject, so the other N−1 notes are the crowd), but the sample is three times
larger and nothing depends on which third was picked. Assumed workload mixes
stay where they belong, in the report-side weighting. `--series` runs the
ladder plus the contradiction bench and writes one combined JSON.

## The big-context question

The obvious objection to any memory layer is that context windows keep growing,
so retrieval is a temporary problem — just put the notes in the prompt. Three
measured things make that argument weaker than it sounds, and one makes it
stronger, so all four are here.

**It stops working, and it stops abruptly.** A 1500-note graph is 377,260
tokens. That is not a large bill; it is past the window. There is no partial
version of this failure — the strategy works, works, works, then does not work
at all, and the note that would have answered the question is not missing from
the ranking, it is missing from the request. Engram's brief is 3,062 tokens for
the same graph, and it grows with what is *relevant*, not with what exists.

**Growth is on the wrong side of the ratio.** A project's memory grows without
bound; a context window grows in occasional steps. The flat-file arm pays for
every note ever written on every question asked. Engram pays 538 tokens for the
ten it ranked. Doubling the window doubles what the dump can hold once; it does
nothing about the next thousand notes.

**Recall is not the same as use.** `whole-file` scores 1.00 on every column here
*by construction* — the harness credits the fact as retrieved because the text
was present. Whether a model can find and use one fact inside a 375k-token
prompt is a completely different question, and this harness does not measure it.
It is not evidence that dumping works; it is the definition of the baseline.

**And the honest one: prompt caching narrows the cost gap.** The dump is
byte-identical across every question in a session, so a real deployment would
cache it and pay roughly a tenth for cache reads. At small and medium corpus
sizes that is a genuine argument, and the ~5× token advantage above should be
read as an uncached comparison. It does not rescue the strategy at 375k tokens —
nothing does, because the limit there is the window and not the price — but
below that limit, caching is the strongest version of the counter-argument and
it is fair to say so.

Which of these dominates in practice is not decidable from retrieval metrics
alone. It needs a live model answering from each arm's context, and that is the
online half — built as a contract (`src/online.rs`), not yet run. **Those
results will be published here when they exist.** Until then, the claims on this
page are about what was retrieved and what it cost, never about answer quality.

---

## Method

### The corpus is invented on purpose

Every fact is about a subject that does not exist — *"Kelnor lease broker uses a
retry budget of 7 attempts"*. No model can answer that from pretraining, and no
agent can answer it from having read this repository or from understanding how
the tool works.

That immunity is the point. Evidence gathered by watching a well-informed agent
use a tool it helped build measures the agent, not the tool, and no quantity of
it is worth one clean number.

It also means **grading never needs a judge**: the corpus knows the answer
contains `7 attempts`, so correctness is a substring check. Nothing here asks a
model whether a model did well.

### Shaped like real notes

An early version wrote 150-character bodies into a graph with no edges at all,
and then measured a graph memory on it. Generated facts are now filled to a
profile measured from this project's own graph:

| | real graph (30 sampled nodes) | generated |
|---|---|---|
| title chars | median 89 | matched |
| body chars | median 748 (p25 461, p75 1209) | matched, by quartile |
| code_refs | median 1 | matched |
| edges per node | 1.06 | matched |
| isolated nodes | 4% | matched |
| edge verbs | about 33%, builds-on 24%, because 23%, answers 15% | matched |

Both dimensions change the result rather than the decoration: body length is how
much surface a question has to match against, and edges are the whole mechanism
by which a graph memory could beat a flat one.

Bodies are padded with filler naming only the fact's own subject. A filler
sentence containing another fact's answer would manufacture a false positive and
corrupt every recall number, so a test asserts that never happens.

Since the ladder work the vocabulary spans three registers — software
infrastructure still dominates, salted with laboratory and abstract-process
categories (assay stations, provenance ledgers, escrow chambers) so no result
is tuned to one genre's wording. Tables above that predate the widening were
measured on the software-only vocabulary; the ladder re-measures everything
under the current corpus.

`replaces` and `conflicts-with` are generated **not at all**, despite being 2.7%
of real edges: both mutate node state at write time — archival and trust
demotion — which would silently remove gold facts from search.

`--terse` restores the old shape, so the difference can be quantified rather
than asserted.

### Attention is the budget

Two columns price what recall cannot see. **focus** — the share of delivered
tokens that belonged to the answering record, when it was delivered at all. A
dump can hold every answer and still score ~0.004 here; present is not the
same as readable. **noise** — the share of delivered records that were *not*
the answer, counting a miss with ten results as 1.00 (everything delivered was
a false positive) and an empty return as 0.00, because saying nothing tells no
lies. Noise is the one column where declining to answer scores better than
guessing, which is what makes calibrated delivery measurable at all.

`--floor` sweeps a delivery floor over the engram arm and produced the
product's calibrated-delivery defaults: a trim floor at the top of the
measured free zone (tail hits removed at zero recall cost), and the finding
that **hard abstention by absolute score is unaffordable** — at 500 notes,
declining 62% of never-written questions costs 38% of real answers, because
oblique answers and unanswerable questions genuinely overlap in score space.
Abstention therefore ships as a *verdict label* (strong / weak / none) on the
search reply, not as a harder floor.

### Three ways to ask

- **lexical** — quotes the fact's own words. `grep` wins these by construction.
- **paraphrase** — names the subject, rewords everything else.
- **oblique** — *never names the subject*, and shares no content vocabulary with
  the fact. Only meaning can find these.

Two tests defend the property: one asserts an oblique question shares at most
one content word with its own fact, the other that body filler reuses no
paraphrase vocabulary anywhere. The second exists because thirteen collisions
got in when the check was done by eye.

How often each phrasing occurs is **an assumption, not a measurement** —
`45/45/10` by default — and it decides which arm wins. Every report prints the
weighting that produced it, and the crossover point where the ranking would flip.
Questions are also weighted by node type (`decision=35, caution=20, insight=20,
problem=15, principle=10`) on the same footing: stated, not discovered.

### What it is compared against

`chance` returns random facts, so a weak score can be told apart from no
retrieval at all. `grep` is keyword search over a flat file of the same notes.
`rag` is pure vector top-k — no keyword channel, no priors, no reranker, no
graph — which is what a conventional stack does. `whole-file` puts everything in
context: perfect recall, no ranking, full bill every session.

`engram` and `engram + graph` are two readings of one run: the same hits scored
without and with the 1-hop neighbourhood, which makes the graph ablation free.
They are separate rows because a neighbour reference carries a title and an id
and nothing else — the caller still has to fetch the node. Collapsing them would
overstate the graph.

---

## The tuning result

The retrieval stack has two knobs that decide how a question that *describes*
something rather than *naming* it gets answered: how much of a hit's relevance
comes from keyword matching, and how much authority the cross-encoder has over
the final order.

Measured alone, both looked dead. Sweeping the keyword weight under the shipped
reranker was flat. Changing the reranker's authority at the shipped keyword
weight measured negative five runs out of five. So they were measured together —
oblique recall@5, mean of 3 seeds:

| keyword weight | reranker **decides** | reranker **votes** | what voting is worth |
|---|---|---|---|
| 0.00 | 0.304 | **0.424** | +0.120 |
| **0.15** | 0.292 | **0.357** | **+0.065** |
| 0.30 | 0.266 | 0.272 | +0.006 |
| 0.50 *(old default)* | 0.237 | 0.196 | −0.041 |

It is an interaction, and an interaction is invisible to any experiment that
moves one knob at a time. Voting is worth a lot when the keyword channel is
quiet and is actively harmful when it is loud — because at a high keyword weight
a name-free query's correct answer is dropped from the candidate set *before*
the reranker is ever called, and no amount of re-ordering recovers a candidate
nobody was shown.

**Deciding** means the cross-encoder's score replaces the fused retrieval score
outright. **Voting** combines the two orderings by reciprocal rank, so the
cross-encoder becomes one ranker of two and has to out-vote the retrieval
channels rather than overrule them. This is the ranking form of a rule the rest
of Engram already follows: a model's judgment nominates, it does not decide.

Shipped: keyword weight **0.15**, reranker **votes**. Verified on the real
engine, 3 seeds:

| | before | after | `rag` |
|---|---|---|---|
| weighted recall | 0.916 | **0.929** | 0.926 |
| recall@5 | 0.726 | **0.771** | 0.796 |
| recall@1 | 0.652 | **0.684** | 0.663 |
| oblique | 0.184 | **0.319** | 0.427 |
| paraphrase | 0.99 | 0.99 | 0.96 |
| tokens/query | 525 | **521** | 2613 |
| threshold separation | 0.76 | 0.76 | 0.80 |

Engram now beats pure vector search on aggregate recall, having previously lost
to it, at a fifth of the tokens and with no cost anywhere else on the table.

**Why 0.15 and not 0.** Zero scored higher on oblique questions, but silencing
the keyword channel also silences the snippet: a keyword match yields a 12-token
window around the hit, and without one the result falls back to a flat
160-character excerpt. Going to zero costs about a third more delivered tokens
and loses match highlighting, to buy 0.067. Token efficiency is the product's
actual edge; this would have spent it.

### Where pure vectors still lead — and where the ceiling is

On oblique questions specifically, `rag` stays slightly ahead — 0.47 against
0.40–0.42 at 1500 notes on the current corpus. The gap is real but small, and
a 2026-08-03 grid put a ceiling over the whole question: at 100 notes, **no
configuration of any arm exceeds 0.97 recall@5 / 0.92 oblique — pure vectors
included**. A 12-point fusion sweep, a 13-row strategy bench, and a bge-base
run all converge on the same residual misses, so the last few points are the
embedding models' semantic limit, not a tuning failure. Two other things the
grid settled: letting the reranker *decide* scores 0.80 oblique in every
single configuration (the vote design, re-confirmed a third time), and with
bge-base as the embedder the shipped stack reaches **full rag parity — 0.97 /
0.92 — at 346 tokens per query against rag's 2,308.** Tied with pure vectors
at a seventh of the tokens is the honest sentence.

---

## Mechanisms that were tried and rejected

Recorded because a harness that only reports its wins is an advertisement. Each
was implemented, measured, and abandoned:

| tried | result |
|---|---|
| spreading activation along edges into the ranking | wrecks it — lexical recall 1.00 → 0.88. Real edges *do* carry ~0.09 of oblique signal that randomly rewired edges do not, so the premise was right and the delivery wrong |
| feeding the cross-encoder whole notes instead of excerpts | worse, 0.31 → 0.28. More text to attend to does not help a small model |
| rerank depth 60 instead of 30 | no better, twice the cost |
| the bge query-instruction prefix | no measurable effect |
| tuning the semantic floor | flat across the entire grid |
| tuning the score cut and the relative cut | identical recall at every setting |

The rejected list being longer than the shipped list is the normal ratio, and
the graph-spreading row is the useful one: it is why Engram attaches neighbours
*after* ranking as delivery rather than *before* it as evidence. That choice buys
+0.06 oblique recall and costs nothing; the alternative was measured and is a
clear loss.

---

## What this does not show

- **Nothing declines outright, still.** The false-positive rate is 1.00 for
  every arm — asked about a subject that was never written, all of them return
  something. The `--floor` sweep showed why a hard fix is not free: the score
  populations overlap (separation 0.75–0.78), and buying abstention with an
  absolute floor spends oblique recall. What shipped instead is calibrated
  delivery: a measured tail-trim plus a strong/weak/none verdict on the search
  reply, so the assistant is told when the graph is silent or unsure — the
  honest form of declining that costs no recall.
- **The logic layer is not scored here.** Whether the memory notices that two
  things it holds disagree is a separate question from whether it can find them
  and needs its own metric — see `CONTRADICTIONS.md`, which has since replaced
  the NLI model and added a confidence gate on the strength of it. Nothing on
  this page depends on it.
- **The corpus is hostile to keyword search** in a way real notes are not. Real
  notes here carry 9.1% identifier-like tokens (`crates/engram-core/src/store.rs`,
  `keyword_weight`); generated ones carry 0.9%, and generated bodies are filler
  drawn from a shared pool. Any conclusion that says *weaken the keyword channel*
  should be read as a direction, not a magnitude.
- **Synthetic facts are clean** — one subject, one slot, one value. Real project
  memory is mushy and stated implicitly across paragraphs. Tuning retrieval to
  ace this corpus can overfit to that tidiness.
- **Some of the fall in oblique recall with corpus size is structural**: more
  facts share each component, so an oblique question has more plausible
  candidates. That is a fair model of a growing project, not the retriever
  getting worse.
- Without `--features fastembed` the embedder is a character-frequency bag. The
  harness runs and the lexical path is exercised; the semantic numbers are noise,
  and every fake run says so at the top.

---

## The online half — not yet run

`src/online.rs` is the contract for the part that needs a live model. It
deliberately ships no API client and reuses none of the offline results, because
it answers a different question: *given this context, does the model get it
right?*

```sh
cargo run -p engram-eval --features fastembed -- --sizes 50 --emit-tasks tasks.json
```

The manifest covers all four arms — `whole-file`, `grep`, `rag`, `engram` — and
each task carries **the context that arm actually delivered**, verbatim: whole
records for `grep`, whole chunks for `rag`, a title and the matched snippet for
`engram`. That distinction is the entire point and is enforced by a test, because
an earlier version rebuilt context from full note bodies for every ranked arm and
so measured them all as if they returned the same thing.

Each task also carries the question and the substring a correct answer must
contain — empty meaning the honest answer is `NOT IN MEMORY`. Implement
`Responder`, hand it the manifest, grade with `Task::grade`: a substring check,
never a judge.

Two things only the online half can settle, and both are open:

- **Does the model decline when it should?** Offline, the false-positive rate is
  1.00 for every arm including `chance` — nothing ever refuses. But abstention is
  a property of the model, not the retriever. Whether a model says
  `NOT IN MEMORY` when Engram hands it weak hits decides whether a confidence
  threshold is needed at all.
- **Does a smaller, ranked context produce better answers than a bigger one?**
  `whole-file` has perfect recall here by construction. If 538 ranked tokens beat
  375,890 dumped ones on *answer accuracy*, that is a far stronger claim than
  "cheaper" — and it is one this page does not currently make.

Results will be added here once they are measured.

## Layout

| file | |
|---|---|
| `generate.rs` | the corpus: facts, three phrasings, distractors, controls, twins, NLI pairs |
| `arms.rs` | the baselines and their token accounting |
| `variants.rs` | retrieval strategies, including ones that do not ship |
| `metrics.rs` | recall@k, MRR, twin confusion, threshold separation |
| `nli_eval.rs` | confusion matrix and per-label precision/recall |
| `run.rs` | the suite, the fusion sweep, the strategy grid, the contradiction bench |
| `CONTRADICTIONS.md` | the logic layer's own metric — the model swap, the gate, the real-graph check |
| `online.rs` | the online half's contract |
| `rng.rs` | seeded splitmix64 — every run reproduces from `--seed` |

To isolate density from corpus size, hold `--sizes` fixed and vary
`--distractors`: the tested facts and their questions stay byte-identical while
the graph around them grows.
