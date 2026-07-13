#!/usr/bin/env bash
# SessionStart hook: print the Engram brief so the harness injects it as
# session context — the assistant starts every session already briefed
# instead of having to remember to call the `brief` tool (PLAN §10 hooks,
# candidate b).
#
# Portable by construction: Claude Code, Codex CLI, and Gemini CLI all treat
# a SessionStart hook's stdout as injected context, so this one script serves
# all three (only the settings registration differs per harness).
#
# A memory hook must never break a session: every failure path exits 0 with
# no output. Budget override: ENGRAM_BRIEF_CHARS (default 16000 — keep in
# sync with DEFAULT_BRIEF_CHARS in crates/engram-core/src/policy.rs).
set -u

# -P resolves symlinks so the path compares equal to the daemon's
# canonicalized /health db (macOS /tmp vs /private/tmp and friends).
ROOT="$(cd -P "${CLAUDE_PROJECT_DIR:-$PWD}" 2>/dev/null && pwd)" || exit 0
DB="$ROOT/.engram/graph.db"
MAX_CHARS="${ENGRAM_BRIEF_CHARS:-16000}"

# Not an Engram-wired repo (or a brand-new one) — stay silent.
[ -e "$DB" ] || exit 0

# The Claude Code plugin runs this script too (ENGRAM_HOOK_SOURCE=plugin).
# When the repo also registers its own copy (engram-alpha setup, or a checkout of
# engram itself), the repo-level hook wins — the brief must never inject twice.
if [ "${ENGRAM_HOOK_SOURCE:-}" = "plugin" ]; then
    grep -qsE 'engram-brief|session-brief' \
        "$ROOT/.claude/settings.json" "$ROOT/.claude/settings.local.json" && exit 0
fi

# Preferred source: the running daemon (fast; discovers the real port from
# daemon.json and trusts it only if /health advertises this repo's DB —
# port-walking makes cross-repo collisions routine).
if [ -f "$ROOT/.engram/daemon.json" ]; then
    PORT="$(sed -n 's/.*"port": \([0-9]*\).*/\1/p' "$ROOT/.engram/daemon.json")"
    if [ -n "$PORT" ]; then
        HEALTH="$(curl -sf --max-time 2 "http://127.0.0.1:${PORT}/health" 2>/dev/null || true)"
        case "$HEALTH" in
            *"$DB"*)
                BRIEF="$(curl -sf --max-time 5 "http://127.0.0.1:${PORT}/brief?max_chars=${MAX_CHARS}" 2>/dev/null || true)"
                if [ -n "$BRIEF" ]; then
                    printf '%s\n' "$BRIEF"
                    exit 0
                fi
                ;;
        esac
    fi
fi

# Fallback: read the DB directly (WAL — safe beside a daemon). The brief
# never embeds anything, so --fake-embeddings just skips the ONNX model load
# that would otherwise slow session start.
BIN="$(command -v engram-alpha)" 2>/dev/null || exit 0
"$BIN" brief --db "$DB" --max-chars "$MAX_CHARS" --fake-embeddings 2>/dev/null || true
exit 0
