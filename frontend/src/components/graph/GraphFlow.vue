<script setup lang="ts">
import { computed, markRaw, nextTick, ref, shallowRef, watch } from 'vue'
import {
    VueFlow,
    PanOnScrollMode,
    useVueFlow,
    type Connection,
    type Edge,
    type FitViewParams,
    type Node,
    type NodeDragEvent,
    type NodeMouseEvent,
} from '@vue-flow/core'
import { Background } from '@vue-flow/background'
import { ControlButton, Controls } from '@vue-flow/controls'
import { MiniMap } from '@vue-flow/minimap'
import { storeToRefs } from 'pinia'
import EngramNode from '@/components/nodes/EngramNode.vue'
import EngramEdge from '@/components/nodes/EngramEdge.vue'
import ConnectDialog from '@/components/panels/ConnectDialog.vue'
import { layoutGraph, type XY } from '@/composables/useLayout'
import { useConfigStore } from '@/stores/config'
import { heatColor, useCrowdingStore } from '@/stores/crowding'
import { useGraphStore } from '@/stores/graph'
import { useLayoutStore } from '@/stores/layout'
import { useProjectsStore } from '@/stores/projects'
import type { GraphNode } from '@/types/graph'
import { edgeFocus, FOCUS_RINGS, focusWidth, selectionHops } from './focus'

const store = useGraphStore()
const config = useConfigStore()
const layout = useLayoutStore()
const projects = useProjectsStore()
const { visibleNodeList, visibleEdgeList, selectedId } = storeToRefs(store)
const { findNode, fitView, setCenter, viewport } = useVueFlow()

const nodeTypes = { engram: markRaw(EngramNode) }
const edgeTypes = { engram: markRaw(EngramEdge) }

/**
 * Zoom-compensated selection outline: a fixed 0.2rem outline scales down
 * with the canvas transform until the selected node is unfindable. Width
 * follows a continuous piecewise-linear curve through the tuned stops
 * (zoom → rem): 1→0.2, 0.5→0.4, 0.25→0.8, 0.1→1.2, then ~2 screen px
 * below (0.05→3.4, 0.025→7.4, 0.01→18) — clamped at both ends. The offset
 * rides along: 0.3rem at 100%, ~1.2× the width far out.
 */
const OUTLINE_STOPS: [number, number][] = [
    [1, 0.2],
    [0.5, 0.4],
    [0.25, 0.8],
    [0.1, 1.2],
    [0.05, 3.4],
    [0.025, 7.4],
    [0.01, 18],
]

const selectedOutlineRem = computed(() => {
    const z = viewport.value.zoom
    let [hiZ, hiW] = OUTLINE_STOPS[0]!
    if (z >= hiZ) return hiW
    // Stops are zoom-descending: lerp inside the first segment containing z.
    for (const [loZ, loW] of OUTLINE_STOPS.slice(1)) {
        if (z >= loZ) {
            const t = (hiZ - z) / (hiZ - loZ)
            return hiW + (loW - hiW) * t
        }
        ;[hiZ, hiW] = [loZ, loW]
    }
    return hiW
})

const outlineVars = computed(() => ({
    '--selected-outline-w': `${selectedOutlineRem.value.toFixed(3)}rem`,
    '--selected-outline-o': `${(selectedOutlineRem.value * 1.2 + 0.06).toFixed(3)}rem`,
}))

/**
 * Zoom-gated glass: below ~0.7 zoom the cards are minimal-form thumbnails and
 * per-card backdrop blur (engram-purple) + big shadows only burn compositor
 * time. Hysteresis (off < 0.72, back on ≥ 0.8) so the boundary never flickers.
 */
const lowZoom = ref(false)
watch(
    () => viewport.value.zoom,
    (z) => {
        if (lowZoom.value && z >= 0.8) lowZoom.value = false
        else if (!lowZoom.value && z < 0.72) lowZoom.value = true
    },
    { immediate: true },
)

/**
 * Fit with a 3% breathing margin on every side, except a 64px top gap so the
 * floating search/settings bar never covers the topmost nodes.
 */
const FIT_VIEW_PARAMS: FitViewParams = {
    padding: { top: '64px', left: '3%', right: '3%', bottom: '3%' },
}

// Auto-fit once, when nodes first get their dimensions — not on every later
// SSE/poll insert, which would keep yanking the viewport around.
let didInitialFit = false
async function onNodesInitialized(): Promise<void> {
    if (didInitialFit || visibleNodeList.value.length === 0) return
    didInitialFit = true
    await nextTick()
    await fitView(FIT_VIEW_PARAMS)
}

/** Hand-placed positions survive re-layout (PLAN: pane is editable). */
const overrides = ref(new Map<string, XY>())

/*
 * Laid out when the nodes or the arrangement change, not the edges: a new
 * connection keeps every card where it is (GraphGL's shapeKey has the why).
 */
const flowShape = computed(() => `${layout.current}|${visibleNodeList.value.map((n) => (n.type === 'Anchor' ? `${n.id}*` : n.id)).join(',')}`)
const positions = shallowRef(new Map<string, XY>())
watch(
    [flowShape, overrides],
    () => {
        positions.value = layoutGraph(visibleNodeList.value, visibleEdgeList.value, overrides.value, layout.current)
    },
    { immediate: true },
)

// Switching layouts rearranges everything — re-fit so the user lands on
// the new shape instead of an empty corner of the old one.
watch(
    () => layout.current,
    async () => {
        await nextTick()
        await fitView({ ...FIT_VIEW_PARAMS, duration: 400 })
    },
)

// Feed→graph sync: coming back to the canvas with a selection (the feed's
// centered card, pushed into the store as it unmounts), center on that node
// — an off-screen selection outline reads as "the sync didn't happen". Zoom
// stays the user's; the zoom-compensated outline keeps it findable anyway.
watch(
    () => layout.view,
    async (v) => {
        if (v !== 'graph') return
        await nextTick()
        const node = selectedId.value ? findNode(selectedId.value) : undefined
        if (!node) return
        void setCenter(
            node.position.x + node.dimensions.width / 2,
            node.position.y + node.dimensions.height / 2,
            { zoom: viewport.value.zoom, duration: 300 },
        )
    },
)

// Switching projects replaces the whole graph, and graph sizes differ wildly
// — the one-time initial fit doesn't cover this, so re-fit once the freshly
// rendered nodes have dimensions (one frame after the DOM settles).
watch(
    () => projects.switchEpoch,
    async () => {
        await nextTick()
        await new Promise(requestAnimationFrame)
        if (visibleNodeList.value.length > 0) {
            await fitView({ ...FIT_VIEW_PARAMS, duration: 400 })
        }
    },
)

const flowNodes = computed<Node<GraphNode>[]>(() =>
    visibleNodeList.value.map((n) => ({
        id: n.id,
        type: 'engram',
        position: positions.value.get(n.id) ?? { x: 0, y: 0 },
        data: n,
        selected: n.id === selectedId.value,
    })),
)

const flowEdges = computed<Edge[]>(() => {
    // Selection focus (focus.ts): the selection's rings widen and rise, the rest dims.
    const sel = selectedId.value
    const edges = visibleEdgeList.value
    const hops = sel && edges.some((e) => e.from_id === sel || e.to_id === sel) ? selectionHops(sel, edges) : null
    return edges.map((e) => {
        const focus = edgeFocus(hops, e.from_id, e.to_id)
        return {
            id: e.id,
            source: e.from_id,
            target: e.to_id,
            label: e.type,
            type: 'engram',
            data: { note: e.note },
            animated: config.edgeAnimated(e.type),
            class: focus < 0 ? 'edge-dim' : undefined,
            zIndex: focus > 0 ? FOCUS_RINGS + 1 - focus : 0,
            style: {
                stroke: config.edgeColor(e.type),
                strokeWidth: focus > 0 ? 2 * focusWidth(focus) : 2,
                strokeDasharray: config.edgeDashed(e.type) ? '6 4' : undefined,
            },
        }
    })
})

function onNodeClick({ node }: NodeMouseEvent): void {
    store.select(node.id)
}

/**
 * A handle-to-handle drag proposes an edge; the dialog asks for the verb that
 * makes it a sentence (PLAN §10 pane CRUD — edge creation by dragging).
 */
const pendingConnection = ref<{ source: string; target: string } | null>(null)

function onConnect(conn: Connection): void {
    if (!conn.source || !conn.target || conn.source === conn.target) return
    pendingConnection.value = { source: conn.source, target: conn.target }
}

function onNodeDragStop({ node }: NodeDragEvent): void {
    const next = new Map(overrides.value)
    next.set(node.id, { x: node.position.x, y: node.position.y })
    overrides.value = next
}

function onPaneClick(): void {
    store.select(null)
}

const crowding = useCrowdingStore()

/** The minimap follows the cards: type colors, or crowding heat (0.11.0). */
const minimapColor = (node: Node<GraphNode>): string =>
    crowding.heatOn
        ? (heatColor(crowding.heatOf(node.id)) ?? 'var(--border-default)')
        : config.accent(node.data?.type ?? '')

/** Click-to-navigate: center the viewport on the clicked minimap spot (flow
 * coords), keeping the current zoom — replaces drag-panning, whose axes felt
 * inverted. */
function onMiniMapClick({ position }: { event: MouseEvent; position: { x: number; y: number } }): void {
    void setCenter(position.x, position.y, { zoom: viewport.value.zoom, duration: 250 })
}

// Drop overrides for nodes that no longer exist so the map can't leak.
// (Filtered-out nodes keep theirs — hand placement survives a filter round-trip.)
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
})
</script>

<template>
<div class="canvas-root" :class="{ 'low-zoom': lowZoom }" :style="outlineVars">
    <VueFlow
        :nodes="flowNodes"
        :edges="flowEdges"
        :node-types="nodeTypes"
        :edge-types="edgeTypes"
        :min-zoom="0.05"
        :max-zoom="1"
        :pan-on-scroll="true"
        :pan-on-scroll-mode="PanOnScrollMode.Free"
        :zoom-on-scroll="false"
        :zoom-on-pinch="true"
        :zoom-activation-key-code="['Meta', 'Control']"
        class="engram-canvas"
        @node-click="onNodeClick"
        @node-drag-stop="onNodeDragStop"
        @pane-click="onPaneClick"
        @nodes-initialized="onNodesInitialized"
        @connect="onConnect"
    >
        <Background :gap="22" :size="1.4" pattern-color="var(--canvas-dots)" />
        <Controls position="bottom-left" :fit-view-params="FIT_VIEW_PARAMS">
            <ControlButton
                :class="{ 'heat-on': crowding.heatOn }"
                :title="crowding.heatOn ? 'crowding heat: on — the minimap colors notes by how crowded they are in vector space' : 'crowding heat — color the minimap by how crowded each note is in vector space'"
                aria-label="toggle crowding heat"
                @click="crowding.toggleHeat()"
            >
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 32"><path d="M13.5 0S16 6.5 11 11.5C7.6 14.9 4 18.4 4 23.2 4 28.1 7.6 32 12 32s8-3.9 8-8.8c0-3.5-1.7-6.2-3.4-8.2.1 2.6-1 4.6-2.8 5.4.9-4.6-.1-9.4-1.3-12.1C11.4 4.6 13.5 0 13.5 0z" /></svg>
            </ControlButton>
        </Controls>
        <MiniMap
            zoomable
            position="bottom-right"
            :width="100"
            :height="75"
            :node-color="minimapColor"
            mask-color="var(--surface-overlay)"
            @click="onMiniMapClick"
        />
        <div class="canvas-glow" aria-hidden="true" />
    </VueFlow>

    <ConnectDialog
        v-if="pendingConnection"
        :source="pendingConnection.source"
        :target="pendingConnection.target"
        @close="pendingConnection = null"
    />
</div>
</template>

<style scoped>
.canvas-root {
    position: relative;
    width: 100%;
    height: 100%;
}

.engram-canvas {
    width: 100%;
    height: 100%;
    background-color: var(--canvas-bg);
}

/* Brand radial wash — only engram-purple defines non-transparent glows. */
.canvas-glow {
    position: absolute;
    inset: 0;
    z-index: 0;
    pointer-events: none;
    background:
        radial-gradient(60rem 60rem at 18% 12%, var(--canvas-glow-1), transparent 70%),
        radial-gradient(70rem 70rem at 82% 88%, var(--canvas-glow-2), transparent 70%);
}
</style>

<style>
/*
 * Unscoped: Vue Flow renders nodes/handles into its own DOM, so scoped
 * selectors wouldn't reach them. Kept minimal and namespaced to .engram-canvas.
 */
/* Zoom-gated glass (see lowZoom above): cheap flat cards while zoomed out —
 * the :hover variant is listed too so it can't re-enable the heavy styles.
 * With no blur behind it, a translucent surface (the hover reveal) would
 * show raw canvas through the card — so the surface goes fully opaque too. */
.canvas-root.low-zoom .engram-node,
.canvas-root.low-zoom .engram-node:hover {
    backdrop-filter: none;
    box-shadow: var(--shadow-sm);
    background-color: var(--surface-elevated);
}

.engram-canvas .vue-flow__handle.engram-handle {
    width: 0.9rem;
    height: 0.9rem;
    border: 2px solid var(--surface-base);
    background-color: var(--border-strong);
}

/* Lift a hovered node (and its wrapper) above its neighbours so the expanded
 * body is never clipped by adjacent cards. step-end keeps z snapping instant. */
.engram-canvas .vue-flow__node:has(.engram-node:hover) {
    z-index: 1000 !important;
    transition: z-index var(--duration-fast) step-end;
}

/*
 * Floating canvas chrome (controls + minimap). Same glass opt-in as
 * .glass-panel: translucent + blurred only in engram-purple, flat opaque
 * elsewhere. overflow:hidden clips the inner SVG/buttons to the rounded
 * border so the corners aren't squared off over it.
 */
.engram-canvas .vue-flow__controls,
.engram-canvas .vue-flow__minimap {
    overflow: hidden;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-lg);
    background-color: var(--surface-glass);
    backdrop-filter: var(--glass-backdrop);
    box-shadow: var(--shadow-md);
}

.engram-canvas .vue-flow__minimap svg {
    display: block;
    border-radius: inherit;
}

.engram-canvas .vue-flow__controls-button {
    border: none;
    border-bottom: 1px solid var(--border-subtle);
    background-color: transparent;
    fill: var(--text-secondary);
}

.engram-canvas .vue-flow__controls-button:last-child {
    border-bottom: none;
}

.engram-canvas .vue-flow__controls-button:hover {
    background-color: var(--interactive-ghost-hover);
    fill: var(--text-primary);
}

.engram-canvas .vue-flow__edge {
    transition: opacity 250ms var(--ease-default);
}

.engram-canvas .vue-flow__edge-path {
    transition: stroke-width 250ms var(--ease-default);
}

/* Selection focus (focus.ts FOCUS_DIM): edges past the selection's rings. */
.engram-canvas .vue-flow__edge.edge-dim {
    opacity: 0.22;
}

.engram-canvas .vue-flow__edge-text {
    font-family: var(--font-sans);
}

.engram-canvas .vue-flow__controls-button.heat-on svg {
    fill: var(--node-caution);
}
</style>
