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
# no output. Daemon discovery (daemon.json port + /health db match, then the
# machine core's scoped route) is mirrored in hooks/file-read-match.sh — fix
# both when fixing one. Budget override: ENGRAM_BRIEF_CHARS (default 16000 —
# keep in sync with DEFAULT_BRIEF_CHARS in crates/engram-core/src/policy.rs).
set -u

# -P resolves symlinks so the path compares equal to the daemon's
# canonicalized /health db (macOS /tmp vs /private/tmp and friends).
ROOT="$(cd -P "${CLAUDE_PROJECT_DIR:-$PWD}" 2>/dev/null && pwd)" || exit 0
DB="$ROOT/.engram/graph.db"
MAX_CHARS="${ENGRAM_BRIEF_CHARS:-16000}"

# Not an Engram-wired repo (or a brand-new one) — stay silent. Either
# backend counts: tepin-born repos never have a graph.db.
[ -e "$DB" ] || [ -e "${DB%.db}.tepin" ] || exit 0

# The Claude Code plugin runs this script too (ENGRAM_HOOK_SOURCE=plugin).
# When the repo also registers its own copy (engram-alpha setup, or a checkout of
# engram itself), the repo-level hook wins — the brief must never inject twice.
if [ "${ENGRAM_HOOK_SOURCE:-}" = "plugin" ]; then
    grep -qsE 'engram-brief|session-brief' \
        "$ROOT/.claude/settings.json" "$ROOT/.claude/settings.local.json" && exit 0
fi

daemon_port() { sed -n 's/.*"port": \([0-9]*\).*/\1/p' "$1" 2>/dev/null | head -1; }

# Preferred source: a running daemon — either the repo's own or the machine
# core, which serves every project from one port. Ask for THIS project by its
# directory (`?project=`): the core is rooted at the home graph, so its plain
# /brief is home's canon — a "cold start" digest that looks exactly like an
# empty project. GET, never POST: a brief is a read and registers nothing.
#
# A daemon that predates `?project=` ignores the parameter and answers with
# its own launch project instead — silently the wrong graph. Probing with a
# selector that can resolve to nothing tells the two apart without version
# arithmetic: a daemon that understands it refuses (400), one that doesn't
# hands back a brief. Older daemons are served by the CLI fallback below,
# which reaches them through the per-project route they do understand.
BRIEF=""
for CANDIDATE in "$ROOT/.engram/daemon.json" "${ENGRAM_HOME:-$HOME/.engram}/daemon.json"; do
    [ -f "$CANDIDATE" ] || continue
    PORT="$(daemon_port "$CANDIDATE")"
    [ -n "$PORT" ] || continue
    curl -sfG --max-time 3 --data-urlencode "project=/engram-no-such-project" \
        --data "max_chars=1" "http://127.0.0.1:${PORT}/brief" >/dev/null 2>&1 && continue
    BRIEF="$(curl -sfG --max-time 5 \
        --data-urlencode "project=$ROOT" --data-urlencode "max_chars=$MAX_CHARS" \
        "http://127.0.0.1:${PORT}/brief" 2>/dev/null || true)"
    [ -n "$BRIEF" ] && break
done

if [ -n "$BRIEF" ]; then
    printf '%s\n' "$BRIEF"
    exit 0
fi

# Fallback: the CLI, itself a thin client to whatever daemon owns the store
# (it opens the DB directly only when nothing does; the brief never embeds
# anything, so --fake-embeddings just skips the ONNX load that would slow
# session start). `command -v` alone is not enough — hooks run with a
# login-less PATH that often carries neither install dir, and exiting on that
# produced an empty brief indistinguishable from an empty graph.
BIN="$(command -v engram-alpha 2>/dev/null || true)"
for CANDIDATE in "$HOME/.cargo/bin/engram-alpha" "$HOME/.local/bin/engram-alpha" \
    /usr/local/bin/engram-alpha /opt/homebrew/bin/engram-alpha; do
    [ -n "$BIN" ] && break
    [ -x "$CANDIDATE" ] && BIN="$CANDIDATE"
done
[ -n "$BIN" ] || exit 0
"$BIN" brief --db "$DB" --max-chars "$MAX_CHARS" --fake-embeddings 2>/dev/null || true
exit 0
