"""Combine the per-adapter run_engram.py JSON receipts for one suite into
the single deliverable file eval/results/<date>-forgeteval-<suite>.json.

Usage: python merge_receipts.py <suite> <out.json> <adapter1.json> [adapter2.json ...]
"""
from __future__ import annotations

import json
import sys
from datetime import datetime, timezone
from pathlib import Path


def main() -> None:
    suite = sys.argv[1]
    out_path = Path(sys.argv[2])
    in_paths = [Path(p) for p in sys.argv[3:]]

    receipts = {}
    for p in in_paths:
        r = json.loads(p.read_text())
        receipts[r["adapter"]] = r

    any_r = next(iter(receipts.values()))
    combined = {
        "benchmark": "ForgetEval",
        "source": any_r["source"],
        "suite": suite,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "lethe_git_rev": any_r["lethe_git_rev"],
        "engram_health": next((r["engram_health"] for r in receipts.values()
                              if r.get("engram_health")), None),
        "adapters": {
            name: {
                "adapter_policy": r["adapter_policy"],
                "params": r["params"],
                "wall_seconds": r["wall_seconds"],
                "timing_ms": r["timing_ms"],
                "aggregate": r["aggregate"],
                "totals": r["totals"],
                # Abstention measurement (added alongside the engram-mcp/grep
                # adapters -- see README's "Abstention" section). Older
                # receipts predate these keys, so default to an explicit
                # empty/zeroed shape rather than KeyError.
                "controls": r.get("controls", {"by_bucket": {}, "totals":
                            {"asked": 0, "answered": 0, "fp": 0.0}}),
                "hedge": r.get("hedge", {"by_bucket": {}, "total": 0.0,
                        "note": "not measured in this receipt"}),
                "absence_signal": r.get("absence_signal", {"by_bucket": {}, "totals":
                        {"qualifying": 0, "forgets_and_says_so": 0, "rate": 0.0},
                        "note": "not measured in this receipt"}),
                "separation": r.get("separation", {"by_bucket": {}, "totals":
                        {"auc": None, "answerable_mean": None, "control_mean": None,
                         "n_answerable": 0, "n_control": 0},
                        "note": "not measured in this receipt"}),
                "cases": r["cases"],
            }
            for name, r in receipts.items()
        },
    }
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(combined, indent=2))
    print(f"wrote {out_path}  adapters={list(receipts.keys())}")

    # Standard pass rate NEXT TO the abstention numbers -- fp alone rewards
    # a mute system (see README's "Abstention" section), so it is never
    # printed without the recall it comes with. sep.AUC is the
    # threshold-free companion: whether an adapter's own scores COULD
    # support a decline rule at all, independent of fp/hedge's fixed line.
    print(f"\n  {'adapter':<16} {'pass rate':>10}   {'fp(ctrl)':>9}   "
         f"{'hedge':>7}   {'sep.AUC':>7}   absence")
    for name, a in combined["adapters"].items():
        t = a["totals"]
        fp = a["controls"]["totals"]["fp"]
        hedge = a["hedge"]["total"]
        abz = a["absence_signal"]["totals"]
        abz_str = (f"{abz['forgets_and_says_so']}/{abz['qualifying']} "
                  f"({abz['rate']:.0%})") if abz["qualifying"] else "n/a"
        sep = a["separation"]["totals"]
        auc_str = f"{sep['auc']:.2f}" if sep.get("auc") is not None else "n/a"
        print(f"  {name:<16} {t['pass']:>4}/{t['total']:<4} ({t['rate']:.0%})  "
             f"{fp:>8.0%}   {hedge:>6.0%}   {auc_str:>7}   {abz_str}")
    print("\n  fp(ctrl) lower is better; read next to pass rate, not alone (a mute "
         "system scores fp=0%). hedge/absence are 0/n.a. by construction for any "
         "adapter without a verdict signal (lethe, grep, engram, engram-notomb). "
         "sep.AUC is threshold-free (P(answerable score > control score), 0.5 = no "
         "separation, 1.0 = perfect) -- n/a for engram/engram-notomb, which log no "
         "native score.")


if __name__ == "__main__":
    main()
