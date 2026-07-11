#!/usr/bin/env sh
# Engram installer (Linux, macOS, and WSL) — fetches the binary, then hands
# repo wiring to the binary itself:
#
#   curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh
#
# Run it from your project's root. It downloads the platform binary from
# GitHub Releases (checksum-verified) into ~/.local/bin and then runs
# `engram-alpha setup`, which auto-detects your installed AI assistants and wires
# MCP + capture instructions for them (all assets embedded in the binary).
#
# Native Windows: use install.ps1 instead. Inside WSL this script installs
# the Linux binary — the daemon, the assistants, and the graph all stay on
# the WSL side, sharing one filesystem.
#
# Options (forwarded to `engram-alpha setup`):
#   --cli claude|codex|gemini|opencode|kilo|antigravity|all
#                    assistants to wire (comma-separated; default: auto-detect)
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
CLI=""
BIN_ONLY=0

while [ $# -gt 0 ]; do
    case "$1" in
        --cli) CLI="$2"; shift 2 ;;
        --cli=*) CLI="${1#--cli=}"; shift ;;
        --skill) SKILL="$2"; shift 2 ;;
        --skill=*) SKILL="${1#--skill=}"; shift ;;
        --bin-only) BIN_ONLY=1; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null || die "curl is required"

# ---- platform ---------------------------------------------------------------
OS="$(uname -s)" ARCH="$(uname -m)"
case "$OS" in
    Darwin)
        case "$ARCH" in
            arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
            x86_64) die "no prebuilt binary for Intel Macs (onnxruntime upstream dropped them) — build from source: cargo install --path crates/engram-cli" ;;
            *) die "unsupported macOS arch: $ARCH" ;;
        esac ;;
    Linux)
        [ "$ARCH" = "x86_64" ] || die "unsupported Linux arch: $ARCH (x86_64 only for now)"
        TARGET="x86_64-unknown-linux-gnu" ;;
    MINGW*|MSYS*|CYGWIN*)
        die "native Windows uses install.ps1 — see the README (WSL runs this script as Linux)" ;;
    *) die "unsupported OS: $OS" ;;
esac

# ---- resolve version --------------------------------------------------------
if [ "$VERSION" = "latest" ]; then
    VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
        sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')"
    [ -n "$VERSION" ] || die "could not resolve the latest release tag"
fi
ASSET="engram-alpha-$VERSION-$TARGET.tar.gz"
URL="https://github.com/$REPO/releases/download/$VERSION/$ASSET"

# ---- download + verify + install --------------------------------------------
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

say "downloading $ASSET ($VERSION)"
curl -fL --progress-bar -o "$TMP/$ASSET" "$URL" || die "download failed: $URL"
curl -fsSL -o "$TMP/$ASSET.sha256" "$URL.sha256" || die "checksum download failed"

say "verifying checksum"
(
    cd "$TMP"
    if command -v sha256sum >/dev/null; then sha256sum -c "$ASSET.sha256" >/dev/null
    else shasum -a 256 -c "$ASSET.sha256" >/dev/null; fi
) || die "checksum mismatch — refusing to install"

say "installing engram-alpha to $BIN_DIR"
mkdir -p "$BIN_DIR"
tar -xzf "$TMP/$ASSET" -C "$TMP"
mv "$TMP/engram-alpha" "$BIN_DIR/engram-alpha"
chmod +x "$BIN_DIR/engram-alpha"

case ":$PATH:" in *":$BIN_DIR:"*) ;; *)
    say "NOTE: $BIN_DIR is not on your PATH — add it to your shell profile" ;;
esac

# v0.3.0 → v0.4.0 cleanup: drop a stale pre-rename binary so nothing keeps
# launching the old version — but only if it is actually OUR engram ("engram"
# is a contested name). `setup` below re-points any wiring at engram-alpha.
OLD="$BIN_DIR/engram"
if [ -x "$OLD" ] && "$OLD" --help 2>/dev/null | grep -q "Durable graph memory"; then
    rm -f "$OLD"
    say "removed the pre-rename engram binary (the product binary is engram-alpha since v0.4.0)"
fi

[ "$BIN_ONLY" = 1 ] && { say "done (binary only)"; exit 0; }

# ---- repo wiring: the binary owns it -----------------------------------------
if [ -n "$CLI" ]; then
    "$BIN_DIR/engram-alpha" setup --cli "$CLI" --skill "$SKILL"
else
    "$BIN_DIR/engram-alpha" setup --skill "$SKILL" ||
        say "no assistants detected — wire one explicitly: engram-alpha setup --cli claude"
fi

cat <<DONE

Next steps:
  1. start the daemon in this repo:   engram-alpha serve
     (first run downloads the local embedding model, ~30 MB)
  2. open the pane:                   http://127.0.0.1:8787
       JetBrains:  https://plugins.jetbrains.com/plugin/32654-engram
       VS Code:    https://marketplace.visualstudio.com/items?itemName=techtheist.engram-alpha
  3. restart your assistant's session. All wired assistants share this graph.
  Later: update with \`engram-alpha update\`.
DONE
