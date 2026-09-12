"""ForgetEval adapter for Engram (https://github.com/<engram repo>), driven
purely over its local HTTP API (no Rust code touched, no MCP).

Two variants are exposed, both required by the calling harness so the
tombstone-marker leak (see class docstring below) is never hidden inside
a single headline number:

  EngramAdapter(include_tombstones=True)   name="engram"          (headline)
  EngramAdapter(include_tombstones=False)  name="engram-notomb"   (marker-filtered)

Mapping from the ForgetEval primitives to engram's REST surface
------------------------------------------------------------------
  reset()                 rm the project's sqlite/tepin store file; the
                           daemon's identity-checked engine cache (dev+ino)
                           evicts the deleted store and reopens a fresh one
                           lazily on the next request (engram 0.8.13+).
  inscribe(text)           POST /nodes  type=Insight, durability=episodic,
                           source=user.  title = first sentence of text
                           (<=120 chars), body = text verbatim.
  recall_texts(query, k)   GET /search?q=&limit=k, hits already in score
                           order.  "engram" returns every hit's text
                           (Tombstone markers included, honest headline);
                           "engram-notomb" drops Tombstone-typed hits.
  supersede(old_q, new)    resolve old_q the same way LetheAdapter does
                           (search top-1); POST the new node; POST
                           /edges type=replaces new->old (archives old,
                           which then drops out of search on its own).
  release(query)           resolve query as a *group* (adaptive-gap
                           threshold over the score column of a 20-deep
                           search — the same `_gap_threshold` numerical
                           procedure LetheAdapter/Mem0Adapter/LangGraphAdapter
                           all use); DELETE each match with
                           ?tombstone=true&keep_text=false&reason=released.
  purge(query)             resolve a 20-deep search, then keep only hits
                           whose ORIGINAL inscribed text (not the search
                           snippet) is NFKC/lower/whitespace-equivalent to
                           the top hit's text -- LetheAdapter.purge's own
                           dedup rule, ported verbatim; hard-DELETE each.

Why mirror Lethe specifically: three different reference adapters in this
repo disagree on what "purge" should resolve to (Lethe: lexical-identity
dedup; Mem0: reuses the release gap-threshold; LangGraph: raw string
equality). Lethe is the benchmark's own flagship/default adapter, so its
policy is treated as the canonical one to mirror when the adapters disagree
-- see eval/forgeteval/README.md ("query-resolution policy") for the full
comparison table and the reasoning.

Config deviation -- the calibrated delivery floor is disabled.
engram ships a calibrated delivery floor (policy.delivery_floor /
knee_cliff / semantic_floor / search_min_score / search_relative_cut)
tuned against a large, noisy, cross-session real memory graph (see the
project's own eval/ ladder, 100-1500 notes). Measured directly against a
ForgetEval case (six-node graph, one obviously-relevant hit at
score=0.201): the DEFAULT policy (delivery_floor=0.22) returns an empty
result set for "What theme does Charlie use?" even though the single
matching note is right there -- and the same floor also blinds this
adapter's OWN internal target-resolution search, so supersede/release/
purge silently no-op (falls through to "no hits" -> just inscribes the
new fact, never archives the old one) purely because the confidence
floor never clears in a 6-note graph. None of the reference adapters in
this repo have an analogous absolute floor: Lethe's `recall()` is
unconditional top-k nearest-neighbor, Mem0's `.search(top_k=k)` has no
score floor, LangGraph's `InMemoryStore.search(limit=k)` has no score
floor. To keep the comparison meaningful (testing forgetting semantics
rather than an unrelated, differently-scaled confidence calibration),
every EngramAdapter instance PUTs policy.delivery_floor=knee_cliff=
semantic_floor=search_min_score=search_relative_cut=0 on ITS OWN
isolated project once at construction, and RE-APPLIES it after every
reset() (config lives inside the tepin store file itself, so deleting
the file for reset() also reverts policy to the store's defaults).
This is scoped to the two throwaway forgeteval-store* project
directories this benchmark creates -- never the developer's real
graphs. The magnitude of the effect at default policy is measured
separately and reported as a labeled control in the README, not folded
into the headline number.
"""
from __future__ import annotations

import os
import time
import unicodedata

import requests

ENGRAM_BASE = os.environ.get("ENGRAM_BASE", "http://127.0.0.1:8787")

# All engram node types except Tombstone -- used to keep internal
# supersede/release/purge target-resolution from ever selecting a
# Tombstone marker as if it were live content.
_NON_TOMBSTONE_TYPES = "Decision,Principle,Caution,Problem,Resolution,Insight,Intent,Anchor"


def _gap_threshold(sims: list[float], min_gap: float = 0.05) -> float:
    """Ported verbatim from bench.forgeteval.adapter.LetheAdapter._gap_threshold.

    Finds the natural cutoff in a sorted-descending similarity/score list:
    the midpoint of the largest gap. Falls back to top * 0.95 when there is
    no significant gap (one tight cluster of hits)."""
    if not sims:
        return float("inf")
    if len(sims) == 1:
        return sims[0] * 0.95
    s = sorted(sims, reverse=True)
    best_gap = 0.0
    best_mid = s[0] * 0.95
    for i in range(len(s) - 1):
        gap = s[i] - s[i + 1]
        if gap > best_gap:
            best_gap = gap
            best_mid = (s[i] + s[i + 1]) / 2.0
    return best_mid if best_gap >= min_gap else s[0] * 0.95


def _norm_lexical(s: str) -> str:
    """Ported verbatim from bench.forgeteval.adapter.LetheAdapter._norm_lexical."""
    return " ".join(unicodedata.normalize("NFKC", s).lower().split())


def _title_from_text(text: str, limit: int = 120) -> str:
    """First sentence of `text`, capped at `limit` chars -- used as the
    node's title. For the single-sentence facts ForgetEval generates,
    this is very often close to verbatim, which matters for the
    tombstone-leak analysis (see README): the mint always writes
    "Removed: <victim title>", so a near-verbatim title means the
    victim's substance survives in the tombstone even with
    keep_text=false."""
    text = text.strip()
    for sep in (". ", "! ", "? ", "\n"):
        idx = text.find(sep)
        if 0 <= idx <= limit:
            return text[: idx + 1].strip()
    return text[:limit].strip()


class EngramAdapter:
    """See module docstring for the full mapping and the rationale."""

    #: policy fields zeroed on every (re)creation of the store -- see the
    #: "Config deviation" section of the module docstring.
    _FLOOR_FIELDS = ("delivery_floor", "semantic_floor",
                     "search_min_score", "search_relative_cut")
    #: The knee trim is an Option: `null` is OFF, while `0` is the most
    #: aggressive setting there is (the cut fires at the largest relative
    #: drop of EVERY score curve, because every drop is >= 0). The first
    #: 2026-09-12 run zeroed it and so trimmed nearly every query to its
    #: head -- the release rule never saw the target's sibling facts and
    #: amnesia queries lost the surviving peer. Adapter bug, fixed here.
    _NULL_FIELDS = ("knee_cliff",)

    def __init__(self, project_dir: str, *, include_tombstones: bool = True,
                 base_url: str = ENGRAM_BASE, timeout: float = 30.0,
                 stats: dict | None = None, disable_delivery_floor: bool = True):
        self.project_dir = project_dir
        self.include_tombstones = include_tombstones
        self.name = "engram" if include_tombstones else "engram-notomb"
        self.base = base_url.rstrip("/")
        self.timeout = timeout
        self.disable_delivery_floor = disable_delivery_floor
        self.session = requests.Session()
        self.session.trust_env = False
        self._project_sel: str | None = None
        self._store_path: str | None = None
        self._texts: dict[str, str] = {}
        self._policy_payload: dict | None = None
        # Optional shared dict this run's caller can pass in to accumulate
        # timing/behavioral stats across every case (see run_engram.py).
        self.stats = stats if stats is not None else {}
        self._register()

    # ─── plumbing ───────────────────────────────────────────────────

    def _register(self) -> None:
        os.makedirs(self.project_dir, exist_ok=True)
        r = self.session.post(f"{self.base}/projects", json={"path": self.project_dir},
                              timeout=self.timeout)
        r.raise_for_status()
        info = r.json()
        self._project_sel = info["name"]
        # Materialize the store and resolve its on-disk path (the registry's
        # `db` field can lag the resolved sibling -- see engram cheat sheet).
        r = self.session.get(f"{self.base}/brief",
                             params={"project": self.project_dir, "max_chars": 1},
                             timeout=max(self.timeout, 30.0))
        r.raise_for_status()
        engram_dir = os.path.join(self.project_dir, ".engram")
        for fname in ("graph.tepin", "graph.db"):
            p = os.path.join(engram_dir, fname)
            if os.path.exists(p):
                self._store_path = p
                break
        if self.disable_delivery_floor:
            cfg = self.session.get(f"{self._base_url}/config",
                                   timeout=self.timeout).json()
            for f in self._FLOOR_FIELDS:
                cfg["policy"][f] = 0.0
            for f in self._NULL_FIELDS:
                cfg["policy"][f] = None
            self._policy_payload = cfg
            self._apply_policy()

    def _apply_policy(self) -> None:
        if self._policy_payload is None:
            return
        r = self.session.put(f"{self._base_url}/config", json=self._policy_payload,
                             timeout=self.timeout)
        r.raise_for_status()

    @property
    def _base_url(self) -> str:
        return f"{self.base}/projects/{self._project_sel}"

    def _timed(self, key: str, fn, *a, **kw):
        t0 = time.perf_counter()
        out = fn(*a, **kw)
        dt = time.perf_counter() - t0
        bucket = self.stats.setdefault(key, [])
        bucket.append(dt)
        return out

    # ─── protocol ───────────────────────────────────────────────────

    def reset(self) -> None:
        def _do():
            if self._store_path and os.path.exists(self._store_path):
                os.remove(self._store_path)
            self._texts = {}
            # Touch the graph once so the daemon reopens/creates the store
            # before the case's first inscribe -- keeps reset() cost
            # separate from first-inscribe cost in the stats.
            r = self.session.get(f"{self._base_url}/graph", timeout=self.timeout)
            r.raise_for_status()
            # Config lives inside the store file itself, so recreating it
            # reverted policy to defaults -- reapply the floor override.
            self._apply_policy()
        self._timed("reset", _do)

    def inscribe(self, text: str) -> str:
        def _do():
            body = {
                "type": "Insight",
                "title": _title_from_text(text),
                "body": text,
                "durability": "episodic",
                "source": "user",
            }
            r = self.session.post(f"{self._base_url}/nodes", json=body,
                                  timeout=self.timeout)
            r.raise_for_status()
            node = r.json()
            self._texts[node["id"]] = text
            return node["id"]
        return self._timed("inscribe", _do)

    def _search(self, query: str, limit: int, *, exclude_tombstones: bool) -> list[dict]:
        params = {"q": query, "limit": limit}
        if exclude_tombstones:
            params["types"] = _NON_TOMBSTONE_TYPES
        r = self.session.get(f"{self._base_url}/search", params=params,
                             timeout=self.timeout)
        r.raise_for_status()
        return r.json()

    def _hit_text(self, hit: dict) -> str:
        cached = self._texts.get(hit["id"])
        if cached is not None:
            return cached
        # Not one of ours (e.g. a Tombstone the server minted) -- fetch the
        # full node. Its title + body both count: the mint always names the
        # victim's title in the body's deletion notice regardless of
        # keep_text, so title alone can leak the fact (see README).
        def _do():
            r = self.session.get(f"{self._base_url}/nodes/{hit['id']}",
                                 timeout=self.timeout)
            r.raise_for_status()
            n = r.json()
            text = f"{n.get('title', '')}\n{n.get('body', '')}"
            self._texts[hit["id"]] = text
            return text
        return self._timed("node_fetch", _do)

    def recall_texts(self, query: str, k: int = 5) -> list[str]:
        def _do():
            # The role-aware reader asks the server for k NON-tombstone hits
            # (the REST `types` filter), so removal markers never occupy
            # top-k slots. The first 2026-09-12 run filtered client-side
            # AFTER taking top-k, which let markers push surviving peers
            # out of the window -- an adapter artifact, not a mechanism.
            hits = self._search(query, k,
                                exclude_tombstones=not self.include_tombstones)
            return [self._hit_text(h) for h in hits]
        return self._timed("recall", _do)

    # ─── mutations ──────────────────────────────────────────────────

    def supersede(self, old_query: str, new_text: str) -> None:
        def _do():
            hits = self._search(old_query, 1, exclude_tombstones=True)
            new_id = None
            if not hits:
                body = {
                    "type": "Insight", "title": _title_from_text(new_text),
                    "body": new_text, "durability": "episodic", "source": "user",
                }
                r = self.session.post(f"{self._base_url}/nodes", json=body,
                                      timeout=self.timeout)
                r.raise_for_status()
                new_id = r.json()["id"]
                self._texts[new_id] = new_text
                return
            old_id = hits[0]["id"]
            body = {
                "type": "Insight", "title": _title_from_text(new_text),
                "body": new_text, "durability": "episodic", "source": "user",
            }
            r = self.session.post(f"{self._base_url}/nodes", json=body,
                                  timeout=self.timeout)
            r.raise_for_status()
            new_id = r.json()["id"]
            self._texts[new_id] = new_text
            r = self.session.post(f"{self._base_url}/edges",
                                  json={"type": "replaces", "from_id": new_id,
                                        "to_id": old_id, "source": "user"},
                                  timeout=self.timeout)
            r.raise_for_status()
            self._texts.pop(old_id, None)
        self._timed("supersede", _do)

    def release(self, query: str) -> int:
        def _do():
            hits = self._search(query, 20, exclude_tombstones=True)
            if not hits:
                return 0
            scores = [h["score"] for h in hits]
            thr = _gap_threshold(scores)
            targets = [h["id"] for h in hits if h["score"] >= thr]
            for tid in targets:
                r = self.session.delete(
                    f"{self._base_url}/nodes/{tid}",
                    params={"tombstone": "true", "keep_text": "false",
                            "reason": "released"},
                    timeout=self.timeout)
                r.raise_for_status()
                self._texts.pop(tid, None)
            return len(targets)
        return self._timed("release", _do)

    def purge(self, query: str) -> int:
        def _do():
            hits = self._search(query, 20, exclude_tombstones=True)
            if not hits:
                return 0
            top_text = self._texts.get(hits[0]["id"])
            if top_text is None:
                top_text = self._hit_text(hits[0])
            target_norm = _norm_lexical(top_text)
            targets = [h["id"] for h in hits
                      if _norm_lexical(self._texts.get(h["id"], "")) == target_norm]
            for tid in targets:
                r = self.session.delete(f"{self._base_url}/nodes/{tid}",
                                        timeout=self.timeout)
                r.raise_for_status()
                self._texts.pop(tid, None)
            return len(targets)
        return self._timed("purge", _do)

    # ─── introspection helper for the runner (not part of the Adapter
    #     protocol) ────────────────────────────────────────────────
    def health(self) -> dict:
        r = self.session.get(f"{self.base}/health", timeout=self.timeout)
        r.raise_for_status()
        return r.json()
