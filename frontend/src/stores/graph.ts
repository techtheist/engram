import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import { TRUSTED_TRUST } from '@/constants/ontology'
import { api } from '@/services/api'
import type { Graph, GraphEdge, GraphNode } from '@/types/graph'

/** Pure client-side canvas filters; empty group = no restriction. */
export interface GraphFilters {
    types: string[]
    durabilities: string[]
    sources: string[]
    statuses: string[]
    trust: string[] // 'trusted' | 'provisional'
    showArchived: boolean
}

const NO_FILTERS: GraphFilters = {
    types: [],
    durabilities: [],
    sources: [],
    statuses: [],
    trust: [],
    showArchived: true,
}

export function trustLevel(n: GraphNode): 'trusted' | 'provisional' | 'stale' {
    if (n.stale) return 'stale'
    return n.approved_at != null || n.trust >= TRUSTED_TRUST ? 'trusted' : 'provisional'
}

interface ChangeEvent {
    type: 'node_added' | 'node_updated' | 'node_deleted' | 'edge_added' | 'edge_updated' | 'edge_deleted'
    data: GraphNode | GraphEdge | { id: string }
}

export const useGraphStore = defineStore('graph', () => {
    const nodes = ref(new Map<string, GraphNode>())
    const edges = ref(new Map<string, GraphEdge>())
    const selectedId = ref<string | null>(null)
    const loading = ref(false)
    const error = ref<string | null>(null)
    const connected = ref(false)

    let stream: EventSource | null = null
    let lastSig = ''

    const nodeList = computed(() => [...nodes.value.values()])
    const edgeList = computed(() => [...edges.value.values()])
    const selected = computed(() =>
        selectedId.value ? (nodes.value.get(selectedId.value) ?? null) : null,
    )

    // --- client-side canvas filters ---------------------------------------

    const filters = ref<GraphFilters>(structuredClone(NO_FILTERS))

    function nodeMatches(n: GraphNode): boolean {
        const f = filters.value
        if (!f.showArchived && n.valid_until != null) return false
        if (f.types.length && !f.types.includes(n.type)) return false
        if (f.durabilities.length && !f.durabilities.includes(n.durability)) return false
        if (f.sources.length && !f.sources.includes(n.source)) return false
        if (f.statuses.length && !(n.status && f.statuses.includes(n.status))) return false
        if (f.trust.length && !f.trust.includes(trustLevel(n))) return false
        return true
    }

    /** What the canvas renders; edges survive only between visible nodes. */
    const visibleNodeList = computed(() => nodeList.value.filter(nodeMatches))
    const visibleEdgeList = computed(() => {
        const ids = new Set(visibleNodeList.value.map((n) => n.id))
        return edgeList.value.filter((e) => ids.has(e.from_id) && ids.has(e.to_id))
    })

    const activeFilterCount = computed(() => {
        const f = filters.value
        return (
            f.types.length +
            f.durabilities.length +
            f.sources.length +
            f.statuses.length +
            f.trust.length +
            (f.showArchived ? 0 : 1)
        )
    })

    function toggleFilter(group: Exclude<keyof GraphFilters, 'showArchived'>, value: string): void {
        const list = filters.value[group]
        const i = list.indexOf(value)
        if (i >= 0) list.splice(i, 1)
        else list.push(value)
    }

    function clearFilters(): void {
        filters.value = structuredClone(NO_FILTERS)
    }

    function applyGraph(g: Graph): void {
        nodes.value = new Map(g.nodes.map((n) => [n.id, n]))
        edges.value = new Map(g.edges.map((e) => [e.id, e]))
    }

    async function load(): Promise<void> {
        loading.value = true
        error.value = null
        try {
            const g = await api.graph()
            applyGraph(g)
            lastSig = JSON.stringify(g)
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e)
        } finally {
            loading.value = false
        }
    }

    /*
     * Fallback for cross-process writes (Claude via the MCP server): the
     * daemon's SSE only broadcasts writes made in its own process, so poll the
     * shared DB and reconcile when — and only when — the snapshot changed.
     * SSE still delivers pane-originated writes instantly; this just backfills
     * what SSE can't see.
     */
    async function refresh(): Promise<void> {
        try {
            const g = await api.graph()
            const sig = JSON.stringify(g)
            if (sig !== lastSig) {
                applyGraph(g)
                lastSig = sig
            }
        } catch {
            /* ignore transient poll errors; SSE/error overlay handle real outages */
        }
    }

    function upsertNode(node: GraphNode): void {
        const next = new Map(nodes.value)
        next.set(node.id, node)
        nodes.value = next
    }

    function dropNode(id: string): void {
        const next = new Map(nodes.value)
        next.delete(id)
        nodes.value = next
        const nextEdges = new Map(edges.value)
        for (const [eid, e] of nextEdges) {
            if (e.from_id === id || e.to_id === id) nextEdges.delete(eid)
        }
        edges.value = nextEdges
        if (selectedId.value === id) selectedId.value = null
    }

    function applyEvent(raw: string): void {
        const msg = JSON.parse(raw) as ChangeEvent
        switch (msg.type) {
            case 'node_added':
            case 'node_updated':
                upsertNode(msg.data as GraphNode)
                break
            case 'node_deleted':
                dropNode((msg.data as { id: string }).id)
                break
            case 'edge_added':
            case 'edge_updated': {
                const e = msg.data as GraphEdge
                const next = new Map(edges.value)
                next.set(e.id, e)
                edges.value = next
                break
            }
            case 'edge_deleted': {
                const next = new Map(edges.value)
                next.delete((msg.data as { id: string }).id)
                edges.value = next
                break
            }
        }
    }

    function connect(): void {
        if (stream) return
        stream = new EventSource(api.eventsUrl())
        stream.onopen = () => (connected.value = true)
        stream.onerror = () => (connected.value = false)
        stream.onmessage = (ev) => {
            try {
                applyEvent(ev.data)
            } catch {
                /* ignore malformed frame */
            }
        }
    }

    function disconnect(): void {
        stream?.close()
        stream = null
        connected.value = false
    }

    function select(id: string | null): void {
        selectedId.value = id
    }

    async function reconfirm(id: string): Promise<void> {
        upsertNode(await api.reconfirm(id))
    }

    /** Explicit user approval: trust restarts at 100% on the slow curve. */
    async function approve(id: string): Promise<void> {
        upsertNode(await api.approve(id))
    }

    async function setEdgeStatus(id: string, status: 'active' | 'resolved' | 'dismissed'): Promise<void> {
        const edge = await api.patchEdge(id, { status })
        const next = new Map(edges.value)
        next.set(edge.id, edge)
        edges.value = next
    }

    async function remove(id: string): Promise<void> {
        await api.deleteNode(id)
        dropNode(id) // SSE also emits node_deleted; dropping twice is harmless
    }

    return {
        nodes,
        edges,
        nodeList,
        edgeList,
        visibleNodeList,
        visibleEdgeList,
        filters,
        activeFilterCount,
        toggleFilter,
        clearFilters,
        selectedId,
        selected,
        loading,
        error,
        connected,
        load,
        refresh,
        connect,
        disconnect,
        select,
        reconfirm,
        approve,
        setEdgeStatus,
        remove,
    }
})
