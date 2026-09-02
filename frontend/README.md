# Engram pane

The graph UI for [Engram](../README.md): a Vue 3 + TypeScript app built with Vite and Bun, rendering the memory graph with [Vue Flow](https://vueflow.dev/).

The production build (`dist/`) is embedded into the `engram-alpha` binary (served by `engram-alpha serve`) and bundled into the VSCode extension; the JetBrains plugin loads it from the daemon. To rebuild everything after a frontend change, use [`scripts/deploy-pane.sh`](../scripts/deploy-pane.sh) from the repo root.

## Develop

```sh
bun install
bun dev          # dev server with hot reload (expects a daemon on 127.0.0.1:8787)
```

## Check & build

```sh
bun run lint         # ESLint
bun run lint:style   # Stylelint
bun run build        # type-check + production build
```

## Documentation screenshots

The images in `.screenshots/` are generated from the browser demo by
[`scripts/screenshots.sh`](../scripts/screenshots.sh) — headless Chromium
drives the demo build and clips one PNG per shot. Re-run it whenever the pane
changes shape; [`scripts/shots/README.md`](./scripts/shots/README.md) covers
the manifest, what keeps the images reproducible, and the three shots that
stay hand-made.
