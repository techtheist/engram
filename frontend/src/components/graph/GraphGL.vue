<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, shallowRef, watch } from 'vue'
import { storeToRefs } from 'pinia'
import EngramCard from '@/components/nodes/EngramCard.vue'
import ConnectDialog from '@/components/panels/ConnectDialog.vue'
import { layoutGraph, type XY } from '@/composables/useLayout'
import { useConfigStore } from '@/stores/config'
import { useCrowdingStore } from '@/stores/crowding'
import { heatColor, heatRgb } from '@/utils/heat'
import { useGraphStore } from '@/stores/graph'
import { useLayoutStore } from '@/stores/layout'
import { useProjectsStore } from '@/stores/projects'
import type { GraphNode } from '@/types/graph'
import GraphControls from './GraphControls.vue'
import GraphMiniMap, { type MiniNode } from './GraphMiniMap.vue'
import { layoutCard, resetMeasures, setTextHost, type CardLayout } from './cardPainter'
import { calibrate, cardFonts, css, readTokens, resolveColor, type CardMetrics, type RGBA, type Tokens } from './palette'
import { GraphRenderer, type CardDraw, type EdgeDraw, type GlowDraw } from './renderer'
import { edgeFocus, FOCUS_EASE, selectionHops } from './focus'

/**
 * The graph canvas on the GPU (0.10.1) — what GraphFlow (Vue Flow) does,
 * drawn by WebGL2: same layouts, cards, edges, glass, controls, minimap,
 * gestures. Only the selected and the hovered card are HTML (EngramCard,
 * over their GPU twins), so the expanded card, its links and its glass are
 * the real thing; everything else is three instanced draws. GraphCanvas
 * picks this when WebGL2 runs on a GPU and falls back to Vue Flow if not.
 */
const emit = defineEmits<{ lost: [] }>()

const store = useGraphStore()
const config = useConfigStore()
const layout = useLayoutStore()
const projects = useProjectsStore()
const crowding = useCrowdingStore()
const { visibleNodeList, visibleEdgeList, selectedId } = storeToRefs(store)

/**
 * Crowding heat (0.11.0): a glow around each crowded card, yellow through
 * orange to red — a strip color alone did not read from far out. Notes
 * below GLOW_MIN stay dark. The knobs are live in dev: `engramGlow({...})`.
 */
const glow = { spread: 170, alpha: 0.09, min: 0.08, tint: 0.11 }

/** The card surface's heat tint: the ramp color at `tint` × heat opacity. */
function heatTint(id: string): RGBA | null {
    if (!crowding.heatOn) return null
    const heat = crowding.heatOf(id)
    if (heat == null || heat < glow.min) return null
    const [r, g, b] = heatRgb(heat)
    // A floor of 40%: the coolest lit card must still read as tinted.
    return [r, g, b, glow.tint * (0.4 + 0.6 * Math.min(1, heat))]
}

/** The HTML twin's matching tint (EngramCard's --heat-bg). */
function heatBg(id: string): string | undefined {
    const t = heatTint(id)
    return t ? `rgb(${t[0] * 255} ${t[1] * 255} ${t[2] * 255} / ${t[3]})` : undefined
}

function rebuildGlows(): void {
    if (!renderer) return
    const out: GlowDraw[] = []
    if (crowding.heatOn && crowding.report) {
        for (const c of cards) {
            const heat = crowding.heatOf(c.id)
            if (heat == null || heat < glow.min) continue
            out.push({ x: c.x, y: c.y, w: c.w, h: c.h, color: heatRgb(heat), intensity: Math.min(1, heat) })
        }
        // Hottest last: red reads over the yellow fringe of its neighbours.
        out.sort((a, b) => a.intensity - b.intensity)
    }
    renderer.setGlowStyle(glow.spread, glow.alpha)
    renderer.setGlows(out)
}

if (import.meta.env.DEV) {
    ;(window as unknown as { engramGlow: (o: Partial<typeof glow>) => typeof glow }).engramGlow = (o) => {
        Object.assign(glow, o)
        rebuild()
        return { ...glow }
    }
}

const MAX_ZOOM = 1
/**
 * How far out the camera goes: exactly far enough to fit the whole graph
 * (0.10.1) — a fixed 0.05 could not show five thousand notes at once, and
 * let a small graph shrink to a speck. Small graphs still get room to step
 * back (never above FIT_CAP), and FLOOR guards a degenerate fit.
 */
const FIT_CAP = 0.5
const FLOOR = 0.002
const minZoom = ref(0.05)
const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform)

const root = ref<HTMLDivElement | null>(null)
const canvas = ref<HTMLCanvasElement | null>(null)
const probe = ref<HTMLDivElement | null>(null)
const viewport = ref<HTMLDivElement | null>(null)
let renderer: GraphRenderer | null = null
const stats = { frames: 0, ms: 0, last: 0, layoutMs: 0, layouts: 0, cardsMs: 0, rebuilds: 0 }

// ---- camera -----------------------------------------------------------------

/**
 * The camera is a plain object: wheel and pinch write it dozens of times a
 * frame, and as reactive state each write re-ran Vue effects (overlay
 * transform, outline vars, controls, minimap). `frame()` applies it to the
 * DOM directly and publishes `view` for the chrome once per frame.
 */
const cam = { x: 0, y: 0, zoom: 1 }
const view = shallowRef({ x: 0, y: 0, zoom: 1 })
const viewW = ref(1)
const viewH = ref(1)
const t0 = performance.now()
const clock = (): number => (performance.now() - t0) / 1000
let lastZoomAt = -1
let camAnim: { from: { x: number; y: number; zoom: number }; to: { x: number; y: number; zoom: number }; start: number; dur: number } | null = null

/** The camera with its offset on the device-pixel grid — both layers draw with this. */
function snapped(): { x: number; y: number; zoom: number } {
    const d = renderer?.dpr ?? window.devicePixelRatio ?? 1
    return { x: Math.round(cam.x * d) / d, y: Math.round(cam.y * d) / d, zoom: cam.zoom }
}

function clampZoom(z: number): number {
    return Math.min(MAX_ZOOM, Math.max(minZoom.value, z))
}

function setCam(x: number, y: number, zoom: number): void {
    if (zoom !== cam.zoom) lastZoomAt = clock()
    cam.x = x
    cam.y = y
    cam.zoom = zoom
    invalidate()
}

function moveCam(to: { x: number; y: number; zoom: number }, ms = 0): void {
    to = { ...to, zoom: clampZoom(to.zoom) }
    if (ms <= 0) {
        camAnim = null
        setCam(to.x, to.y, to.zoom)
        return
    }
    camAnim = { from: { x: cam.x, y: cam.y, zoom: cam.zoom }, to, start: clock(), dur: ms / 1000 }
    invalidate()
}

function stepCam(now: number): void {
    if (!camAnim) return
    const t = Math.min(1, (now - camAnim.start) / camAnim.dur)
    const e = t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2
    const { from, to } = camAnim
    const zoom = Math.exp(Math.log(from.zoom) + (Math.log(to.zoom) - Math.log(from.zoom)) * e)
    // Keep the screen point under both ends steady: interpolate the world center.
    const fcx = (viewW.value / 2 - from.x) / from.zoom
    const fcy = (viewH.value / 2 - from.y) / from.zoom
    const tcx = (viewW.value / 2 - to.x) / to.zoom
    const tcy = (viewH.value / 2 - to.y) / to.zoom
    const cx = fcx + (tcx - fcx) * e
    const cy = fcy + (tcy - fcy) * e
    setCam(viewW.value / 2 - cx * zoom, viewH.value / 2 - cy * zoom, zoom)
    if (t >= 1) camAnim = null
}

function zoomAt(sx: number, sy: number, factor: number, ms = 0): void {
    const z = clampZoom(cam.zoom * factor)
    const k = z / cam.zoom
    moveCam({ x: sx - (sx - cam.x) * k, y: sy - (sy - cam.y) * k, zoom: z }, ms)
}

/** The scene's world bounds — kept by `rebuild`, read by fit and the zoom limit. */
let bounds = { x0: 0, y0: 0, x1: 1, y1: 1 }

/** Fit with a 3% margin on every side but a 64px top (the floating bar). */
function fitFrame(): { pad: { top: number; left: number }; aw: number; ah: number; zoom: number } {
    const W = viewW.value
    const H = viewH.value
    const pad = { top: 64, left: W * 0.03, right: W * 0.03, bottom: H * 0.03 }
    const aw = Math.max(1, W - pad.left - pad.right)
    const ah = Math.max(1, H - pad.top - pad.bottom)
    const zoom = Math.min(aw / (bounds.x1 - bounds.x0), ah / (bounds.y1 - bounds.y0))
    return { pad, aw, ah, zoom }
}

function updateMinZoom(): void {
    minZoom.value = cards.length ? Math.max(FLOOR, Math.min(FIT_CAP, fitFrame().zoom)) : 0.05
}

function fitView(ms = 0): void {
    if (!cards.length) return
    const { x0, y0, x1, y1 } = bounds
    const { pad, aw, ah, zoom: fit } = fitFrame()
    const zoom = clampZoom(fit)
    moveCam(
        {
            x: pad.left + (aw - (x1 - x0) * zoom) / 2 - x0 * zoom,
            y: pad.top + (ah - (y1 - y0) * zoom) / 2 - y0 * zoom,
            zoom,
        },
        ms,
    )
}

function setCenter(wx: number, wy: number, zoom: number, ms = 0): void {
    moveCam({ x: viewW.value / 2 - wx * zoom, y: viewH.value / 2 - wy * zoom, zoom }, ms)
}

// ---- theme & metrics ----------------------------------------------------------

const tokens = shallowRef<Tokens | null>(null)
const metrics = shallowRef<CardMetrics | null>(null)
const colorCache = new Map<string, RGBA>()

function colorOf(value: string): RGBA {
    let c = colorCache.get(value)
    if (!c) {
        c = resolveColor(probe.value!, value)
        colorCache.set(value, c)
    }
    return c
}

let calibrateTimer = 0
function recalibrate(): void {
    calibrateTimer = 0
    const el = probe.value
    if (!el || !renderer) return
    const m = calibrate(el)
    if (!m) {
        // Not laid out yet (a hidden screen on a cold start): try again shortly.
        calibrateTimer = window.setTimeout(recalibrate, 250)
        return
    }
    colorCache.clear()
    resetMeasures()
    const t = readTokens(el)
    const family = getComputedStyle(el).fontFamily
    labelFont = `11px ${family}`
    renderer.setTheme(t, {
        width: m.width,
        maxH: m.maxH,
        radius: m.radius,
        border: m.border,
        labelFont,
        greek: {
            left: m.border + m.padX,
            top: m.border + m.padY,
            headerH: m.headerH,
            gap: m.gap,
            titleLh: m.title.lh,
            bodyLh: m.body.text.lh,
            contentW: m.width - 2 * (m.border + m.padX),
        },
    })
    tokens.value = t
    metrics.value = m
    fontsSeen = fontState()
    // Load every card font now: the one recalibration that brings lands at
    // startup, not on a later click that happens to show the face first.
    for (const f of [...cardFonts(m), labelFont]) void document.fonts?.load(f)
}

/** `<html>`'s classes minus the drag marker — what a theme change could touch. */
function themeClassOf(): string {
    return [...document.documentElement.classList].filter((c) => c !== 'ui-dragging').join(' ')
}

/** The edge verbs' font: painted into the renderer's label atlas. */
let labelFont = ''
/** Which card and label fonts are loaded, as of the last calibration. The
 * label face counts too: a verb atlas painted before it loads kept the
 * fallback font until some later recalibration. */
let fontsSeen = ''
function fontState(): string {
    const m = metrics.value
    if (!m || !document.fonts) return ''
    return [...cardFonts(m), labelFont]
        .map((f) => (document.fonts.check(f) ? '1' : '0'))
        .join('')
}

/**
 * A font finished loading: recalibrate only when it is one the cards draw
 * with. Recalibrating re-lays out every card — ~200 ms at 5,000 notes — and
 * the first node selection loads Inter 700 for the drawer's title, which no
 * card uses: that click froze the canvas.
 */
function onFontsLoaded(): void {
    if (fontState() !== fontsSeen) scheduleCalibrate()
}

function scheduleCalibrate(): void {
    if (!calibrateTimer) calibrateTimer = window.setTimeout(recalibrate, 30)
}

/** A card exactly like the ones on the canvas, with every part present — read back by `calibrate`. */
const PROBE_NODE: GraphNode = {
    id: '__probe',
    type: 'Decision',
    title: 'Probe',
    body: 'Probe **strong** `code` https://example.dev\n\n- item',
    durability: 'stable',
    source: 'claude',
    session_id: null,
    created_at: 0,
    valid_from: null,
    valid_until: 1,
    status: 'open',
    last_seen: null,
    confirmed_at: null,
    approved_at: 1,
    demoted_at: null,
    trust_override: null,
    trust: 1,
    stale: false,
    code_refs: ['src/probe.ts'],
    tags: ['probe'],
}

// ---- scene ------------------------------------------------------------------

/** Hand-placed positions survive re-layout (the pane is editable). */
const overrides = ref(new Map<string, XY>())
/**
 * The layout re-runs only when the graph's shape changes — which nodes and
 * which arrangement (and Orbit's Anchor exception, so the type). Vue Flow's
 * canvas re-laid everything on every node update and every drag release; at
 * a thousand notes in one component that is dagre for seconds. Hand
 * placements ride on top of the cached result.
 *
 * Edges are read, not watched (0.10.2): drawing a connection re-laid out
 * the whole graph, and the result landed a moment later — mid-drag of the
 * card just connected, teleporting the view. A new or removed edge keeps
 * every card where it is; the next layout (a layout switch, notes added or
 * removed) takes the current edges.
 */
const shapeKey = computed(() => {
    const nodes = visibleNodeList.value.map((n) => (n.type === 'Anchor' ? `${n.id}*` : n.id)).join(',')
    return `${layout.current}|${nodes}`
})
const basePositions = shallowRef(new Map<string, XY>())
let layoutWorker: Worker | null = null
let layoutSeq = 0
try {
    layoutWorker = new Worker(new URL('./layout.worker.ts', import.meta.url), { type: 'module' })
    layoutWorker.onmessage = (e: MessageEvent<{ seq: number; pos: [string, XY][]; ms: number }>) => {
        if (e.data.seq !== layoutSeq) return // a newer shape is already on its way
        basePositions.value = new Map(e.data.pos)
        stats.layoutMs = e.data.ms
        stats.layouts++
    }
} catch {
    layoutWorker = null
}
watch(
    shapeKey,
    () => {
        if (layoutWorker) {
            layoutWorker.postMessage({
                seq: ++layoutSeq,
                mode: layout.current,
                nodes: visibleNodeList.value.map((n) => ({ id: n.id, type: n.type })),
                edges: visibleEdgeList.value.map((e) => ({ from_id: e.from_id, to_id: e.to_id })),
            })
            return
        }
        const t = performance.now()
        basePositions.value = layoutGraph(visibleNodeList.value, visibleEdgeList.value, new Map(), layout.current)
        stats.layoutMs = performance.now() - t
        stats.layouts++
    },
    { immediate: true },
)
const positions = computed(() => {
    if (overrides.value.size === 0) return basePositions.value
    const merged = new Map(basePositions.value)
    for (const [id, o] of overrides.value) if (merged.has(id)) merged.set(id, o)
    return merged
})

const layouts = new WeakMap<GraphNode, { epoch: number; layout: CardLayout }>()
const appearAt = new Map<string, number>()
let lastAppear = -1
let cards: CardDraw[] = []
let cardById = new Map<string, CardDraw>()
const nodeById = shallowRef(new Map<string, GraphNode>())
let hasAnimated = false
/** The dragged node's live position — committed to `overrides` on release. */
let dragPos: { id: string; x: number; y: number } | null = null
const sceneTick = ref(0)

function layoutOf(n: GraphNode): CardLayout {
    const m = metrics.value!
    const hit = layouts.get(n)
    if (hit && hit.epoch === m.epoch) return hit.layout
    const l = layoutCard(n, m, tokens.value!, colorOf(config.accent(n.type)))
    layouts.set(n, { epoch: m.epoch, layout: l })
    return l
}

function rebuild(): void {
    if (!renderer || !metrics.value || !tokens.value) return
    const now = clock()
    const tc = performance.now()
    const pos = positions.value
    const next: CardDraw[] = []
    const byId = new Map<string, CardDraw>()
    const nodes = new Map<string, GraphNode>()
    let dragged: CardDraw | null = null
    for (const n of visibleNodeList.value) {
        const placed = dragPos?.id === n.id ? dragPos : pos.get(n.id)
        if (!placed) continue // laid out in the worker; appears when it lands
        const l = layoutOf(n)
        let appear = appearAt.get(n.id)
        if (appear === undefined) {
            appear = now
            appearAt.set(n.id, now)
            lastAppear = now
        }
        const p = placed
        const card: CardDraw = {
            id: n.id,
            // Whole pixels: the HTML twin's text snaps to the device grid,
            // so the GPU card must start on it too or rows drift by half a pixel.
            x: Math.round(p.x),
            y: Math.round(p.y),
            w: metrics.value.width,
            h: l.h,
            accent: colorOf(config.accent(n.type)),
            heat: heatTint(n.id),
            archived: n.valid_until != null,
            appear,
            layout: l,
        }
        byId.set(n.id, card)
        nodes.set(n.id, n)
        if (dragPos?.id === n.id) dragged = card
        else next.push(card)
    }
    if (dragged) next.push(dragged)
    for (const id of appearAt.keys()) if (!byId.has(id)) {
        appearAt.delete(id)
        renderer.forget(id)
    }
    stats.cardsMs = performance.now() - tc
    stats.rebuilds++
    let x0 = Infinity
    let y0 = Infinity
    let x1 = -Infinity
    let y1 = -Infinity
    for (const c of next) {
        if (c.x < x0) x0 = c.x
        if (c.y < y0) y0 = c.y
        if (c.x + c.w > x1) x1 = c.x + c.w
        if (c.y + c.h > y1) y1 = c.y + c.h
    }
    if (next.length) bounds = { x0, y0, x1, y1 }
    cards = next
    cardById = byId
    updateMinZoom()
    nodeById.value = nodes
    rebuildEdges()
    rebuildGlows()
    sceneTick.value++
    invalidate()
}

/** The edges as last handed to the renderer, in its order: ids and ends. */
let drawnEdges: { id: string; from_id: string; to_id: string }[] = []
/** Per edge: its focus easing (focus.ts) — kept so a change eases from the old value. */
const edgeFocusAt = new Map<string, { from: number; to: number; at: number }>()
let lastFocusAt = -1

function rebuildEdges(): void {
    if (!renderer) return
    const edges: EdgeDraw[] = []
    const drawn: typeof drawnEdges = []
    hasAnimated = false
    // One config lookup per verb, not per edge: each goes through the store's
    // reactive proxies, and a card drag rebuilds every edge per pointer move.
    const styles = new Map<string, { color: RGBA; dashed: boolean; animated: boolean }>()
    for (const e of visibleEdgeList.value) {
        const a = cardById.get(e.from_id)
        const b = cardById.get(e.to_id)
        if (!a || !b) continue
        let st = styles.get(e.type)
        if (!st) {
            st = { color: colorOf(config.edgeColor(e.type)), dashed: config.edgeDashed(e.type), animated: config.edgeAnimated(e.type) }
            styles.set(e.type, st)
        }
        const animated = st.animated
        hasAnimated ||= animated
        drawn.push({ id: e.id, from_id: e.from_id, to_id: e.to_id })
        edges.push({
            id: e.id,
            // Handle outer edges (Vue Flow's anchor points): 4px past the padding box.
            sx: a.x + a.w + 3,
            sy: a.y + a.h / 2,
            tx: b.x - 3,
            ty: b.y + b.h / 2,
            color: st.color,
            dashed: st.dashed,
            animated,
            label: e.type,
            appear: Math.max(a.appear, b.appear),
        })
    }
    drawnEdges = drawn
    renderer.setEdges(edges)
    applyFocus()
}

/**
 * The selection's neighborhood (focus.ts): every drawn edge's focus, eased
 * from its previous one. Runs alone on a selection change — the edges
 * themselves are not rebuilt.
 */
function applyFocus(): void {
    if (!renderer) return
    const sel = selectedId.value
    const hops = sel && cardById.has(sel) ? selectionHops(sel, drawnEdges) : null
    const now = clock()
    const out = new Float32Array(drawnEdges.length * 3)
    const seen = new Set<string>()
    drawnEdges.forEach((e, i) => {
        const focus = edgeFocus(hops, e.from_id, e.to_id)
        let f = edgeFocusAt.get(e.id)
        if (!f) {
            f = { from: focus, to: focus, at: 0 }
            edgeFocusAt.set(e.id, f)
        } else if (f.to !== focus) {
            f.from = f.to
            f.to = focus
            f.at = now
            lastFocusAt = now
        }
        seen.add(e.id)
        out[i * 3] = f.from
        out[i * 3 + 1] = f.to
        out[i * 3 + 2] = f.at
    })
    for (const id of edgeFocusAt.keys()) if (!seen.has(id)) edgeFocusAt.delete(id)
    renderer.setEdgeFocus(out)
}

watch(selectedId, () => {
    applyFocus()
    invalidate()
})

watch(
    [visibleNodeList, visibleEdgeList, positions, metrics, () => config.cfg, () => crowding.heatOn, () => crowding.report],
    rebuild,
)

// ---- frame loop ---------------------------------------------------------------

let raf = 0
let retryTimer = 0
let animTimer = 0
let lastApplied = { x: NaN, y: NaN, zoom: NaN }

/** Push the camera into the DOM: the HTML layer's transform, the outline width, the chrome's view. */
function applyCamera(c: { x: number; y: number; zoom: number }): void {
    if (c.x === lastApplied.x && c.y === lastApplied.y && c.zoom === lastApplied.zoom) return
    if (viewport.value) viewport.value.style.transform = `translate(${c.x}px, ${c.y}px) scale(${c.zoom})`
    if (c.zoom !== lastApplied.zoom && root.value) {
        const w = outlineRem(c.zoom)
        root.value.style.setProperty('--selected-outline-w', `${w.toFixed(3)}rem`)
        root.value.style.setProperty('--selected-outline-o', `${outlineOffset(w).toFixed(3)}rem`)
    }
    lastApplied = { ...c }
    view.value = { ...c }
}
/** Cards whose HTML twin is up: the GPU skips them. */
const glSkip = new Set<string>()

function invalidate(): void {
    if (!raf) raf = requestAnimationFrame(frame)
}

/** Draw now, inside the current task (the HTML swap's frame). */
function renderNow(): void {
    cancelAnimationFrame(raf)
    raf = 0
    frame()
}

function frame(): void {
    raf = 0
    if (!renderer) return
    const now = clock()
    stepCam(now)
    const settled = now - lastZoomAt > 0.3
    const sc = snapped()
    applyCamera(sc)
    const start = performance.now()
    const appearing = now - lastAppear < 0.4 || now - lastFocusAt < FOCUS_EASE + 0.05
    const more = renderer.render(sc, now, cards, glSkip, settled, appearing)
    const ms = performance.now() - start
    stats.frames++
    stats.ms += ms
    stats.last = ms
    const live = layout.view === 'graph'
    if (more || camAnim || !settled || appearing) invalidate()
    else if (hasAnimated && live && renderer.animatedVisible) {
        // Only dashes are moving: 30 fps is plenty, and nothing runs when
        // no animated edge is on screen.
        if (!animTimer) {
            animTimer = window.setTimeout(() => {
                animTimer = 0
                invalidate()
            }, 33)
        }
    } else if (renderer.backoff && !retryTimer) {
        retryTimer = window.setTimeout(() => {
            retryTimer = 0
            invalidate()
        }, 500)
    }
}

// ---- HTML twins: the selected and the hovered card -------------------------------

const hoveredId = ref<string | null>(null)
const expandedId = ref<string | null>(null)
const lingering = reactive(new Set<string>())
const lingerTimers = new Set<number>()
const mounted = shallowRef<string[]>([])

const wanted = computed(() => {
    const ids = [selectedId.value, ...lingering, hoveredId.value].filter((id): id is string => !!id && nodeById.value.has(id))
    return [...new Set(ids)]
})

/** Twins mounted but still hidden: laid out a frame ahead of their reveal. */
const pendingTwins = reactive(new Set<string>())

/**
 * The swap is atomic: a new twin mounts hidden; on the next frame one rAF
 * task reveals it, tells the GPU to skip its card and draws — DOM and
 * canvas change in the same composited frame. Dropping a twin is the
 * mirror image. Anything else showed for a frame: both cards at once
 * (the frosted HTML over its GPU self — a flash) or neither.
 */
watch(wanted, (want) => {
    const add = want.filter((id) => !mounted.value.includes(id))
    if (add.length) {
        for (const id of add) pendingTwins.add(id)
        mounted.value = [...mounted.value, ...add]
    }
    requestAnimationFrame(() => {
        void nextTick().then(() => {
            const now = wanted.value
            for (const id of mounted.value) {
                if (now.includes(id)) glSkip.add(id)
                else glSkip.delete(id)
                pendingTwins.delete(id)
            }
            mounted.value = mounted.value.filter((id) => now.includes(id))
            if (hoveredId.value && now.includes(hoveredId.value)) {
                const id = hoveredId.value
                requestAnimationFrame(() => {
                    if (hoveredId.value === id) expandedId.value = id
                })
            }
            renderNow()
        })
    })
})

const htmlCards = computed(() => {
    void sceneTick.value
    const order = [...mounted.value].sort((a, b) => Number(a === hoveredId.value) - Number(b === hoveredId.value))
    return order.flatMap((id) => {
        const card = cardById.get(id)
        const node = nodeById.value.get(id)
        return card && node ? [{ id, card, node }] : []
    })
})

function setHover(id: string | null): void {
    const prev = hoveredId.value
    if (id === prev) return
    hoveredId.value = id
    if (expandedId.value !== id) expandedId.value = null
    if (id && mounted.value.includes(id) && glSkip.has(id)) {
        // Already up (the selected card): expand on the next frame.
        requestAnimationFrame(() => {
            if (hoveredId.value === id) expandedId.value = id
        })
    }
    if (prev) {
        lingering.add(prev)
        const timer = window.setTimeout(() => {
            lingerTimers.delete(timer)
            lingering.delete(prev)
        }, 200)
        lingerTimers.add(timer)
    }
}

// ---- input ------------------------------------------------------------------

interface Press {
    kind: 'pan' | 'node' | 'connect'
    pointer: number
    x0: number
    y0: number
    lx: number
    ly: number
    moved: boolean
    touch: boolean
    nodeId?: string
    origin?: XY
    side?: 'source' | 'target'
}

const locked = ref(false)
let press: Press | null = null
let lastTap: { t: number; x: number; y: number } | null = null
const pointers = new Map<number, { x: number; y: number }>()
let pinch: { d0: number; z0: number; wx: number; wy: number } | null = null
const connectTo = ref<XY | null>(null)
const connectFrom = ref<{ id: string; side: 'source' | 'target' } | null>(null)
const pendingConnection = ref<{ source: string; target: string } | null>(null)
const tooltip = ref<{ x: number; y: number; text: string } | null>(null)

function local(e: { clientX: number; clientY: number }): { x: number; y: number } {
    const r = root.value!.getBoundingClientRect()
    return { x: e.clientX - r.left, y: e.clientY - r.top }
}

function hitTest(sx: number, sy: number): string | null {
    const wx = (sx - cam.x) / cam.zoom
    const wy = (sy - cam.y) / cam.zoom
    for (let i = cards.length - 1; i >= 0; i--) {
        const c = cards[i]!
        if (wx >= c.x && wx <= c.x + c.w && wy >= c.y && wy <= c.y + c.h) return c.id
    }
    return null
}

function nodeAt(target: EventTarget | null, sx: number, sy: number): string | null {
    const el = (target as Element | null)?.closest?.('[data-node-id]')
    return el?.getAttribute('data-node-id') ?? hitTest(sx, sy)
}

function ours(target: EventTarget | null): boolean {
    const el = target as Element | null
    return !!el && (el === canvas.value || !!el.closest?.('.gl-viewport'))
}

function onPointerDown(e: PointerEvent): void {
    if (!ours(e.target)) return
    if (e.pointerType === 'mouse' && e.button !== 0) return
    const p = local(e)
    pointers.set(e.pointerId, p)
    if (pointers.size === 2) {
        const [a, b] = [...pointers.values()] as [XY, XY]
        const mx = (a.x + b.x) / 2
        const my = (a.y + b.y) / 2
        pinch = { d0: Math.hypot(a.x - b.x, a.y - b.y) || 1, z0: cam.zoom, wx: (mx - cam.x) / cam.zoom, wy: (my - cam.y) / cam.zoom }
        press = null
        connectFrom.value = null
        dragPos = null
        return
    }
    const el = e.target as Element
    if (!el.closest('a')) e.preventDefault()
    const base = { pointer: e.pointerId, x0: p.x, y0: p.y, lx: p.x, ly: p.y, moved: false, touch: e.pointerType !== 'mouse' }
    const handle = el.closest('[data-handle]')
    const nodeId = nodeAt(e.target, p.x, p.y)
    if (handle && nodeId && !locked.value) {
        press = { ...base, kind: 'connect', nodeId, side: handle.getAttribute('data-handle') as 'source' | 'target' }
    } else if (nodeId) {
        const c = cardById.get(nodeId)
        press = { ...base, kind: 'node', nodeId, origin: c ? { x: c.x, y: c.y } : { x: 0, y: 0 } }
    } else {
        press = { ...base, kind: 'pan' }
    }
}

let lastPointer: { x: number; y: number; cx: number; cy: number } | null = null

function onPointerMove(e: PointerEvent): void {
    const p = local(e)
    if (e.pointerType === 'mouse') lastPointer = { ...p, cx: e.clientX, cy: e.clientY }
    if (pointers.has(e.pointerId)) pointers.set(e.pointerId, p)
    if (pinch && pointers.size >= 2) {
        const [a, b] = [...pointers.values()] as [XY, XY]
        const z = clampZoom(pinch.z0 * (Math.hypot(a.x - b.x, a.y - b.y) / pinch.d0))
        const mx = (a.x + b.x) / 2
        const my = (a.y + b.y) / 2
        camAnim = null
        setCam(mx - pinch.wx * z, my - pinch.wy * z, z)
        return
    }
    if (press && press.pointer === e.pointerId) {
        const dx = p.x - press.lx
        const dy = p.y - press.ly
        press.lx = p.x
        press.ly = p.y
        if (!press.moved) {
            if (Math.hypot(p.x - press.x0, p.y - press.y0) < (press.touch ? 6 : 3)) return
            press.moved = true
            root.value?.setPointerCapture(e.pointerId)
            document.documentElement.classList.add('ui-dragging')
            if (press.kind === 'node' && locked.value) press.kind = 'pan'
            if (press.kind === 'connect') connectFrom.value = { id: press.nodeId!, side: press.side! }
            setHover(null)
        }
        if (press.kind === 'pan') {
            camAnim = null
            setCam(cam.x + dx, cam.y + dy, cam.zoom)
        } else if (press.kind === 'node') {
            const o = press.origin!
            dragPos = {
                id: press.nodeId!,
                x: Math.round(o.x + (p.x - press.x0) / cam.zoom),
                y: Math.round(o.y + (p.y - press.y0) / cam.zoom),
            }
            const c = cardById.get(press.nodeId!)
            if (c) {
                c.x = dragPos.x
                c.y = dragPos.y
                // The dragged card rides on top.
                cards = [...cards.filter((x) => x !== c), c]
            }
            rebuildEdges()
            sceneTick.value++
            invalidate()
        } else {
            connectTo.value = { x: (p.x - cam.x) / cam.zoom, y: (p.y - cam.y) / cam.zoom }
        }
        return
    }
    if (e.pointerType === 'mouse' && !press && ours(e.target)) {
        const id = nodeAt(e.target, p.x, p.y)
        setHover(id)
        labelHover(id ? null : p)
    }
}

function endPress(e: PointerEvent, cancelled: boolean): void {
    pointers.delete(e.pointerId)
    if (pinch) {
        if (pointers.size < 2) pinch = null
        return
    }
    if (!press || press.pointer !== e.pointerId) return
    const pr = press
    press = null
    document.documentElement.classList.remove('ui-dragging')
    if (root.value?.hasPointerCapture(e.pointerId)) root.value.releasePointerCapture(e.pointerId)
    if (cancelled) {
        dragPos = null
        connectFrom.value = null
        connectTo.value = null
        rebuild()
        return
    }
    if (!pr.moved) {
        // Double click / double tap zooms in ×2 at the pointer (shift: out), like Vue Flow's pane.
        const p = local(e)
        const now = performance.now()
        if (lastTap && now - lastTap.t < 320 && Math.hypot(p.x - lastTap.x, p.y - lastTap.y) < 24) {
            lastTap = null
            zoomAt(p.x, p.y, e.shiftKey ? 0.5 : 2, 260)
            return
        }
        lastTap = { t: now, x: p.x, y: p.y }
        if (pr.kind === 'node') {
            store.select(pr.nodeId!)
            if (pr.touch) setHover(pr.nodeId!)
        } else if (pr.kind === 'pan') {
            store.select(null)
            if (pr.touch) setHover(null)
        }
        return
    }
    if (pr.kind === 'node' && dragPos) {
        const next = new Map(overrides.value)
        next.set(dragPos.id, { x: dragPos.x, y: dragPos.y })
        dragPos = null
        overrides.value = next
    } else if (pr.kind === 'connect') {
        const p = local(e)
        const under = document.elementFromPoint(e.clientX, e.clientY)
        const target = nodeAt(under, p.x, p.y)
        if (target && target !== pr.nodeId) {
            pendingConnection.value = pr.side === 'source' ? { source: pr.nodeId!, target } : { source: target, target: pr.nodeId! }
        }
        connectFrom.value = null
        connectTo.value = null
    }
}

function onPointerLeave(e: PointerEvent): void {
    if (e.pointerType === 'mouse' && !press) {
        setHover(null)
        tooltip.value = null
    }
}

function onWheel(e: WheelEvent): void {
    if (!ours(e.target)) return
    e.preventDefault()
    const p = local(e)
    camAnim = null
    pauseHover()
    if (e.ctrlKey || e.metaKey) {
        const delta = -e.deltaY * (e.deltaMode === 1 ? 0.05 : e.deltaMode ? 1 : 0.002) * (e.ctrlKey ? 10 : 1)
        zoomAt(p.x, p.y, Math.pow(2, delta))
        return
    }
    const norm = e.deltaMode === 1 ? 20 : 1
    let dx = e.deltaX * norm
    let dy = e.deltaY * norm
    if (!IS_MAC && e.shiftKey && !dx && dy) {
        dx = dy
        dy = 0
    }
    setCam(cam.x - dx * 0.5, cam.y - dy * 0.5, cam.zoom)
}

/** Mid-scroll, cards slide under a still pointer: no hover twins until it rests. */
let wheelHoverTimer = 0
function pauseHover(): void {
    if (hoveredId.value) setHover(null)
    tooltip.value = null
    window.clearTimeout(wheelHoverTimer)
    wheelHoverTimer = window.setTimeout(() => {
        if (!lastPointer || press) return
        const el = document.elementFromPoint(lastPointer.cx, lastPointer.cy)
        if (ours(el)) setHover(nodeAt(el, lastPointer.x, lastPointer.y))
    }, 150)
}

/** An edge's free-text note shows over its label (EngramEdge's <title>). */
function labelHover(p: XY | null): void {
    if (!p || !renderer) {
        tooltip.value = null
        return
    }
    const wx = (p.x - cam.x) / cam.zoom
    const wy = (p.y - cam.y) / cam.zoom
    const box = renderer.labels.find((b) => wx >= b.x && wx <= b.x + b.w && wy >= b.y && wy <= b.y + b.h)
    const note = box ? store.edges.get(box.edgeId)?.note : null
    tooltip.value = note ? { x: p.x + 12, y: p.y + 16, text: note } : null
}

// ---- controls & minimap ----------------------------------------------------------

function zoomBy(factor: number): void {
    zoomAt(viewW.value / 2, viewH.value / 2, factor, 200)
}

const miniNodes = computed<MiniNode[]>(() => {
    void sceneTick.value
    void tokens.value
    const heat = crowding.heatOn
    return cards.map((c) => ({
        x: c.x,
        y: c.y,
        w: c.w,
        h: c.h,
        color: (heat && heatColor(crowding.heatOf(c.id))) || css(c.accent),
    }))
})
const maskColor = computed(() => (tokens.value && probe.value ? css(resolveColor(probe.value, 'var(--surface-overlay)')) : 'transparent'))

function onMiniCenter(x: number, y: number): void {
    setCenter(x, y, cam.zoom, 250)
}

function onMiniZoom(factor: number): void {
    zoomBy(factor)
}

// ---- selection outline (zoom-compensated, as GraphFlow) ------------------------------

const OUTLINE_STOPS: [number, number][] = [
    [1, 0.2],
    [0.5, 0.4],
    [0.25, 0.8],
    [0.1, 1.2],
    // Below 0.1 the ring holds ~2 screen px (0.10.1: at 0.03 — five
    // thousand notes fitted — the old stops left it under one pixel).
    [0.05, 3.4],
    [0.025, 7.4],
    [0.01, 18],
]

/** The gap outside the card: 0.3rem at 100%, ~1.2× the width when zoomed far out. */
function outlineOffset(w: number): number {
    return w * 1.2 + 0.06
}

function outlineRem(z: number): number {
    let [hiZ, hiW] = OUTLINE_STOPS[0]!
    if (z >= hiZ) return hiW
    for (const [loZ, loW] of OUTLINE_STOPS.slice(1)) {
        if (z >= loZ) return hiW + (loW - hiW) * ((hiZ - z) / (hiZ - loZ))
        ;[hiZ, hiW] = [loZ, loW]
    }
    return hiW
}

const connectPath = computed(() => {
    const from = connectFrom.value
    const to = connectTo.value
    const c = from ? cardById.get(from.id) : null
    if (!from || !to || !c) return ''
    const s = from.side === 'source' ? { x: c.x + c.w + 3, y: c.y + c.h / 2 } : { x: c.x - 3, y: c.y + c.h / 2 }
    const mid = Math.abs(to.x - s.x) / 2
    const dir = from.side === 'source' ? 1 : -1
    return `M${s.x},${s.y} C${s.x + dir * mid},${s.y} ${to.x - dir * mid},${to.y} ${to.x},${to.y}`
})

// ---- view sync (as GraphFlow) ------------------------------------------------------

let didInitialFit = false
watch(sceneTick, async () => {
    if (didInitialFit || cards.length === 0) return
    didInitialFit = true
    await nextTick()
    fitView()
})

watch(
    () => layout.current,
    async () => {
        await nextTick()
        fitView(400)
    },
)

watch(
    () => layout.view,
    async (v) => {
        if (v !== 'graph') return
        invalidate()
        await nextTick()
        const c = selectedId.value ? cardById.get(selectedId.value) : undefined
        if (c) setCenter(c.x + c.w / 2, c.y + c.h / 2, cam.zoom, 300)
    },
)

watch(
    () => projects.switchEpoch,
    async () => {
        await nextTick()
        await new Promise(requestAnimationFrame)
        if (cards.length > 0) fitView(400)
    },
)

// Drop overrides for nodes that no longer exist (filtered-out ones keep theirs).
watch(
    computed(() => [...store.nodes.values()]),
    (list) => {
        const ids = new Set(list.map((n) => n.id))
        let changed = false
        const next = new Map(overrides.value)
        for (const id of next.keys()) {
            if (!ids.has(id)) {
                next.delete(id)
                changed = true
            }
        }
        if (changed) overrides.value = next
    },
)

// ---- lifecycle ------------------------------------------------------------------

let resizeObs: ResizeObserver | null = null
let themeObs: MutationObserver | null = null

function resize(): void {
    const el = root.value
    if (!el || !renderer) return
    const w = el.clientWidth
    const h = el.clientHeight
    if (!w || !h) return
    viewW.value = w
    viewH.value = h
    renderer.setSize(w, h, Math.min(window.devicePixelRatio || 1, 3))
    updateMinZoom()
    invalidate()
}

function onLost(e: Event): void {
    e.preventDefault()
    emit('lost')
}

onMounted(() => {
    try {
        renderer = new GraphRenderer(canvas.value!)
    } catch {
        emit('lost')
        return
    }
    canvas.value!.addEventListener('webglcontextlost', onLost)
    root.value!.addEventListener('wheel', onWheel, { passive: false })
    resizeObs = new ResizeObserver(resize)
    resizeObs.observe(root.value!)
    resize()
    setTextHost(probe.value)
    recalibrate()
    // `ui-dragging` is a drag in progress (this canvas and the Tasks board
    // toggle it), not a theme: recalibrating on it re-laid out every card at
    // each drag's start and end — ~150 ms stalls at 5,000 notes, and the
    // cards repainted at half resolution while they caught up.
    let themeClass = themeClassOf()
    themeObs = new MutationObserver((records) => {
        const cls = themeClassOf()
        if (records.some((r) => r.attributeName !== 'class') || cls !== themeClass) scheduleCalibrate()
        themeClass = cls
    })
    themeObs.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'style', 'class'] })
    document.fonts?.addEventListener('loadingdone', onFontsLoaded)
    // Bench / test hook: frame stats and where each card sits on screen.
    ;(window as unknown as { __engramGL?: unknown }).__engramGL = {
        stats,
        cam,
        cards: () => cards.length,
        health: () => (renderer ? { blank: renderer.blank, ...renderer.atlasInfo() } : null),
        screenRects: () =>
            cards.map((c) => ({ id: c.id, x: c.x * cam.zoom + cam.x, y: c.y * cam.zoom + cam.y, w: c.w * cam.zoom, h: c.h * cam.zoom })),
        setCam: (x: number, y: number, zoom: number) => moveCam({ x, y, zoom }),
        find: (title: string) => visibleNodeList.value.find((n) => n.title === title)?.id ?? null,
        rect: (id: string) => {
            const c = cardById.get(id)
            return c ? { x: c.x, y: c.y, w: c.w, h: c.h, parts: c.layout.parts } : null
        },
        /** Mount the HTML twin at rest (as a hover would, without expanding). */
        showTwin: (id: string) => {
            lingering.add(id)
        },
    }
})

onBeforeUnmount(() => {
    cancelAnimationFrame(raf)
    window.clearTimeout(retryTimer)
    window.clearTimeout(animTimer)
    window.clearTimeout(wheelHoverTimer)
    for (const t of lingerTimers) window.clearTimeout(t)
    delete (window as unknown as { __engramGL?: unknown }).__engramGL
    window.clearTimeout(calibrateTimer)
    resizeObs?.disconnect()
    themeObs?.disconnect()
    document.fonts?.removeEventListener('loadingdone', onFontsLoaded)
    root.value?.removeEventListener('wheel', onWheel)
    renderer?.dispose()
    renderer = null
    layoutWorker?.terminate()
})

watch(
    () => config.cfg,
    () => scheduleCalibrate(),
)
</script>

<template>
<div
    ref="root"
    class="gl-root"
    :class="{ glass: tokens?.glass }"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="endPress($event, false)"
    @pointercancel="endPress($event, true)"
    @pointerleave="onPointerLeave"
>
    <div class="gl-slide">
        <canvas ref="canvas" class="gl-canvas" />
        <div ref="viewport" class="gl-viewport">
            <div
                v-for="h in htmlCards"
                :key="h.id"
                class="gl-card"
                :class="{ pending: pendingTwins.has(h.id) }"
                :data-node-id="h.id"
                :style="{ left: `${h.card.x}px`, top: `${h.card.y}px`, '--rest-h': `${h.card.h}px`, '--heat-bg': heatBg(h.id) }"
            >
                <EngramCard :node="h.node" :selected="h.id === selectedId" :expanded="h.id === expandedId" :appear="false">
                    <template #handles>
                        <span class="gl-handle left" data-handle="target" />
                        <span class="gl-handle right" data-handle="source" />
                    </template>
                </EngramCard>
            </div>
            <svg v-if="connectPath" class="gl-connection">
                <path :d="connectPath" />
            </svg>
        </div>
    </div>

    <div v-if="tooltip" class="gl-tooltip gl-chrome" :style="{ left: `${tooltip.x}px`, top: `${tooltip.y}px` }">{{ tooltip.text }}</div>

    <GraphControls
        :locked="locked"
        :can-zoom-in="view.zoom < MAX_ZOOM"
        :can-zoom-out="view.zoom > minZoom + 1e-6"
        :heat="crowding.heatOn"
        @zoom-in="zoomBy(1.2)"
        @zoom-out="zoomBy(1 / 1.2)"
        @fit="fitView(300)"
        @toggle-lock="locked = !locked"
        @toggle-heat="crowding.toggleHeat()"
    />
    <GraphMiniMap
        :nodes="miniNodes"
        :camera="view"
        :view-w="viewW"
        :view-h="viewH"
        :mask="maskColor"
        :tick="sceneTick"
        @center="onMiniCenter"
        @zoom="onMiniZoom"
    />

    <div ref="probe" class="gl-probe" aria-hidden="true">
        <EngramCard :node="PROBE_NODE" :appear="false" />
    </div>

    <ConnectDialog
        v-if="pendingConnection"
        :source="pendingConnection.source"
        :target="pendingConnection.target"
        @close="pendingConnection = null"
    />
</div>
</template>

<style scoped>
.gl-root {
    position: relative;
    width: 100%;
    height: 100%;
    overflow: hidden;
    background-color: var(--canvas-bg);
    touch-action: none;
    cursor: grab;
    user-select: none;
}

.gl-slide {
    position: absolute;
    inset: 0;
}

.gl-canvas {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    display: block;
}

.gl-viewport {
    position: absolute;
    top: 0;
    left: 0;
    transform-origin: 0 0;
    pointer-events: none;
}

.gl-card {
    position: absolute;
    pointer-events: all;
    cursor: grab;
}

.gl-card.pending {
    visibility: hidden;
}

/* Glass at rest (brand theme): the GPU cards are frosted, so their HTML
   twin is too — same surface, same blur, no seam when one swaps in. */
.gl-root.glass .gl-card :deep(.engram-node:not(.expanded, :hover)) {
    background-color: color-mix(in srgb, var(--surface-elevated) 80%, transparent);
}

/*
 * The dots sit where the edges end: the resting card's middle (edges meet
 * y + h/2 of the GPU card). A hovered or expanded card lifts 0.6rem and grows,
 * so 50% would drift — the dot stays at the resting middle instead, offset
 * by the lift and eased with EngramCard's own margin transition (fast in,
 * slow out) so it holds still on screen while the card rises around it.
 * `top` counts from inside the card's 1px border, the edge's middle from
 * its outer edge: the border comes off.
 */
.gl-handle {
    position: absolute;
    top: calc(var(--rest-h) / 2 - 1px);
    width: 0.9rem;
    height: 0.9rem;
    border: 2px solid var(--surface-base);
    border-radius: 100%;
    background-color: var(--border-strong);
    transform: translateY(-50%);
    cursor: crosshair;
    transition: top var(--duration-fast) var(--ease-enter);
}

.gl-card :deep(.engram-node:is(:hover, .expanded)) .gl-handle {
    top: calc(var(--rest-h) / 2 - 1px + 0.6rem);
    transition: top var(--duration-slow) var(--ease-default);
}

.gl-handle.left {
    left: -4px;
}

.gl-handle.right {
    right: -4px;
}

.gl-connection {
    position: absolute;
    overflow: visible;
    width: 1px;
    height: 1px;
    pointer-events: none;
}

.gl-connection path {
    fill: none;
    stroke: var(--border-strong);
    stroke-width: 2;
}

.gl-tooltip {
    position: absolute;
    z-index: 6;
    max-width: 32rem;
    padding: 0.4rem 0.8rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md);
    background-color: var(--surface-glass);
    backdrop-filter: var(--glass-backdrop);
    box-shadow: var(--shadow-md);
    color: var(--text-secondary);
    font-size: var(--text-caption);
    pointer-events: none;
}

.gl-probe {
    position: absolute;
    top: 0;
    left: -10000px;
    visibility: hidden;
    pointer-events: none;
}
</style>
