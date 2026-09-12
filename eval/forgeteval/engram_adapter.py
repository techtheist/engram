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

import json
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


def _hit_is_tombstone(hit: dict) -> bool:
    """The 0.9.4 role flag, with a type-name fallback for a core that
    predates it (see Caution 00d36lue869d / Decision 00d36lue869d in this
    repo's own graph -- the flag is stamped by the engine from the
    ontology and serialized only when true, so its ABSENCE on a hit that
    has no `type` field either is not itself proof of "not a tombstone";
    the fallback only fires when the flag key is missing outright)."""
    if "tombstone" in hit:
        return bool(hit["tombstone"])
    return str(hit.get("type", "")).lower() == "tombstone"


class EngramMCPAdapter(EngramAdapter):
    """ForgetEval adapter variant `engram-mcp`.

    Identical to `EngramAdapter(include_tombstones=False)` ("engram-notomb")
    in every respect -- same floor-zeroed / knee_cliff=null policy override
    on its own throwaway project, same REST-driven inscribe/supersede/
    release/purge -- with exactly one difference: `recall_texts` is routed
    through the MCP `search` TOOL over HTTP (the same streamable-HTTP
    JSON-RPC transport a real MCP client speaks: POST .../mcp, an
    `mcp-session-id` from `initialize`, `notifications/initialized`, then
    `tools/call`), not the bare REST `/search` endpoint.

    Why this is a distinct adapter rather than a flag on EngramAdapter:
    the MCP tool's reply carries a `confidence` verdict (strong/weak/none)
    that the REST endpoint's JSON body also carries but that
    `EngramAdapter.recall_texts` never looks at -- ForgetEval's own
    substring-match scoring has no concept of "the system told you it
    wasn't sure," so measuring it needs a side channel. This adapter opens
    exactly that channel: every `recall_texts` call appends
    `{query, confidence, n_hits_raw, n_hits, all_tombstone}` to
    `self.stats["mcp_confidence_log"]`, which `run_engram.py` reads back
    to compute the FP/hedge/absence-signal numbers described in
    eval/forgeteval/README.md. The standard pass/fail is untouched: the
    texts handed back to the scoring loop are the same hits' texts either
    way (title+body, tombstones excluded).

    Tombstone filtering deliberately does NOT use the server-side `types`
    exclusion the REST `engram-notomb` variant uses -- it filters
    CLIENT-SIDE on each hit's `tombstone: true` flag (0.9.4), matching the
    task's ask to exercise that flag specifically and, as a side effect,
    letting `all_tombstone` in the log see the pre-filter hit set (a
    server-side type exclusion would hide that a release marker was
    found at all).

    Gotcha this class exists to route around: an MCP session's engine
    handle is captured ONCE, at session creation
    (`streamable_http_service_for` hands `StreamableHttpService` a factory
    closure that calls `Engram::for_project(hub, selector)` per NEW
    session -- see crates/engram-mcp/src/lib.rs) and is never rebound
    after that. `EngramAdapter.reset()` deletes the project's store file
    out from under the daemon so a plain REST call reopens a fresh engine
    on next touch -- correct for a REST caller, which re-resolves the
    engine on every request, but fatal for a LONG-LIVED MCP session: if
    the same `mcp-session-id` survives a `reset()`, every subsequent
    `search` call keeps querying the pre-reset engine object frozen in
    memory (still holding the FIRST case's data) instead of the reopened
    store, because nothing ever tells that session to rebind. This never
    happens in real usage -- nobody deletes a live project's store file
    out from under an open bridge session -- but ForgetEval's reset()
    protocol does exactly that every case, so this adapter forces a fresh
    MCP handshake after every reset (`reset()` below), trading one extra
    initialize round-trip per case for correctness.
    """

    name = "engram-mcp"

    def __init__(self, project_dir: str, *, base_url: str = ENGRAM_BASE,
                 timeout: float = 30.0, stats: dict | None = None,
                 disable_delivery_floor: bool = True):
        super().__init__(project_dir, include_tombstones=False,
                         base_url=base_url, timeout=timeout, stats=stats,
                         disable_delivery_floor=disable_delivery_floor)
        self.name = "engram-mcp"
        self._mcp_session_id: str | None = None
        self.stats.setdefault("mcp_confidence_log", [])

    # ─── MCP transport plumbing ─────────────────────────────────────

    def _mcp_url(self) -> str:
        return f"{self._base_url}/mcp"

    def _mcp_call(self, payload: dict) -> tuple[requests.Response, dict | None]:
        headers = {"Accept": "application/json, text/event-stream",
                  "Content-Type": "application/json"}
        if self._mcp_session_id:
            headers["mcp-session-id"] = self._mcp_session_id
        r = self.session.post(self._mcp_url(), headers=headers, json=payload,
                              timeout=self.timeout)
        r.raise_for_status()
        body = r.text
        if "text/event-stream" in r.headers.get("content-type", ""):
            datas = [ln[5:].strip() for ln in body.splitlines() if ln.startswith("data:")]
            body = datas[-1] if datas else ""
        return r, (json.loads(body) if body.strip() else None)

    def _mcp_ensure_session(self) -> None:
        if self._mcp_session_id:
            return
        r, init = self._mcp_call({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-03-26", "capabilities": {},
                      "clientInfo": {"name": "forgeteval-engram-mcp", "version": "0"}},
        })
        sid = r.headers.get("mcp-session-id")
        if not sid:
            raise RuntimeError(f"MCP initialize returned no mcp-session-id: {init}")
        self._mcp_session_id = sid
        self._mcp_call({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def reset(self) -> None:
        super().reset()
        # Force a fresh MCP handshake next call -- see the class docstring's
        # "Gotcha" paragraph. Without this, every case after the first
        # queries a frozen, pre-reset engine handle.
        self._mcp_session_id = None

    # ─── the one overridden protocol method ─────────────────────────

    def recall_texts(self, query: str, k: int = 5) -> list[str]:
        def _do():
            self._mcp_ensure_session()
            _, res = self._mcp_call({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "search",
                          "arguments": {"query": query, "limit": k, "detail": "full"}},
            })
            if not res or "result" not in res:
                raise RuntimeError(f"MCP search failed for {query!r}: {res}")
            result = res["result"]
            if result.get("isError"):
                raise RuntimeError(f"MCP search tool error for {query!r}: {result}")
            payload_text = result["content"][0]["text"]
            d = json.loads(payload_text)
            confidence = d.get("confidence")
            raw_hits = d.get("hits", [])
            texts = []
            for h in raw_hits:
                if _hit_is_tombstone(h):
                    continue
                title = h.get("title", "")
                body = h.get("body", h.get("snippet", ""))
                text = f"{title}\n{body}"
                texts.append(text)
                if h.get("id"):
                    self._texts[h["id"]] = text
            top_score = raw_hits[0].get("score") if raw_hits else None
            self.stats["mcp_confidence_log"].append({
                "query": query,
                "confidence": confidence,
                "n_hits_raw": len(raw_hits),
                "n_hits": len(texts),
                "all_tombstone": bool(raw_hits) and all(_hit_is_tombstone(h) for h in raw_hits),
                # Threshold-free companion to `confidence` (see README's
                # "Abstention" section, separation/AUC paragraph): the raw
                # top-hit score on engram's OWN native scale, pre-tombstone-
                # filter, None when the reply carried no hits at all.
                "top_score": top_score,
            })
            return texts
        return self._timed("recall", _do)
