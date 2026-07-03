// Copies the built Vue pane into the extension, then bundles the extension.
const esbuild = require('esbuild')
const { cpSync, existsSync, rmSync } = require('fs')

const PANE_SRC = '../frontend/dist'
const PANE_DEST = 'media/pane'

if (!existsSync(PANE_SRC)) {
  console.error(`Missing ${PANE_SRC} — run \`npm run build:pane\` first.`)
  process.exit(1)
}
rmSync(PANE_DEST, { recursive: true, force: true })
cpSync(PANE_SRC, PANE_DEST, { recursive: true })
console.log(`Copied pane ${PANE_SRC} -> ${PANE_DEST}`)

esbuild
  .build({
    entryPoints: ['src/extension.ts'],
    bundle: true,
    platform: 'node',
    target: 'node20',
    format: 'cjs',
    external: ['vscode'],
    outfile: 'dist/extension.js',
    sourcemap: true,
    minify: process.argv.includes('--minify'),
  })
  .then(() => console.log('Bundled dist/extension.js'))
  .catch((e) => {
    console.error(e)
    process.exit(1)
  })
