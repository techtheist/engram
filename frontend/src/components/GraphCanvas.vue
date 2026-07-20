<script setup lang="ts">
import { computed, markRaw, nextTick, ref, watch } from 'vue'
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
import { Controls } from '@vue-flow/controls'
import { MiniMap } from '@vue-flow/minimap'
import { storeToRefs } from 'pinia'
import EngramNode from '@/components/nodes/EngramNode.vue'
import EngramEdge from '@/components/nodes/EngramEdge.vue'
import ConnectDialog from '@/components/panels/ConnectDialog.vue'
import { layoutGraph, type XY } from '@/composables/useLayout'
import { EDGE_ANIMATED, EDGE_COLOR, EDGE_DASHED, NODE_ACCENT_VAR } from '@/constants/ontology'
import { useGraphStore } from '@/stores/graph'
import { useLayoutStore } from '@/stores/layout'
import { useProjectsStore } from '@/stores/projects'
import type { GraphNode } from '@/types/graph'

const store = useGraphStore()
const layout = useLayoutStore()
const projects = useProjectsStore()
const { visibleNodeList, visibleEdgeList, selectedId } = storeToRefs(store)
const { fitView, setCenter, viewport } = useVueFlow()

const nodeTypes = { engram: markRaw(EngramNode) }
const edgeTypes = { engram: markRaw(EngramEdge) }

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

const positions = computed(() =>
    layoutGraph(visibleNodeList.value, visibleEdgeList.value, overrides.value, layout.current),
)

// Switching Skyline ↔ Nebula rearranges everything — re-fit so the user
// lands on the new shape instead of an empty corner of the old one.
watch(
    () => layout.current,
    async () => {
        await nextTick()
        await fitView({ ...FIT_VIEW_PARAMS, duration: 400 })
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

const flowEdges = computed<Edge[]>(() =>
    visibleEdgeList.value.map((e) => ({
        id: e.id,
        source: e.from_id,
        target: e.to_id,
        label: e.type,
        type: 'engram',
        data: { note: e.note },
        animated: EDGE_ANIMATED.has(e.type),
        style: {
            stroke: EDGE_COLOR[e.type],
            strokeWidth: 2,
            strokeDasharray: EDGE_DASHED.has(e.type) ? '6 4' : undefined,
        },
    })),
)

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

const minimapColor = (node: Node<GraphNode>): string =>
    NODE_ACCENT_VAR[node.data?.type ?? 'Anchor']

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
<div class="canvas-root">
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
        <Controls position="bottom-left" :fit-view-params="FIT_VIEW_PARAMS" />
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

.engram-canvas .vue-flow__edge-text {
    font-family: var(--font-sans);
}
</style>
