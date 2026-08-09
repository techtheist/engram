#!/usr/bin/env bash
# Measure the daemon's memory footprint through a realistic workload.
#
# The daemon holds three ONNX sessions for the process lifetime, and what it
# costs is not a single number — it is a curve. This probe makes that curve
# visible: what a cold daemon costs, what each cortex layer adds the first
# time it runs, and — the number that actually matters for a process that
# lives in the background all day — whether any of it comes back when idle.
#
# Run it before and after any change to `engram_core::onnx`. Guessing at what
# holds the memory is how you end up optimising the wrong thing: the arena
# allocator and the allocator's free lists both looked like the culprit here
# and both measured as red herrings. Batch width was the answer.
#
# It runs against an isolated COPY of the graph on its own port with its own
# ENGRAM_HOME, so it never touches the live daemon, the machine registry, or
# the real store's single-writer lock.
#
# Usage: scripts/mem-probe.sh [--label NAME] [--db PATH] [--idle SECONDS]
#                             [--bin PATH] [--json OUT] [--fake] [--keep]
#
# On macOS the number reported is `phys_footprint` (what Activity Monitor
# shows), which counts compressed pages — RSS alone reads near-zero on an idle
# daemon because the compressor has already eaten it. On Linux it is VmRSS.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LABEL="probe"
SRC_DB=""
IDLE=30
BIN="$HOME/.cargo/bin/engram-alpha"
JSON_OUT=""
KEEP=0
PORT=8799
# --fake: run without any ONNX model, to separate store cost from model cost
FAKE=""
STARTUP_WHAT="(3 models)"

while [ $# -gt 0 ]; do
    case "$1" in
        --label) LABEL="$2"; shift 2 ;;
        --db)    SRC_DB="$2"; shift 2 ;;
        --idle)  IDLE="$2"; shift 2 ;;
        --bin)   BIN="$2"; shift 2 ;;
        --json)  JSON_OUT="$2"; shift 2 ;;
        --port)  PORT="$2"; shift 2 ;;
        --fake)  FAKE="--fake-embeddings"; STARTUP_WHAT="(no models)"; shift ;;
        --keep)  KEEP=1; shift ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
done

if [ -z "$SRC_DB" ]; then
    SRC_DB="$ROOT/.engram/graph.db"
    [ -f "$ROOT/.engram/graph.tepin" ] && SRC_DB="$ROOT/.engram/graph.tepin"
fi
[ -f "$SRC_DB" ] || { echo "no graph at $SRC_DB" >&2; exit 1; }
[ -x "$BIN" ] || { echo "no binary at $BIN" >&2; exit 1; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/engram-memprobe.XXXXXX")"
cleanup() {
    [ -n "${DAEMON_PID:-}" ] && kill "$DAEMON_PID" 2>/dev/null || true
    [ "$KEEP" = 1 ] || rm -rf "$WORK"
}
trap cleanup EXIT

mkdir -p "$WORK/home" "$WORK/graph"
DB="$WORK/graph/$(basename "$SRC_DB")"
cp "$SRC_DB" "$DB"
export ENGRAM_HOME="$WORK/home"

# --- platform-specific footprint read ------------------------------------
# macOS: phys_footprint (Activity Monitor's number; counts compressed pages).
# Linux: VmRSS. Both in KB, normalized to MB by the caller.
footprint_kb() {
    local pid="$1"
    if [ "$(uname)" = "Darwin" ]; then
        /usr/bin/footprint -p "$pid" 2>/dev/null |
            awk '/phys_footprint:/ {
                    v = $2; u = $3
                    if (u ~ /^GB/) v *= 1024 * 1024
                    else if (u ~ /^MB/) v *= 1024
                    print int(v); exit
                 }'
    else
        awk '/^VmRSS:/ {print $2; exit}' "/proc/$pid/status" 2>/dev/null
    fi
}

STAGE_NAMES=()
STAGE_KB=()
record() {
    local name="$1"
    local kb
    kb="$(footprint_kb "$DAEMON_PID")"
    [ -n "$kb" ] || kb=0
    STAGE_NAMES+=("$name")
    STAGE_KB+=("$kb")
    printf '  %-26s %8.1f MB\n' "$name" "$(echo "$kb" | awk '{print $1/1024}')"
}

api() { curl -sf --max-time 120 "http://127.0.0.1:$PORT$1"; }

echo "==> $LABEL"
echo "    binary $BIN"
echo "    graph  $(basename "$SRC_DB") ($(du -h "$DB" | cut -f1))"
echo

# shellcheck disable=SC2086  # FAKE is intentionally word-split (may be empty)
"$BIN" serve --http-only --http-port "$PORT" $FAKE --db "$DB" >"$WORK/serve.log" 2>&1 &
DAEMON_PID=$!

for _ in $(seq 1 240); do
    sleep 0.5
    api /health >/dev/null 2>&1 && break
done
api /health >/dev/null 2>&1 || {
    echo "daemon never came up — log:" >&2
    cat "$WORK/serve.log" >&2
    exit 1
}
# The cortex loads lazily-but-eagerly relative to the port bind; give the
# reranker and NLI a beat to finish so "startup" means all three sessions.
sleep 3
record "startup ${STARTUP_WHAT}"

# One embed-only query: exercises the embedder session alone.
api "/search?q=retrieval%20eval%20ladder&limit=8" >/dev/null
record "after 1 search"

# Ten more: the reranker session is in this path (fetch = limit*3, clamped
# 12..50), so this is the cross-encoder's widest working set.
for q in "conflict scan suspects" "tepindb store trait" "attention metrics focus noise" \
         "phantom probe weak line" "delivery floor calibration" "auto-tune conflict floor" \
         "supersession retires archived" "session brief hook" "model selection hot swap" \
         "vector index rebuild"; do
    api "/search?q=$(printf '%s' "$q" | sed 's/ /%20/g')&limit=8" >/dev/null
done
record "after 10 searches"

# The NLI session's turn: check_claim judges up to 16 premise/hypothesis
# pairs at 512 tokens through DeBERTa — the heaviest single request the
# daemon serves.
curl -sf --max-time 180 -X POST "http://127.0.0.1:$PORT/claims/check" \
    -H 'content-type: application/json' \
    -d '{"text":"The cross-encoder reranker is what spends the oblique recall budget","limit":16}' \
    >/dev/null || echo "  (check_claim failed — NLI may be unavailable)" >&2
record "after check_claim (NLI)"

# The heaviest sweep in the product: re-embeds and NLI-judges across the graph.
curl -sf --max-time 600 -X POST "http://127.0.0.1:$PORT/conflicts/scan" \
    -H 'content-type: application/json' -d '{}' >/dev/null \
    || echo "  (conflict scan failed)" >&2
record "after conflict scan"

echo "    idling ${IDLE}s…"
sleep "$IDLE"
record "after ${IDLE}s idle"

echo
PEAK=0
for kb in "${STAGE_KB[@]}"; do [ "$kb" -gt "$PEAK" ] && PEAK="$kb"; done
FINAL="${STAGE_KB[${#STAGE_KB[@]}-1]}"
START="${STAGE_KB[0]}"
awk -v p="$PEAK" -v f="$FINAL" -v s="$START" 'BEGIN {
    printf "    peak %.1f MB · idle %.1f MB · reclaimed %.1f MB (%.0f%% of growth)\n",
        p/1024, f/1024, (p-f)/1024, (p > s) ? 100*(p-f)/(p-s) : 0
}'

if [ -n "$JSON_OUT" ]; then
    {
        printf '{\n  "label": %s,\n  "stages": [\n' "\"$LABEL\""
        for i in "${!STAGE_NAMES[@]}"; do
            sep=","
            [ "$i" -eq $((${#STAGE_NAMES[@]} - 1)) ] && sep=""
            printf '    {"stage": "%s", "kb": %s}%s\n' \
                "${STAGE_NAMES[$i]}" "${STAGE_KB[$i]}" "$sep"
        done
        printf '  ]\n}\n'
    } >"$JSON_OUT"
    echo "    receipt $JSON_OUT"
fi
