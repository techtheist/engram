#!/usr/bin/env bash
# Prefix every receipt in eval/results with its run date (file mtime, YYYY-MM-DD-)
# so a plain `ls` sorts them chronologically and the newest run is at the bottom.
# Idempotent: files already carrying a date prefix are skipped. Tracked files are
# moved with `git mv` so history follows the rename. Run after every eval run.
set -euo pipefail
dir="$(cd "$(dirname "$0")/.." && pwd)/eval/results"
cd "$dir"
for f in *; do
  [ -f "$f" ] || continue
  [[ "$f" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}- ]] && continue
  d=$(stat -f %Sm -t %Y-%m-%d "$f" 2>/dev/null || date -r "$f" +%Y-%m-%d)
  if git ls-files --error-unmatch "$f" >/dev/null 2>&1; then
    git mv "$f" "$d-$f"
  else
    mv "$f" "$d-$f"
  fi
  echo "$f -> $d-$f"
done
