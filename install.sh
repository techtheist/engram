#!/usr/bin/env sh
# Engram installer — sets up Engram for the current repository.
#
#   curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh
#
# Run it from your project's root. It will:
#   1. detect your platform and download the engram binary from GitHub Releases
#      (checksum-verified) into ~/.local/bin
#   2. wire the repo: .mcp.json (MCP server for Claude Code), .gitignore entry
#      for the personal .engram/ graph, and the capture skill in .claude/skills
#
# Windows: run this inside WSL2 — it detects WSL and installs the native
# Windows binary (engram.exe), which WSL runs transparently via interop.
#
# Options:
#   --skill relaxed|normal|aggressive   capture intensity (default: relaxed)
#   --bin-only                          install the binary, skip repo wiring
# Environment:
#   ENGRAM_VERSION   pin a release tag (default: latest)
#   ENGRAM_BIN_DIR   install directory (default: ~/.local/bin)

set -eu

REPO="techtheist/engram"
BIN_DIR="${ENGRAM_BIN_DIR:-$HOME/.local/bin}"
VERSION="${ENGRAM_VERSION:-latest}"
SKILL="relaxed"
BIN_ONLY=0

while [ $# -gt 0 ]; do
    case "$1" in
        --skill) SKILL="$2"; shift 2 ;;
        --skill=*) SKILL="${1#--skill=}"; shift ;;
        --bin-only) BIN_ONLY=1; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done
case "$SKILL" in relaxed|normal|aggressive) ;; *)
    echo "error: --skill must be relaxed, normal, or aggressive" >&2; exit 2 ;;
esac

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null || die "curl is required"

# ---- platform ---------------------------------------------------------------
OS="$(uname -s)" ARCH="$(uname -m)" EXT="tar.gz" BIN="engram"
case "$OS" in
    Darwin)
        case "$ARCH" in
            arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
            x86_64)        TARGET="x86_64-apple-darwin" ;;
            *) die "unsupported macOS arch: $ARCH" ;;
        esac ;;
    Linux)
        [ "$ARCH" = "x86_64" ] || die "unsupported Linux arch: $ARCH (x86_64 only for now)"
        if grep -qi microsoft /proc/version 2>/dev/null; then
            # WSL: use the native Windows binary so the daemon runs Windows-side.
            TARGET="x86_64-pc-windows-msvc" EXT="zip" BIN="engram.exe"
        else
            TARGET="x86_64-unknown-linux-gnu"
        fi ;;
    MINGW*|MSYS*|CYGWIN*)
        TARGET="x86_64-pc-windows-msvc" EXT="zip" BIN="engram.exe" ;;
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
curl -fsSL -o "$TMP/$ASSET" "$URL" || die "download failed: $URL"
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
    tar.gz) tar -xzf "$TMP/$ASSET" -C "$TMP" ;;
    zip)    command -v unzip >/dev/null || die "unzip is required to extract $ASSET"
            unzip -oq "$TMP/$ASSET" -d "$TMP" ;;
esac
mv "$TMP/$BIN" "$BIN_DIR/$BIN"
chmod +x "$BIN_DIR/$BIN"

case ":$PATH:" in *":$BIN_DIR:"*) ;; *)
    say "NOTE: $BIN_DIR is not on your PATH — add it to your shell profile" ;;
esac

[ "$BIN_ONLY" = 1 ] && { say "done (binary only)"; exit 0; }

# ---- repo wiring ------------------------------------------------------------
REPO_DIR="$PWD"
say "preparing this repository ($REPO_DIR)"

# .gitignore: the graph is personal — never commit it.
if [ -f .gitignore ]; then
    grep -q '^\.engram/$' .gitignore || printf '\n# Engram local graph (personal)\n.engram/\n' >> .gitignore
else
    printf '# Engram local graph (personal)\n.engram/\n' > .gitignore
fi

# .mcp.json: register the MCP server for Claude Code.
if [ -f .mcp.json ]; then
    if grep -q '"engram"' .mcp.json; then
        say ".mcp.json already has an engram server — leaving it untouched"
    else
        say ".mcp.json exists — add this to its mcpServers block manually:"
        printf '    "engram": { "command": "%s", "args": ["mcp", "--db", "%s/.engram/graph.db"] }\n' \
            "$BIN_DIR/$BIN" "$REPO_DIR"
    fi
else
    cat > .mcp.json <<JSON
{
  "mcpServers": {
    "engram": {
      "command": "$BIN_DIR/$BIN",
      "args": ["mcp", "--db", "$REPO_DIR/.engram/graph.db"]
    }
  }
}
JSON
    say "wrote .mcp.json"
fi

# Capture skill for Claude Code (variant: $SKILL).
mkdir -p .claude/skills/engram
curl -fsSL -o .claude/skills/engram/SKILL.md \
    "https://raw.githubusercontent.com/$REPO/$VERSION/skills/engram/$SKILL/SKILL.md" ||
    die "skill download failed"
say "installed the '$SKILL' capture skill to .claude/skills/engram"

cat <<DONE

Engram $VERSION is ready. Next steps:
  1. start the daemon in this repo:   $BIN serve
     (first run downloads the local embedding model, ~30 MB)
  2. open the pane:                   http://127.0.0.1:8787
     or install the IDE plugin:
       JetBrains:  https://plugins.jetbrains.com/plugin/32654-engram
       VS Code:    engram-*.vsix from the GitHub release
  3. restart your Claude Code session so it picks up .mcp.json + the skill.
DONE
