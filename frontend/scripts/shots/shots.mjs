/**
 * The documentation screenshots, one entry per PNG in `.screenshots/`.
 *
 * Every shot runs against the browser demo (`bun run build:demo`), so what
 * the docs show is the pane a reader can click through themselves at
 * <https://techtheist.github.io/engram/demo/> — the invented Lantern graph,
 * never this repo's own working notes.
 *
 * Three files are deliberately absent and stay hand-made: the standalone,
 * VS Code and JetBrains shots frame the pane inside an OS window or an IDE,
 * which no headless browser can stage.
 *
 * Fields:
 *   name     output basename under `.screenshots/`
 *   doc      where it is used — kept honest by hand, read by `--list`
 *   viewport [w, h] CSS pixels; images come out at 2x (deviceScaleFactor)
 *   view     'graph' | 'feed' | 'history'  (seeded before the app boots)
 *   layout   'skyline' | 'archipelago' | 'orbit'
 *   act      the click sequence that stages the shot
 *   clip     what to capture; omitted = the whole viewport
 *   pad      extra pixels around `clip` (default 0)
 *   hide     extra CSS selectors to blank out for this shot
 *   settle   ms to wait after `act` (default 400)
 */

/** The side panel currently on screen (only ever one at a time here). */
const PANEL = 'aside.side-panel'

/** Chrome that would only distract from a canvas-only layout shot. */
const CANVAS_CHROME = [
    '.topbar',
    '.health',
    '.vue-flow__controls',
    '.vue-flow__minimap',
]

/**
 * Open the gear menu and pick one of its rows. The row is clicked by
 * dispatch rather than by pointer: the menu scrolls inside a capped height,
 * and Chromium's scroll-into-view never settles for an element inside it,
 * which hangs a real click on the lower rows.
 */
const fromSettings = (row) => async (page) => {
    await page.locator('.settings > .gear').click()
    // Rows read "⚙ Graph settings" — the glyph rides in the same button.
    await page.locator('.settings .menu').getByRole('button', { name: row }).dispatchEvent('click')
}

/** Park a block of a long settings panel at the top of its scroll area. */
async function scrollTo(page, heading) {
    await page
        .locator(`${PANEL} h3.block-title`, { hasText: heading })
        .evaluate((el) => el.scrollIntoView({ block: 'start' }))
}

export const shots = [
    // ---- the graph screens ---------------------------------------------
    {
        name: 'layout-skyline-example',
        doc: 'docs/pane.md § Three layouts',
        viewport: [1280, 880],
        layout: 'skyline',
        clip: '.vue-flow__pane',
        hide: CANVAS_CHROME,
        settle: 1400,
    },
    {
        name: 'layout-archipelago-example',
        doc: 'docs/pane.md § Three layouts',
        viewport: [1280, 880],
        layout: 'archipelago',
        clip: '.vue-flow__pane',
        hide: CANVAS_CHROME,
        settle: 1400,
    },
    {
        name: 'layout-orbit-example',
        doc: 'docs/pane.md § Three layouts',
        viewport: [1280, 880],
        layout: 'orbit',
        clip: '.vue-flow__pane',
        hide: CANVAS_CHROME,
        settle: 1400,
    },
    {
        name: 'layout-feed',
        doc: 'README.md, docs/pane.md § The timeline feed',
        viewport: [1120, 1040],
        view: 'feed',
        act: async (page) => {
            // Centering a card is what opens its full story after a short
            // settle — the point of the feed, which a wall of collapsed
            // previews doesn't show.
            await page.locator('.feed .title-btn', { hasText: 'Sync is folder replication' }).click()
        },
        settle: 1200,
    },

    // ---- the drawers ----------------------------------------------------
    {
        name: 'engram-alpha-review-feature',
        doc: 'docs/pane.md § The Review drawer',
        viewport: [1280, 760],
        act: async (page) => {
            await page.getByTitle('Review recent & provisional memory').click()
        },
        clip: PANEL,
    },
    {
        name: 'engram-alpha-checkup-feature',
        doc: 'docs/conflicts-and-checkup.md § Checkup',
        viewport: [1280, 760],
        act: async (page) => {
            await page.getByTitle(/^Audit the graph with the local cortex/).click()
        },
        clip: PANEL,
    },
    {
        name: 'engram-alpha-claim-check-feature',
        doc: 'docs/conflicts-and-checkup.md § Checkup: interrogate the canon',
        viewport: [1280, 900],
        act: async (page) => {
            await page.getByTitle(/^Audit the graph with the local cortex/).click()
            await page
                .getByPlaceholder('One declarative sentence', { exact: false })
                .fill('the reader scrolls continuously instead of paginating')
            await page.locator(`${PANEL}`).getByRole('button', { name: 'Check', exact: true }).click()
            // The verdict lands below the fold of a long panel.
            await page
                .locator(`${PANEL} h3.block-title`, { hasText: 'Check a claim' })
                .evaluate((el) => el.scrollIntoView({ block: 'start' }))
        },
        clip: PANEL,
        settle: 900,
    },

    {
        name: 'engram-alpha-filter-and-tags-feature',
        doc: 'docs/pane.md § Tags and filters',
        viewport: [1280, 980],
        act: async (page) => {
            await page.getByTitle('Filter the canvas').click()
        },
        clip: '.filter-root .popover',
    },
    {
        name: 'engram-alpha-add-memory-feature',
        doc: 'docs/pane.md § Edit everything by hand',
        viewport: [1280, 720],
        act: async (page) => {
            await page.getByTitle('Create a memory node').click()
        },
        clip: PANEL,
    },
    {
        name: 'engram-alpha-audit-log-feature',
        doc: 'docs/pane.md § Every change on the record',
        viewport: [1280, 800],
        act: async (page) => {
            await fromSettings('Audit log')(page)
            // One expanded entry shows the field-level record.
            await page.locator(`${PANEL} .entry-head`).first().click()
        },
        clip: PANEL,
    },

    // ---- new in this pass ------------------------------------------------
    {
        name: 'engram-alpha-node-detail-feature',
        doc: 'docs/pane.md § Edit everything by hand, docs/memory-model.md',
        viewport: [1280, 700],
        act: async (page) => {
            await page
                .locator('.vue-flow__node')
                .filter({ hasText: 'Highlights land in the wrong paragraph' })
                .first()
                .click()
        },
        clip: PANEL,
        settle: 700,
    },
    {
        name: 'engram-alpha-ontology-feature',
        doc: 'docs/customization.md § The ontology redactor',
        viewport: [1280, 980],
        act: async (page) => {
            await fromSettings('Graph settings')(page)
            await scrollTo(page, 'Node types')
        },
        // The heading and its hint down to the end of the first type card.
        clip: [
            `${PANEL} h3.block-title:has-text("Node types")`,
            `${PANEL} section.block:has-text("Node types") article.card`,
        ],
        pad: 4,
        settle: 600,
    },
    {
        name: 'engram-alpha-custom-fields-feature',
        doc: 'docs/customization.md § Custom fields',
        viewport: [1280, 1120],
        act: async (page) => {
            await fromSettings('Graph settings')(page)
            await scrollTo(page, 'Custom fields')
        },
        // Heading through the second declared field — the demo graph's
        // `impact` enum and `tracker` url.
        clip: [
            `${PANEL} h3.block-title:has-text("Custom fields")`,
            `${PANEL} section.block:has-text("Custom fields") article.card:nth-of-type(2)`,
        ],
        pad: 4,
        settle: 600,
    },
    {
        name: 'engram-alpha-memory-lens-feature',
        doc: 'docs/recall-and-capture.md § The session brief',
        viewport: [1280, 860],
        act: fromSettings('Get brief'),
        clip: PANEL,
        settle: 700,
    },
    {
        name: 'engram-alpha-system-feature',
        doc: 'docs/pane.md § Settings → System, docs/runtime.md, docs/models.md',
        viewport: [1280, 900],
        act: fromSettings('System info'),
        clip: PANEL,
        settle: 700,
    },
    {
        name: 'engram-alpha-history-feature',
        doc: 'docs/pane.md § History at the knowledge level',
        viewport: [1320, 820],
        // `engram.view` only restores the feed, so the screen switch does it.
        act: async (page) => {
            await page.locator('.view-toggle').getByRole('radio', { name: 'History' }).click()
            // An unread lane list is half the screen: open a session so the
            // transcript beside it shows what "history" actually means.
            await page.locator('.lane-list .lane').nth(1).click()
        },
        settle: 1200,
    },
]
