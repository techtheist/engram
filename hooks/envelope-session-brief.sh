#!/usr/bin/env bash
# SessionStart hook wrapper for harnesses that inject context ONLY through
# the hookSpecificOutput JSON envelope — plain stdout is dropped there. Devin
# CLI and Codex CLI share this contract, so setup installs this same wrapper
# for both (as engram-brief.sh), next to the portable brief script (as
# engram-brief-text.sh) — one implementation of daemon discovery.
#
# A memory hook must never break a session: every failure path exits 0 with
# no output.
set -u

DIR="$(cd "$(dirname "$0")" 2>/dev/null && pwd)" || exit 0

# The session may start in a subdirectory (codex allows it); the brief script
# roots itself at CLAUDE_PROJECT_DIR, so point that at the repo root.
if [ -z "${CLAUDE_PROJECT_DIR:-}" ]; then
    CLAUDE_PROJECT_DIR="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
    export CLAUDE_PROJECT_DIR
fi

BRIEF="$("$DIR/engram-brief-text.sh" 2>/dev/null)" || true
[ -n "${BRIEF:-}" ] || exit 0

# JSON-escape with awk: backslash first, then quotes; tabs escaped, carriage
# returns dropped, newlines re-joined as \n between records.
printf '%s' "$BRIEF" | awk '
BEGIN { printf "{\"hookSpecificOutput\":{\"hookEventName\":\"SessionStart\",\"additionalContext\":\"" }
{
    gsub(/\\/, "\\\\"); gsub(/"/, "\\\""); gsub(/\t/, "\\t"); gsub(/\r/, "")
    if (NR > 1) printf "\\n"
    printf "%s", $0
}
END { print "\"}}" }'
exit 0
