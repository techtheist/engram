# Does it come back?

An offline harness for the only question that matters about a memory layer:
**when something was written down, does it come back — and what did that cost?**

```sh
cargo test -p engram-eval                                      # fast, no models, what CI runs
cargo run -p engram-eval --features fastembed -- --sizes 500   # the headline table
cargo run -p engram-eval --features fastembed -- --bench       # the strategy grid
cargo run -p engram-eval -- --sizes 50 --sample                # read what it generates
```

Everything below was produced by these commands. Nothing was measured by
watching an agent use the tool.

---

## Result

At 1500 notes shaped like real ones — median 748-character bodies, 1590 edges —
asked 858 questions with known answers:

| arm | standing | tok/query | R@1 | R@5 | lexical | paraphrase | oblique | recall per 1k tokens |
|---|---|---|---|---|---|---|---|---|
| chance | 0 | 2494 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| grep | 0 | 2739 | 0.65 | 0.67 | 1.00 | 0.99 | 0.03 | 0.25 |
| rag (pure vectors) | 0 | 2571 | 0.59 | 0.72 | 0.99 | 0.93 | 0.24 | 0.28 |
| **engram** | 2996 | **538** | **0.66** | 0.71 | **1.00** | **0.99** | 0.15 | **1.33** |
| **engram + graph** | 2996 | **538** | **0.66** | 0.72 | **1.00** | **0.99** | 0.16 | **1.34** |
| curated-file | 2991 | 2991 | 0.02 | 0.02 | 0.02 | 0.02 | 0.02 | 0.01 |
| whole-file | 375890 | 375890 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 0.003 |

Weighted for how often each phrasing actually occurs, the ranking is
**engram 0.913, grep 0.90, rag 0.89**.

Three things follow.

**Engram delivers the most recall per token, by about 5×.** It returns a title
and a matched snippet; the flat-file arms return whole records. That ratio is
the design argument, and it is the only column where the gap is an order of
magnitude rather than a few points.

**It is also the cheapest way to run a session, from the second question
onward.** Engram pays 2996 tokens once for its session brief and 538 per query;
`rag` pays 2571 every query and nothing up front. The lines cross at 1.5
questions.

**Reading the whole file is no longer expensive — it is impossible.** 375,890
tokens, against a 2,996-token brief. An earlier figure of 45,227 was measured
over 150-character stub bodies; at realistic note length the file baseline
exceeds every current context window, and it stops working abruptly rather than
gradually.

On questions that name what they are looking for — which is most of them —
Engram is at **1.00 lexical and 0.99 paraphrase**, against `rag`'s 0.99 and 0.93.
It also has the best **R@1** of any retrieving arm, meaning the right answer is
more often the *first* thing returned, not merely somewhere in the list.

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

What it cannot do is hold everything, and that is the whole measurement:

| budget | facts held | recall@5 | lexical / paraphrase / oblique |
|---|---|---|---|
| 3,000 (a realistic `CLAUDE.md`) | 37 / 1500 (2.5%) | 0.017 | 0.02 / 0.02 / 0.02 |
| 30,000 (an unusually diligent one) | 371 / 1500 (24.7%) | 0.192 | 0.19 / 0.19 / 0.19 |

Two things fall out, and the second matters more than the first.

**Recall tracks the budget, linearly.** Extrapolating the line, a
hand-maintained file would need roughly **110,000 tokens** to match what Engram
retrieves — carried in context on every question, against Engram's 538. The file
is not bad at finding things; it simply cannot hold a project's memory and stay
a file.

**Its recall is flat across phrasings**, at every budget. A static file does not
care whether a question quotes a note, paraphrases it, or describes it without
naming it — if the text is present, it is present. That flatness is what makes
this a fair baseline rather than a strawman, and it is also the honest limit of
the comparison: the curated file loses on *capacity*, never on retrieval
quality, because it does no retrieval.

The practical reading: below a few hundred notes, a maintained markdown file is
a perfectly reasonable memory and this project is not needed. The curve crosses
somewhere in the low thousands of tokens' worth of knowledge, and everything
Engram claims is about what happens after that.

## The big-context question

The obvious objection to any memory layer is that context windows keep growing,
so retrieval is a temporary problem — just put the notes in the prompt. Three
measured things make that argument weaker than it sounds, and one makes it
stronger, so all four are here.

**It stops working, and it stops abruptly.** A 1500-note graph is 375,890
tokens. That is not a large bill; it is past the window. There is no partial
version of this failure — the strategy works, works, works, then does not work
at all, and the note that would have answered the question is not missing from
the ranking, it is missing from the request. Engram's brief is 2,996 tokens for
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

`replaces` and `conflicts-with` are generated **not at all**, despite being 2.7%
of real edges: both mutate node state at write time — archival and trust
demotion — which would silently remove gold facts from search.

`--terse` restores the old shape, so the difference can be quantified rather
than asserted.

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

### Where pure vectors still lead

On oblique questions specifically, `rag` is ahead — 0.24 against 0.16 at 1500
notes. The retune roughly doubled Engram's number and closed most of the gap,
but not all of it. The honest framing is the crossover: **the two arms tie when
26% of questions never name their subject.** Below that, Engram wins overall;
above it, pure vectors do. The default assumption is 10%, and it is an
assumption.

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

- **Nothing declines a question it cannot answer.** The false-positive rate is
  1.00 for every arm including `chance` — asked about a subject that was never
  written, all of them return something. A threshold would mostly work
  (separation 0.75–0.78) and none is applied. This is the most important open
  problem the harness has found, and it is not fixed.
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
