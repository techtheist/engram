#!/usr/bin/env bash
# PostToolUse hook (Read|Edit|Write|MultiEdit): when the assistant touches a
# file whose path is covered by stored code_refs, inject the connected memory
# as context — the constraint finds you when you touch the code, regardless
# of what you searched for (PLAN §10 ambient hooks, candidate a).
#
# Noise control lives SERVER-side (/refs/match): non-stale nodes only,
# trust-ordered, capped per injection, and deduplicated per session — the
# same node is never injected twice in one session. This script only finds
# the daemon, asks, and wraps the answer.
#
# A memory hook must never break a session: every failure path exits 0 with
# no output. Requires python3 (JSON in, JSON out); silently inert without it.
# Daemon discovery mirrors hooks/session-brief.sh — fix both when fixing one.
set -u

command -v python3 >/dev/null 2>&1 || exit 0

# -P resolves symlinks so the path compares equal to the daemon's
# canonicalized /health db (macOS /tmp vs /private/tmp and friends).
ROOT="$(cd -P "${CLAUDE_PROJECT_DIR:-$PWD}" 2>/dev/null && pwd)" || exit 0

# Not an Engram-wired repo — stay silent. Either backend counts.
[ -e "$ROOT/.engram/graph.db" ] || [ -e "$ROOT/.engram/graph.tepin" ] || exit 0

# The Claude Code plugin ships this hook too (ENGRAM_HOOK_SOURCE=plugin).
# When the repo registers its own copy, the repo-level hook wins — matches
# must never inject twice.
if [ "${ENGRAM_HOOK_SOURCE:-}" = "plugin" ]; then
    grep -qsE 'file-read-match|engram-refs' \
        "$ROOT/.claude/settings.json" "$ROOT/.claude/settings.local.json" && exit 0
fi

ENGRAM_HOOK_ROOT="$ROOT" exec python3 -c "
import json, os, sys, urllib.parse, urllib.request

def get(url, timeout=3):
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return r.read().decode()
    except Exception:
        return None

def post(url, payload, timeout=3):
    try:
        req = urllib.request.Request(
            url, data=json.dumps(payload).encode(),
            headers={'content-type': 'application/json'})
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read().decode())
    except Exception:
        return None

def daemon_port(path):
    try:
        return json.load(open(path)).get('port')
    except Exception:
        return None

try:
    event = json.load(sys.stdin)
except Exception:
    sys.exit(0)

root = os.environ['ENGRAM_HOOK_ROOT']
path = (event.get('tool_input') or {}).get('file_path') or ''
session = event.get('session_id') or ''
if not path:
    sys.exit(0)
# The endpoint expects a repo-relative path; anything outside the repo is
# not this graph's business.
if os.path.isabs(path):
    real = os.path.realpath(path)
    if not real.startswith(root + os.sep):
        sys.exit(0)
    path = os.path.relpath(real, root)

def ask(port, prefix=''):
    q = urllib.parse.urlencode({'path': path, 'session': session})
    return get(f'http://127.0.0.1:{port}{prefix}/refs/match?{q}')

text = None
# Preferred: the repo's own daemon, verified by /health advertising this
# repo's store.
port = daemon_port(os.path.join(root, '.engram', 'daemon.json'))
if port:
    health = get(f'http://127.0.0.1:{port}/health', timeout=2) or ''
    # /health advertises the served db path; matching the repo's
    # '.engram/graph.' prefix covers both backends.
    if os.path.join(root, '.engram', 'graph.') in health:
        text = ask(port)
# Fallback: the machine core — register/resolve this repo, use the scoped
# route (the same gesture the MCP bridge makes).
if text is None:
    home = os.environ.get('ENGRAM_HOME') or os.path.expanduser('~/.engram')
    port = daemon_port(os.path.join(home, 'daemon.json'))
    if port:
        entry = post(f'http://127.0.0.1:{port}/projects', {'path': root})
        if entry and entry.get('id'):
            text = ask(port, f\"/projects/{entry['id']}\")

if not text or not text.strip():
    sys.exit(0)
print(json.dumps({
    'hookSpecificOutput': {
        'hookEventName': 'PostToolUse',
        'additionalContext': text.strip(),
    }
}))
"
