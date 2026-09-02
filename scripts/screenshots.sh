#!/usr/bin/env bash
# Regenerate the documentation screenshots in .screenshots/ from the browser
# demo. Builds the demo first, then drives it with headless Chromium — see
# frontend/scripts/shots/README.md for what each shot is and what stays
# hand-made (standalone, VS Code, JetBrains).
#
#   scripts/screenshots.sh                      # everything, into .screenshots/
#   scripts/screenshots.sh --only review,system # a subset
#   scripts/screenshots.sh --out /tmp/shots     # somewhere to compare first
set -euo pipefail

cd "$(dirname "$0")/../frontend"

if [ ! -d node_modules/playwright ]; then
    echo "installing the screenshot driver…"
    bun add -d playwright
    bunx playwright install chromium
fi

echo "building the demo…"
bun run build:demo >/dev/null

echo "shooting…"
node scripts/shots/run.mjs "$@"
