# Local models

Everything that keeps the graph honest runs on your machine, on small local
models — no API calls, no keys, no telemetry. The whole loop works on a
plane.

## The cortex

Three model layers, each optional beyond the first, each replaceable:

| Layer | Default model | What it does |
|---|---|---|
| **Embeddings** | `bge-small-en-v1.5` (384-dim) | Semantic recall: the vector half of hybrid search, and the similarity behind duplicate/conflict detection |
| **Reranker** | `jina-reranker-v1-turbo-en` | Precision: a cross-encoder re-scores the top candidates against your query |
| **NLI** | `nli-deberta-v3-small` | Logic: entailment/contradiction verdicts for [Checkup](./conflicts-and-checkup.md), claim checks, and suspect triage |

Models download once, over HTTPS, into `~/.cache/engram/<model-name>/`, and
load from disk on every later start. The reranker and NLI layers are
upgrades, never dependencies: if one can't load, that layer degrades
gracefully (search keeps hybrid order, hints switch off) and the System
panel says so.

What never runs in the daemon: an LLM. Engram's models classify and rank;
they don't generate.

## Choosing models

Settings → System → **Choose models** swaps any layer:

- **Presets** — known-good ONNX exports: `bge-small`/`bge-base`/`all-MiniLM`
  embeddings, `jina-turbo`/`bge-reranker-base` rerankers.
- **Custom by URL** — any compatible ONNX export: give it a name, a base URL
  (Hugging Face `…/resolve/main` style), and for embeddings the vector width
  and pooling.

Reranker and NLI swaps apply instantly — they hold no stored state. An
embedding swap is bigger: every stored vector is in the old model's space,
so Engram rebuilds vector storage and **re-embeds every open graph on the
spot** — one guarded pass, no restart, progress reported back. Graphs not
currently open re-embed automatically the next time they're opened. The
selection persists machine-wide in `~/.engram/models.json` and applies to
all your projects.

One honest caveat, shown in the pane too: the similarity thresholds behind
duplicate and conflict detection were calibrated on the default embedding
model. On a different model they still work, but detection quality is
unvalidated until re-tuned.

## Offline behavior

First run needs the network once per model (the embedding model is ~30 MB,
the NLI model ~35 MB). After that, everything is local. If a first run
happens offline, the affected layer degrades gracefully and provisions
itself on the next online start — `engram-alpha doctor` reports which models
are cached.

Power knobs: `ENGRAM_MODEL_DIR`, `ENGRAM_RERANKER_DIR`, and
`ENGRAM_NLI_DIR` override where each default model loads from — useful for
air-gapped setups where you place the files yourself.
