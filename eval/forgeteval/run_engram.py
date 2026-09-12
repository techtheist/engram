"""ForgetEval runner for the engram adapters (and, as an in-harness
reference, lethe's own bundled adapter). Wraps the upstream lethe repo's
generators/scoring (bench/forgeteval/{generate,adversarial,tests}.py)
without patching anything inside the clone.

Usage
-----
  # smoke (~20 cases/suite)
  python run_engram.py --adapter engram         --suite template    --limit 20
  python run_engram.py --adapter engram-notomb  --suite adversarial --limit 20

  # full receipts
  python run_engram.py --adapter engram         --suite template    --scale 200 \\
      --out ../results/2026-09-12-forgeteval-template.json
  python run_engram.py --adapter engram         --suite adversarial \\
      --out ../results/2026-09-12-forgeteval-adversarial.json
  python run_engram.py --adapter lethe          --suite template --scale 200 ...

See eval/forgeteval/README.md for the full protocol writeup.
"""
from __future__ import annotations

import argparse
import json
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

from engram_adapter import EngramAdapter, ENGRAM_BASE  # noqa: E402


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
}


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
    if name == "lethe":
        from bench.forgeteval.adapter import LetheAdapter
        print("loading embedder: sentence-transformers/all-MiniLM-L6-v2", file=sys.stderr)
        from fastembed import TextEmbedding
        model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
        def embedder(text: str) -> list[float]:
            return list(next(iter(model.embed([text]))))
        return LetheAdapter(embedder=embedder, vector_dim=384)
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
        if not vals:
            continue
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
                    choices=["engram", "engram-notomb", "lethe"])
    ap.add_argument("--suite", required=True, choices=["template", "adversarial"])
    ap.add_argument("--scale", type=int, default=200,
                    help="template only: cases per family (200 -> 1000 total)")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--distractors", type=int, default=4)
    ap.add_argument("--limit", type=int, default=0,
                    help="if >0, run only a strided sample of this many cases (smoke mode)")
    ap.add_argument("--project-dir", default=None,
                    help="engram/engram-notomb: the registered project directory")
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

    cases_out = []
    by_bucket: dict[str, list[tuple[str, bool, str | None]]] = defaultdict(list)
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
        cases_out.append({
            "id": tc.id, "bucket": bucket, "passed": passed, "error": err,
            "duration_seconds": round(dt, 4),
        })
        mark = "PASS" if passed else ("N/A " if err and "N/A" in err else "FAIL")
        print(f"  [{mark}] {bucket:26s} {tc.id:40s} {dt*1000:7.1f}ms"
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

    print(f"\n{'='*64}\n  ForgetEval — {adapter.name} — {args.suite}\n{'='*64}")
    print(f"  {'bucket':<28} {'pass':>5} / {'total':>5}  na  rate")
    for bucket, a in sorted(agg.items()):
        print(f"  {bucket:<28} {a['pass']:>5} / {a['total']:>5}  {a['na']:>2}  {a['rate']:>5.0%}")
    print(f"  {'-'*28} {'-'*5}   {'-'*5}")
    print(f"  {'OVERALL':<28} {total_pass:>5} / {total:>5}  {total_na:>2}  "
          f"{(total_pass/max(total,1)):>5.0%}")
    if total_na:
        print(f"  (N/A: {total_na} cases skipped -- adapter lacks the operation)")
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
            "engram_health": engram_health() if args.adapter != "lethe" else None,
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
            "cases": cases_out,
        }
        out_path = Path(args.out)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(receipt, indent=2))
        print(f"\n  wrote {out_path}")


if __name__ == "__main__":
    main()
