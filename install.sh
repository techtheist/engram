#!/usr/bin/env sh
# Engram installer — sets up Engram for the current repository.
#
#   curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh
#
# Run it from your project's root. It will:
#   1. detect your platform and download the engram binary from GitHub Releases
#      (checksum-verified) into ~/.local/bin
#   2. wire the repo for your AI assistant(s): MCP registration + capture
#      instructions, per --cli (see below)
#
# Windows: run this inside WSL2 — it detects WSL and installs the native
# Windows binary (engram.exe), which WSL runs transparently via interop.
#
# Options:
#   --cli claude|codex|gemini|opencode|kilo|antigravity|all
#                    assistants to wire (comma-separated; default: claude)
#   --skill relaxed|normal|aggressive   Claude capture intensity (default: relaxed)
#   --bin-only                          install the binary, skip repo wiring
# Environment:
#   ENGRAM_VERSION   pin a release tag (default: latest)
#   ENGRAM_BIN_DIR   install directory (default: ~/.local/bin)

set -eu

REPO="techtheist/engram"
BIN_DIR="${ENGRAM_BIN_DIR:-$HOME/.local/bin}"
VERSION="${ENGRAM_VERSION:-latest}"
SKILL="relaxed"
CLIS="claude"
BIN_ONLY=0

while [ $# -gt 0 ]; do
    case "$1" in
        --cli) CLIS="$2"; shift 2 ;;
        --cli=*) CLIS="${1#--cli=}"; shift ;;
        --skill) SKILL="$2"; shift 2 ;;
        --skill=*) SKILL="${1#--skill=}"; shift ;;
        --bin-only) BIN_ONLY=1; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done
case "$SKILL" in relaxed|normal|aggressive) ;; *)
    echo "error: --skill must be relaxed, normal, or aggressive" >&2; exit 2 ;;
esac
[ "$CLIS" = "all" ] && CLIS="claude,codex,gemini,opencode,kilo,antigravity"
for c in $(printf '%s' "$CLIS" | tr ',' ' '); do
    case "$c" in claude|codex|gemini|opencode|kilo|antigravity) ;; *)
        echo "error: unknown --cli '$c' (claude|codex|gemini|opencode|kilo|antigravity|all)" >&2; exit 2 ;;
    esac
done

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null || die "curl is required"

# ---- platform ---------------------------------------------------------------
OS="$(uname -s)" ARCH="$(uname -m)" EXT="tar.gz" BIN="engram"
case "$OS" in
    Darwin)
        case "$ARCH" in
            arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
            x86_64) die "no prebuilt binary for Intel Macs (onnxruntime upstream dropped them) — build from source: cargo install --path crates/engram-cli" ;;
            *) die "unsupported macOS arch: $ARCH" ;;
        esac ;;
    Linux)
        [ "$ARCH" = "x86_64" ] || die "unsupported Linux arch: $ARCH (x86_64 only for now)"
        if grep -qi microsoft /proc/version 2>/dev/null; then
            # WSL: use the native Windows binary so the daemon runs Windows-side.
            # Raw .exe, not the zip — fresh WSL distros ship without unzip.
            TARGET="x86_64-pc-windows-msvc" EXT="exe" BIN="engram.exe"
        else
            TARGET="x86_64-unknown-linux-gnu"
        fi ;;
    MINGW*|MSYS*|CYGWIN*)
        TARGET="x86_64-pc-windows-msvc" EXT="exe" BIN="engram.exe" ;;
    *) die "unsupported OS: $OS" ;;
esac

# ---- resolve version --------------------------------------------------------
if [ "$VERSION" = "latest" ]; then
    VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
        sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')"
    [ -n "$VERSION" ] || die "could not resolve the latest release tag"
fi
ASSET="engram-$VERSION-$TARGET.$EXT"
URL="https://github.com/$REPO/releases/download/$VERSION/$ASSET"

# ---- download + verify + install --------------------------------------------
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

say "downloading $ASSET ($VERSION)"
# --progress-bar: the binary is ~15 MB and slow links deserve feedback.
curl -fL --progress-bar -o "$TMP/$ASSET" "$URL" || die "download failed: $URL"
curl -fsSL -o "$TMP/$ASSET.sha256" "$URL.sha256" || die "checksum download failed"

say "verifying checksum"
(
    cd "$TMP"
    if command -v sha256sum >/dev/null; then sha256sum -c "$ASSET.sha256" >/dev/null
    else shasum -a 256 -c "$ASSET.sha256" >/dev/null; fi
) || die "checksum mismatch — refusing to install"

say "installing $BIN to $BIN_DIR"
mkdir -p "$BIN_DIR"
case "$EXT" in
    tar.gz) tar -xzf "$TMP/$ASSET" -C "$TMP" && mv "$TMP/$BIN" "$BIN_DIR/$BIN" ;;
    exe)    mv "$TMP/$ASSET" "$BIN_DIR/$BIN" ;;
esac
chmod +x "$BIN_DIR/$BIN"

case ":$PATH:" in *":$BIN_DIR:"*) ;; *)
    say "NOTE: $BIN_DIR is not on your PATH — add it to your shell profile" ;;
esac

[ "$BIN_ONLY" = 1 ] && { say "done (binary only)"; exit 0; }

# ---- repo wiring ------------------------------------------------------------
REPO_DIR="$PWD"
BIN_PATH="$BIN_DIR/$BIN"
DB_PATH="$REPO_DIR/.engram/graph.db"
say "preparing this repository ($REPO_DIR) for: $CLIS"

# .gitignore: the graph is personal — never commit it.
if [ -f .gitignore ]; then
    grep -q '^\.engram/$' .gitignore || printf '\n# Engram local graph (personal)\n.engram/\n' >> .gitignore
else
    printf '# Engram local graph (personal)\n.engram/\n' > .gitignore
fi

# The harness-neutral capture instructions. One block, delivered to each
# CLI's expected instructions file; markers keep re-runs idempotent.
agents_block() {
    cat <<'BLOCK'
<!-- engram:begin -->
## Engram — durable project memory (MCP server: `engram`)

This project keeps a local, user-owned knowledge graph of its *reasoning*:
Decisions and their reasons, Principles, Cautions that bit us, Problems and
Resolutions, Insights, open Intents. Not code structure — the code holds that.

**Recall.** Call the `brief` tool once at session start and read it before
planning. Before any non-trivial decision, `search` the graph; hits carry
their 1-hop neighbors — read `conflicts-with` / `replaces` edges first. A hit
marked `stale: true` has decayed trust: verify it before relying on it, and
refresh it with `update_node` if it's still accurate.

**Capture.** At natural stopping points, silently record durable knowledge
with `add_note` and connect it with `link` using sentence-shaped edges
(because / answers / about / builds-on / replaces / conflicts-with / needs).
If `add_note` returns `{matched, created: false}`, merge into the match with
`update_node` instead of duplicating. Never store secrets, credentials, or
volatile implementation detail (line numbers, transient state).

**Trust.** `approve_node` is restricted: only on the user's explicit demand,
or after verifying a node's content word-by-word. Routine "still relevant"
signals are `update_node`, never approval.

The user sees and curates the graph at http://127.0.0.1:8787 — started with
`engram serve` in the repo root (one daemon per repo).
<!-- engram:end -->
BLOCK
}

append_agents_section() { # $1 = target file
    if [ -f "$1" ] && grep -q '<!-- engram:begin -->' "$1"; then
        say "$1 already has the Engram section — leaving it"
    else
        [ -f "$1" ] && printf '\n' >> "$1"
        agents_block >> "$1"
        say "wrote Engram section to $1"
    fi
}

json_mcp_snippet() { # stdout: the server entry for mcpServers-style configs
    printf '"engram": { "command": "%s", "args": ["mcp", "--db", "%s"] }' "$BIN_PATH" "$DB_PATH"
}

wire_claude() {
    if [ -f .mcp.json ]; then
        if grep -q '"engram"' .mcp.json; then
            say ".mcp.json already has an engram server — leaving it untouched"
        else
            say ".mcp.json exists — add this to its mcpServers block manually:"
            printf '    %s\n' "$(json_mcp_snippet)"
        fi
    else
        cat > .mcp.json <<JSON
{
  "mcpServers": {
    "engram": {
      "command": "$BIN_PATH",
      "args": ["mcp", "--db", "$DB_PATH"]
    }
  }
}
JSON
        say "wrote .mcp.json"
    fi
    mkdir -p .claude/skills/engram
    curl -fsSL -o .claude/skills/engram/SKILL.md \
        "https://raw.githubusercontent.com/$REPO/$VERSION/skills/engram/$SKILL/SKILL.md" ||
        die "skill download failed"
    say "installed the '$SKILL' capture skill to .claude/skills/engram"
}

wire_codex() {
    # Codex CLI's MCP config is global (~/.codex/config.toml). No --db there:
    # engram resolves .engram/graph.db against the cwd, so the same entry
    # serves every repo — as long as codex is launched from the repo root.
    CODEX_TOML="$HOME/.codex/config.toml"
    if [ -f "$CODEX_TOML" ] && grep -q '\[mcp_servers\.engram\]' "$CODEX_TOML"; then
        say "codex: ~/.codex/config.toml already has engram — leaving it"
    else
        mkdir -p "$HOME/.codex"
        cat >> "$CODEX_TOML" <<TOML

# Engram — durable project memory (db resolves per-repo against the cwd)
[mcp_servers.engram]
command = "$BIN_PATH"
args = ["mcp"]
TOML
        say "codex: registered engram in ~/.codex/config.toml (launch codex from the repo root)"
    fi
    append_agents_section AGENTS.md
}

wire_gemini() {
    if [ -f .gemini/settings.json ]; then
        if grep -q '"engram"' .gemini/settings.json; then
            say "gemini: .gemini/settings.json already has engram — leaving it"
        else
            say "gemini: .gemini/settings.json exists — add this to its mcpServers manually:"
            printf '    %s\n' "$(json_mcp_snippet)"
        fi
    else
        mkdir -p .gemini
        cat > .gemini/settings.json <<JSON
{
  "mcpServers": {
    "engram": {
      "command": "$BIN_PATH",
      "args": ["mcp", "--db", "$DB_PATH"]
    }
  }
}
JSON
        say "gemini: wrote .gemini/settings.json"
    fi
    append_agents_section GEMINI.md
}

wire_antigravity() {
    # Antigravity reads workspace MCP servers from .agents/mcp_config.json
    # (standard mcpServers shape) and instructions from AGENTS.md.
    AG_CONF=".agents/mcp_config.json"
    if [ -f "$AG_CONF" ]; then
        if grep -q '"engram"' "$AG_CONF"; then
            say "antigravity: $AG_CONF already has engram — leaving it"
        else
            say "antigravity: $AG_CONF exists — add this to its mcpServers manually:"
            printf '    %s\n' "$(json_mcp_snippet)"
        fi
    else
        mkdir -p .agents
        cat > "$AG_CONF" <<JSON
{
  "mcpServers": {
    "engram": {
      "command": "$BIN_PATH",
      "args": ["mcp", "--db", "$DB_PATH"]
    }
  }
}
JSON
        say "antigravity: wrote $AG_CONF"
    fi
    append_agents_section AGENTS.md
}

wire_opencode_style() { # $1 = config file (opencode.json / kilo.json), $2 = name
    if [ -f "$1" ]; then
        if grep -q '"engram"' "$1"; then
            say "$2: $1 already has engram — leaving it"
        else
            say "$2: $1 exists — add this to its \"mcp\" block manually:"
            printf '    "engram": { "type": "local", "command": ["%s", "mcp", "--db", "%s"], "enabled": true }\n' "$BIN_PATH" "$DB_PATH"
        fi
    else
        cat > "$1" <<JSON
{
  "mcp": {
    "engram": {
      "type": "local",
      "command": ["$BIN_PATH", "mcp", "--db", "$DB_PATH"],
      "enabled": true
    }
  }
}
JSON
        say "$2: wrote $1"
    fi
    append_agents_section AGENTS.md
}

for c in $(printf '%s' "$CLIS" | tr ',' ' '); do
    case "$c" in
        claude)   wire_claude ;;
        codex)    wire_codex ;;
        gemini)   wire_gemini ;;
        opencode)    wire_opencode_style opencode.json opencode ;;
        kilo)        wire_opencode_style kilo.json kilo ;;
        antigravity) wire_antigravity ;;
    esac
done

cat <<DONE

Engram $VERSION is ready (wired: $CLIS). Next steps:
  1. start the daemon in this repo:   $BIN serve
     (first run downloads the local embedding model, ~30 MB)
  2. open the pane:                   http://127.0.0.1:8787
     or install the IDE plugin:
       JetBrains:  https://plugins.jetbrains.com/plugin/32654-engram
       VS Code:    https://marketplace.visualstudio.com/items?itemName=techtheist.engram-alpha
       Open VSX:   https://open-vsx.org/extension/techtheist/engram-alpha
  3. restart your assistant's session so it picks up the MCP server and
     the capture instructions. All wired assistants share this one graph.
DONE
