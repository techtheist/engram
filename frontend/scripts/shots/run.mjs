/**
 * Regenerates the documentation screenshots from the browser demo.
 *
 *   bun run shots                 # every shot into .screenshots/
 *   bun run shots -- --only review,ontology
 *   bun run shots -- --out /tmp/shots --list
 *
 * It serves `dist-demo/` over loopback, drives it with headless Chromium and
 * writes one PNG per manifest entry. Three things make the output stable
 * enough to commit: the demo's ages resolve against a FIXED clock, the graph
 * layouts are deterministic by construction (`useLayout.ts`), and every
 * transition and animation is switched off before the shutter.
 *
 * Deliberately not wired into CI: font rasterization differs between macOS
 * and the runners, so a regenerated image would never match a committed one.
 */
import { createServer } from 'node:http'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { readFile, mkdir } from 'node:fs/promises'
import { existsSync } from 'node:fs'
import { extname, join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { chromium } from 'playwright'
import { shots } from './shots.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const frontend = resolve(here, '../..')
const repo = resolve(frontend, '..')
const DIST = join(frontend, 'dist-demo')

/** Every relative day in the demo resolves against this instant. */
const FIXED_NOW = new Date('2026-06-11T14:20:00Z')

/** Motion is the enemy of a reproducible pixel. */
const STILL = `
*, *::before, *::after {
    transition: none !important;
    animation: none !important;
}
/* The demo's own badge is not part of the product. */
.demo-chrome { display: none !important; }
`

const MIME = {
    '.html': 'text/html; charset=utf-8',
    '.js': 'text/javascript; charset=utf-8',
    '.css': 'text/css; charset=utf-8',
    '.svg': 'image/svg+xml',
    '.ico': 'image/x-icon',
    '.png': 'image/png',
    '.json': 'application/json',
    '.woff2': 'font/woff2',
}

function serve(root) {
    const server = createServer(async (req, res) => {
        const path = decodeURIComponent((req.url ?? '/').split('?')[0])
        const file = join(root, path.endsWith('/') ? `${path}index.html` : path)
        try {
            const body = await readFile(file)
            res.writeHead(200, { 'content-type': MIME[extname(file)] ?? 'application/octet-stream' })
            res.end(body)
        } catch {
            // A single-page app: unknown paths are the app itself.
            res.writeHead(404).end('not found')
        }
    })
    return new Promise((ok) => {
        server.listen(0, '127.0.0.1', () => ok({ server, port: server.address().port }))
    })
}

function parseArgs(argv) {
    const args = { only: null, out: join(repo, '.screenshots'), list: false, headed: false }
    for (let i = 0; i < argv.length; i++) {
        const a = argv[i]
        if (a === '--only') args.only = argv[++i].split(',').map((s) => s.trim())
        else if (a === '--out') args.out = resolve(argv[++i])
        else if (a === '--list') args.list = true
        else if (a === '--headed') args.headed = true
        else throw new Error(`unknown argument: ${a}`)
    }
    return args
}

/** `--only review` matches every shot whose name contains "review". */
const selected = (only) =>
    only == null ? shots : shots.filter((s) => only.some((n) => s.name.includes(n)))

/**
 * The rectangle to shoot: the union of every `clip` selector's box (one
 * selector, or a first/last pair spanning a run of elements), grown by
 * `pad` and clamped to the viewport — a panel taller than the window is
 * captured down to its bottom edge, never off-screen.
 */
async function clipRect(page, shot, width, height) {
    const boxes = []
    for (const selector of [shot.clip].flat()) {
        const target = page.locator(selector).first()
        await target.waitFor({ state: 'visible', timeout: 10_000 })
        boxes.push(await target.boundingBox())
    }
    const pad = shot.pad ?? 0
    const x = Math.max(0, Math.min(...boxes.map((b) => b.x)) - pad)
    const y = Math.max(0, Math.min(...boxes.map((b) => b.y)) - pad)
    const right = Math.min(width, Math.max(...boxes.map((b) => b.x + b.width)) + pad)
    const bottom = Math.min(height, Math.max(...boxes.map((b) => b.y + b.height)) + pad)
    return { x, y, width: right - x, height: bottom - y }
}

async function capture(browser, shot, url, outDir) {
    const [width, height] = shot.viewport ?? [1280, 900]
    const context = await browser.newContext({
        viewport: { width, height },
        deviceScaleFactor: 2,
        colorScheme: 'dark',
        reducedMotion: 'reduce',
    })
    // Seeded before the app boots: the stores read these on first tick.
    await context.addInitScript(
        ([theme, layout, view]) => {
            localStorage.setItem('engram.theme', theme)
            localStorage.setItem('engram.layout', layout)
            localStorage.setItem('engram.view', view)
        },
        [shot.theme ?? 'engram-purple', shot.layout ?? 'skyline', shot.view ?? 'graph'],
    )

    const page = await context.newPage()
    const failures = []
    page.on('pageerror', (e) => failures.push(String(e)))
    await page.clock.setFixedTime(FIXED_NOW)
    await page.goto(url, { waitUntil: 'networkidle' })

    // The app is up once its shell rendered and the graph arrived.
    await page.waitForSelector('.topbar', { timeout: 15_000 })
    if ((shot.view ?? 'graph') === 'graph') {
        await page.waitForSelector('.vue-flow__node', { timeout: 15_000 })
    }
    await page.addStyleTag({ content: STILL })
    if (shot.hide?.length) {
        await page.addStyleTag({ content: `${shot.hide.join(',')} { display: none !important; }` })
    }

    // The first-run history notice is a one-time nag, not part of any shot.
    const notice = page.getByRole('button', { name: 'Got it' })
    if (await notice.count()) await notice.click()

    await shot.act?.(page)
    await page.waitForTimeout(shot.settle ?? 400)

    const path = join(outDir, `${shot.name}.png`)
    if (shot.clip) {
        await page.screenshot({ path, clip: await clipRect(page, shot, width, height) })
    } else {
        await page.screenshot({ path })
    }

    await context.close()
    if (failures.length) throw new Error(`page errors: ${failures.join(' | ')}`)
    return path
}

/**
 * A lossless squeeze if oxipng happens to be installed — these are 2x
 * images and the repo carries them forever. Optional on purpose: no
 * screenshot depends on the tool being there.
 */
async function squeeze(files) {
    if (files.length === 0) return
    try {
        await promisify(execFile)('oxipng', ['-o', '2', '--strip', 'safe', '-q', ...files])
        console.log('  (oxipng: lossless squeeze applied)')
    } catch {
        // No oxipng, or it disliked something — the PNGs are already written.
    }
}

async function main() {
    const args = parseArgs(process.argv.slice(2))
    const list = selected(args.only)

    if (args.list) {
        for (const s of list) console.log(`${s.name.padEnd(42)} ${s.doc ?? ''}`)
        return
    }
    if (list.length === 0) throw new Error('no shot matched --only')
    if (!existsSync(join(DIST, 'index.html'))) {
        throw new Error(`no demo build at ${DIST} — run: bun run build:demo`)
    }

    await mkdir(args.out, { recursive: true })
    const { server, port } = await serve(DIST)
    const url = `http://127.0.0.1:${port}/`
    const browser = await chromium.launch({ headless: !args.headed })

    let failed = 0
    const written = []
    for (const shot of list) {
        process.stdout.write(`  ${shot.name} … `)
        try {
            written.push(await capture(browser, shot, url, args.out))
            console.log('ok')
        } catch (e) {
            failed++
            const why = e.message.split('\n').slice(0, 6).join('\n      ')
            console.log(`FAILED\n      ${why}`)
        }
    }

    await browser.close()
    server.close()
    await squeeze(written)
    console.log(`\n${list.length - failed}/${list.length} shots written to ${args.out}`)
    if (failed) process.exitCode = 1
}

await main()
