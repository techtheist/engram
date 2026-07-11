// Mirrors the engram-core wire types (serde output of Node / Edge).

export type NodeType =
    | 'Principle'
    | 'Decision'
    | 'Caution'
    | 'Problem'
    | 'Resolution'
    | 'Insight'
    | 'Intent'
    | 'Anchor'

export type EdgeType =
    | 'about'
    | 'because'
    | 'answers'
    | 'builds-on'
    | 'replaces'
    | 'conflicts-with'
    | 'needs'

export type Durability = 'stable' | 'episodic' | 'volatile'
export type Source = 'user' | 'claude'
export type NodeStatus = 'open' | 'resolved' | 'obsolete'

export interface GraphNode {
    id: string
    type: NodeType
    title: string
    body: string | null
    durability: Durability
    source: Source
    session_id: string | null
    created_at: number
    valid_from: number | null
    valid_until: number | null
    status: NodeStatus | null
    last_seen: number | null
    approved_at: number | null
    /** Computed by the backend at read time from the three timestamps. */
    trust: number
    stale: boolean
    code_refs: string[]
    /** Free-form slice labels (kebab-cased by the backend). */
    tags: string[]
}

/** POST /nodes payload — the pane creates user-sourced nodes. */
export interface NewNode {
    type: NodeType
    title: string
    body?: string
    durability: Durability
    source: Source
    status?: NodeStatus
    code_refs?: string[]
    tags?: string[]
}

/** POST /edges payload. */
export interface NewEdge {
    type: EdgeType
    from_id: string
    to_id: string
    source: Source
    note?: string
}

/** One tag with usage stats (GET /tags), freshest first. */
export interface TagStat {
    tag: string
    count: number
    last_used: number
}

export interface GraphEdge {
    id: string
    type: EdgeType
    from_id: string
    to_id: string
    source: Source
    created_at: number
    confidence: number | null
    strength: number | null
    note: string | null
    valid_from: number | null
    valid_until: number | null
    status: string | null
}

export interface Graph {
    nodes: GraphNode[]
    edges: GraphEdge[]
}

export interface ExportGraph {
    version: number
    nodes: GraphNode[]
    edges: GraphEdge[]
}

export interface ImportSummary {
    nodes: number
    edges: number
}

export type SuspectVerdict = 'conflict' | 'replaces' | 'dismiss'

/** A pending suspected conflict from the local scan (a = newer node). */
export interface SuspectView {
    id: string
    similarity: number
    created_at: number
    a: SuspectEndpoint
    b: SuspectEndpoint
}

export interface SuspectEndpoint {
    id: string
    type: NodeType
    title: string
}

/**
 * A node whose path-shaped code_refs no longer exist in the project
 * (GET /drift): the code moved and the memory didn't — needs review.
 */
export interface DriftEntry {
    id: string
    type: NodeType
    title: string
    missing: string[]
}

/**
 * Daemon-side diagnostics (GET /system): the doctor's facts as structured
 * JSON — binary version, store health, model cache, per-assistant wiring.
 */
export interface SystemInfo {
    version: string
    daemon: {
        pid: number
        uptime_secs: number
        repo_root: string
    }
    store: {
        db: string | null
        size_bytes: number | null
        nodes: number
        edges: number
        embedded: number
        fts: number
        journal_mode: string
        integrity_ok: boolean
        embed_composition: number
        embed_composition_current: boolean
    }
    model_cached: boolean
    wiring: { agent: string; wired: boolean; prerename: boolean }[]
}

/**
 * One generation in a node's `replaces` chain (GET /nodes/{id}/timeline),
 * oldest first. `replaced_note` is the note on the edge that retired this
 * generation — usually the why of the change.
 */
export interface TimelineEntry {
    id: string
    type: NodeType
    title: string
    created_at: number
    valid_until?: number
    replaced_note?: string
}

/**
 * One append-only audit journal row (GET /audit): a node/edge mutation with
 * before/after snapshots plus the writing process's context.
 */
export interface AuditEntry {
    seq: number
    ts: number
    /** created | updated | approved | archived | deleted | imported */
    action: string
    /** node | edge | graph */
    entity: string
    entity_id: string
    /** Display label snapshot — survives the entity's later deletion. */
    title: string | null
    before: Record<string, unknown> | null
    after: Record<string, unknown> | null
    /** pane | mcp | daemon | cli | library */
    origin: string
    session_id: string | null
    cwd: string | null
    pid: number | null
    version: string | null
}

/** One journal page, newest first, with the total row count for progress. */
export interface AuditPage {
    entries: AuditEntry[]
    total: number
}

export interface SearchHit {
    id: string
    type: NodeType
    title: string
    snippet: string
    score: number
    durability: Durability
    status: NodeStatus | null
}
