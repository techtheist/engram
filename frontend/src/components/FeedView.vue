<script setup lang="ts">
import UiButton from '@/components/common/UiButton.vue'
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, useTemplateRef, watch } from 'vue'
import { storeToRefs } from 'pinia'
import EmptyGraph from '@/components/common/EmptyGraph.vue'
import FeedCard from '@/components/feed/FeedCard.vue'
import { HeightMap, anchorShift, distanceToLine } from '@/components/feed/heightMap'
import SegmentedControl from '@/components/common/SegmentedControl.vue'
import { onProjectSwitch } from '@/composables/onProjectSwitch'
import { useGraphStore } from '@/stores/graph'
import { useConfigStore } from '@/stores/config'
import { useLayoutStore } from '@/stores/layout'
import type { GraphEdge, GraphNode } from '@/types/graph'

/**
 * The feed screen (the "timeline" idea): no graph — a vertical, centered
 * feed of node cards with the neighbors peeking above and below, so depth
 * is always visible. Two lenses over the same feed:
 *   - Timeline: everything, chronological, with version markers where the
 *     project moved on — history reads as a story.
 *   - Review: only what needs a human eye (provisional / stale / drifted),
 *     with the judgment actions inline.
 * Cards traverse: clicking a neighbor jumps the feed there, and Back walks
 * the trail (session-scoped, capped) — the graph's edges as navigation.
 */
const store = useGraphStore()
const layout = useLayoutStore()
const config = useConfigStore()
const { visibleNodeList, nodeList, nodes, edgeList, driftByNode } = storeToRefs(store)

type FeedMode = 'timeline' | 'review'
const mode = ref<FeedMode>('timeline')
const sort = ref<string>('newest')

const MODE_OPTIONS = [
    { value: 'timeline', label: 'Timeline' },
    { value: 'review', label: 'Review' },
]

const sortOptions = computed(() =>
    mode.value === 'review'
        ? [
              { value: 'newest', label: 'newest' },
              { value: 'oldest', label: 'oldest' },
              { value: 'trust', label: 'low trust' },
          ]
        : [
              { value: 'newest', label: 'newest' },
              { value: 'oldest', label: 'oldest' },
          ],
)

/* Each lens has its own natural order: the timeline reads newest-first, the
   review backlog leads with the weakest trust — the notes most likely to be
   wrong. Switching lens also returns to the top of the new list. */
watch(mode, (m) => {
    sort.value = m === 'review' ? 'trust' : 'newest'
    void nextTick(() => jump('top'))
})

// ---- the feed list ---------------------------------------------------------

const active = (n: GraphNode): boolean => n.valid_until == null

/** Needs a human eye — the Review lens (mirrors the Review drawer's backlog). */
function needsReview(n: GraphNode): boolean {
    if (!active(n)) return false
    return (
        (n.source === 'claude' && n.approved_at == null && n.trust_override == null) ||
        n.stale ||
        driftByNode.value.has(n.id)
    )
}

/** Traversal targets stay visible even when the lens or filters exclude them. */
const extraIds = ref<Set<string>>(new Set())
watch([mode, () => store.filters], () => extraIds.value.clear(), { deep: true })

const feedNodes = computed<GraphNode[]>(() => {
    const visible = new Set(visibleNodeList.value.map((n) => n.id))
    const base = nodeList.value.filter(
        (n) =>
            extraIds.value.has(n.id) ||
            (visible.has(n.id) && (mode.value === 'timeline' || needsReview(n))),
    )
    const s = sort.value
    return base.sort((a, b) =>
        s === 'oldest'
            ? a.created_at - b.created_at
            : s === 'trust'
              ? a.trust - b.trust || b.created_at - a.created_at
              : b.created_at - a.created_at,
    )
})

/** Cards interleaved with version markers (timeline only). */
type FeedItem = { kind: 'node'; node: GraphNode } | { kind: 'version'; label: string; key: string }

const items = computed<FeedItem[]>(() => {
    if (mode.value === 'review') return feedNodes.value.map((node) => ({ kind: 'node', node }))
    const out: FeedItem[] = []
    let lastVersion: string | null | undefined
    for (const node of feedNodes.value) {
        const v = node.version ?? null
        if (v !== lastVersion && v != null) {
            out.push({ kind: 'version', label: v, key: `v-${v}-${node.id}` })
        }
        if (v != null) lastVersion = v
        out.push({ kind: 'node', node })
    }
    return out
})

// ---- edges on a card -------------------------------------------------------

const EMPTY: readonly never[] = Object.freeze([])

const outEdges = computed(() => {
    const m = new Map<string, GraphEdge[]>()
    for (const e of edgeList.value) {
        if (!m.has(e.from_id)) m.set(e.from_id, [])
        m.get(e.from_id)!.push(e)
    }
    return m
})

const inEdges = computed(() => {
    const m = new Map<string, GraphEdge[]>()
    for (const e of edgeList.value) {
        if (!m.has(e.to_id)) m.set(e.to_id, [])
        m.get(e.to_id)!.push(e)
    }
    return m
})

/** Same cap as FeedCard's chip rows — only shown chips shape a card. */
const EDGE_CAP = 6

function title(id: string): string {
    return nodes.value.get(id)?.title ?? id
}

// ---- geometry: measured heights, the window, the mount budget ---------------
//
// The feed keeps its full length, but only the cards within a screen of the
// viewport exist (0.10.1): a thousand mounted cards cost ~160 ms on every
// screen switch and ~120 ms a frame in a fling. Positions come from a
// height map (feed/heightMap.ts) filled by measuring each card once, off
// screen, in idle time — real heights, so a jump lands exactly at any
// distance. A card that has not been measured yet stands in with an
// estimate; when its measurement lands above the centered card, the scroll
// moves by the same amount and nothing on screen shifts.

const feedEl = useTemplateRef<HTMLElement>('feedEl')
const trackEl = useTemplateRef<HTMLElement>('trackEl')
const currentId = ref<string | null>(null)

const keyOf = (it: FeedItem): string => (it.kind === 'node' ? it.node.id : it.key)

/** What a card's height depends on — a change re-measures it. */
function sigOf(it: FeedItem): string {
    if (it.kind === 'version') return `v|${it.label}`
    const n = it.node
    const outs = (outEdges.value.get(n.id) ?? EMPTY).slice(0, EDGE_CAP).map((e) => e.type + title(e.to_id))
    const ins = (inEdges.value.get(n.id) ?? EMPTY).slice(0, EDGE_CAP).map((e) => e.type + title(e.from_id))
    return [
        n.type,
        n.version,
        n.status,
        n.valid_until != null,
        driftByNode.value.has(n.id),
        n.title,
        n.body,
        n.tags.join(' '),
        n.code_refs.join(' '),
        Math.round(n.trust * 100),
        outs.join('\u0001'),
        ins.join('\u0001'),
    ].join('\u0002')
}

interface Measured {
    sig: string
    h: number
    /** The layout generation it was measured under (width, theme, fonts). */
    gen: number
}

const geo = new HeightMap()
const measured = new Map<string, Measured>()
const sigs = new Map<string, string>()
let itemByKey = new Map<string, FeedItem>()
/** Live heights of the mounted cards (ResizeObserver) — they win over the cache. */
const seen = new Map<string, number>()
let gen = 0
let estimate = 220
let firstNode = -1
let lastNode = -1
/** The track's top inside the scroller (the feed's top padding). */
let trackTop = 0
/** Bumped whenever the geometry changes; the template reads it. */
const layoutSeq = ref(0)

const fresh = (k: string): boolean => {
    const m = measured.get(k)
    return m !== undefined && m.gen === gen && m.sig === sigs.get(k)
}

function heightFor(k: string): number {
    return seen.get(k) ?? measured.get(k)?.h ?? estimate
}

/** A new list (lens, sort, filters, an edit): new map, the centered card stays put. */
function rebuild(list: FeedItem[]): void {
    const root = feedEl.value
    const anchor = currentId.value
    const ai = anchor ? geo.indexOf(anchor) : -1
    const before = root && ai >= 0 ? trackTop + geo.offset(ai) - root.scrollTop : null
    sigs.clear()
    itemByKey = new Map()
    const keys = list.map((it) => {
        const k = keyOf(it)
        sigs.set(k, sigOf(it))
        itemByKey.set(k, it)
        return k
    })
    geo.reset(keys, heightFor)
    firstNode = list.findIndex((it) => it.kind === 'node')
    lastNode = -1
    for (let i = list.length - 1; i >= 0 && lastNode < 0; i--) if (list[i]!.kind === 'node') lastNode = i
    layoutSeq.value++
    const ni = anchor ? geo.indexOf(anchor) : -1
    if (root && before != null && ni >= 0) {
        const top = trackTop + geo.offset(ni) - before
        void nextTick(() => (root.scrollTop = top))
    }
    queueFrame()
    scheduleMeasure()
}

let pendingShift = 0
let shiftBase: number | null = null
/** The card a smooth scroll is gliding to — a correction re-aims it. */
let glideTo: string | null = null
let glideTimer: number | undefined
let glideAt = 0

/** Apply height changes; the view stays still for changes above the centered card. */
function applyChanges(changes: [number, number][]): void {
    if (!changes.length) return
    const root = feedEl.value
    const anchor = currentId.value ? geo.indexOf(currentId.value) : -1
    const shift = anchorShift(changes, anchor)
    layoutSeq.value++
    if (shift && root) {
        // Absolute, not `+=`: Chrome re-snaps a snapped card after layout
        // on its own, and a relative shift on top of that moved the view a
        // second time. After the DOM update, so the track already has its
        // new length and the browser does not clamp to the old one.
        if (shiftBase === null) {
            shiftBase = root.scrollTop
            void nextTick(() => {
                root.scrollTop = shiftBase! + pendingShift
                shiftBase = null
                pendingShift = 0
                // An instant scroll cancels a smooth one: aim it again.
                if (glideTo) glide(glideTo, root)
            })
        }
        pendingShift += shift
    }
    queueFrame()
}

/** The scroll position that centers an item. */
function centerOf(key: string, root: HTMLElement): number {
    const i = geo.indexOf(key)
    return trackTop + geo.offset(i) + geo.height(i) / 2 - root.clientHeight / 2
}

function glide(key: string, root: HTMLElement): void {
    glideTo = key
    glideAt = performance.now()
    window.clearTimeout(glideTimer)
    glideTimer = window.setTimeout(endGlide, 1500)
    root.scrollTo({ top: centerOf(key, root), behavior: 'smooth' })
}

/** scrollend also fires for the instant hop that starts a glide — not the end. */
function onScrollEnd(): void {
    if (performance.now() - glideAt > 150) endGlide()
}

function endGlide(): void {
    glideTo = null
    window.clearTimeout(glideTimer)
}

// -- the measurer: cards rendered off screen, read, discarded --

const measureKeys = shallowRef<string[]>([])
const measureEl = useTemplateRef<HTMLElement>('measureEl')
let measuring: Promise<void> | null = null

async function measure(keys: string[]): Promise<void> {
    while (measuring) await measuring
    if (!keys.length) return
    measuring = (async () => {
        measureKeys.value = keys
        await nextTick()
        const host = measureEl.value
        const changes: [number, number][] = []
        if (host) {
            let sum = 0
            let count = 0
            for (const el of host.children as HTMLCollectionOf<HTMLElement>) {
                const k = el.dataset.key
                if (!k) continue
                const h = el.getBoundingClientRect().height
                measured.set(k, { sig: sigs.get(k) ?? '', h, gen })
                if (itemByKey.get(k)?.kind === 'node') {
                    sum += h
                    count++
                }
                if (seen.has(k)) continue
                const i = geo.indexOf(k)
                const d = i >= 0 ? geo.set(i, h) : 0
                if (d) changes.push([i, d])
            }
            if (count) estimate = 0.8 * estimate + 0.2 * (sum / count)
        }
        measureKeys.value = []
        applyChanges(changes)
    })()
    await measuring
    measuring = null
}

/** Stale items, nearest the centered card first. */
function staleKeys(limit: number): string[] {
    const out: string[] = []
    const n = geo.length
    const c = Math.max(0, currentId.value ? geo.indexOf(currentId.value) : 0)
    for (let d = 0; d < n && out.length < limit; d++) {
        for (const i of d === 0 ? [c] : [c + d, c - d]) {
            const k = geo.keys[i]
            if (k !== undefined && !seen.has(k) && !fresh(k)) out.push(k)
        }
    }
    return out.slice(0, limit)
}

let measureQueued = false
let lastScrollAt = 0
let perItem = 1
const idle = (fn: (d?: IdleDeadline) => void): void => {
    // Safari has no requestIdleCallback.
    if (typeof requestIdleCallback === 'function') requestIdleCallback(fn, { timeout: 1000 })
    else setTimeout(() => fn(), 60)
}

/** Background measuring in idle slices — paused while the feed is hidden or moving. */
function scheduleMeasure(): void {
    if (measureQueued) return
    measureQueued = true
    idle((deadline) => void measureSlice(deadline))
}

async function measureSlice(deadline?: IdleDeadline): Promise<void> {
    measureQueued = false
    if (layout.view !== 'feed') return // arrive() restarts it
    if (measuring || glideTo || performance.now() - lastScrollAt < 250) {
        scheduleMeasure()
        return
    }
    // At most ~12 ms a slice even when the browser offers a long idle
    // period: a slice is one task, and input waits behind it.
    const budget = Math.min(12, Math.max(4, deadline?.timeRemaining() ?? 8))
    const keys = staleKeys(Math.min(40, Math.max(2, Math.floor(budget / perItem))))
    if (!keys.length) return
    const t0 = performance.now()
    await measure(keys)
    perItem = 0.7 * perItem + 0.3 * ((performance.now() - t0) / keys.length)
    scheduleMeasure()
}

// -- the window: what exists in the DOM, and what is a real card yet --

const win = shallowRef({ start: 0, end: -1 })
/** Keys mounted as real cards; the rest of the window shows skeletons. */
const live = shallowRef<Set<string>>(new Set())
/** Cards promoted to real per frame while scrolling — a fling never drops frames. */
const MOUNT_BUDGET = 6
let frameRaf = 0
let urgent = false

function queueFrame(all = false): void {
    urgent ||= all
    if (!frameRaf) frameRaf = requestAnimationFrame(onFrame)
}

/** Run the frame now (a jump), not on the next animation frame. */
function frameNow(): void {
    if (frameRaf) cancelAnimationFrame(frameRaf)
    urgent = true
    onFrame()
}

function onFrame(): void {
    frameRaf = 0
    const root = feedEl.value
    if (!root || !geo.length) return
    const vh = root.clientHeight
    const top = root.scrollTop - trackTop
    const [s, e] = geo.range(top - vh, top + 2 * vh)
    if (s !== win.value.start || e !== win.value.end) win.value = { start: s, end: e }

    // Promote: what is on screen first, then outward through the window.
    const next = new Set<string>()
    let changed = false
    for (const k of live.value) {
        const i = geo.indexOf(k)
        if (i >= s && i <= e) next.add(k)
        else changed = true
    }
    const [vs, ve] = geo.range(top, top + vh)
    let budget = urgent ? Infinity : MOUNT_BUDGET
    let behind = false
    const promote = (i: number): void => {
        const k = geo.keys[i]
        if (k === undefined || next.has(k) || itemByKey.get(k)?.kind !== 'node') return
        if (budget <= 0) {
            behind = true
            return
        }
        next.add(k)
        budget--
        changed = true
    }
    for (let i = vs; i <= ve; i++) promote(i)
    for (let d = 1; ve + d <= e || vs - d >= s; d++) {
        if (ve + d <= e) promote(ve + d)
        if (vs - d >= s) promote(vs - d)
    }
    urgent = false
    if (changed) live.value = next
    if (behind) queueFrame()
    restyle(root)
}

// ---- depth effect + current card ------------------------------------------

/** The mounted slots (v-track), by key. */
const slots = new Map<string, HTMLElement>()

/** How far the fade steps go: cards this many notes or more from the current one are the faintest. */
const DEPTH_STEPS = 3

/** Notes between item `i` and the current item `ci` (version markers don't count), capped at DEPTH_STEPS. */
function depthOf(i: number, ci: number): number {
    if (ci < 0) return 0
    const step = i > ci ? 1 : -1
    let n = 0
    for (let j = ci; j !== i && n < DEPTH_STEPS; ) {
        j += step
        if (itemByKey.get(geo.keys[j]!)?.kind === 'node') n++
    }
    return n
}

/** The selection ring that glides to the current card (one element, not one per card). */
const ringEl = useTemplateRef<HTMLElement>('ringEl')

let ringKey: string | null = null

/**
 * Move the ring onto item `ci`. It glides only when the focus moves to a
 * nearby card; everything else snaps — the first placement, a far jump, and
 * any layout shift under the same card (re-measured cards above it move the
 * card instantly, and a gliding ring would trail behind it).
 */
function placeRing(ci: number, vh: number): void {
    const ring = ringEl.value
    if (!ring) return
    if (ci < 0) {
        ring.style.opacity = '0'
        ringKey = null
        return
    }
    const key = geo.keys[ci]!
    const top = geo.offset(ci)
    const y = `translateY(${top}px)`
    const h = `${geo.height(ci)}px`
    if (ring.style.transform === y && ring.style.height === h && ringKey === key) return
    const from = Number.parseFloat(ring.style.transform.slice(11)) || 0
    const glide = ringKey !== null && key !== ringKey && Math.abs(top - from) < vh * 1.5
    ringKey = key
    ring.classList.toggle('gliding', glide)
    ring.style.transform = y
    ring.style.height = h
    ring.style.opacity = '1'
}

/**
 * The depth cue, all from the height map — no layout reads, and only the
 * mounted cards are touched. Picks the centered card first; then every
 * card scales by its distance from the viewport center (continuous, it
 * follows the scroll) and fades — and, in the brand theme, blurs — by how
 * many notes it sits from the current one (`data-depth`, stepped: CSS
 * eases it, so the fade moves with the focus, not the scroll). Distance is
 * to the card's nearest edge (0 while it covers the line), so an expanded
 * card taller than the screen stays solid while it is read.
 */
function restyle(root: HTMLElement): void {
    if (firstNode < 0) return
    const vh = root.clientHeight
    /*
     * The center line, clamped to the first and last card's own centers.
     * Without the clamp a short card at either end can never reach the
     * middle of the screen (the feed's padding is smaller than half a
     * viewport), so it stays dimmed and unfocusable while its taller
     * neighbor — which does cover the line — wins every time.
     */
    const center = (i: number): number => geo.offset(i) + geo.height(i) / 2
    const mid = Math.min(Math.max(root.scrollTop + vh / 2 - trackTop, center(firstNode)), center(lastNode))
    const at = geo.indexAt(mid)
    let best: { k: string; d: number } | null = null
    for (let i = Math.max(0, at - 2); i <= Math.min(geo.length - 1, at + 2); i++) {
        const k = geo.keys[i]!
        if (itemByKey.get(k)?.kind !== 'node') continue
        const d = distanceToLine(geo.offset(i), geo.height(i), mid)
        if (!best || d < best.d) best = { k, d }
    }
    if (best && best.k !== currentId.value) currentId.value = best.k
    const ci = currentId.value ? geo.indexOf(currentId.value) : -1
    for (const [k, el] of slots) {
        const i = geo.indexOf(k)
        if (i < 0 || itemByKey.get(k)?.kind !== 'node') continue
        const t = Math.max(0, 1 - distanceToLine(geo.offset(i), geo.height(i), mid) / (vh * 0.75))
        el.style.transform = `scale(${0.945 + 0.055 * t})`
        const depth = String(depthOf(i, ci))
        if (el.dataset.depth !== depth) el.dataset.depth = depth
    }
    placeRing(ci, vh)
}

/** Slot registry + live heights: one observer for every mounted card. */
const slotObserver = new ResizeObserver((entries) => {
    const changes: [number, number][] = []
    for (const en of entries) {
        const el = en.target as HTMLElement
        const k = el.dataset.key
        if (!k || el.dataset.live !== '1') continue
        const h = en.borderBoxSize[0]?.blockSize ?? el.offsetHeight
        seen.set(k, h)
        const i = geo.indexOf(k)
        const d = i >= 0 ? geo.set(i, h) : 0
        if (d) changes.push([i, d])
    }
    applyChanges(changes)
})

const vTrack = {
    mounted(el: HTMLElement, b: { value: string }): void {
        slots.set(b.value, el)
        slotObserver.observe(el)
    },
    unmounted(el: HTMLElement, b: { value: string }): void {
        if (slots.get(b.value) === el) slots.delete(b.value)
        slotObserver.unobserve(el)
        seen.delete(b.value)
    },
}

/** The window as the template draws it. */
const windowItems = computed(() => {
    void layoutSeq.value
    const { start, end } = win.value
    return items.value.slice(start, end + 1).map((item, j) => ({
        item,
        key: keyOf(item),
        top: geo.offset(start + j),
        h: geo.height(start + j),
    }))
})

const trackHeight = computed(() => {
    void layoutSeq.value
    return geo.total
})

/** Re-read what CSS decides: the track's top padding and the gap between cards. */
function refreshMetrics(): void {
    const track = trackEl.value
    if (!track) return
    trackTop = track.offsetTop
    const gap = parseFloat(getComputedStyle(track).rowGap) || 0
    if (gap !== geo.gap) {
        geo.setGap(gap)
        layoutSeq.value++
    }
}

/** Width, theme or fonts changed: every measurement is stale (kept as estimates). */
function relayout(): void {
    gen++
    refreshMetrics()
    queueFrame()
    scheduleMeasure()
}

let lastWidth = 0
const viewObserver = new ResizeObserver(([en]) => {
    const w = en?.contentRect.width ?? 0
    if (w === 0) return // hidden
    if (w !== lastWidth) {
        lastWidth = w
        relayout()
    } else {
        queueFrame()
    }
})
const themeObserver = new MutationObserver(relayout)

// ---- traversal + trail -----------------------------------------------------

const TRAIL_KEY = 'engram.feed.trail'
const TRAIL_CAP = 40

function loadTrail(): string[] {
    try {
        const raw = sessionStorage.getItem(TRAIL_KEY)
        const list = raw ? (JSON.parse(raw) as unknown) : []
        return Array.isArray(list) ? list.filter((x): x is string => typeof x === 'string') : []
    } catch {
        return []
    }
}

const trail = ref<string[]>(loadTrail())
watch(
    trail,
    (t) => sessionStorage.setItem(TRAIL_KEY, JSON.stringify(t.slice(-TRAIL_CAP))),
    { deep: true },
)

/**
 * Center a card in the feed, from the height map — the card may not exist
 * in the DOM yet. The landing zone is measured first, so the target and its
 * neighbors sit at their real heights before anything moves. A far smooth
 * jump lands 1.5 screens short instantly and glides the rest: the same
 * arrival, without a long ride through a few hundred cards. Scrolls the
 * feed's own scroller, vertically only (scrollIntoView dragged the whole
 * app sideways mid-switch).
 */
async function scrollToNode(id: string, smooth = true): Promise<void> {
    await nextTick()
    const root = feedEl.value
    if (!root || geo.indexOf(id) < 0) return
    const vh = root.clientHeight
    let target = centerOf(id, root)
    const dir = Math.sign(target - root.scrollTop)
    const far = smooth && Math.abs(target - root.scrollTop) > 3 * vh
    const from = far ? target - dir * 1.5 * vh : target
    const [a, b] = geo.range(Math.min(from, target) - trackTop - vh, Math.max(from, target) - trackTop + 2 * vh)
    const stale = geo.keys.slice(a, b + 1).filter((k) => !seen.has(k) && !fresh(k))
    if (stale.length) {
        await measure(stale)
        await nextTick()
        target = centerOf(id, root)
    }
    endGlide()
    if (!smooth) {
        root.scrollTo({ top: target, behavior: 'instant' })
        frameNow()
        return
    }
    if (far) {
        root.scrollTo({ top: target - dir * 1.5 * vh, behavior: 'instant' })
        frameNow()
    }
    glide(id, root)
}

/** Make sure the node has a card even when the active lens filters it out. */
function ensureInFeed(id: string): void {
    if (!feedNodes.value.some((n) => n.id === id)) {
        const next = new Set(extraIds.value)
        next.add(id)
        extraIds.value = next
    }
}

/** Follow an edge: remember where we were, make the target visible, go. */
function traverse(targetId: string): void {
    if (!nodes.value.has(targetId)) return
    if (currentId.value && currentId.value !== targetId) {
        trail.value = [...trail.value, currentId.value].slice(-TRAIL_CAP)
    }
    ensureInFeed(targetId)
    void scrollToNode(targetId)
}

function goBack(): void {
    // Walk past ids that no longer resolve (deleted nodes, another project).
    const idx = trail.value.map((id) => nodes.value.has(id)).lastIndexOf(true)
    if (idx < 0) {
        trail.value = []
        return
    }
    const prev = trail.value[idx]!
    trail.value = trail.value.slice(0, idx)
    ensureInFeed(prev)
    void scrollToNode(prev)
}

/**
 * Fast traverse to the ends of the feed. Deliberately instant rather than
 * smooth: the point of the jump is to be there.
 */
function jump(where: 'top' | 'bottom'): void {
    const target = where === 'top' ? feedNodes.value[0] : feedNodes.value.at(-1)
    if (target) void scrollToNode(target.id, false)
}

/**
 * Expansion model (settled after four iterations): every card ALWAYS renders
 * its full markdown, clipped to a fixed preview height — one content, no
 * text swap, and every card's geometry is constant, so far-away jump targets
 * never drift. Only the centered card un-clips, and only once scrolling has
 * SETTLED (idle debounce) — nothing pulses while cards stream past center,
 * and the growth is downward from a card already anchored at center.
 */
const expandedId = ref<string | null>(null)
let settleTimer: number | undefined

function scheduleSettle(): void {
    window.clearTimeout(settleTimer)
    settleTimer = window.setTimeout(() => (expandedId.value = currentId.value), 180)
}

/* A card that left the DOM while expanded still holds its expanded height
   in the map; once it collapses, give it back its measured preview height. */
watch(expandedId, (_, prev) => {
    if (!prev || seen.has(prev)) return
    const i = geo.indexOf(prev)
    const m = measured.get(prev)
    if (i < 0 || !m) return
    const d = geo.set(i, m.h)
    if (d) applyChanges([[i, d]])
})

function onFeedScroll(): void {
    lastScrollAt = performance.now()
    queueFrame()
    scheduleSettle()
}

/* The trail, the pulled-in traversal targets and the centered card are all
   ids of the graph we are leaving — a Back button offering to return into
   another project's node is the visible symptom. Start the new feed at the
   top with an empty trail. */
onProjectSwitch(() => {
    trail.value = []
    extraIds.value = new Set()
    currentId.value = null
    expandedId.value = null
    measured.clear()
    void nextTick(() => jump('top'))
})

function onKey(e: KeyboardEvent): void {
    // The feed stays mounted under other screens (App keeps screens alive);
    // its keys belong to it only while it is the screen on show.
    if (layout.view !== 'feed') return
    const t = e.target as HTMLElement | null
    if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp' && e.key !== 'j' && e.key !== 'k') return
    const dir = e.key === 'ArrowDown' || e.key === 'j' ? 1 : -1
    // A taller-than-screen expanded card reads with the arrows: while it
    // still has content beyond the viewport in that direction, let native
    // scrolling continue instead of jumping to the neighbor.
    const root = feedEl.value
    const ci = currentId.value ? geo.indexOf(currentId.value) : -1
    if (root && ci >= 0 && geo.height(ci) > root.clientHeight) {
        const top = trackTop + geo.offset(ci)
        const room =
            dir === 1
                ? top + geo.height(ci) > root.scrollTop + root.clientHeight + 8
                : top < root.scrollTop - 8
        if (room && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) return
    }
    const list = feedNodes.value
    const idx = list.findIndex((n) => n.id === currentId.value)
    const next = list[idx < 0 ? 0 : Math.min(list.length - 1, Math.max(0, idx + dir))]
    if (next) {
        e.preventDefault()
        void scrollToNode(next.id)
    }
}

/** Arrive centered on the canvas's selection (graph→feed sync). Runs on
 *  first mount and every time the feed becomes the screen again. */
function arrive(): void {
    refreshMetrics()
    const sel = store.selectedId
    if (sel && nodes.value.has(sel)) {
        ensureInFeed(sel)
        void scrollToNode(sel, false)
    }
    void nextTick(() => {
        queueFrame(true)
        scheduleSettle()
        scheduleMeasure()
    })
}

/**
 * Feed→graph sync on the way out: the centered card becomes the canvas
 * selection, with its drawer open only when the graph is where we're going.
 * Leaving for tasks or history keeps the selection quietly, so a later
 * return to the canvas centers on it without a drawer nobody asked for.
 */
function handOff(): void {
    if (currentId.value && nodes.value.has(currentId.value)) {
        if (layout.view === 'graph') store.select(currentId.value)
        else store.setSelection(currentId.value)
    }
}

watch(
    () => layout.view,
    (view, prev) => {
        if (view === 'feed') arrive()
        else if (prev === 'feed') handOff()
    },
)

// Declared last: it runs at once, and everything it reaches must exist.
watch(items, rebuild, { immediate: true })

onMounted(() => {
    window.addEventListener('keydown', onKey)
    if (feedEl.value) {
        viewObserver.observe(feedEl.value)
        // The reader takes over: no correction may re-aim the old glide.
        for (const ev of ['wheel', 'touchstart', 'pointerdown'] as const) {
            feedEl.value.addEventListener(ev, endGlide, { passive: true })
        }
        feedEl.value.addEventListener('scrollend', onScrollEnd)
    }
    // Not `class`: the canvas toggles ui-dragging there on every drag.
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'style'] })
    document.fonts?.addEventListener('loadingdone', relayout)
    arrive()
})

onBeforeUnmount(() => {
    window.removeEventListener('keydown', onKey)
    viewObserver.disconnect()
    slotObserver.disconnect()
    themeObserver.disconnect()
    document.fonts?.removeEventListener('loadingdone', relayout)
    if (frameRaf) cancelAnimationFrame(frameRaf)
    window.clearTimeout(settleTimer)
    endGlide()
})

// ---- the action bar --------------------------------------------------------

const busyId = ref<string | null>(null)

async function run(id: string, action: () => Promise<void>): Promise<void> {
    busyId.value = id
    try {
        await action()
    } finally {
        busyId.value = null
    }
}

const approve = (n: GraphNode) => run(n.id, () => store.approve(n.id))
const reconfirm = (n: GraphNode) => run(n.id, () => store.reconfirm(n.id))
const revoke = (n: GraphNode) => run(n.id, () => store.revokeApproval(n.id))
const togglePin = (n: GraphNode) =>
    run(n.id, () => store.pin(n.id, n.trust_override == null ? 1 : null))

/** The quick delete (issue #14: kept on purpose — the user's fast way to
 *  drop notes mid-review; no Tombstone, the drawer's delete leaves one).
 *  Confirmed inline: IDE webviews sandbox window.confirm into a silent
 *  `false`, which made the button look dead in VS Code. */
const confirmingDeleteId = ref<string | null>(null)

async function removeNode(n: GraphNode): Promise<void> {
    confirmingDeleteId.value = null
    await run(n.id, () => store.remove(n.id))
}

/** Edit needs the full form — the one gesture that still opens the drawer. */
function openDetail(n: GraphNode): void {
    store.select(n.id)
}

/** v-memo token for the card list: fresh on every live render, frozen while hidden. */
let renderSeq = 0
function listToken(): number {
    return layout.view === 'feed' ? ++renderSeq : -1
}

/** The centered card — what the bottom action bar operates on. */
const currentNode = computed(() =>
    currentId.value ? (nodes.value.get(currentId.value) ?? null) : null,
)
</script>


<template>
<div class="feed-screen">
    <div class="feed-toolbar glass-panel screen-content">
        <SegmentedControl v-model="mode" :options="MODE_OPTIONS" aria-label="Feed lens" />
        <SegmentedControl v-model="sort" :options="sortOptions" aria-label="Feed order" />
        <span class="jumps">
            <button
                class="jump"
                type="button"
                title="Jump to the top of the feed"
                aria-label="Jump to the top of the feed"
                @click="jump('top')"
            >↑</button>
            <button
                class="jump"
                type="button"
                title="Jump to the end of the feed"
                aria-label="Jump to the end of the feed"
                @click="jump('bottom')"
            >↓</button>
        </span>
        <button
            v-if="trail.length"
            class="back"
            type="button"
            :title="`Return to the previous node (${trail.length} in the trail)`"
            @click="goBack"
        >
            ← back
            <span class="back-count">{{ trail.length }}</span>
        </button>
        <span class="feed-count">{{ feedNodes.length }} notes</span>
    </div>

    <div ref="feedEl" class="feed screen-content" @scroll.passive="onFeedScroll">
        <div v-if="!feedNodes.length" class="feed-empty glass-panel">
            <!-- An empty graph gets ONE empty state — the canvas's overlay is
                 graph-only, so the feed shows the same card here. -->
            <EmptyGraph v-if="!nodeList.length" />
            <template v-else-if="mode === 'review'">
                <p class="empty-title">Nothing needs review</p>
                <p>No provisional, stale, or drifted notes in sight — the backlog is clear.</p>
            </template>
            <template v-else>
                <p class="empty-title">Nothing to show</p>
                <p>No notes match the current filters.</p>
            </template>
        </div>

        <!-- The whole feed's length; only the window near the viewport is in it. -->
        <div ref="trackEl" class="feed-track" :style="{ height: `${trackHeight}px` }">
            <!-- Hidden under another screen, the window does not re-render
                 (a selection on the canvas would re-run it for nobody).
                 Live renders always pass a fresh token; a hidden one the
                 same frozen one, and arriving on the feed renders fresh. -->
            <div v-memo="[listToken()]" class="feed-list">
                <div
                    v-for="w in windowItems"
                    :key="w.key"
                    v-track="w.key"
                    class="slot"
                    :class="w.item.kind === 'node' ? 'node-slot' : 'version-slot'"
                    :data-key="w.key"
                    :data-live="w.item.kind === 'version' || live.has(w.key) ? '1' : '0'"
                    :style="{
                        top: `${w.top}px`,
                        height: w.item.kind === 'node' && !live.has(w.key) ? `${w.h}px` : undefined,
                    }"
                >
                    <div v-if="w.item.kind === 'version'" class="version-marker">
                        <span class="version-line" />
                        <span class="version-chip">{{ w.item.label }}</span>
                        <span class="version-line" />
                    </div>
                    <FeedCard
                        v-else
                        :node="w.item.node"
                        :skeleton="!live.has(w.key)"
                        :focus="w.key === currentId"
                        :expanded="w.key === expandedId"
                        :drifted="driftByNode.has(w.key)"
                        :out-edges="outEdges.get(w.key) ?? EMPTY"
                        :in-edges="inEdges.get(w.key) ?? EMPTY"
                        :missing-refs="driftByNode.get(w.key) ?? EMPTY"
                        @traverse="traverse"
                        @center="scrollToNode"
                    />
                </div>
            </div>

            <!-- The current card's selection ring: one element that glides between cards. -->
            <div ref="ringEl" class="feed-ring" aria-hidden="true" />

            <!-- The measurer: cards rendered once, off screen, for their real
                 height — never seen, never in the scroll length. -->
            <div ref="measureEl" class="feed-measure" aria-hidden="true">
                <div v-for="k in measureKeys" :key="k" :data-key="k">
                    <template v-if="itemByKey.get(k)">
                        <div v-if="itemByKey.get(k)!.kind === 'version'" class="version-marker">
                            <span class="version-line" />
                            <span class="version-chip">{{ (itemByKey.get(k) as { label: string }).label }}</span>
                            <span class="version-line" />
                        </div>
                        <FeedCard
                            v-else
                            :node="(itemByKey.get(k) as { node: GraphNode }).node"
                            :drifted="driftByNode.has(k)"
                            :out-edges="outEdges.get(k) ?? EMPTY"
                            :in-edges="inEdges.get(k) ?? EMPTY"
                            :missing-refs="driftByNode.get(k) ?? EMPTY"
                        />
                    </template>
                </div>
            </div>
        </div>
    </div>
    <!-- The side pane's controls, feed-style: one bottom bar acting on the
         centered card — no drawer pops in this view (Edit is the exception,
         it needs the full form). -->
    <div v-if="currentNode" class="action-bar glass-panel screen-content">
        <span class="bar-dot" :style="{ background: config.accent(currentNode.type) }" />
        <span class="bar-title" :title="currentNode.title">{{ currentNode.title }}</span>
        <UiButton
            v-if="currentNode.approved_at == null" size="sm" pill variant="primary"
            :disabled="busyId === currentNode.id"
            title="Approve: you vouch for it — trust 100%"
            @click="approve(currentNode)"
        >
            Approve
        </UiButton>
        <UiButton
            v-else size="sm" pill variant="ghost"
            :disabled="busyId === currentNode.id"
            title="Withdraw approval — trust falls back to its anchor"
            @click="revoke(currentNode)"
        >
            Revoke
        </UiButton>
        <UiButton
            size="sm" pill variant="ghost"
            :disabled="busyId === currentNode.id"
            title="Confirm still true — refreshes trust without approval"
            @click="reconfirm(currentNode)"
        >
            Still true
        </UiButton>
        <UiButton
            size="sm" pill variant="ghost"
            :disabled="busyId === currentNode.id"
            :title="currentNode.trust_override == null ? 'Pin: constant trust, decay and demotion off' : 'Unpin'"
            @click="togglePin(currentNode)"
        >
            {{ currentNode.trust_override == null ? 'Pin' : 'Unpin' }}
        </UiButton>
        <UiButton
            size="sm" pill variant="ghost"
            title="Edit in the detail drawer"
            @click="openDetail(currentNode)"
        >
            Edit
        </UiButton>
        <template v-if="confirmingDeleteId === currentNode.id">
            <UiButton
                size="sm"
                pill
                variant="danger-text"
                :disabled="busyId === currentNode.id"
                title="Delete this note and its edges"
                @click="removeNode(currentNode)"
            >
                Confirm delete
            </UiButton>
            <UiButton size="sm" pill variant="ghost" @click="confirmingDeleteId = null">
                Cancel
            </UiButton>
        </template>
        <UiButton
            v-else
            size="sm"
            pill
            variant="danger-text"
            :disabled="busyId === currentNode.id"
            title="Hard delete (user-only) — cascades edges"
            @click="confirmingDeleteId = currentNode.id"
        >
            Delete
        </UiButton>
    </div>
</div>
</template>

<style scoped>
.feed-screen {
    position: absolute;
    inset: 0;
    background:
        radial-gradient(60% 45% at 18% 8%, var(--canvas-glow-1), transparent),
        radial-gradient(55% 45% at 85% 92%, var(--canvas-glow-2), transparent),
        var(--canvas-bg);
}

.feed-toolbar {
    position: absolute;
    top: 7.4rem;
    left: 50%;
    z-index: 8;
    display: flex;
    align-items: center;
    gap: 1rem;
    max-width: calc(100vw - 3.2rem);
    padding: 0.6rem 1rem;
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-md);
    transform: translateX(-50%);
    flex-wrap: nowrap;
}

.back {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 1rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-control);
    background: transparent;
    color: var(--text-secondary);
    font-size: var(--text-caption);
    font-weight: 600;
    white-space: nowrap;
    cursor: pointer;
}

.back:hover {
    color: var(--text-primary);
    background: var(--interactive-ghost-hover);
}

.jumps {
    display: inline-flex;
    gap: 0.4rem;
}

/* Fast traverse: same pill language as Back, sized to a single glyph. */
.jump {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 2.6rem;
    height: 2.6rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-control);
    background: transparent;
    color: var(--text-secondary);
    font-size: var(--text-body-sm);
    line-height: 1;
    cursor: pointer;
}

.jump:hover {
    color: var(--text-primary);
    background: var(--interactive-ghost-hover);
}

.back-count {
    min-width: 1.7rem;
    padding: 0.05rem 0.4rem;
    border-radius: var(--radius-full);
    background: var(--interactive-primary);
    color: var(--text-inverse);
    text-align: center;
}

.feed-count {
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-tertiary);
    white-space: nowrap;
}

.feed {
    position: absolute;
    inset: 0;
    overflow: hidden auto;
    /* The feed keeps the centered card still itself (heightMap anchorShift);
       the browser's own anchoring would correct the same change twice. */
    overflow-anchor: none;

    /* Room so the first/last card can center, with neighbors peeking. */
    padding: 40vh 1.6rem;
    scroll-padding-block: 22vh;

    /* Gentle centering — proximity, so free scrolling still feels free. */
    scroll-snap-type: y proximity;
}

.feed-empty {
    display: flex;
    margin-inline: auto;
    flex-direction: column;
    gap: 0.6rem;
    max-width: 42rem;
    margin-top: 10vh;
    padding: 1.6rem 2rem;
    border-radius: var(--radius-lg);
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
    text-align: center;
}

.empty-title {
    font-size: var(--text-h3);
    font-weight: 600;
    color: var(--text-primary);
}

.version-marker {
    display: flex;
    align-items: center;
    gap: 1rem;
    width: 100%;
    padding: 0.4rem 0;
}

.version-line {
    flex: 1;
    height: 1px;
    background: var(--border-default);
}

.version-chip {
    padding: 0.25rem 1rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-full);
    background: var(--surface-sunken);
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-secondary);
}

/* The whole feed's length; the gap between cards is read back by the
   height map (refreshMetrics), so CSS stays the one place it is set. */
.feed-track {
    position: relative;
    width: min(64rem, 100%);
    margin: 0 auto;
    row-gap: 1.6rem;
}

/* The card list wrapper only exists for v-memo — layout-wise it is not there. */
.feed-list {
    display: contents;
}

/* One positioned slot per item in the window; the depth effect scales it. */
.slot {
    position: absolute;
    left: 0;
    right: 0;
}

.node-slot {
    scroll-snap-align: center;
    will-change: transform, opacity;
    transition:
        opacity 280ms var(--ease-default),
        filter 280ms var(--ease-default);
}

/* The fade by notes from the current card (restyle's data-depth). */
.node-slot[data-depth='1'] {
    opacity: 0.7;
}

.node-slot[data-depth='2'] {
    opacity: 0.55;
}

.node-slot[data-depth='3'] {
    opacity: 0.4;
}

/* The brand theme adds a light depth of field past the nearest cards:
   0.6px two notes out, 1.2px from three. */
:root[data-theme='engram-purple'] .node-slot[data-depth='2'] {
    filter: blur(0.6px);
}

:root[data-theme='engram-purple'] .node-slot[data-depth='3'] {
    filter: blur(1.2px);
}

/* The graph's selection outline at its resting width, gliding card to card. */
.feed-ring {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    z-index: 1;
    border-radius: var(--radius-lg);
    outline: 0.2rem solid var(--interactive-primary);
    outline-offset: 0.3rem;
    opacity: 0;
    pointer-events: none;
}

.feed-ring.gliding {
    transition:
        transform 320ms var(--ease-default),
        height 320ms var(--ease-default);
}

@media (prefers-reduced-motion: reduce) {
    .node-slot,
    .feed-ring.gliding {
        transition: none;
    }
}

.feed-measure {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 0;
    overflow: hidden;
    visibility: hidden;
    pointer-events: none;
    contain: layout paint style;
}

/* Bottom action bar — the drawer's controls for the centered card. */
.action-bar {
    position: absolute;
    /* Host chrome (the demo badge) anchors above this bar on narrow panes. */
    anchor-name: --feed-action-bar;
    bottom: 1.6rem;
    left: 50%;
    z-index: 9;
    display: flex;
    align-items: center;
    gap: 0.7rem;
    max-width: calc(100vw - 3.2rem);
    padding: 0.6rem 1.1rem;
    border-radius: var(--radius-control);
    box-shadow: var(--shadow-md);
    transform: translateX(-50%);
}

.bar-dot {
    flex-shrink: 0;
    width: 0.9rem;
    height: 0.9rem;
    border-radius: 50%;
}

.bar-title {
    overflow: hidden;
    max-width: 22rem;
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--text-primary);
    text-overflow: ellipsis;
    white-space: nowrap;
}

@media (width <= 700px) {
    .feed {
        padding: 24vh 0.8rem;
        scroll-padding-block: 20vh;
    }

    .feed-track {
        row-gap: 1.2rem;
    }

    .feed-toolbar {
        top: 6.6rem;
        left: 0.8rem;
        right: 0.8rem;
        justify-content: center;
        gap: 0.5rem;
        max-width: none;
        padding: 0.4rem 0.6rem;
        transform: none;
        flex-wrap: wrap;
    }

    .feed-toolbar :deep(.segment) {
        padding: 0.35rem 0.8rem;
    }

    .feed-count {
        display: none;
    }

    .jump {
        width: 2.2rem;
        height: 2.2rem;
    }

    .action-bar {
        flex-wrap: wrap;
        justify-content: center;
        border-radius: var(--radius-lg);
        left: 0.8rem;
        right: 0.8rem;
        gap: 0.5rem;
        row-gap: 0.6rem;
        max-width: none;
        padding: 0.6rem 0.8rem;
        transform: none;
    }

    .bar-title {
        flex: 1 0 calc(100% - 1.4rem);
        max-width: none;
        min-width: 0;
    }

    /* The bar's UiButtons share the row evenly when it wraps. */
    .action-bar .ui-btn {
        flex: 1 1 auto;
        padding: 0.4rem 0.6rem;
        text-align: center;
    }
}

/* Sliver panes: the top bar wraps onto two rows (App.vue) — start below it. */
@media (width <= 250px) {
    .feed-toolbar {
        top: 10rem;
    }
}
</style>
