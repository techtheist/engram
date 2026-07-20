#!/usr/bin/env bash
# Deploy the Engram pane end-to-end, safely from ANY working directory:
#   1. build the Vue pane (bun)
#   2. reinstall the engram-alpha binary (the pane is rust-embedded)
#   3. restart the local daemon on this repo's DB (absolute path — a relative
#      --db from the wrong cwd silently creates a fresh empty DB)
#   4. verify /health serves the right DB and report the node count
#
# Usage: scripts/deploy-pane.sh [--vsix] [--jetbrains] [--fake]
#   --vsix       also repackage the VSCode extension (bundles the pane)
#   --jetbrains  also rebuild the JetBrains plugin zip
#   --fake       run the daemon with fake embeddings. Real is the default:
#                MCP writes real vectors, and a fake-embedding daemon
#                searching real-embedded nodes returns pure noise.
#
# Both humans and AI assistants should use this instead of hand-chaining
# cd/build/install/restart — the cwd mistakes are exactly what it prevents.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DB="$ROOT/.engram/graph.db"
# A migrated repo's graph lives in graph.tepin next to the old file; the
# binary resolves that itself — the health check must expect the same path.
[ -f "$ROOT/.engram/graph.tepin" ] && DB="$ROOT/.engram/graph.tepin"
LOG="$ROOT/.engram/serve.log"
BIN="$HOME/.cargo/bin/engram-alpha"

VSIX=0 JETBRAINS=0 EMBED_FLAG=""
for arg in "$@"; do
    case "$arg" in
        --vsix) VSIX=1 ;;
        --jetbrains) JETBRAINS=1 ;;
        --fake) EMBED_FLAG="--fake-embeddings" ;;
        --real) ;; # legacy no-op: real embeddings are the default
        *) echo "unknown flag: $arg" >&2; exit 2 ;;
    esac
done

echo "==> building pane"
(cd "$ROOT/frontend" && bun run build)

echo "==> reinstalling engram-alpha binary"
cargo install --path "$ROOT/crates/engram-cli" --force --quiet

echo "==> restarting daemon"
pkill -f "engram(-alpha)? serve" 2>/dev/null || true
sleep 1
mkdir -p "$ROOT/.engram"
# shellcheck disable=SC2086  # EMBED_FLAG is intentionally word-split (may be empty)
nohup "$BIN" serve --http-only $EMBED_FLAG --db "$DB" >"$LOG" 2>&1 &
disown

echo "==> waiting for health"
PORT=8787
for _ in $(seq 1 20); do
    sleep 0.5
    [ -f "$ROOT/.engram/daemon.json" ] &&
        PORT="$(sed -n 's/.*"port": \([0-9]*\).*/\1/p' "$ROOT/.engram/daemon.json")" &&
        curl -sf "http://127.0.0.1:${PORT}/health" >/dev/null && break
done

HEALTH="$(curl -sf "http://127.0.0.1:${PORT}/health")" || {
    echo "daemon failed to come up — see $LOG" >&2
    exit 1
}
case "$HEALTH" in
    *"$DB"*) ;;
    *) echo "daemon serves the WRONG db: $HEALTH" >&2; exit 1 ;;
esac
NODES="$(curl -sf "http://127.0.0.1:${PORT}/graph" | grep -o '"id"' | wc -l | tr -d ' ')"
echo "==> healthy on port $PORT, serving $DB (~$((NODES / 2)) nodes+edges rows)"

if [ "$VSIX" = 1 ]; then
    echo "==> packaging VSCode extension"
    (cd "$ROOT/engram-vscode" && npm run package | tail -1)
fi

if [ "$JETBRAINS" = 1 ]; then
    echo "==> building JetBrains plugin"
    (cd "$ROOT/engram-jetbrains" && ./gradlew buildPlugin -q >/dev/null && ls build/distributions/)
fi

echo "==> done"
