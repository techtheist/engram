import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import { TRUSTED_TRUST } from '@/constants/ontology'
import { api } from '@/services/api'
import type {
    DriftEntry,
    EdgeType,
    Graph,
    GraphEdge,
    GraphNode,
    NewEdge,
    NewNode,
    SuspectVerdict,
    SuspectView,
} from '@/types/graph'

/** Pure client-side canvas filters; empty group = no restriction. */
export interface GraphFilters {
    types: string[]
    durabilities: string[]
    sources: string[]
    statuses: string[]
    trust: string[] // 'trusted' | 'provisional'
    tags: string[]
    showArchived: boolean
}

const NO_FILTERS: GraphFilters = {
    types: [],
    durabilities: [],
    sources: [],
    statuses: [],
    trust: [],
    tags: [],
    showArchived: true,
}

export function trustLevel(n: GraphNode): 'trusted' | 'provisional' | 'stale' {
    if (n.stale) return 'stale'
    return n.approved_at != null || n.trust >= TRUSTED_TRUST ? 'trusted' : 'provisional'
}

interface ChangeEvent {
    type:
        | 'node_added'
        | 'node_updated'
        | 'node_deleted'
        | 'edge_added'
        | 'edge_updated'
        | 'edge_deleted'
        | 'suspects_changed'
    data: GraphNode | GraphEdge | { id: string }
}

export const useGraphStore = defineStore('graph', () => {
    const nodes = ref(new Map<string, GraphNode>())
    const edges = ref(new Map<string, GraphEdge>())
    const suspects = ref<SuspectView[]>([])
    const drift = ref<DriftEntry[]>([])
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
        if (f.tags.length && !n.tags.some((t) => f.tags.includes(t))) return false
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
            f.tags.length +
            (f.showArchived ? 0 : 1)
        )
    })

    /** The live tag vocabulary, most-used first — dropdowns and filter chips. */
    const allTags = computed(() => {
        const counts = new Map<string, number>()
        for (const n of nodeList.value) {
            for (const t of n.tags) counts.set(t, (counts.get(t) ?? 0) + 1)
        }
        return [...counts.entries()]
            .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
            .map(([tag]) => tag)
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
            await loadSuspects()
            await loadDrift()
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e)
        } finally {
            loading.value = false
        }
    }

    async function loadSuspects(): Promise<void> {
        try {
            suspects.value = await api.suspects()
        } catch {
            suspects.value = []
        }
    }

    /** node id → its missing code_refs, for badges on cards and the detail. */
    const driftByNode = computed(() => new Map(drift.value.map((d) => [d.id, d.missing])))

    async function loadDrift(): Promise<void> {
        try {
            drift.value = await api.drift()
        } catch {
            drift.value = []
        }
    }

    /** Run the local conflict scan now; returns how many new pairs it queued. */
    async function scanConflicts(): Promise<number> {
        const { added } = await api.scanConflicts()
        await loadSuspects()
        return added
    }

    /** Judge a suspected pair; graph changes (edges/archival) arrive via SSE. */
    async function resolveSuspect(id: string, verdict: SuspectVerdict): Promise<void> {
        await api.resolveSuspect(id, verdict)
        await loadSuspects()
        await refresh() // pick up the confirmed edge / archived node immediately
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
            // Suspects can change with no graph delta (a scan queued pairs), so
            // reconcile them on every poll — the list is tiny. Drift is NOT
            // polled: each scan stats every code_ref on disk under the engine
            // lock, so it reloads only on store load and Review-panel open.
            await loadSuspects()
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
            case 'suspects_changed':
                void loadSuspects()
                break
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

    /** Edit node fields from the pane (title/body/type/durability/tags). */
    async function patchNode(id: string, patch: Record<string, unknown>): Promise<void> {
        upsertNode(await api.patchNode(id, patch))
    }

    /** Create a user-sourced node from the pane; returns it selected-ready. */
    async function createNode(node: Omit<NewNode, 'source'>): Promise<GraphNode> {
        const created = await api.createNode({ ...node, source: 'user' })
        upsertNode(created)
        return created
    }

    /** Create a user-sourced edge (canvas drag or detail panel). */
    async function createEdge(edge: Omit<NewEdge, 'source'>): Promise<GraphEdge> {
        const created = await api.createEdge({ ...edge, source: 'user' })
        const next = new Map(edges.value)
        next.set(created.id, created)
        edges.value = next
        return created
    }

    /** Rewrite an edge's verb in place (pane CRUD parity). */
    async function retypeEdge(id: string, type: EdgeType): Promise<void> {
        const edge = await api.patchEdge(id, { type })
        const next = new Map(edges.value)
        next.set(edge.id, edge)
        edges.value = next
    }

    async function removeEdge(id: string): Promise<void> {
        await api.deleteEdge(id)
        const next = new Map(edges.value)
        next.delete(id) // SSE also emits edge_deleted; dropping twice is harmless
        edges.value = next
    }

    return {
        nodes,
        edges,
        suspects,
        loadSuspects,
        drift,
        driftByNode,
        loadDrift,
        scanConflicts,
        resolveSuspect,
        patchNode,
        createNode,
        createEdge,
        retypeEdge,
        removeEdge,
        nodeList,
        edgeList,
        visibleNodeList,
        visibleEdgeList,
        filters,
        allTags,
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
