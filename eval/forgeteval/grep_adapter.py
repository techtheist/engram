"""ForgetEval adapter `grep` -- a deterministic keyword-overlap baseline.

No daemon, no embeddings, no calibration: this is the "what does a system
with zero semantic machinery score" floor. Every fact is tokenized into
lowercased content words (stopwords and single-character tokens dropped);
a query's overlap score against a stored fact is the size of the set
intersection between the query's content words and the fact's. Hits are
ordered by overlap score descending, ties broken by insertion order (dict
iteration order, which Python guarantees since 3.7) so results are fully
deterministic. A query that shares zero content words with every stored
fact declines -- `recall_texts` returns `[]`, exactly like a grep with no
matches.

`supersede` / `release` / `purge` mirror the SAME three target-resolution
rules `engram_adapter.EngramAdapter` mirrors from
`bench.forgeteval.adapter.LetheAdapter` (see eval/forgeteval/README.md's
"query-resolution policy" table), just run over the overlap-score column
instead of a cosine-similarity column:

  supersede  ->  top-1 overlap match; no match -> just inscribe the new fact
  release    ->  top-20 overlap search, LetheAdapter's own adaptive-gap
                 threshold (`_gap_threshold`, ported verbatim, imported
                 from engram_adapter) selects the release set
  purge      ->  top-20 overlap search, then NFKC/lower/whitespace lexical
                 equivalence against the top hit's ORIGINAL text
                 (`_norm_lexical`, same import) selects the purge set

This keeps `grep` runnable against both ForgetEval suites with the exact
same mutation semantics `engram`/`engram-mcp` use, so the three adapters'
family/category tables are directly comparable -- the only variable is
what "target resolution" is built on (calibrated hybrid retrieval vs.
plain keyword overlap), not what the mutation rules do once a target is
found.
"""
from __future__ import annotations

import re
import time

from engram_adapter import _gap_threshold, _norm_lexical

_STOPWORDS = frozenset("""
a an and are as at be by for from has have he her his if in into is it
its of on or our she that the their them there these they this those to
under was we were will with you your i me my mine yours ours theirs him
himself herself itself themselves am been being do does did doing but so
than then too very not no nor can could should would may might must shall
about above after again against all am been below between both down during
each few further here how just more most no not now off once only other
out over own same should so some such than that the then there these they
this through too under until up very was we were what when where which
while who whom why will with won't you're you've
""".split())

_WORD_RE = re.compile(r"[a-z0-9]+")


def _content_words(text: str) -> list[str]:
    return [w for w in _WORD_RE.findall(text.lower())
            if len(w) > 1 and w not in _STOPWORDS]


def _overlap_score(query_words: set[str], text: str) -> int:
    if not query_words:
        return 0
    text_words = set(_content_words(text))
    return len(query_words & text_words)


class GrepAdapter:
    """See module docstring. `name = "grep"`."""

    name = "grep"

    def __init__(self, stats: dict | None = None):
        self._texts: dict[int, str] = {}
        self._next_id = 0
        self.stats = stats if stats is not None else {}

    def _timed(self, key: str, fn, *a, **kw):
        t0 = time.perf_counter()
        out = fn(*a, **kw)
        dt = time.perf_counter() - t0
        self.stats.setdefault(key, []).append(dt)
        return out

    # ─── protocol ───────────────────────────────────────────────────

    def reset(self) -> None:
        def _do():
            self._texts = {}
            self._next_id = 0
        self._timed("reset", _do)

    def inscribe(self, text: str) -> int:
        def _do():
            nid = self._next_id
            self._next_id += 1
            self._texts[nid] = text
            return nid
        return self._timed("inscribe", _do)

    def _scored(self, query: str) -> list[tuple[int, int]]:
        qwords = set(_content_words(query))
        scored = [(nid, _overlap_score(qwords, text))
                  for nid, text in self._texts.items()]
        scored = [(nid, s) for nid, s in scored if s > 0]
        scored.sort(key=lambda t: (-t[1], t[0]))
        return scored

    def recall_texts(self, query: str, k: int = 5) -> list[str]:
        def _do():
            full = self._scored(query)
            # Threshold-free companion to the standard scoring (see
            # eval/forgeteval/README.md's "Abstention" section, separation/
            # AUC paragraph): the top overlap COUNT on grep's own native
            # scale, None when nothing overlapped at all.
            self.stats.setdefault("grep_score_log", []).append({
                "query": query,
                "top_score": float(full[0][1]) if full else None,
            })
            scored = full[:k]
            return [self._texts[nid] for nid, _ in scored]
        return self._timed("recall", _do)

    # ─── mutations ──────────────────────────────────────────────────

    def supersede(self, old_query: str, new_text: str) -> None:
        def _do():
            scored = self._scored(old_query)
            if scored:
                self._texts.pop(scored[0][0], None)
            nid = self._next_id
            self._next_id += 1
            self._texts[nid] = new_text
        self._timed("supersede", _do)

    def release(self, query: str) -> int:
        def _do():
            scored = self._scored(query)[:20]
            if not scored:
                return 0
            scores = [float(s) for _, s in scored]
            thr = _gap_threshold(scores)
            targets = [nid for nid, s in scored if s >= thr]
            for nid in targets:
                self._texts.pop(nid, None)
            return len(targets)
        return self._timed("release", _do)

    def purge(self, query: str) -> int:
        def _do():
            scored = self._scored(query)[:20]
            if not scored:
                return 0
            top_text = self._texts.get(scored[0][0], "")
            target_norm = _norm_lexical(top_text)
            targets = [nid for nid, _ in scored
                      if _norm_lexical(self._texts.get(nid, "")) == target_norm]
            for nid in targets:
                self._texts.pop(nid, None)
            return len(targets)
        return self._timed("purge", _do)

    # ─── introspection helper for the runner (not part of the Adapter
    #     protocol; mirrors EngramAdapter.health so run_engram.py can call
    #     it uniformly) ────────────────────────────────────────────
    def health(self) -> dict:
        return {"status": "ok", "adapter": "grep", "n_facts": len(self._texts)}
