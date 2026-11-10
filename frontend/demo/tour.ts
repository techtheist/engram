/**
 * The landing-page tour: the docs site scrolls, this frame acts. The page
 * posts `{ type: 'engram-tour', chapter, t }` (t = progress through that
 * chapter, 0 → 1) and the bridge plays it as three layers:
 *
 * - a **scene** — the pane's state at t (a screen, a selection, an open
 *   panel, a task moved, a session open), applied through the pane's own
 *   stores, so what the visitor sees is the real pane doing it;
 * - a **cursor** — drawn in the theme's accent, travelling a path of real
 *   elements and clicking where the scene changes;
 * - **frame effects** — what a person's hand would do between clicks:
 *   scrolling a drawer, carrying a task card across the board.
 *
 * All three are functions of t, not steps: scrolling back walks the pane,
 * the cursor and the card back. Only in `?embed` (embed.ts), where nothing
 * persists, so the tour never touches a real graph.
 */
import { nextTick } from 'vue'
import { useGraphStore } from '@/stores/graph'
import { useHistoryStore } from '@/stores/history'
import { useLayoutStore, type ViewMode } from '@/stores/layout'
import { useTasksStore } from '@/stores/tasks'
import { useThemeStore } from '@/stores/theme'
import { useMemoryLens } from '@/composables/useMemoryLens'
import type { TaskStatus } from '@/types/graph'
import { synthId } from './engine'

type Panel = 'review' | 'checkup' | 'settings' | 'projects'

interface Scene {
    view: ViewMode
    select?: string
    panel?: Panel
    lens?: boolean
    /** Whether the dragged task has been dropped on Active. */
    dropped?: boolean
    /** How many sessions have been clicked open, top first. */
    sessions?: number
    theme?: string
}

interface Pt {
    x: number
    y: number
}

/** Where the cursor goes at `at`; `click` pulses it there. */
interface Step {
    at: number
    to: () => Pt | null
    click?: boolean
}

interface Chapter {
    scene: (t: number) => Scene
    path: Step[]
    frame?: (t: number) => void
    /** False: the cursor only rests here, highlighting nothing under it. */
    hover?: boolean
}

/** The note the graph chapter opens. */
const NODE = synthId('a-anno')
/** The task carried from Backlog to Active. */
const TASK = synthId('task:reading-time')
const DROP_TO: TaskStatus = 'active'
/** Where on the card the cursor takes hold of it. */
const GRIP = { x: 0.35, y: 0.5 }

const THEMES = ['engram-purple', 'vscode-light', 'vscode-dark']

const clamp = (v: number): number => Math.min(1, Math.max(0, v))
/** Progress through [a, b] of t. */
const seg = (t: number, a: number, b: number): number => clamp((t - a) / (b - a))
const ease = (u: number): number => (u < 0.5 ? 2 * u * u : 1 - (-2 * u + 2) ** 2 / 2)

function el(selector: string): HTMLElement | null {
    const found = document.querySelector<HTMLElement>(selector)
    return found && found.getClientRects().length ? found : null
}

/** A point inside an element, as fractions of its box. */
function on(selector: string | (() => HTMLElement | null), fx = 0.5, fy = 0.5): () => Pt | null {
    return () => {
        const node = typeof selector === 'string' ? el(selector) : selector()
        if (!node) return null
        const r = node.getBoundingClientRect()
        return { x: r.left + r.width * fx, y: r.top + r.height * fy }
    }
}

/** The nth (1-based) visible match of a selector. */
function nth(selector: string, n: number): () => HTMLElement | null {
    return () => [...document.querySelectorAll<HTMLElement>(selector)].filter((e) => e.getClientRects().length)[n - 1] ?? null
}

/** The visible match whose text contains `text`. */
function byText(selector: string, text: string): () => HTMLElement | null {
    return () =>
        [...document.querySelectorAll<HTMLElement>(selector)].find(
            (e) => e.getClientRects().length && e.textContent?.includes(text),
        ) ?? null
}

/** A point on the pane itself, as fractions of the window. */
function screen(fx: number, fy: number): () => Pt {
    return () => ({ x: innerWidth * fx, y: innerHeight * fy })
}

/** The open left drawer's scroller (Review, Checkup, Memory Lens). */
const leftScroll = (): HTMLElement | null => el('.side-panel.left .scroll')

function scrollTo(node: HTMLElement | null, u: number): void {
    if (node) easeScroll(node, u * (node.scrollHeight - node.clientHeight))
}

/**
 * Scrollers move like the cursor: each message sets a goal and the scroll
 * eases to it every frame. The position is kept as a float of our own —
 * scrollTop rounds to device pixels, and the last sub-pixel steps of an
 * ease would round away to nothing.
 */
const scrolls = new Map<HTMLElement, { goal: number; pos: number }>()
let scrollRaf = 0
let scrollThen = 0

function easeScroll(node: HTMLElement, goal: number): void {
    const s = scrolls.get(node)
    if (s) s.goal = goal
    else scrolls.set(node, { goal, pos: node.scrollTop })
    if (!scrollRaf) scrollRaf = requestAnimationFrame(stepScrolls)
}

function stepScrolls(now: number): void {
    const dt = Math.min(64, now - (scrollThen || now - 16))
    scrollThen = now
    const k = 1 - Math.exp(-dt / EASE_MS)
    for (const [node, s] of scrolls) {
        const d = s.goal - s.pos
        s.pos = Math.abs(d) < 0.3 ? s.goal : s.pos + d * k
        node.scrollTop = s.pos
        if (s.pos === s.goal || !node.isConnected) scrolls.delete(node)
    }
    scrollRaf = scrolls.size ? requestAnimationFrame(stepScrolls) : 0
    if (!scrollRaf) scrollThen = 0
}

// ---- the graph node, found once -------------------------------------------------

/**
 * The canvas draws cards on the GPU, so a node has no element to aim at —
 * except while it is selected, when its HTML twin is laid over it. The
 * bridge selects the node once before the frame is shown, reads the twin's
 * box back into world coordinates, and from then on projects that through
 * the camera (`.gl-viewport`'s transform) like the canvas does.
 */
let nodeWorld: { x: number; y: number; w: number; h: number } | null = null

function camera(): { m: DOMMatrix; ox: number; oy: number } | null {
    const vp = el('.gl-viewport')
    const host = vp?.parentElement
    if (!vp || !host) return null
    const r = host.getBoundingClientRect()
    return { m: new DOMMatrix(getComputedStyle(vp).transform), ox: r.left, oy: r.top }
}

async function measureNode(select: (id: string | null) => void): Promise<boolean> {
    select(NODE)
    await nextTick()
    await new Promise((r) => requestAnimationFrame(r))
    const twin = el(`.gl-card[data-node-id="${NODE}"]`)
    const cam = camera()
    if (twin && cam) {
        const r = twin.getBoundingClientRect()
        nodeWorld = {
            x: (r.left - cam.ox - cam.m.e) / cam.m.a,
            y: (r.top - cam.oy - cam.m.f) / cam.m.d,
            w: r.width / cam.m.a,
            h: r.height / cam.m.d,
        }
    }
    select(null)
    return nodeWorld != null || !!el('.vue-flow')
}

function nodeAt(fx = 0.5, fy = 0.3): () => Pt | null {
    return () => {
        // No WebGL: the Vue Flow fallback draws every card in the DOM.
        const flow = el(`.vue-flow__node[data-id="${NODE}"]`)
        if (flow) return on(() => flow, fx, fy)()
        const cam = camera()
        if (!nodeWorld || !cam) return null
        return {
            x: cam.ox + cam.m.e + (nodeWorld.x + nodeWorld.w * fx) * cam.m.a,
            y: cam.oy + cam.m.f + (nodeWorld.y + nodeWorld.h * fy) * cam.m.d,
        }
    }
}

// ---- the cursor --------------------------------------------------------------------

const STYLE = `
.tour-cursor {
    position: fixed;
    left: 0;
    top: 0;
    z-index: 2147483647;
    width: 22px;
    height: 22px;
    pointer-events: none;
    opacity: 0;
    transition: opacity 200ms;
    will-change: transform;
    filter: drop-shadow(0 2px 3px rgb(0 0 0 / 35%));
}
.tour-cursor.on { opacity: 1; }
.tour-cursor svg {
    width: 22px;
    height: 22px;
    transition: transform 120ms;
    transform-origin: 3px 2px;
    /* The page scales the pane down (to ~0.3 on a phone); the cursor grows
       back so it still reads as a cursor there. */
    transform: scale(var(--tour-cursor-scale, 1));
}
.tour-cursor .ring { transform-origin: 16px 16px; scale: var(--tour-cursor-scale, 1); }
.tour-cursor.press svg { transform: scale(calc(var(--tour-cursor-scale, 1) * 0.82)); }
.tour-cursor path { fill: var(--interactive-primary); stroke: #fff; stroke-width: 1.4; stroke-linejoin: round; }
.tour-cursor .ring {
    position: absolute;
    left: -13px;
    top: -14px;
    width: 32px;
    height: 32px;
    border: 2px solid var(--interactive-primary);
    border-radius: 50%;
    opacity: 0;
}
.tour-cursor .ring.go { animation: tour-ring 520ms ease-out; }
@keyframes tour-ring {
    from { opacity: 0.8; transform: scale(0.35); }
    to { opacity: 0; transform: scale(1.5); }
}
/* A stand-in for :hover, which scripts can't set: a transparent control gets
   the theme's hover fill, a solid one (Approve, New) lightens the way the
   pane's own accent hover does. */
[data-tour-hover="fill"]:not(.active, .on, .current) { background-color: var(--interactive-ghost-hover) !important; }
[data-tour-hover="lift"] { filter: brightness(1.12); }
.tour-lifted { opacity: 0.3 !important; }
.tour-ghost {
    /* !important: the clone keeps the card's scoped styles, which outrank this. */
    position: fixed !important;
    z-index: 2147483646;
    margin: 0 !important;
    opacity: 0.96 !important;
    pointer-events: none;
    transform: rotate(-2deg);
    box-shadow: var(--shadow-lg);
}
`

/**
 * The cursor eases toward its goal every frame (exponential smoothing, frame-
 * rate independent): scroll messages set the goal as fast as they come, and a
 * touchpad's stream of small steps becomes one continuous glide. `onFrame`
 * runs after each step, for whatever rides on the cursor (the carried card,
 * the hover).
 */
const EASE_MS = 90

/** Whether an element paints a background of its own (an accent button) or sits on its surface. */
function isSolid(node: HTMLElement): boolean {
    const bg = getComputedStyle(node).backgroundColor
    const alpha = bg.startsWith('rgba') ? Number(bg.split(',')[3]?.replace(')', '') ?? 1) : bg === 'transparent' ? 0 : 1
    return alpha > 0.5
}

function makeCursor(onFrame: () => void) {
    const style = document.createElement('style')
    style.textContent = STYLE
    document.head.append(style)
    const root = document.createElement('div')
    root.className = 'tour-cursor'
    root.innerHTML =
        '<span class="ring"></span><svg viewBox="0 0 22 22" aria-hidden="true">' +
        '<path d="M3 2 L3 18 L7.4 14 L10.4 20.5 L13.3 19.2 L10.4 12.9 L16.4 12.9 Z" /></svg>'
    document.body.append(root)
    const ring = root.querySelector<HTMLElement>('.ring')!
    let pos: Pt = { x: innerWidth * 0.62, y: innerHeight * 0.55 }
    let goal: Pt = pos
    let shown = false
    let hovered: HTMLElement | null = null
    let pressTimer = 0
    let raf = 0
    let then = 0

    function place(): void {
        root.style.transform = `translate(${pos.x - 3}px, ${pos.y - 2}px)`
        onFrame()
    }

    function tick(now: number): void {
        const dt = Math.min(64, now - (then || now - 16))
        then = now
        const k = 1 - Math.exp(-dt / EASE_MS)
        const dx = goal.x - pos.x
        const dy = goal.y - pos.y
        const settled = Math.abs(dx) < 0.25 && Math.abs(dy) < 0.25
        pos = settled ? goal : { x: pos.x + dx * k, y: pos.y + dy * k }
        place()
        raf = settled ? 0 : requestAnimationFrame(tick)
        if (!raf) then = 0
    }

    return {
        get pos(): Pt {
            return pos
        },
        show(on: boolean): void {
            // Appearing, it is simply there — no flight in from the last spot.
            if (on && !shown) {
                pos = goal
                place()
            }
            shown = on
            root.classList.toggle('on', on)
            if (!on) this.hover(false)
        },
        move(p: Pt): void {
            goal = p
            if (!raf) raf = requestAnimationFrame(tick)
        },
        click(): void {
            ring.classList.remove('go')
            void ring.offsetWidth // restart the animation
            ring.classList.add('go')
            root.classList.add('press')
            window.clearTimeout(pressTimer)
            pressTimer = window.setTimeout(() => root.classList.remove('press'), 160)
        },
        /** Hover what lies under the cursor: real pointer events on the canvas, a stand-in highlight elsewhere. */
        hover(active: boolean, canvas = false): void {
            const under = active ? document.elementFromPoint(pos.x, pos.y) : null
            if (canvas && under?.closest('.gl-slide')) {
                under.dispatchEvent(
                    new PointerEvent('pointermove', { clientX: pos.x, clientY: pos.y, bubbles: true, pointerType: 'mouse' }),
                )
            }
            const target =
                (under?.closest('button, .card[data-task-id]') as HTMLElement | null) ?? null
            if (target === hovered) return
            hovered?.removeAttribute('data-tour-hover')
            hovered = target
            if (hovered) hovered.setAttribute('data-tour-hover', isSolid(hovered) ? 'lift' : 'fill')
        },
    }
}

// ---- the chapters ----------------------------------------------------------------------

/** Each panel's trigger; `.active` on it means open. */
const TRIGGERS: Record<Panel, string> = {
    review: '.review-root > .toggle',
    checkup: '.checkup-root > .toggle',
    settings: '.settings > .gear',
    projects: '.switcher-root > .toggle',
}

function setPanel(panel: Panel, open: boolean): void {
    const button = document.querySelector<HTMLElement>(TRIGGERS[panel])
    if (button && button.classList.contains('active') !== open) button.click()
}

/** Open a drawer, then sit in it and scroll it to the bottom. */
function drawerChapter(panel: 'review' | 'checkup'): Chapter {
    return {
        scene: (t) => ({ view: 'graph', panel: t >= 0.2 ? panel : undefined }),
        path: [
            { at: 0, to: screen(0.6, 0.5) },
            { at: 0.2, to: on(TRIGGERS[panel]), click: true },
            { at: 0.36, to: on('.side-panel.left .scroll', 0.62, 0.45) },
            { at: 1, to: on('.side-panel.left .scroll', 0.66, 0.55) },
        ],
        frame: (t) => scrollTo(leftScroll(), ease(seg(t, 0.38, 0.95))),
    }
}

/** A button on the feed's action bar, by label, else by position. */
function actionButton(label: string, n: number): () => HTMLElement | null {
    return () => byText('.action-bar button', label)() ?? nth('.action-bar button', n)()
}

function lane(n: number): () => HTMLElement | null {
    return () => el(`.lane-list li:nth-child(${n}) .lane`)
}

function themeOpt(n: number): () => HTMLElement | null {
    return nth('.settings .themes .theme-opt', n)
}

export function startTour(): void {
    const graph = useGraphStore()
    const layout = useLayoutStore()
    const tasks = useTasksStore()
    const history = useHistoryStore()
    const theme = useThemeStore()
    const lens = useMemoryLens()
    // What rides on the cursor, every frame it moves.
    let hoverOn = false
    let hoverCanvas = false
    const cursor = makeCursor(() => {
        placeGhost()
        if (hoverOn) cursor.hover(true, hoverCanvas)
    })

    /** The page's own skin, worn whenever a scene doesn't name one. */
    let pageTheme = theme.current
    /** The themes chapter clicks through the two skins the page isn't wearing. */
    const other = (n: number): string => THEMES.filter((id) => id !== pageTheme)[n] ?? pageTheme

    /** The page's scroll for one chapter, in frame pixels: the feed scrolls at it. */
    let pagePx = 600

    // The carried task: the ghost that follows the cursor, held by the spot
    // the path grabs it at (GRIP, as fractions of the card).
    let ghost: HTMLElement | null = null
    let grip: Pt = { x: 0, y: 0 }

    function carry(t: number, grab: number, drop: number): void {
        const card = el(`.card[data-task-id="${TASK}"]`)
        const flying = t > grab && t < drop
        if (flying && card && !ghost) {
            const r = card.getBoundingClientRect()
            grip = { x: r.width * GRIP.x, y: r.height * GRIP.y }
            ghost = card.cloneNode(true) as HTMLElement
            ghost.classList.add('tour-ghost')
            ghost.removeAttribute('data-tour-hover')
            ghost.style.width = `${r.width}px`
            document.body.append(ghost)
        }
        if (!flying && ghost) {
            ghost.remove()
            ghost = null
        }
        card?.classList.toggle('tour-lifted', flying)
        placeGhost()
    }

    function placeGhost(): void {
        if (!ghost) return
        ghost.style.left = `${cursor.pos.x - grip.x}px`
        ghost.style.top = `${cursor.pos.y - grip.y}px`
    }

    const chapters: Record<string, Chapter> = {
        // Open a note, read it, close it: the feed then starts from its top
        // rather than centered on the note.
        graph: {
            scene: (t) => ({ view: 'graph', select: t >= 0.34 && t < 0.84 ? NODE : undefined }),
            path: [
                { at: 0, to: screen(0.62, 0.62) },
                { at: 0.34, to: nodeAt(), click: true },
                { at: 0.6, to: on('.side-panel.right', 0.4, 0.2) },
                { at: 0.84, to: on('.side-panel.right .close'), click: true },
                { at: 1, to: screen(0.8, 0.3) },
            ],
        },
        // A plain scroll from the top, at the page's own pace (`pagePx`: the
        // page's pixels for this chapter, in the frame's pixels), while the
        // cursor drifts along the action bar, Approve to Edit.
        feed: {
            scene: () => ({ view: 'feed' }),
            path: [
                { at: 0, to: screen(0.5, 0.6) },
                { at: 0.15, to: on(actionButton('Approve', 1), 0.5, 0.5) },
                { at: 0.95, to: on(actionButton('Edit', 4), 0.5, 0.5) },
                { at: 1, to: on(actionButton('Edit', 4), 0.5, 0.5) },
            ],
            frame: (t) => {
                const feed = el('.feed')
                if (!feed) return
                // The feed snaps each card to center (scroll-snap-type), so
                // every position the tour set was pulled to the nearest card
                // — a step per card. Off for the tour: it glides.
                feed.style.scrollSnapType = 'none'
                easeScroll(feed, t * pagePx)
            },
        },
        tasks: {
            scene: (t) => ({ view: 'tasks', dropped: t >= 0.72 }),
            path: [
                { at: 0, to: screen(0.5, 0.6) },
                { at: 0.3, to: on(`.card[data-task-id="${TASK}"]`, GRIP.x, GRIP.y), click: true },
                { at: 0.72, to: on('.col-active .cards', 0.5, 1.08), click: true },
                { at: 1, to: on(`.card[data-task-id="${TASK}"]`, 0.75, 0.9) },
            ],
            frame: (t) => carry(t, 0.3, 0.72),
        },
        history: {
            scene: (t) => ({ view: 'history', sessions: t >= 0.72 ? 3 : t >= 0.45 ? 2 : t >= 0.18 ? 1 : 0 }),
            path: [
                { at: 0, to: screen(0.5, 0.5) },
                { at: 0.18, to: on(lane(1), 0.4, 0.45), click: true },
                { at: 0.45, to: on(lane(2), 0.45, 0.45), click: true },
                { at: 0.72, to: on(lane(3), 0.4, 0.45), click: true },
                { at: 1, to: screen(0.62, 0.5) },
            ],
        },
        review: drawerChapter('review'),
        checkup: drawerChapter('checkup'),
        brief: {
            scene: (t) =>
                t >= 0.34
                    ? { view: 'graph', lens: true }
                    : { view: 'graph', panel: t >= 0.16 ? 'settings' : undefined },
            path: [
                { at: 0, to: screen(0.4, 0.55) },
                { at: 0.16, to: on(TRIGGERS.settings), click: true },
                { at: 0.34, to: on(byText('.settings .row', 'brief'), 0.3, 0.5), click: true },
                { at: 0.46, to: on('.side-panel.left .scroll', 0.6, 0.45) },
                { at: 1, to: on('.side-panel.left .scroll', 0.64, 0.55) },
            ],
            frame: (t) => scrollTo(leftScroll(), ease(seg(t, 0.48, 0.95))),
        },
        // Open the switcher, hover the graphs, then open "+ Add project":
        // the folder browser a project is registered from.
        projects: {
            scene: (t) => ({ view: 'graph', panel: t >= 0.3 ? 'projects' : undefined }),
            path: [
                { at: 0, to: screen(0.5, 0.5) },
                { at: 0.3, to: on(TRIGGERS.projects), click: true },
                { at: 0.44, to: on(nth('.switcher-root .project-row', 1), 0.55, 0.5) },
                { at: 0.58, to: on(nth('.switcher-root .project-row', 2), 0.5, 0.5) },
                { at: 0.72, to: on('.switcher-root .browse-open', 0.4, 0.5) },
                { at: 0.8, to: on('.switcher-root .browse-open', 0.42, 0.5), click: true },
                { at: 1, to: on('.switcher-root .dir-list', 0.45, 0.35) },
            ],
            // The browser is the switcher's own state, opened and closed by
            // its own buttons: open past the click, closed before it.
            frame: (t) => {
                const browsing = !el('.switcher-root .browse-open') && !!el('.switcher-root .browser')
                const want = t >= 0.8
                if (want && !browsing) el('.switcher-root .browse-open')?.click()
                if (!want && browsing) el('.switcher-root .cancel-btn')?.click()
            },
        },
        themes: {
            scene: (t) => ({
                view: 'graph',
                panel: t >= 0.16 ? 'settings' : undefined,
                theme: t >= 0.7 ? other(1) : t >= 0.42 ? other(0) : undefined,
            }),
            path: [
                { at: 0, to: screen(0.6, 0.5) },
                { at: 0.16, to: on(TRIGGERS.settings), click: true },
                { at: 0.42, to: on(() => themeOpt(THEMES.indexOf(other(0)) + 1)(), 0.3, 0.5), click: true },
                { at: 0.7, to: on(() => themeOpt(THEMES.indexOf(other(1)) + 1)(), 0.3, 0.5), click: true },
                { at: 1, to: on(() => themeOpt(THEMES.indexOf(other(1)) + 1)(), 0.4, 0.6) },
            ],
        },
    }

    // ---- scenes ----------------------------------------------------------------

    let current: { chapter: string; t: number } = { chapter: 'idle', t: 0 }
    let want: Scene = { view: 'graph' }
    let dirty = false
    let busy = false
    let original: TaskStatus | null = null

    let lastView: ViewMode = layout.view

    /** Back on the graph from another screen: fit it, so every aim is on screen. */
    function refit(): void {
        window.setTimeout(() => {
            document.querySelector<HTMLElement>('button[aria-label="fit view"]')?.click()
            // The fit eases in; aim again once it has.
            window.setTimeout(redraw, 360)
        }, 40)
    }

    async function apply(s: Scene): Promise<void> {
        for (const p of Object.keys(TRIGGERS) as Panel[]) if (p !== s.panel) setPanel(p, false)
        if (!s.lens) lens.hide()

        theme.set(s.theme ?? pageTheme)
        layout.setView(s.view)
        if (s.view === 'graph' && lastView !== 'graph') refit()
        lastView = s.view

        if (s.view === 'graph') {
            if (s.select) {
                if (graph.selectedId !== s.select || !graph.detailOpen) graph.select(s.select)
            } else if (graph.selectedId) {
                graph.select(null)
            }
        }
        if (s.lens) lens.show()
        // After the screen switch: a drawer claim closes the detail pane.
        if (s.panel) setPanel(s.panel, true)

        // The carried task is dropped on Active as a state: before the drop
        // (or off the Tasks screen) it is wherever the seed had it.
        const task = tasks.tasks.find((t) => t.id === TASK)
        if (task) {
            original ??= task.status
            const status = s.view === 'tasks' && s.dropped ? DROP_TO : original
            if (task.status !== status) await tasks.patch(TASK, { status })
        }

        if (s.view === 'history') {
            if (!history.sessions.length) await history.load()
            const n = Math.min(s.sessions ?? 0, history.sessions.length)
            const target = n ? history.sessions[n - 1] : null
            if (!target) {
                history.active = null
                history.messages = []
            } else if (history.active?.session !== target.session) {
                await history.open(target.session)
            }
        }
    }

    /** One apply at a time; a scene asked for mid-apply runs right after. */
    async function pump(): Promise<void> {
        if (busy) return
        busy = true
        try {
            while (dirty) {
                dirty = false
                await apply(want)
            }
        } finally {
            busy = false
        }
    }

    // ---- the cursor's frame ---------------------------------------------------------

    const resolved = new Map<string, Pt>()
    let last = { chapter: '', t: 0 }

    function point(chapter: string, i: number, step: Step): Pt {
        const key = `${chapter}:${i}`
        const p = step.to()
        if (p) resolved.set(key, p)
        return p ?? resolved.get(key) ?? cursor.pos
    }

    /** After the scene has rendered: place the cursor, click, run the chapter's hand. */
    function draw(): void {
        const { chapter: id, t } = current
        const ch = chapters[id]
        if (!ch) {
            hoverOn = false
            cursor.show(false)
            carry(0, 1, 0)
            last = { chapter: id, t }
            return
        }
        cursor.show(true)
        const path = ch.path
        let i = path.findIndex((s) => s.at > t) - 1
        if (i < 0) i = path[0]!.at > t ? 0 : path.length - 1
        const a = path[i]!
        const b = path[Math.min(i + 1, path.length - 1)]!
        const pa = point(id, i, a)
        const pb = b === a ? pa : point(id, i + 1, b)
        const u = b === a ? 1 : ease(seg(t, a.at, b.at))
        cursor.move({ x: pa.x + (pb.x - pa.x) * u, y: pa.y + (pb.y - pa.y) * u })

        if (last.chapter === id) {
            const lo = Math.min(last.t, t)
            const hi = Math.max(last.t, t)
            if (path.some((s) => s.click && s.at > lo && s.at <= hi)) cursor.click()
        }
        if (id !== 'tasks') carry(0, 1, 0)
        ch.frame?.(t)
        hoverOn = ch.hover !== false
        if (!hoverOn) cursor.hover(false)
        hoverCanvas = id === 'graph'
        last = { chapter: id, t }
    }

    // One draw per frame at most, reading the latest message when it runs.
    // (Cancelling and re-queueing on every message starved it: a touchpad
    // sends one per frame, so the cursor only moved once the scroll stopped.)
    // A target that isn't laid out yet (a drawer mid-slide) falls back to
    // where it was last seen, and the next message finds it.
    let frame = 0
    let settle = 0
    function redraw(): void {
        if (frame) return
        frame = requestAnimationFrame(() => {
            frame = 0
            draw()
        })
    }

    window.addEventListener('message', (e: MessageEvent) => {
        if (e.origin !== location.origin || e.data?.type !== 'engram-tour') return
        const { chapter, t, theme: skin, px } = e.data as {
            chapter?: string
            t?: number
            theme?: string
            px?: number
        }
        if (typeof px === 'number' && px > 0) pagePx = px
        const { scale } = e.data as { scale?: number }
        if (typeof scale === 'number' && scale > 0) {
            const grow = Math.min(2.6, Math.max(1, 0.75 / scale))
            document.documentElement.style.setProperty('--tour-cursor-scale', String(grow))
        }
        if (skin && THEMES.includes(skin)) pageTheme = skin
        if (chapter != null) {
            current = { chapter, t: t ?? 0 }
            want = chapters[chapter]?.scene(current.t) ?? { view: 'graph' }
        }
        dirty = true
        void pump().then(redraw)
        redraw()
        // Once the scroll rests, aim again: a drawer that was still sliding
        // in at the last message has its real box by now.
        window.clearTimeout(settle)
        settle = window.setTimeout(redraw, 320)
    })

    // Ready once the graph is in and the node is found: the page swaps its
    // placeholder for the frame.
    let tries = 0
    const ready = async (): Promise<void> => {
        // A pane that never lays the node out still tours, its cursor just
        // doesn't find that one card.
        const found = !graph.loading && graph.nodeList.length > 0 && (await measureNode(graph.select))
        if (!found && ++tries < 60) {
            window.setTimeout(() => void ready(), 150)
            return
        }
        window.parent.postMessage({ type: 'engram-tour', ready: true }, location.origin)
    }
    void ready()
}
