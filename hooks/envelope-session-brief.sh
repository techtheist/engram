#!/usr/bin/env bash
# Devin CLI SessionStart hook: Devin injects context only through the
# hookSpecificOutput JSON envelope (plain stdout is dropped), so this wrapper
# runs the portable brief script installed next to it and wraps its markdown.
# Setup installs both under .devin/hooks/ — the shared script keeps one
# implementation of daemon discovery.
#
# A memory hook must never break a session: every failure path exits 0 with
# no output.
set -u

DIR="$(cd "$(dirname "$0")" 2>/dev/null && pwd)" || exit 0
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
