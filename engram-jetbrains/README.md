# Engram — JetBrains plugin

[![Build](https://github.com/techtheist/engram/actions/workflows/jetbrains.yml/badge.svg)](https://github.com/techtheist/engram/actions/workflows/jetbrains.yml)
[![Version](https://img.shields.io/jetbrains/plugin/v/32654-engram.svg)](https://plugins.jetbrains.com/plugin/32654-engram)
[![Downloads](https://img.shields.io/jetbrains/plugin/d/32654-engram.svg)](https://plugins.jetbrains.com/plugin/32654-engram)

Embeds the [Engram](https://github.com/techtheist/engram) graph pane in a
JetBrains IDE tool window. Engram is a durable, graph-based long-term memory for
AI coding assistants — the *reasoning/decision* layer (why we chose this, what
bit us, what's still open), shown as a graph you can see, edit, and own.

The plugin is a thin client: it hosts the Engram pane (a Vue app) in a JCEF
browser pointed at the local **`engram serve`** daemon — the same daemon your AI
assistant reads from and writes to over MCP. Decisions, cautions, problems, and
insights surface and update live as you work.

![Engram tool window in IntelliJ IDEA: the graph pane docked right, updating live while Claude Code works in the terminal](../.screenshots/engram-alpha-jetbrains.png)

## Requirements

- **The Engram backend.** The plugin renders nothing useful on its own; it
  connects to `engram serve` on `http://127.0.0.1:8787`.
- A JetBrains IDE on build **261** (2026.1) or later, with JCEF (the default in
  all standard IDE distributions).

## Install

1. **Install the backend.** From your project's root:
   ```sh
   curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh
   ```
   This installs the `engram` binary (checksum-verified, into `~/.local/bin`)
   and wires the repo for Claude Code (`.mcp.json` + the capture skill). On
   Windows, run the same command inside WSL2 — it installs the native
   `engram.exe`. Then start the daemon:
   ```sh
   engram serve
   ```
   This serves both the JSON API and the graph pane on port 8787.
2. **Install this plugin** — from the JetBrains Marketplace (search **“Engram
   Alpha”**) or from a downloaded zip via
   *Settings → Plugins → ⚙ → Install Plugin from Disk…*.
3. Open the **Engram** tool window (right dock). It connects automatically once
   the daemon is up; if it isn't, the pane explains how to start it and re-checks
   in the background.

## Build

JDK 21 is the required compile toolchain (the build auto-provisions it via the
foojay resolver; or install Temurin 21 yourself). Gradle itself runs on newer
JDKs fine.

```sh
./gradlew buildPlugin     # -> build/distributions/engram-<version>.zip
./gradlew runIde          # launch a sandbox IDE with the plugin
./gradlew verifyPlugin    # structure + compatibility checks
```

`build/distributions/engram-<version>.zip` is the installable artifact
(*Install Plugin from Disk*).

## Architecture

A modular, split-mode-ready plugin, so it works in remote-dev / JetBrains Client
as well as a monolithic IDE:

- **`frontend`** — UI only. The `Engram` tool window and the JCEF panel
  (`EngramPanel`) that hosts the pane and handles the backend-down state. JCEF
  lives here because it must render on the frontend/client side in split mode.
- **`backend`** — intentionally empty for now. The future home for backend-side
  logic (e.g. managing the `engram serve` lifecycle) that must run on the backend
  IDE in split mode.
- **`shared`** — cross-boundary contracts (DTOs/RPC) when the backend grows them.

> Split-mode caveat: in a remote-dev session the JCEF browser runs on the client,
> so `127.0.0.1:8787` is the *client's* localhost. Until the backend module
> manages/forwards the daemon, run `engram serve` where the pane renders.

## Release & signing

CI lives in the repo root's `.github/workflows/`:

- **jetbrains.yml** — builds and verifies the plugin on every push/PR and uploads
  the zip as an artifact.
- **release.yml** — on a published GitHub Release, signs the plugin, attaches the
  signed zip to the release, and publishes to the Marketplace. The channel
  derives from the version: a `-beta.1` suffix goes to **beta**, plain pre-1.0
  versions go to **alpha**.

Required repository secrets (JetBrains Marketplace signing/publishing):

| Secret | Purpose |
|---|---|
| `CERTIFICATE_CHAIN` | Plugin signing certificate chain |
| `PRIVATE_KEY` | Signing private key |
| `PRIVATE_KEY_PASSWORD` | Private key password |
| `PUBLISH_TOKEN` | Marketplace publish token |

See the JetBrains docs on
[plugin signing](https://plugins.jetbrains.com/docs/intellij/plugin-signing.html)
and [publishing](https://plugins.jetbrains.com/docs/intellij/publishing-plugin.html).
