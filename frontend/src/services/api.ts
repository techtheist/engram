import type {
    AnsweredHint,
    AuditPage,
    AuditSweep,
    ClaimReport,
    DriftEntry,
    ExportGraph,
    FsListing,
    Graph,
    GraphEdge,
    GraphNode,
    ImportSummary,
    NewEdge,
    NewNode,
    ProjectEntry,
    ProjectInfo,
    PromotionCandidate,
    SearchHit,
    SuspectVerdict,
    SuspectView,
    ModelApplyResult,
    ModelSelection,
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

/**
 * `/projects/{id}` while the pane is switched to another project (PLAN §7C);
 * '' = the daemon's launch project. Every graph call below rides this prefix;
 * hub-level meta calls (`/projects`, `/health`, `/system`, promotions) don't.
 */
let projectPrefix = ''

export function setApiProject(id: string | null): void {
    projectPrefix = id ? `/projects/${encodeURIComponent(id)}` : ''
}

async function fetchJson<T>(url: string, init?: RequestInit): Promise<T> {
    const res = await fetch(url, {
        headers: { 'content-type': 'application/json' },
        ...init,
    })
    if (!res.ok) {
        const detail = await res.text().catch(() => '')
        throw new Error(`${init?.method ?? 'GET'} ${url} → ${res.status} ${detail}`)
    }
    if (res.status === 204) return undefined as T
    return (await res.json()) as T
}

/** Project-scoped call: honors the active project prefix. */
function request<T>(path: string, init?: RequestInit): Promise<T> {
    return fetchJson(`${API_BASE}${projectPrefix}${path}`, init)
}

/** Hub-level call: never scoped, whatever project the pane shows. */
function metaRequest<T>(path: string, init?: RequestInit): Promise<T> {
    return fetchJson(`${API_BASE}${path}`, init)
}

async function requestText(path: string): Promise<string> {
    const res = await fetch(`${API_BASE}${projectPrefix}${path}`)
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

    /** Daemon-side diagnostics for the System info panel (launch project). */
    system: () => metaRequest<SystemInfo>('/system'),

    /** The machine-level cortex model selection (PLAN §7A). */
    models: () => metaRequest<ModelSelection>('/models'),

    /**
     * Apply a model selection. Blocking on purpose: the response arrives
     * after download + load + live swap (+ full re-embed for embeddings).
     */
    applyModel: (body: { role: string; preset?: string; custom?: object }) =>
        metaRequest<ModelApplyResult>('/models', {
            method: 'POST',
            body: JSON.stringify(body),
        }),

    /** Every project the hub can reach: current, home, registry (PLAN §7C). */
    projects: () => metaRequest<ProjectInfo[]>('/projects'),

    /** Register a repo on the machine registry by absolute path. */
    registerProject: (path: string) =>
        metaRequest<ProjectEntry>('/projects', {
            method: 'POST',
            body: JSON.stringify({ path }),
        }),

    /** Withdraw a project from the registry (its data stays where it lives). */
    unregisterProject: (id: string) =>
        metaRequest<{ ok: boolean }>(`/projects/${encodeURIComponent(id)}`, { method: 'DELETE' }),

    /** Daemon-backed directory listing for the folder picker — the browser
     * can't reveal absolute paths, the daemon can. Omit `path` for home. */
    fsDirs: (path?: string) =>
        metaRequest<FsListing>(`/fs/dirs${path ? `?path=${encodeURIComponent(path)}` : ''}`),

    /** Promotion nominations: Principles/Cautions recurring across projects. */
    promotions: () =>
        metaRequest<{ candidates: PromotionCandidate[]; skipped: string[] }>('/audit/promotions', {
            method: 'POST',
        }),

    /** The user's approval of a promotion: create the node in the home graph. */
    promoteToHome: (node: NewNode) =>
        metaRequest<GraphNode>('/projects/home/nodes', {
            method: 'POST',
            body: JSON.stringify(node),
        }),

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

    /** SSE stream URL for live graph mutations (project-scoped). */
    eventsUrl: () => `${API_BASE}${projectPrefix}/events`,
}
