# LongMemEval evaluation

The first corpus this harness runs that we did not generate.
[LongMemEval](https://github.com/xiaowu0162/LongMemEval) (Wu et al., ICLR
2025, MIT license) is 500 questions, each asked over its own multi-session
chat history — real human-register conversations, with the evidence sessions
labelled and ~6% of the questions deliberately unanswerable (`question_id`
suffix `_abs`). The `S` variant used here carries ~115k tokens of haystack
per question: the size at which "just use a long context window" becomes a
real, priced alternative.

```sh
cargo run -p engram-eval --features fastembed --release -- --longmemeval s
cargo run -p engram-eval --features fastembed --release -- --longmemeval s --lme-limit 20   # smoke
# ~16 h on CPU (the write path is 99.5% embedder time). A local Ollama or
# llama-server serving the same bge-small weights (GGUF F16) cuts it to ~2 h
# with grades identical to the digit — see --lme-embedder / --lme-workers:
cargo run -p engram-eval --features fastembed --release -- --longmemeval s --lme-embedder ollama
```

The dataset is fetched on demand into `eval/data/` (gitignored) and verified
against a SHA-256 pinned in `longmem.rs` on every open. The repo carries only
the loader, the digests, and this page.

---

## This test is fundamentally different — deliberately

Read nothing here as a LongMemEval *score*. The published benchmark is an
**online** evaluation: a memory system retrieves, an LLM reads what was
retrieved and writes an answer, and a judge grades the answer text. Our run
is the **offline retrieval half only**, and it differs from the official
protocol in two load-bearing ways:

1. **The data is flat, not LLM-processed.** Ingestion is *as-is*: one note
   per chat turn, verbatim, filler and all. No extraction, no summarisation,
   no model anywhere in the ingestion path. This is deliberately the
   unflattering register — in real deployment the memory is written by an
   agent as distilled, typed notes, and Engram deliberately ships no
   extractor of its own. So these numbers are a *floor* for the deployed
   shape, not an estimate of it.
2. **Grading is retrieval, not answers.** A question counts as answered when
   a note from a labelled evidence session lands in the delivered set. That
   makes the grading deterministic, full-population, and judge-free — the
   same standard every number in this harness meets — at the price of not
   measuring what a model *does* with the delivery.

What the run does show, cleanly, is **relative** value on data we did not
write: the same haystacks, five arms, one embedder — whatever separates the
rows is the retrieval stack and nothing else. And the gap between this page
and the official suite is itself the point of the future online half: run
the answer-generation step on top of these identical deliveries and the
delta shows exactly **how much an LLM improves — or degrades — each arm's
results**. A model can rescue a noisy delivery or hallucinate over a clean
one; that is a property worth measuring separately from retrieval, not
blended into it.

## The chat ontology — fitted as data, not code

The stores for this corpus do not run the stock software ontology. They run
a two-type **chat ontology defined entirely as per-graph data**
(`--lme-ontology chat`, the default): user `statement` (first-party facts,
a small rank prior — the source outranks a restatement at equal relevance)
over assistant `reply` (no prior, muted). Role is the one distinction an
as-is ingester can make honestly without a classifier. Same engine, zero
engine changes — the per-graph GraphConfig machinery the product ships,
demonstrated on a register it was never written for. Notes are stamped with
their session's real date (`created_at`), so recency reads the conversation
timeline. `--lme-ontology default` runs the stock set beside it for the
comparison.

## The arms

Same five stacks as the offline suite, on chat instead of notes:

- **engram** — the shipped stack: hybrid search + reranker + type priors +
  calibrated delivery, titles and matched snippets delivered.
- **rag** — pure vectors over the same store and embeddings, whole turns
  delivered. The stack most memory products actually are.
- **grep** — keyword search over the flat turns, whole turns delivered.
- **curated-file** — a 3,000-token hand-maintained file. Chat turns carry no
  types, so the offline suite's "durable types first" human heuristic
  degrades to its stated blind tie-break (hash order, entries trimmed to 200
  chars). It answers what it kept and nothing else — that ceiling is the
  measurement.
- **whole-file** — the entire haystack in context: the "just use a 128k
  window" answer. Never misses, and the tok/query column is its price.

## Results — full population (500 of 500 questions)

Every question, every haystack: 470 answerable + 30 `_abs`, ~493 turn-notes
per question, chat ontology, bge-small + jina reranker (receipt:
`results/longmemeval-s-full.json`, run 2026-08-08):

| arm | R@1 | R@5 | MRR | tok/query |
|---|---|---|---|---|
| **engram** | 0.91 | 0.95 | 0.93 | **208** |
| rag (pure vectors) | 0.91 | 0.97 | 0.94 | 2,654 |
| grep | 0.71 | 0.86 | 0.77 | 4,761 |
| curated-file (3k) | 0.92 | 0.92 | 0.92 | 2,999 |
| whole-file | 1.00 | 1.00 | 1.00 | 122,515 |

The headline holds from smoke to full population: engram ties pure-vector
recall at R@1 (0.91 vs 0.91), gives up 0.02 R@5, and does it at **8% of
rag's delivered tokens** (13× cheaper) — on real chat data neither system
was written for, with no extraction pass. Grep is far behind on every
column; the hand-file answers only what it happened to keep.

By question type (engram, R@5 / MRR): `single-session-assistant`
1.00 / 0.99, `knowledge-update` 0.99 / 0.98, `single-session-user`
0.98 / 0.97, `multi-session` 0.94 / 0.93, `temporal-reasoning`
0.93 / 0.89, `single-session-preference` 0.87 / 0.76. The weak tail is
honest and legible: preference questions ("what gift should I get my
mother?") are *oblique* — the evidence never shares the question's
vocabulary — and oblique phrasing is exactly the known soft spot the
offline suite already measures; temporal reasoning wants an inference
layer retrieval alone cannot supply.

Two honest notes on the flattering rows. `whole-file` is definitionally
perfect — its column of interest is the 122k tokens *per question*, which
is the "just use a long context" answer priced. And **session-level
grading is generous to dumps**: a hit counts when *any* turn from the
evidence session is delivered, so a blind 3k selection that happens to
keep one filler line ("Sure!") from the right session scores the same as
engram delivering the actual evidence turn. The focus/noise attention
metrics that separate *present* from *readable* in the offline suite
apply here with full force; the token column is their proxy in this table.

## Abstention

The `_abs` questions have no evidence anywhere in their haystack — the
correct behaviour is to say so. They are scored under the calibrated
recommendation verdict the product ships: below the auto-tuned
"likely not in memory" line the reply is warned (honest), and only an
**unwarned** answer counts as a false positive. Candidates are never cut.
The stock LongMemEval protocol needs an LLM to abstain; here abstention is a
property of the retrieval layer itself, which no other row on the atlas
measures.

Full population, 30 `_abs` questions: **2 empty deliveries, 28 warned,
0 unwarned — a false-positive rate of 0.00**, with the weak line
auto-tuned per store from its own graph vocabulary (27 of 30 stores
moved their line). Every arm without a verdict layer answers all 30 with
full confidence. This is the same young-graph honesty the offline suite
measures at 100 notes (FP 0.00 there too) showing up unchanged on real
chat at ~500 notes per store.

---

## The official-suite run — reserved

This section is deliberately empty. The plan of record: run the **online
half** — answer generation over each arm's actual deliveries (the
`emit-tasks` pattern the offline suite already uses), graded by the official
LongMemEval answer-matching protocol — so the numbers on this page become
directly comparable with published systems, and the retrieval-vs-generation
attribution stays clean because both halves share one delivery manifest.
Also reserved for: the **agent-extracted ingestion arm** (a one-time,
disclosed, frozen artifact of typed notes written by an agent — the deployed
shape, measured against the as-is floor above), and `--lme-ontology
default` vs `chat` as a measured pair.

## Provenance

- Dataset: `xiaowu0162/longmemeval-cleaned` (HuggingFace), MIT.
  `longmemeval_s_cleaned.json`
  sha256 `d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442`;
  `longmemeval_oracle.json`
  sha256 `821a2034d219ab45846873dd14c14f12cfe7776e73527a483f9dac095d38620c`.
- Paper: Wu et al., *LongMemEval: Benchmarking Chat Assistants on Long-Term
  Interactive Memory*, ICLR 2025.
- Runner: `eval/src/longmem.rs`; receipts in `eval/results/`.
