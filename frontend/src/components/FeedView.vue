<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, useTemplateRef, watch } from 'vue'
import { storeToRefs } from 'pinia'
import EmptyGraph from '@/components/common/EmptyGraph.vue'
import MarkdownView from '@/components/common/MarkdownView.vue'
import SegmentedControl from '@/components/common/SegmentedControl.vue'
import { onProjectSwitch } from '@/composables/onProjectSwitch'
import { trustLevel, useGraphStore } from '@/stores/graph'
import { useConfigStore } from '@/stores/config'
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

const EDGE_CAP = 6

function title(id: string): string {
    return nodes.value.get(id)?.title ?? id
}

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

const currentId = ref<string | null>(null)

function scrollToNode(id: string, smooth = true): void {
    void nextTick(() => {
        const el = feedEl.value?.querySelector<HTMLElement>(`[data-node-id="${id}"]`)
        el?.scrollIntoView({ behavior: smooth ? 'smooth' : 'instant', block: 'center' })
    })
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
    scrollToNode(targetId)
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
    scrollToNode(prev)
}

/**
 * Fast traverse to the ends of the feed. Deliberately instant rather than
 * smooth: across a few hundred cards a smooth scroll is a long ride, and the
 * point of the jump is to be there.
 */
function jump(where: 'top' | 'bottom'): void {
    const target = where === 'top' ? feedNodes.value[0] : feedNodes.value.at(-1)
    if (target) scrollToNode(target.id, false)
}

// ---- depth effect + current card ------------------------------------------

const feedEl = useTemplateRef<HTMLElement>('feedEl')
let raf = 0

/**
 * Scale/fade cards by distance from the viewport center — the depth cue.
 * Distance is measured to the card's nearest edge (clamped to 0 while the
 * card covers the center line), so an expanded card taller than the screen
 * stays fully solid while the reader scrolls inside it.
 */
function restyle(): void {
    raf = 0
    const root = feedEl.value
    if (!root) return
    const cards = [...root.querySelectorAll<HTMLElement>('[data-node-id]')]
    if (!cards.length) return
    /*
     * The center line, clamped to the first and last card's own centers.
     * Without the clamp a short card at either end can never reach the
     * middle of the screen (the feed's padding is smaller than half a
     * viewport), so it stays dimmed and unfocusable while its taller
     * neighbor — which does cover the line — wins every time.
     */
    const center = (el: HTMLElement) => el.offsetTop + el.offsetHeight / 2
    const mid = Math.min(
        Math.max(root.scrollTop + root.clientHeight / 2, center(cards[0]!)),
        center(cards.at(-1)!),
    )
    let best: { id: string; d: number } | null = null
    for (const el of cards) {
        const half = el.offsetHeight / 2
        const d = Math.max(0, Math.abs(el.offsetTop + half - mid) - half)
        const t = Math.max(0, 1 - d / (root.clientHeight * 0.75))
        el.style.opacity = String(0.45 + 0.55 * t)
        el.style.transform = `scale(${0.945 + 0.055 * t})`
        const id = el.dataset.nodeId ?? ''
        if (!best || d < best.d) best = { id, d }
    }
    if (best) currentId.value = best.id
}

function queueRestyle(): void {
    if (!raf) raf = requestAnimationFrame(restyle)
}

watch(items, () =>
    void nextTick(() => {
        queueRestyle()
        scheduleSettle()
    }),
)

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

function onFeedScroll(): void {
    queueRestyle()
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
    void nextTick(() => jump('top'))
})

function onKey(e: KeyboardEvent): void {
    const t = e.target as HTMLElement | null
    if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp' && e.key !== 'j' && e.key !== 'k') return
    const dir = e.key === 'ArrowDown' || e.key === 'j' ? 1 : -1
    // A taller-than-screen expanded card reads with the arrows: while it
    // still has content beyond the viewport in that direction, let native
    // scrolling continue instead of jumping to the neighbor.
    const root = feedEl.value
    const cur = root?.querySelector<HTMLElement>(`[data-node-id="${currentId.value}"]`)
    if (root && cur && cur.offsetHeight > root.clientHeight) {
        const room =
            dir === 1
                ? cur.offsetTop + cur.offsetHeight > root.scrollTop + root.clientHeight + 8
                : cur.offsetTop < root.scrollTop - 8
        if (room && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) return
    }
    const list = feedNodes.value
    const idx = list.findIndex((n) => n.id === currentId.value)
    const next = list[idx < 0 ? 0 : Math.min(list.length - 1, Math.max(0, idx + dir))]
    if (next) {
        e.preventDefault()
        scrollToNode(next.id)
    }
}

onMounted(() => {
    window.addEventListener('keydown', onKey)
    // Graph→feed sync: arrive centered on the node selected on the canvas.
    const sel = store.selectedId
    if (sel && nodes.value.has(sel)) {
        ensureInFeed(sel)
        scrollToNode(sel, false)
    }
    void nextTick(() => {
        queueRestyle()
        scheduleSettle()
    })
})

onBeforeUnmount(() => {
    window.removeEventListener('keydown', onKey)
    if (raf) cancelAnimationFrame(raf)
    window.clearTimeout(settleTimer)
    // Feed→graph sync: the centered card becomes the canvas selection.
    if (currentId.value && nodes.value.has(currentId.value)) store.select(currentId.value)
})

// ---- card content helpers --------------------------------------------------

function fmtDate(secs: number): string {
    return new Date(secs * 1000).toLocaleDateString(undefined, {
        year: 'numeric',
        month: 'short',
        day: 'numeric',
    })
}

function fmtStamp(secs: number): string {
    return new Date(secs * 1000).toLocaleString(undefined, {
        dateStyle: 'medium',
        timeStyle: 'short',
    })
}

/** The short date carries the whole provenance line on hover. */
function dateTip(n: GraphNode): string {
    const lines = [`Created ${fmtStamp(n.created_at)} · by ${n.source}`]
    if (n.last_seen != null) lines.push(`Last retrieved ${fmtStamp(n.last_seen)}`)
    if (n.valid_until != null) lines.push(`Archived ${fmtStamp(n.valid_until)}`)
    return lines.join('\n')
}

/** Code refs of this node that no longer exist on disk (the drift scan). */
function missingRefs(id: string): string[] {
    return driftByNode.value.get(id) ?? []
}

const levelWord: Record<ReturnType<typeof trustLevel>, string> = {
    pinned: 'pinned',
    trusted: 'trusted',
    provisional: 'provisional',
    stale: 'stale',
}

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

async function removeNode(n: GraphNode): Promise<void> {
    if (!window.confirm(`Delete "${n.title}"? This cascades its edges and cannot be undone.`)) {
        return
    }
    await run(n.id, () => store.remove(n.id))
}

/** Edit needs the full form — the one gesture that still opens the drawer. */
function openDetail(n: GraphNode): void {
    store.select(n.id)
}

/** The centered card — what the bottom action bar operates on. */
const currentNode = computed(() =>
    currentId.value ? (nodes.value.get(currentId.value) ?? null) : null,
)
</script>

<template>
<div class="feed-screen">
    <div class="feed-toolbar glass-panel">
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

    <div ref="feedEl" class="feed" @scroll.passive="onFeedScroll">
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

        <template v-for="item in items" :key="item.kind === 'node' ? item.node.id : item.key">
            <div v-if="item.kind === 'version'" class="version-marker">
                <span class="version-line" />
                <span class="version-chip">{{ item.label }}</span>
                <span class="version-line" />
            </div>

            <article
                v-else
                class="card glass-panel"
                :data-node-id="item.node.id"
                :class="{ archived: item.node.valid_until != null, focus: item.node.id === currentId }"
                :style="{ '--card-accent': config.accent(item.node.type) }"
            >
                <header class="card-head">
                    <span class="type-dot" />
                    <span class="type-name">{{ item.node.type }}</span>
                    <span v-if="item.node.version" class="chip version">{{ item.node.version }}</span>
                    <span v-if="item.node.status" class="chip status" :class="item.node.status">
                        {{ item.node.status }}
                    </span>
                    <span v-if="item.node.valid_until != null" class="chip status">archived</span>
                    <span v-if="driftByNode.has(item.node.id)" class="chip warn">drifted</span>
                    <span class="spacer" />
                    <span class="date" :title="dateTip(item.node)">{{ fmtDate(item.node.created_at) }}</span>
                </header>

                <h3 class="card-title">
                    <button type="button" class="title-btn" title="Center this card" @click="scrollToNode(item.node.id)">
                        {{ item.node.title }}
                    </button>
                </h3>

                <!-- Full markdown always rendered, clipped to a preview; the
                     settled centered card un-clips. Same content in both
                     states — the reveal never swaps or reflows the text. -->
                <div
                    v-if="item.node.body"
                    class="card-body"
                    :class="{ expanded: item.node.id === expandedId }"
                >
                    <MarkdownView :content="item.node.body" />
                </div>

                <div class="trust-line">
                    <span class="trust-dot" :class="trustLevel(item.node)" />
                    <span class="trust-word" :class="trustLevel(item.node)">
                        {{ levelWord[trustLevel(item.node)] }} · {{ Math.round(item.node.trust * 100) }}%
                    </span>
                    <span v-if="item.node.tags.length" class="tags">
                        <span v-for="t in item.node.tags" :key="t" class="tag">#{{ t }}</span>
                    </span>
                </div>

                <div v-if="item.node.code_refs.length" class="refs">
                    <span
                        v-for="codeRef in item.node.code_refs"
                        :key="codeRef"
                        class="ref-chip"
                        :class="{ missing: missingRefs(item.node.id).includes(codeRef) }"
                        :title="missingRefs(item.node.id).includes(codeRef)
                            ? `${codeRef} — this file no longer exists in the project`
                            : codeRef"
                    >{{ codeRef }}</span>
                </div>

                <div
                    v-if="outEdges.get(item.node.id)?.length || inEdges.get(item.node.id)?.length"
                    class="edges"
                >
                    <button
                        v-for="e in (outEdges.get(item.node.id) ?? []).slice(0, EDGE_CAP)"
                        :key="e.id"
                        class="edge-chip"
                        type="button"
                        :style="{ '--edge-accent': config.edgeColor(e.type) }"
                        :title="`${item.node.title} ${e.type} ${title(e.to_id)}`"
                        @click="traverse(e.to_id)"
                    >
                        <span class="edge-verb">{{ e.type }} →</span>
                        <span class="edge-target">{{ title(e.to_id) }}</span>
                    </button>
                    <button
                        v-for="e in (inEdges.get(item.node.id) ?? []).slice(0, EDGE_CAP)"
                        :key="e.id"
                        class="edge-chip in"
                        type="button"
                        :style="{ '--edge-accent': config.edgeColor(e.type) }"
                        :title="`${title(e.from_id)} ${e.type} ${item.node.title}`"
                        @click="traverse(e.from_id)"
                    >
                        <span class="edge-verb">← {{ e.type }}</span>
                        <span class="edge-target">{{ title(e.from_id) }}</span>
                    </button>
                </div>
            </article>
        </template>
    </div>
    <!-- The side pane's controls, feed-style: one bottom bar acting on the
         centered card — no drawer pops in this view (Edit is the exception,
         it needs the full form). -->
    <div v-if="currentNode" class="action-bar glass-panel">
        <span class="bar-dot" :style="{ background: config.accent(currentNode.type) }" />
        <span class="bar-title" :title="currentNode.title">{{ currentNode.title }}</span>
        <button
            v-if="currentNode.approved_at == null"
            class="action"
            type="button"
            :disabled="busyId === currentNode.id"
            title="Approve: you vouch for it — trust 100%"
            @click="approve(currentNode)"
        >
            Approve
        </button>
        <button
            v-else
            class="action ghost"
            type="button"
            :disabled="busyId === currentNode.id"
            title="Withdraw approval — trust falls back to its anchor"
            @click="revoke(currentNode)"
        >
            Revoke
        </button>
        <button
            class="action ghost"
            type="button"
            :disabled="busyId === currentNode.id"
            title="Confirm still true — refreshes trust without approval"
            @click="reconfirm(currentNode)"
        >
            Still true
        </button>
        <button
            class="action ghost"
            type="button"
            :disabled="busyId === currentNode.id"
            :title="currentNode.trust_override == null ? 'Pin: constant trust, decay and demotion off' : 'Unpin'"
            @click="togglePin(currentNode)"
        >
            {{ currentNode.trust_override == null ? 'Pin' : 'Unpin' }}
        </button>
        <button
            class="action ghost"
            type="button"
            title="Edit in the detail drawer"
            @click="openDetail(currentNode)"
        >
            Edit
        </button>
        <button
            class="action ghost danger"
            type="button"
            :disabled="busyId === currentNode.id"
            title="Hard delete (user-only) — cascades edges"
            @click="removeNode(currentNode)"
        >
            Delete
        </button>
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
    border-radius: var(--radius-full);
    box-shadow: var(--shadow-md);
    transform: translateX(-50%);
    flex-wrap: nowrap;
}

@media (width <= 560px) {
    .feed-toolbar {
        flex-wrap: wrap;
        justify-content: center;
        border-radius: var(--radius-lg);
    }
}

.back {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 1rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-full);
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
    border-radius: var(--radius-full);
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
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 1.6rem;
    overflow: hidden auto;

    /* Room so the first/last card can center, with neighbors peeking. */
    padding: 40vh 1.6rem;
    scroll-padding-block: 22vh;

    /* Gentle centering — proximity, so free scrolling still feels free. */
    scroll-snap-type: y proximity;
}

.feed-empty {
    display: flex;
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
    width: min(64rem, 100%);
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

.card {
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
    width: min(64rem, 100%);
    padding: 1.4rem 1.6rem;

    /* Accent spine as a gradient — follows the rounded corners cleanly.
       Sized to the strip so the texture never reaches the right edge
       (full-width gradients bleed a 1px wrap line under transforms). */
    background-image: linear-gradient(
        90deg,
        var(--card-accent, var(--border-strong)) 0,
        var(--card-accent, var(--border-strong)) 0.3rem,
        color-mix(in srgb, var(--card-accent, var(--border-strong)) calc(10% * var(--accent-wash, 1)), transparent) 0.3rem,
        transparent 100%
    );
    background-repeat: no-repeat;
    background-size: 7rem 100%;
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-md);
    scroll-snap-align: center;
    transition: box-shadow var(--duration-normal) var(--ease-default);
    will-change: transform, opacity;

    /* Deliberately NO content-visibility: estimated offscreen heights made
       positions drift, so far-away jump targets could never be landed on.
       Every card's geometry stays real and constant (previews are clamped),
       and only the settled centered card changes height. */
}

.card.focus {
    box-shadow: var(--shadow-lg);
}

.card.archived {
    background-image: linear-gradient(
        90deg,
        color-mix(in srgb, var(--card-accent, var(--border-strong)) 45%, transparent) 0,
        color-mix(in srgb, var(--card-accent, var(--border-strong)) 45%, transparent) 100%
    );
    background-size: 0.3rem 100%;
}

.card-head {
    display: flex;
    align-items: center;
    gap: 0.7rem;
    min-width: 0;
}

.type-dot {
    flex-shrink: 0;
    width: 0.9rem;
    height: 0.9rem;
    border-radius: 50%;
    background: var(--card-accent);
}

.type-name {
    font-size: var(--text-caption);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--card-accent);
}

.chip {
    padding: 0.15rem 0.7rem;
    border-radius: var(--radius-full);
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-secondary);
    background: var(--surface-muted);
    white-space: nowrap;
}

.chip.status.open {
    color: var(--node-intent, #0ea5e9);
}

.chip.status.resolved {
    color: var(--trust-trusted);
}

.chip.warn {
    color: var(--node-caution);
    background: color-mix(in srgb, var(--node-caution) 14%, transparent);
}

/* Verbatim the Review drawer's badge — square-ish corners included, so the
   two surfaces say "stale" in exactly one shape. */
.stale-badge {
    padding: 0.1rem 0.6rem;
    border: 1px solid color-mix(in srgb, var(--node-problem) 40%, transparent);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--node-problem) 14%, transparent);
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--node-problem);
}

.spacer {
    flex: 1;
}

.date {
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-tertiary);
    white-space: nowrap;
}

.card-title {
    margin: 0;
    font-size: var(--text-h3);
    font-weight: 600;
    line-height: var(--leading-tight);
}

.title-btn {
    padding: 0;
    border: none;
    background: none;
    color: var(--text-primary);
    font: inherit;
    text-align: left;
    cursor: pointer;
}

.title-btn:hover {
    color: var(--interactive-primary-hover);
}

/* Preview = the full markdown clipped to a fixed height (constant geometry);
   the settled centered card un-clips. Same rendered content in both states,
   so the reveal only extends downward — the visible text never moves. */
.card-body {
    margin: 0;
    overflow: hidden;
    max-height: 12rem;
    font-size: var(--text-body-sm);
    line-height: var(--leading-normal);
    color: var(--text-secondary);
    overflow-wrap: anywhere;
    transition: max-height 350ms var(--ease-default);
}

.card-body.expanded {
    /* A realistic cap, not `none`: max-height must stay animatable, and the
       closer the cap is to real content heights, the truer the timing. */
    max-height: 120rem;
}

.trust-line {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
}

.trust-dot {
    width: 0.8rem;
    height: 0.8rem;
    border-radius: 50%;
}

.trust-dot.trusted,
.trust-dot.pinned {
    background: var(--trust-trusted);
}

.trust-dot.provisional {
    background: var(--trust-provisional);
}

.trust-word {
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.tags {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
}

.tag {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

/* Code refs, same chips as the node detail — a card should say what code it
   is about, and which of those files have since moved away. */
.refs {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
}

.ref-chip {
    overflow: hidden;
    max-width: 100%;
    padding: 0.2rem 0.7rem;
    border-radius: var(--radius-sm);
    background: var(--surface-sunken);
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-tertiary);
    text-overflow: ellipsis;
    white-space: nowrap;
}

.ref-chip.missing {
    color: var(--node-problem);
    background: color-mix(in srgb, var(--node-problem) 12%, transparent);
    text-decoration: line-through;
}

.edges {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
}

.edge-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    max-width: 100%;
    padding: 0.35rem 0.9rem;
    border: 1px solid color-mix(in srgb, var(--edge-accent) 45%, transparent);
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--edge-accent) 10%, transparent);
    font-size: var(--text-caption);
    cursor: pointer;
}

.edge-chip:hover {
    background: color-mix(in srgb, var(--edge-accent) 20%, transparent);
}

.edge-verb {
    flex-shrink: 0;
    color: var(--edge-accent);
    font-weight: 600;
    white-space: nowrap;
}

.edge-target {
    overflow: hidden;
    max-width: 26rem;
    color: var(--text-secondary);
    text-overflow: ellipsis;
    white-space: nowrap;
}

/* Bottom action bar — the drawer's controls for the centered card. */
.action-bar {
    position: absolute;
    bottom: 1.6rem;
    left: 50%;
    z-index: 9;
    display: flex;
    align-items: center;
    gap: 0.7rem;
    max-width: calc(100vw - 3.2rem);
    padding: 0.6rem 1.1rem;
    border-radius: var(--radius-full);
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

.action {
    padding: 0.4rem 1.2rem;
    border: 1px solid transparent;
    border-radius: var(--radius-full);
    background: var(--interactive-primary);
    color: var(--text-inverse);
    font-size: var(--text-caption);
    font-weight: 600;
    /* "Still true" must never fold into two rows when the bar gets tight. */
    white-space: nowrap;
    cursor: pointer;
}

.action:disabled {
    opacity: 0.5;
    cursor: default;
}

.action:hover:not(:disabled) {
    background: var(--interactive-primary-hover);
}

.action.ghost {
    border-color: var(--border-default);
    background: transparent;
    color: var(--text-secondary);
}

.action.ghost:hover:not(:disabled) {
    color: var(--text-primary);
    background: var(--interactive-ghost-hover);
}

.action.danger:hover:not(:disabled) {
    color: var(--node-problem, #ef4444);
}

/* ---------------------------------------------------------------------------
   Thin panes (IDE side panels, up to ~700px). Every override for that width
   lives here, at the END of the sheet, and deliberately so: a media query
   carries no extra specificity, so a block sitting next to the rule it
   refines silently loses to any same-specificity rule declared below it.
   (That is what kept `.bar-title` at its 22rem cap and the jump buttons at
   2.6rem while the media block looked correct.)

   Two shapes recur. The floating bars stop being centered pills and span the
   pane: an absolutely positioned wrapping box shrinks to fit the gap between
   `left: 50%` and the right edge, so a centered pill only ever gets HALF the
   width to lay out in — which folded the toolbar into three rows while it
   looked half empty. And anything that is context rather than content (the
   note count, the action bar's title) yields its space to the controls.
   --------------------------------------------------------------------------- */
@media (width <= 700px) {
    .feed {
        gap: 1.2rem;
        /* 30vh of nothing reads as a broken screen on a sliver of a viewport. */
        padding: 24vh 0.8rem;
        scroll-padding-block: 20vh;
    }

    .card {
        gap: 0.7rem;
        padding: 1.1rem 1.2rem;
    }

    .card-head {
        gap: 0.5rem;
    }

    .card-title {
        font-size: var(--text-body);
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
    }

    .feed-toolbar :deep(.segment) {
        padding: 0.35rem 0.8rem;
    }

    /* The one control in the bar that answers a question nobody asks mid-scroll. */
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

    /* Basis = the row minus the dot, and no shrinking: that is what makes the
       title take row one alone and truncate there, instead of letting one
       lucky button squeeze in beside it and push the rest into a third row. */
    .bar-title {
        flex: 1 0 calc(100% - 1.4rem);
        max-width: none;
        min-width: 0;
    }

    .action {
        flex: 1 1 auto;
        padding: 0.4rem 0.6rem;
        text-align: center;
    }
}
</style>
