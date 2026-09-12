"""ForgetEval runner for the engram adapters (and, as an in-harness
reference, lethe's own bundled adapter). Wraps the upstream lethe repo's
generators/scoring (bench/forgeteval/{generate,adversarial,tests}.py)
without patching anything inside the clone.

Usage
-----
  # smoke (~20 cases/suite)
  python run_engram.py --adapter engram         --suite template    --limit 20
  python run_engram.py --adapter engram-notomb  --suite adversarial --limit 20
  python run_engram.py --adapter engram-mcp     --suite template    --limit 20 \\
      --project-dir /tmp/forgeteval-store-mcp
  python run_engram.py --adapter grep           --suite adversarial --limit 20

  # full receipts
  python run_engram.py --adapter engram         --suite template    --scale 200 \\
      --out ../results/2026-09-12-forgeteval-template.json
  python run_engram.py --adapter engram         --suite adversarial \\
      --out ../results/2026-09-12-forgeteval-adversarial.json
  python run_engram.py --adapter lethe          --suite template --scale 200 ...

See eval/forgeteval/README.md for the full protocol writeup, including the
"Abstention: what ForgetEval does not measure" section describing the
control-probe / hedge / absence-signal numbers every receipt now carries
alongside the standard pass/fail table.
"""
from __future__ import annotations

import argparse
import json
import random
import re
import statistics
import subprocess
import sys
import time
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
LETHE_REPO = HERE.parent / "data" / "lethe"
sys.path.insert(0, str(LETHE_REPO))
sys.path.insert(0, str(HERE))

from engram_adapter import EngramAdapter, EngramMCPAdapter, ENGRAM_BASE  # noqa: E402
from grep_adapter import GrepAdapter  # noqa: E402
from bench.forgeteval.generate import NAMES  # noqa: E402


ADAPTER_POLICY = {
    "engram": (
        "supersede: top-1 GET /search (types=non-Tombstone) resolves old_query, "
        "then POST /nodes + POST /edges type=replaces (mirrors LetheAdapter.supersede's "
        "recall(k=1)). release: GET /search?limit=20 (types=non-Tombstone), "
        "adaptive-gap threshold over the score column (LetheAdapter._gap_threshold, "
        "min_gap=0.05) selects the release set, each DELETEd with "
        "?tombstone=true&keep_text=false&reason=released. purge: same 20-deep search, "
        "then NFKC/lower/whitespace-equivalence dedup against the top hit's ORIGINAL "
        "inscribed text (LetheAdapter._norm_lexical, ported verbatim), each hard-DELETEd. "
        "recall_texts: GET /search?limit=k, Tombstone-typed hits included (honest headline: "
        "the tombstone marker is itself findable). No policy/config tuning -- runs against "
        "engram's default calibrated-delivery policy exactly as shipped."
    ),
    "engram-notomb": (
        "Identical to 'engram' in every mutating operation. Differs only in "
        "recall_texts: Tombstone-typed hits are filtered out of the search result "
        "before scoring, isolating the effect of the tombstone-marker leak from the "
        "underlying release/purge mechanism."
    ),
    "lethe": "Upstream bench/forgeteval/adapter.py::LetheAdapter, llm=None (deterministic "
             "primitives only): supersede=top-1 atomic surrender; release=hybrid recall + "
             "adaptive-gap; purge=lexical recall + NFKC-lexical dedup.",
    "engram-mcp": (
        "Identical to 'engram-notomb' in reset/inscribe/supersede/release/purge (all REST, "
        "same floor-zeroed/knee_cliff=null policy override). recall_texts instead drives the "
        "MCP 'search' tool over the streamable-HTTP JSON-RPC transport (initialize -> "
        "notifications/initialized -> tools/call), reading the reply's confidence verdict "
        "(strong/weak/none) and filtering tombstone-role hits client-side on each hit's "
        "'tombstone: true' flag (0.9.4) rather than the server-side 'types' exclusion the "
        "REST variants use. Every recall_texts call logs {query, confidence, n_hits_raw, "
        "n_hits, all_tombstone} to stats['mcp_confidence_log'], which feeds the controls/"
        "hedge/absence_signal numbers below -- see README's 'Abstention' section."
    ),
    "grep": (
        "eval/forgeteval/grep_adapter.py::GrepAdapter -- no daemon, no embeddings. Recall "
        "scores each stored fact by content-word overlap (lowercase, stopwords + single-char "
        "tokens dropped) with the query, ranks by overlap count descending (ties broken by "
        "insertion order), and declines (empty list) when the overlap is zero everywhere. "
        "supersede/release/purge mirror the same top-1 / adaptive-gap / lexical-dedup rules "
        "engram mirrors from LetheAdapter, run over the overlap-score column instead of "
        "cosine similarity."
    ),
}

# ─── control-probe infrastructure (abstention measurement) ─────────────
#
# ForgetEval's own scoring never asks a control question -- every case's
# `must_contain`/`must_not_contain` check is about entities the case itself
# inscribed. That leaves a system's false-positive rate on subjects it was
# NEVER told about entirely unmeasured: a system that answers confidently
# about someone it has never heard of would pass every standard ForgetEval
# case while being unusable. This adds exactly that measurement without
# touching the upstream scoring: after each case's setup+mutations run to
# completion (tc.run(adapter) below), three CONTROL queries about names the
# case never mentioned are put to the SAME adapter instance, in the SAME
# live state, before it is reset for the next case.
#
# Templates are grouped by GeneratedCase.family (both suites' cases carry
# one) and lifted near-verbatim from bench/forgeteval/generate.py's own
# final_query f-strings for that family, so a control probe reads exactly
# like a real ForgetEval question -- just about nobody the case ever
# inscribed.
CONTROL_TEMPLATES: dict[str, list[str]] = {
    "supersession": [
        "Where does {name} work?",
        "What theme does {name} use?",
        "What is {name}'s diet?",
    ],
    "decay": [
        "Any one-time codes for {name}?",
        "What are {name}'s travel plans?",
        "Tell me about {name}.",
    ],
    "amnesia": [
        "What does {name} like to eat?",
        "What has {name} accomplished?",
        "Tell me about {name}.",
    ],
    "purge": [
        "Tell me about {name}.",
        "What is the API key for {name}?",
        "Show me customer data for {name}.",
    ],
    "drift": [
        "Where does {name} work?",
        "What is {name}'s address?",
        "What is {name}'s favorite color?",
    ],
}
_CONTROL_FALLBACK = ["Tell me about {name}.", "Where does {name} work?"]

_NAME_RE_CACHE: dict[str, re.Pattern] = {}


def _name_in_text(name: str, text: str) -> bool:
    pat = _NAME_RE_CACHE.get(name)
    if pat is None:
        pat = re.compile(r"\b" + re.escape(name) + r"\b", re.IGNORECASE)
        _NAME_RE_CACHE[name] = pat
    return bool(pat.search(text))


def _names_used_in_case(case, names_pool: list[str]) -> set[str]:
    haystacks = list(case.setup_facts) + [case.final_query]
    for m in case.mutations:
        haystacks.extend(str(x) for x in m[1:])
    blob = " \n ".join(haystacks)
    return {n for n in names_pool if _name_in_text(n, blob)}


def build_controls(case, names_pool: list[str], seed: int) -> list[str]:
    """3 control queries about names never inscribed in `case`, deterministic
    given (seed, case.id). Returns formatted query strings."""
    rng = random.Random(f"{seed}:{case.id}:controls")
    used = _names_used_in_case(case, names_pool)
    available = [n for n in names_pool if n not in used]
    if len(available) < 3:
        available = names_pool  # pool exhausted (shouldn't happen at 30 names)
    names = rng.sample(available, 3)
    templates = CONTROL_TEMPLATES.get(case.family, _CONTROL_FALLBACK)
    picks = rng.choices(templates, k=3)
    return [tmpl.format(name=name) for tmpl, name in zip(picks, names)]


def _control_answered(adapter, texts: list[str]) -> bool:
    """An adapter ANSWERS a control iff it returns >=1 text AND does not
    decline. lethe/grep decline exactly when they return no text (already
    covered by the emptiness check); engram-mcp additionally declines when
    its own verdict on THIS call was weak/none, even if it still returned
    nearest-candidate texts (0.8.1's "likely not in memory" delivery-anyway
    behavior -- see this repo's own README/chronicle)."""
    if not texts:
        return False
    if getattr(adapter, "name", "") == "engram-mcp":
        log = adapter.stats.get("mcp_confidence_log") or []
        if log and log[-1].get("confidence") in ("weak", "none"):
            return False
    return True


def _release_based(case) -> bool:
    return any(m[0] == "release" for m in case.mutations)


# ─── separation (threshold-free companion to fp/hedge) ─────────────────
#
# fp/hedge are both read off a FIXED verdict line (engram-mcp's weak/none
# cut). If that line is miscalibrated for a tiny per-case graph -- which
# the hedge numbers above suggest it is -- fp/hedge alone cannot tell
# "the scores don't separate answerable from control questions" apart from
# "the scores separate fine, but the line is drawn wrong." `separation`
# answers that question directly, independent of any threshold: it looks
# only at each adapter's own native top score (engram-mcp's raw hit score,
# lethe's cosine similarity, grep's overlap count) for every real
# final_query call (the "answerable" / positive class) and every control
# probe (the negative class), and asks whether the positive scores rank
# above the negative ones AT ALL.
_SCORE_LOG_KEY = {
    "engram-mcp": "mcp_confidence_log",
    "lethe": "lethe_score_log",
    "grep": "grep_score_log",
}


def top_score_for(adapter, query: str) -> float | None:
    """The most recent recall_texts(query) call's top score, on that
    adapter's own native scale (None if that adapter doesn't log one, or
    if the reply/recall carried no hits/matches at all). Read-only: this
    never issues a new call, it reads back what recall_texts already
    logged as a side effect for the query that was just run."""
    key = _SCORE_LOG_KEY.get(getattr(adapter, "name", ""))
    if not key:
        return None
    log = adapter.stats.get(key) or []
    if log and log[-1].get("query") == query:
        return log[-1].get("top_score")
    return None


def _auc(pos_scores: list[float | None], neg_scores: list[float | None]) -> float | None:
    """Mann-Whitney U / rank-sum AUC: the probability that a uniformly
    random positive (answerable) top score exceeds a uniformly random
    negative (control) top score, with ties counting as 0.5 -- computed
    via ranks (O(n log n)), not the O(n*m) pairwise definition, so it
    stays fast at full-run scale (1000+ cases x 3 controls).

    `None` (no hits/matches at all) is treated as the LOWEST score
    observed in this comparison, minus a margin -- an explicit decline is
    exactly the behavior separation should credit on a control and
    penalize on an answerable query, so it is never dropped from the
    sample. Returns None only if either class is empty (undefined AUC)."""
    if not pos_scores or not neg_scores:
        return None
    finite = [v for v in pos_scores + neg_scores if v is not None]
    floor = (min(finite) - 1.0) if finite else 0.0
    pos = [v if v is not None else floor for v in pos_scores]
    neg = [v if v is not None else floor for v in neg_scores]
    combined = sorted([(v, 0) for v in pos] + [(v, 1) for v in neg], key=lambda t: t[0])
    n = len(combined)
    ranks = [0.0] * n
    i = 0
    while i < n:
        j = i
        while j < n and combined[j][0] == combined[i][0]:
            j += 1
        avg_rank = (i + 1 + j) / 2.0  # average of 1-indexed ranks i+1..j
        for idx in range(i, j):
            ranks[idx] = avg_rank
        i = j
    rank_sum_pos = sum(r for r, (_v, grp) in zip(ranks, combined) if grp == 0)
    n_pos, n_neg = len(pos), len(neg)
    return (rank_sum_pos - n_pos * (n_pos + 1) / 2.0) / (n_pos * n_neg)


def _mean(vals: list[float]) -> float | None:
    vals = [v for v in vals if v is not None]
    return round(sum(vals) / len(vals), 4) if vals else None


def git_rev(repo: Path) -> str:
    try:
        return subprocess.run(["git", "rev-parse", "HEAD"], cwd=repo,
                              capture_output=True, text=True, check=True
                              ).stdout.strip()
    except Exception as e:
        return f"<unknown: {e}>"


def engram_health() -> dict:
    import requests
    try:
        r = requests.Session()
        r.trust_env = False
        resp = r.get(f"{ENGRAM_BASE}/health", timeout=5)
        return resp.json()
    except Exception as e:
        return {"error": str(e)}


def build_adapter(name: str, project_dir: str | None, stats: dict):
    if name in ("engram", "engram-notomb"):
        if not project_dir:
            raise SystemExit("--project-dir is required for engram/engram-notomb")
        return EngramAdapter(project_dir, include_tombstones=(name == "engram"),
                             stats=stats)
    if name == "engram-mcp":
        if not project_dir:
            raise SystemExit("--project-dir is required for engram-mcp")
        return EngramMCPAdapter(project_dir, stats=stats)
    if name == "grep":
        return GrepAdapter(stats=stats)
    if name == "lethe":
        from bench.forgeteval.adapter import LetheAdapter

        class ScoreCapturingLethe(LetheAdapter):
            """Same as upstream LetheAdapter (nothing in eval/data/lethe is
            patched -- this is a plain Python subclass defined here, in our
            own file) with one addition: `recall_texts` also logs the top
            hit's cosine SIMILARITY -- Lethe's own native ranking scale,
            the same `.similarity` field `release()`'s adaptive-gap
            threshold already reads -- to `stats["lethe_score_log"]`. Used
            only for the threshold-free `separation` (AUC) measurement;
            the texts returned to the standard scoring loop are identical
            to the upstream method's."""

            def recall_texts(self, query: str, k: int = 5) -> list[str]:
                results = self.lethe.recall(query, k=k, hybrid=False)
                top = float(results[0].similarity) if results else None
                self.stats.setdefault("lethe_score_log", []).append({
                    "query": query, "top_score": top,
                })
                return [r.memory.text for r in results]

        print("loading embedder: sentence-transformers/all-MiniLM-L6-v2", file=sys.stderr)
        from fastembed import TextEmbedding
        model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
        def embedder(text: str) -> list[float]:
            return list(next(iter(model.embed([text]))))
        adapter = ScoreCapturingLethe(embedder=embedder, vector_dim=384)
        adapter.stats = stats
        return adapter
    raise SystemExit(f"unknown adapter {name!r}")


def load_test_set(suite: str, *, scale: int, seed: int, distractors: int):
    if suite == "template":
        from bench.forgeteval.generate import generate
        return generate(scale, seed=seed, distractors=distractors, lang="en")
    if suite == "adversarial":
        from bench.forgeteval.adversarial import ADVERSARIAL_TESTS
        return list(ADVERSARIAL_TESTS)
    raise SystemExit(f"unknown suite {suite!r}")


def bucket_for(suite: str, case) -> str:
    if suite == "adversarial":
        from bench.forgeteval.adversarial import case_to_attack_category
        return case_to_attack_category(case.id)
    return case.family


def smoke_sample(test_set: list, n: int) -> list:
    if n <= 0 or n >= len(test_set):
        return test_set
    stride = max(1, len(test_set) // n)
    return test_set[::stride][:n]


def summarize_timing(stats: dict) -> dict:
    out = {}
    for k, vals in stats.items():
        if not vals or not isinstance(vals[0], (int, float)):
            continue  # e.g. EngramMCPAdapter's mcp_confidence_log: a list of
                      # dicts, not durations -- reported separately below
        s = sorted(vals)
        out[k] = {
            "count": len(vals),
            "mean_ms": round(statistics.mean(s) * 1000, 2),
            "p50_ms": round(s[len(s) // 2] * 1000, 2),
            "p95_ms": round(s[min(len(s) - 1, int(len(s) * 0.95))] * 1000, 2),
            "max_ms": round(s[-1] * 1000, 2),
        }
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--adapter", required=True,
                    choices=["engram", "engram-notomb", "engram-mcp", "grep", "lethe"])
    ap.add_argument("--suite", required=True, choices=["template", "adversarial"])
    ap.add_argument("--scale", type=int, default=200,
                    help="template only: cases per family (200 -> 1000 total)")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--distractors", type=int, default=4)
    ap.add_argument("--limit", type=int, default=0,
                    help="if >0, run only a strided sample of this many cases (smoke mode)")
    ap.add_argument("--project-dir", default=None,
                    help="engram/engram-notomb/engram-mcp: the registered project directory")
    ap.add_argument("--out", default=None, help="write a JSON receipt here")
    args = ap.parse_args()

    if sys.stdout.encoding.lower() != "utf-8":
        sys.stdout.reconfigure(encoding="utf-8")

    test_set = load_test_set(args.suite, scale=args.scale, seed=args.seed,
                             distractors=args.distractors)
    n_generated = len(test_set)
    if args.limit:
        test_set = smoke_sample(test_set, args.limit)

    stats: dict[str, list[float]] = defaultdict(list)
    adapter = build_adapter(args.adapter, args.project_dir, stats)

    print(f"\nrunning {len(test_set)} / {n_generated} {args.suite} cases against "
          f"{adapter.name}...\n")

    is_mcp = adapter.name == "engram-mcp"

    cases_out = []
    by_bucket: dict[str, list[tuple[str, bool, str | None]]] = defaultdict(list)
    controls_by_bucket: dict[str, dict] = defaultdict(lambda: {"asked": 0, "answered": 0})
    hedge_by_bucket: dict[str, dict] = defaultdict(lambda: {"weak_or_none": 0, "total": 0})
    absence_by_bucket: dict[str, dict] = defaultdict(
        lambda: {"qualifying": 0, "forgets_and_says_so": 0})
    answerable_scores_by_bucket: dict[str, list] = defaultdict(list)
    control_scores_by_bucket: dict[str, list] = defaultdict(list)
    started = time.perf_counter()
    for tc in test_set:
        bucket = bucket_for(args.suite, tc)
        t0 = time.perf_counter()
        try:
            passed = tc.run(adapter)
            err = None
        except NotImplementedError as e:
            passed = False
            err = f"N/A (capability not supported): {e}"
        except Exception as e:
            passed = False
            err = f"{type(e).__name__}: {e}"
        dt = time.perf_counter() - t0
        by_bucket[bucket].append((tc.id, passed, err))

        # ─── abstention measurement (§3 of the README's "Abstention"
        # section): hedge on the case's own final_query, then 3 control
        # probes about names this case never mentioned, against the SAME
        # live adapter state (no reset in between) ────────────────────
        mcp_log = adapter.stats.get("mcp_confidence_log") if is_mcp else None
        final_confidence = None
        final_all_tombstone = False
        if mcp_log:
            last = mcp_log[-1]
            if last.get("query") == tc.final_query:
                final_confidence = last.get("confidence")
                final_all_tombstone = bool(last.get("all_tombstone"))
        hedge_by_bucket[bucket]["total"] += 1
        if final_confidence in ("weak", "none"):
            hedge_by_bucket[bucket]["weak_or_none"] += 1

        qualifies_for_absence = is_mcp and (tc.family in ("decay", "amnesia")
                                            or _release_based(tc))
        if qualifies_for_absence:
            absence_by_bucket[bucket]["qualifying"] += 1
            if final_confidence in ("weak", "none") or final_all_tombstone:
                absence_by_bucket[bucket]["forgets_and_says_so"] += 1

        # separation (threshold-free companion): the final_query call's own
        # top score, on the adapter's native scale -- read back from
        # whatever recall_texts already logged as a side effect inside
        # tc.run() above, BEFORE the control loop below overwrites the log's
        # last entry.
        answerable_scores_by_bucket[bucket].append(top_score_for(adapter, tc.final_query))

        controls = build_controls(tc, NAMES, args.seed)
        c_answered = 0
        for cq in controls:
            try:
                ctexts = adapter.recall_texts(cq, k=5)
                if _control_answered(adapter, ctexts):
                    c_answered += 1
                control_scores_by_bucket[bucket].append(top_score_for(adapter, cq))
            except Exception as e:
                print(f"    (control probe error, treated as declined: {e})",
                      file=sys.stderr)
                control_scores_by_bucket[bucket].append(None)
        controls_by_bucket[bucket]["asked"] += len(controls)
        controls_by_bucket[bucket]["answered"] += c_answered

        cases_out.append({
            "id": tc.id, "bucket": bucket, "passed": passed, "error": err,
            "duration_seconds": round(dt, 4),
            "final_confidence": final_confidence,
            "controls_asked": len(controls), "controls_answered": c_answered,
        })
        mark = "PASS" if passed else ("N/A " if err and "N/A" in err else "FAIL")
        conf_tag = f" conf={final_confidence}" if final_confidence else ""
        print(f"  [{mark}] {bucket:26s} {tc.id:40s} {dt*1000:7.1f}ms"
              f" ctrl={c_answered}/{len(controls)}{conf_tag}"
              + (f"  ({err})" if err else ""))

    wall = time.perf_counter() - started

    agg = {}
    total_pass = total_fail = total_na = 0
    for bucket, rows in sorted(by_bucket.items()):
        n = len(rows)
        p = sum(1 for _, ok, _ in rows if ok)
        na = sum(1 for _, ok, e in rows if (not ok) and e and "N/A" in e)
        f = n - p - na
        total_pass += p
        total_fail += f
        total_na += na
        agg[bucket] = {"pass": p, "fail": f, "na": na, "total": n,
                       "rate": round(p / n, 4) if n else 0.0}
    total = total_pass + total_fail + total_na

    controls_agg = {}
    ctl_asked_total = ctl_answered_total = 0
    for bucket, c in sorted(controls_by_bucket.items()):
        ctl_asked_total += c["asked"]
        ctl_answered_total += c["answered"]
        controls_agg[bucket] = {
            "asked": c["asked"], "answered": c["answered"],
            "fp": round(c["answered"] / c["asked"], 4) if c["asked"] else 0.0,
        }
    controls_totals = {
        "asked": ctl_asked_total, "answered": ctl_answered_total,
        "fp": round(ctl_answered_total / max(ctl_asked_total, 1), 4),
    }

    hedge_agg = {}
    hedge_weak_total = hedge_n_total = 0
    for bucket, h in sorted(hedge_by_bucket.items()):
        hedge_weak_total += h["weak_or_none"]
        hedge_n_total += h["total"]
        hedge_agg[bucket] = round(h["weak_or_none"] / h["total"], 4) if h["total"] else 0.0
    hedge_total = round(hedge_weak_total / max(hedge_n_total, 1), 4)
    if not is_mcp:
        hedge_note = "0 by construction: this adapter carries no verdict/abstention signal"
    else:
        hedge_note = ("share of real final_query calls where engram-mcp's own verdict "
                      "was weak or none (its cost side)")

    absence_agg = {}
    abs_hit_total = abs_qual_total = 0
    for bucket, a in sorted(absence_by_bucket.items()):
        abs_hit_total += a["forgets_and_says_so"]
        abs_qual_total += a["qualifying"]
        absence_agg[bucket] = {
            "qualifying": a["qualifying"], "forgets_and_says_so": a["forgets_and_says_so"],
            "rate": round(a["forgets_and_says_so"] / a["qualifying"], 4) if a["qualifying"] else 0.0,
        }
    absence_totals = {
        "qualifying": abs_qual_total, "forgets_and_says_so": abs_hit_total,
        "rate": round(abs_hit_total / max(abs_qual_total, 1), 4),
    }
    absence_note = ("engram-specific, informational: decay/amnesia (template) and any "
                    "release-based case (adversarial) where the final query's verdict was "
                    "weak/none or every returned hit was a tombstone -- \"forgets AND says "
                    "so\". 0/0 for every other adapter (no verdict/tombstone-flag signal).")

    # separation (threshold-free companion to fp/hedge) -- see the long
    # comment above top_score_for/_auc. Every adapter here (engram-mcp,
    # lethe, grep) logs a native-scale top score; engram/engram-notomb
    # don't (no logging hook), so their separation is reported as
    # undefined (n_answerable = n_control = 0) rather than a fabricated 0.
    separation_agg = {}
    all_answerable: list[float | None] = []
    all_control: list[float | None] = []
    for bucket in sorted(set(answerable_scores_by_bucket) | set(control_scores_by_bucket)):
        pos = answerable_scores_by_bucket.get(bucket, [])
        neg = control_scores_by_bucket.get(bucket, [])
        all_answerable.extend(pos)
        all_control.extend(neg)
        separation_agg[bucket] = {
            "auc": _auc(pos, neg), "answerable_mean": _mean(pos), "control_mean": _mean(neg),
            "n_answerable": len(pos), "n_control": len(neg),
        }
    separation_totals = {
        "auc": _auc(all_answerable, all_control),
        "answerable_mean": _mean(all_answerable), "control_mean": _mean(all_control),
        "n_answerable": len(all_answerable), "n_control": len(all_control),
    }
    separation_note = (
        "threshold-free: AUC = P(a random answerable top-score > a random control "
        "top-score), ties=0.5, missing/empty scored as the observed floor. Says whether "
        "this adapter's OWN scores could support a decline rule at all, independent of "
        "where fp/hedge's fixed verdict line happens to sit. n/a (0 answerable/0 control) "
        "for engram/engram-notomb -- no native score is logged for them."
    )

    print(f"\n{'='*88}\n  ForgetEval — {adapter.name} — {args.suite}\n{'='*88}")
    print(f"  {'bucket':<28} {'pass':>5} / {'total':>5}  na  rate     fp(ctrl)  hedge   sep.AUC")
    for bucket, a in sorted(agg.items()):
        c = controls_agg.get(bucket, {"fp": 0.0})
        h = hedge_agg.get(bucket, 0.0)
        s = separation_agg.get(bucket, {"auc": None})
        auc_str = f"{s['auc']:.2f}" if s["auc"] is not None else "n/a"
        print(f"  {bucket:<28} {a['pass']:>5} / {a['total']:>5}  {a['na']:>2}  "
              f"{a['rate']:>5.0%}    {c['fp']:>5.0%}    {h:>5.0%}   {auc_str:>6}")
    print(f"  {'-'*28} {'-'*5}   {'-'*5}")
    total_auc_str = (f"{separation_totals['auc']:.2f}"
                     if separation_totals["auc"] is not None else "n/a")
    print(f"  {'OVERALL':<28} {total_pass:>5} / {total:>5}  {total_na:>2}  "
          f"{(total_pass/max(total,1)):>5.0%}    {controls_totals['fp']:>5.0%}    "
          f"{hedge_total:>5.0%}   {total_auc_str:>6}")
    if total_na:
        print(f"  (N/A: {total_na} cases skipped -- adapter lacks the operation)")
    print(f"  fp(ctrl) = share of control probes (never-inscribed names) answered, lower is "
          f"better ({ctl_answered_total}/{ctl_asked_total})")
    print(f"  hedge    = {hedge_note}")
    if is_mcp:
        print(f"  absence signal (informational): {absence_totals['forgets_and_says_so']}/"
              f"{absence_totals['qualifying']} = {absence_totals['rate']:.0%} "
              f"(\"forgets AND says so\" on decay/amnesia/release-based cases)")
    if separation_totals["auc"] is not None:
        print(f"  sep.AUC  = {separation_note}")
        print(f"             answerable mean={separation_totals['answerable_mean']} "
              f"(n={separation_totals['n_answerable']}), control mean="
              f"{separation_totals['control_mean']} (n={separation_totals['n_control']})")
    print(f"\n  Wall: {wall:.1f}s  ({wall/max(len(test_set),1)*1000:.1f} ms/case)")
    timing = summarize_timing(stats)
    if timing:
        print("\n  per-op timing (ms):")
        for op, t in sorted(timing.items()):
            print(f"    {op:<12} n={t['count']:<5} mean={t['mean_ms']:<8} "
                  f"p50={t['p50_ms']:<8} p95={t['p95_ms']:<8} max={t['max_ms']}")

    if args.out:
        receipt = {
            "benchmark": "ForgetEval",
            "source": "https://github.com/deeplethe/lethe (bench/forgeteval), arXiv:2606.15903",
            "suite": args.suite,
            "adapter": adapter.name,
            "adapter_policy": ADAPTER_POLICY.get(adapter.name, ""),
            "engram_health": engram_health() if args.adapter not in ("lethe", "grep") else None,
            "lethe_git_rev": git_rev(LETHE_REPO),
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "params": {
                "scale": args.scale, "seed": args.seed,
                "distractors": args.distractors, "limit": args.limit,
                "n_generated": n_generated, "n_run": len(test_set),
            },
            "wall_seconds": round(wall, 3),
            "timing_ms": timing,
            "aggregate": agg,
            "totals": {"pass": total_pass, "fail": total_fail, "na": total_na,
                      "total": total, "rate": round(total_pass / max(total, 1), 4)},
            "controls": {"by_bucket": controls_agg, "totals": controls_totals},
            "hedge": {"by_bucket": hedge_agg, "total": hedge_total, "note": hedge_note},
            "absence_signal": {"by_bucket": absence_agg, "totals": absence_totals,
                               "note": absence_note},
            "separation": {"by_bucket": separation_agg, "totals": separation_totals,
                          "note": separation_note},
            "cases": cases_out,
        }
        out_path = Path(args.out)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(receipt, indent=2))
        print(f"\n  wrote {out_path}")


if __name__ == "__main__":
    main()
