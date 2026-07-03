import type { ExportGraph, Graph, GraphEdge, GraphNode, ImportSummary, SearchHit } from '@/types/graph'

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
 *  3. `''` (same origin) — the `engram serve` daemon serves the pane itself
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

export const api = {
    graph: () => request<Graph>('/graph'),

    getNode: (id: string) => request<GraphNode>(`/nodes/${id}`),

    search: (query: string, limit = 12) =>
        request<SearchHit[]>(`/search?q=${encodeURIComponent(query)}&limit=${limit}`),

    reconfirm: (id: string) => request<GraphNode>(`/nodes/${id}/reconfirm`, { method: 'POST' }),

    patchNode: (id: string, patch: Record<string, unknown>) =>
        request<GraphNode>(`/nodes/${id}`, { method: 'PATCH', body: JSON.stringify(patch) }),

    patchEdge: (id: string, patch: Record<string, unknown>) =>
        request<GraphEdge>(`/edges/${id}`, { method: 'PATCH', body: JSON.stringify(patch) }),

    deleteNode: (id: string) => request<void>(`/nodes/${id}`, { method: 'DELETE' }),

    decay: (ttlDays: number) =>
        request<{ archived: number; ids: string[] }>(`/decay?ttl_days=${ttlDays}`, { method: 'POST' }),

    /** What decay *would* archive right now — never mutates. */
    decayPreview: (ttlDays = 14) =>
        request<{ archived: number; ids: string[] }>(`/decay?ttl_days=${ttlDays}&dry_run=true`, {
            method: 'POST',
        }),

    exportGraph: () => request<ExportGraph>('/export'),

    importGraph: (graph: ExportGraph) =>
        request<ImportSummary>('/import', { method: 'POST', body: JSON.stringify(graph) }),

    /** SSE stream URL for live graph mutations. */
    eventsUrl: () => `${API_BASE}/events`,
}
