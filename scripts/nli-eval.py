#!/usr/bin/env python3
"""Phase-A eval for the local-cortex NLI layer (PLAN §7A).

Every judged suspect in an Engram graph is a labeled NLI example on real
project prose: `conflict` ≈ contradiction, `replaces` ≈ (directional)
entailment, `dismiss` ≈ neutral. This script turns a graph.db into that
corpus and scores candidate cross-encoders on it, so the model choice is
made on OUR domain, not on MNLI's.

Dev tool only — needs Python with `transformers` + `torch` installed
(`pip install transformers torch`). Never a runtime dependency: the daemon
stays Rust + ONNX.

Usage:
  scripts/nli-eval.py [--db .engram/graph.db] [--models m1 m2 …] [--export corpus.jsonl]
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import sys
from pathlib import Path

DEFAULT_MODELS = [
    "dleemiller/finecat-nli-m",
    "tasksource/ModernBERT-base-nli",
]

# suspect verdict -> expected NLI label for the (newer=a, older=b) pair.
# `replaces` is directional: the newer statement should entail (subsume) the
# older one; contradiction is symmetric; dismissed pairs should be neutral.
VERDICT_LABEL = {
    "conflict": "contradiction",
    "replaces": "entailment",
    "dismiss": "neutral",
}


def claim_text(title: str, body: str | None) -> str:
    """A node's canonical claim: the (declarative, skill-enforced) title,
    plus the body's first sentence when present — enough context to judge,
    short enough to stay in-distribution for sentence-pair models."""
    text = title.strip()
    if body:
        first = body.strip().replace("\n", " ").split(". ")[0].strip()
        if first and first.lower() not in text.lower():
            text = f"{text}. {first}"
    return text


def load_corpus(db: Path) -> list[dict]:
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        """SELECT s.id, s.status, s.a_id, s.b_id,
                  a.title AS a_title, a.body AS a_body,
                  b.title AS b_title, b.body AS b_body
           FROM suspects s
           JOIN nodes a ON a.id = s.a_id
           JOIN nodes b ON b.id = s.b_id
           WHERE s.status != 'suspected'"""
    ).fetchall()

    corpus = []
    for r in rows:
        if r["status"] == "dismissed":
            verdict = "dismiss"
        else:
            # Confirmed pairs: recover WHICH verdict from the edge it created.
            edge = conn.execute(
                """SELECT type FROM edges
                   WHERE (from_id=? AND to_id=?) OR (from_id=? AND to_id=?)
                   ORDER BY created_at DESC""",
                (r["a_id"], r["b_id"], r["b_id"], r["a_id"]),
            ).fetchone()
            if edge is None:
                continue
            verdict = {"conflicts-with": "conflict", "replaces": "replaces"}.get(edge["type"])
            if verdict is None:
                continue
        corpus.append(
            {
                "suspect": r["id"],
                "premise": claim_text(r["a_title"], r["a_body"]),  # newer
                "hypothesis": claim_text(r["b_title"], r["b_body"]),  # older
                "verdict": verdict,
                "label": VERDICT_LABEL[verdict],
            }
        )
    conn.close()
    return corpus


def evaluate(model_name: str, corpus: list[dict]) -> None:
    from transformers import AutoModelForSequenceClassification, AutoTokenizer
    import torch

    tok = AutoTokenizer.from_pretrained(model_name)
    model = AutoModelForSequenceClassification.from_pretrained(model_name)
    model.eval()
    id2label = {i: l.lower() for i, l in model.config.id2label.items()}

    def infer(premise: str, hypothesis: str) -> str:
        enc = tok(premise, hypothesis, return_tensors="pt", truncation=True, max_length=1024)
        with torch.no_grad():
            logits = model(**enc).logits[0]
        return id2label[int(logits.argmax())]

    per_label: dict[str, list[bool]] = {}
    confusion: dict[tuple[str, str], int] = {}
    for ex in corpus:
        # Contradiction is symmetric; entailment (replaces) reads newer→older.
        got = infer(ex["premise"], ex["hypothesis"])
        per_label.setdefault(ex["label"], []).append(got == ex["label"])
        confusion[(ex["label"], got)] = confusion.get((ex["label"], got), 0) + 1

    total = sum(len(v) for v in per_label.values())
    correct = sum(sum(v) for v in per_label.values())
    print(f"\n== {model_name} ==  overall {correct}/{total} ({correct / total:.1%})")
    for label, results in sorted(per_label.items()):
        print(f"  {label:<14} {sum(results)}/{len(results)} ({sum(results) / len(results):.1%})")
    print("  confusion (expected -> got):")
    for (exp, got), n in sorted(confusion.items()):
        if exp != got:
            print(f"    {exp} -> {got}: {n}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--db", type=Path, default=Path(".engram/graph.db"))
    ap.add_argument("--models", nargs="*", default=DEFAULT_MODELS)
    ap.add_argument("--export", type=Path, help="also write the corpus as JSONL")
    args = ap.parse_args()

    if not args.db.exists():
        sys.exit(f"no graph at {args.db} — run from a repo with an Engram graph")
    corpus = load_corpus(args.db)
    counts = {}
    for ex in corpus:
        counts[ex["label"]] = counts.get(ex["label"], 0) + 1
    print(f"corpus: {len(corpus)} judged pairs from {args.db}  {counts}")
    if args.export:
        with open(args.export, "w") as f:
            for ex in corpus:
                f.write(json.dumps(ex) + "\n")
        print(f"exported to {args.export}")
    if not corpus:
        sys.exit("no judged suspects yet — judge some pairs first, then re-run")

    for model in args.models:
        evaluate(model, corpus)


if __name__ == "__main__":
    main()
