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
                "cases": r["cases"],
            }
            for name, r in receipts.items()
        },
    }
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(combined, indent=2))
    print(f"wrote {out_path}  adapters={list(receipts.keys())}  "
         f"totals={{k: v['totals'] for k, v in combined['adapters'].items()}}")
    for name, a in combined["adapters"].items():
        print(f"  {name:16s} {a['totals']}")


if __name__ == "__main__":
    main()
