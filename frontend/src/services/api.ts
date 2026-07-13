import type {
    AnsweredHint,
    AuditPage,
    AuditSweep,
    ClaimReport,
    DriftEntry,
    ExportGraph,
    Graph,
    GraphEdge,
    GraphNode,
    ImportSummary,
    NewEdge,
    NewNode,
    SearchHit,
    SuspectVerdict,
    SuspectView,
    SystemInfo,
    TagStat,
    TimelineEntry,
} from '@/types/graph'

declare global {
    interface Window {
        /** Injected by an embedding host (e.g. the VSCode webview) at runtime. */
        __ENGRAM_API__?: string
    }
}

/**
 * Backend base URL, resolved in order:
 *  1. `window.__ENGRAM_API__` — a host (the VSCode webview loads a *bundled*
 *     SPA, so it can't use same-origin; it injects the daemon URL).
 *  2. `VITE_ENGRAM_API` — the Vite dev server points this at :8787.
 *  3. `''` (same origin) — the `engram-alpha serve` daemon serves the pane itself
 *     (browser standalone, JetBrains JCEF), so relative URLs just work.
 */
export const API_BASE: string =
    (typeof window !== 'undefined' && window.__ENGRAM_API__) ||
    import.meta.env.VITE_ENGRAM_API ||
    ''

async function request<T>(path: string, init?: RequestInit): Promise<T> {
    const res = await fetch(`${API_BASE}${path}`, {
        headers: { 'content-type': 'application/json' },
        ...init,
    })
    if (!res.ok) {
        const detail = await res.text().catch(() => '')
        throw new Error(`${init?.method ?? 'GET'} ${path} → ${res.status} ${detail}`)
    }
    if (res.status === 204) return undefined as T
    return (await res.json()) as T
}

async function requestText(path: string): Promise<string> {
    const res = await fetch(`${API_BASE}${path}`)
    if (!res.ok) {
        const detail = await res.text().catch(() => '')
        throw new Error(`GET ${path} → ${res.status} ${detail}`)
    }
    return res.text()
}

export const api = {
    graph: () => request<Graph>('/graph'),

    /** The session-start digest, as raw markdown (what the assistant receives). */
    brief: () => requestText('/brief'),

    getNode: (id: string) => request<GraphNode>(`/nodes/${id}`),

    createNode: (node: NewNode) =>
        request<GraphNode>('/nodes', { method: 'POST', body: JSON.stringify(node) }),

    createEdge: (edge: NewEdge) =>
        request<GraphEdge>('/edges', { method: 'POST', body: JSON.stringify(edge) }),

    deleteEdge: (id: string) => request<void>(`/edges/${id}`, { method: 'DELETE' }),

    /** Tags in use on current nodes, freshest first. */
    tags: () => request<TagStat[]>('/tags'),

    search: (query: string, limit = 12) =>
        request<SearchHit[]>(`/search?q=${encodeURIComponent(query)}&limit=${limit}`),

    reconfirm: (id: string) => request<GraphNode>(`/nodes/${id}/reconfirm`, { method: 'POST' }),
    approve: (id: string) => request<GraphNode>(`/nodes/${id}/approve`, { method: 'POST' }),

    /** Withdraw an approval (and any pin) — trust falls back to its anchor. */
    revokeApproval: (id: string) =>
        request<GraphNode>(`/nodes/${id}/approve`, { method: 'DELETE' }),

    /** Set (a number in 0..1, pin = 1.0) or clear (null) the constant-trust pin. */
    pin: (id: string, value: number | null) =>
        request<GraphNode>(`/nodes/${id}/pin`, {
            method: 'POST',
            body: JSON.stringify({ value }),
        }),

    patchNode: (id: string, patch: Record<string, unknown>) =>
        request<GraphNode>(`/nodes/${id}`, { method: 'PATCH', body: JSON.stringify(patch) }),

    patchEdge: (id: string, patch: Record<string, unknown>) =>
        request<GraphEdge>(`/edges/${id}`, { method: 'PATCH', body: JSON.stringify(patch) }),

    deleteNode: (id: string) => request<void>(`/nodes/${id}`, { method: 'DELETE' }),

    /** Pending suspected conflicts from the local scan. */
    suspects: () => request<SuspectView[]>('/conflicts/suspects'),

    /** Run the candidate sweep now; returns how many new suspects were queued. */
    scanConflicts: () => request<{ added: number }>('/conflicts/scan', { method: 'POST' }),

    /** Verify a claim against the canon via the local NLI model. */
    checkClaim: (text: string, limit = 8) =>
        request<ClaimReport>('/claims/check', {
            method: 'POST',
            body: JSON.stringify({ text, limit }),
        }),

    /** Checkup: deep conflict sweep (lower similarity floor, NLI-gated). */
    auditConflicts: () => request<AuditSweep>('/audit/conflicts', { method: 'POST' }),

    /** Checkup: mutual-entailment duplicate sweep. */
    auditDuplicates: () => request<AuditSweep>('/audit/duplicates', { method: 'POST' }),

    /** Checkup: open Problems an existing node may already answer. */
    auditAnswered: () => request<AnsweredHint[]>('/audit/answered', { method: 'POST' }),

    /** Nodes whose path-shaped code_refs no longer exist in the project. */
    drift: () => request<DriftEntry[]>('/drift'),

    /** The node's `replaces` chain, oldest generation first. */
    timeline: (id: string) => request<TimelineEntry[]>(`/nodes/${id}/timeline`),

    /** Daemon-side diagnostics for the System info panel. */
    system: () => request<SystemInfo>('/system'),

    resolveSuspect: (id: string, verdict: SuspectVerdict) =>
        request<{ edge: GraphEdge | null }>(`/conflicts/suspects/${id}/resolve`, {
            method: 'POST',
            body: JSON.stringify({ verdict }),
        }),

    decay: (ttlDays: number) =>
        request<{ archived: number; ids: string[] }>(`/decay?ttl_days=${ttlDays}`, { method: 'POST' }),

    /** What decay *would* archive right now — never mutates. */
    decayPreview: (ttlDays = 14) =>
        request<{ archived: number; ids: string[] }>(`/decay?ttl_days=${ttlDays}&dry_run=true`, {
            method: 'POST',
        }),

    /** One page of the audit journal, newest first; pass the last entry's seq as `before` to page on. */
    audit: (limit = 50, before?: number) =>
        request<AuditPage>(`/audit?limit=${limit}${before != null ? `&before=${before}` : ''}`),

    exportGraph: () => request<ExportGraph>('/export'),

    importGraph: (graph: ExportGraph) =>
        request<ImportSummary>('/import', { method: 'POST', body: JSON.stringify(graph) }),

    /** SSE stream URL for live graph mutations. */
    eventsUrl: () => `${API_BASE}/events`,
}
