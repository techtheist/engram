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
    /** Last retrieval surfacing (search/brief). Observability only — trust never reads it. */
    last_seen: number | null
    /** Last deliberate act (update / "Confirm still true") — the unapproved trust anchor. */
    confirmed_at: number | null
    approved_at: number | null
    /** When contradicting evidence landed (judged conflict, drifted refs). */
    demoted_at: number | null
    /** User pin: constant trust (1.0 = pinned), decay and demotion off. */
    trust_override: number | null
    /** Computed by the backend at read time from the timestamps. */
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
    /** Local-NLI triage hint: contradiction | entailment | neutral. A suggestion, never a verdict. */
    nli_label?: string
    nli_score?: number
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
        /** Which storage driver backs this graph: 'sqlite' | 'tepindb'. */
        backend: string
        nodes: number
        edges: number
        embedded: number
        /** SQLite's journal mode; empty on backends without one (redb). */
        journal_mode: string
        integrity_ok: boolean
        embed_composition: number
        embed_composition_current: boolean
    }
    model_cached: boolean
    /** The search precision layer (cross-encoder reranker) is loaded. */
    reranker: boolean
    /** The logic layer (local NLI) is loaded — powers Checkup sweeps and claim checks. */
    nli: boolean
    /** The local cortex, one row per model with its on-disk home. */
    models: { name: string; role: string; path: string; active: boolean }[]
    /** Whether this daemon exposes /models (model selection, PLAN §7A). */
    model_selection: boolean
    wiring: { agent: string; wired: boolean; prerename: boolean }[]
}

/** One provisionable cortex model (PLAN §7A model selection). */
export interface ModelSpec {
    name: string
    base_url: string
    model_file: string
    dim?: number | null
    pooling?: string | null
}

export interface ModelRoleInfo {
    role: 'embedding' | 'reranker' | 'nli'
    /** The model currently in force for this role. */
    active: string
    default: string
    presets: ModelSpec[]
    /** A recorded selection that is not one of the presets. */
    custom?: ModelSpec | null
}

/** GET /models — the machine-level selection, or available:false. */
export interface ModelSelection {
    available: boolean
    fake_embeddings?: boolean
    roles?: ModelRoleInfo[]
}

/** POST /models result: what got applied and what it cost. */
export interface ModelApplyResult {
    role: string
    applied: string
    reembedded_nodes: number
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
    /** created | updated | approved | unapproved | pinned | unpinned | demoted | archived | deleted | imported */
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

/** One node's NLI verdict against a checked claim (POST /claims/check). */
export interface ClaimVerdict {
    id: string
    type: NodeType
    title: string
    trust: number
    stale: boolean
    entailment: number
    neutral: number
    contradiction: number
}

/** The canon's answer to "is this claim true here". All-silent is a gap, not an error. */
export interface ClaimReport {
    claim: string
    supports: ClaimVerdict[]
    contradicts: ClaimVerdict[]
    silent: ClaimVerdict[]
}

/** What a Checkup sweep did (POST /audit/conflicts | /audit/duplicates). */
export interface AuditSweep {
    queued: number
    examined: number
    truncated: boolean
}

/** A nomination that an open Problem may already be answered (POST /audit/answered). */
export interface AnsweredHint {
    problem: SuspectEndpoint
    candidate: SuspectEndpoint
    entailment: number
}

/** One row of the hub's project listing (PLAN §7C): current + home + registry. */
export interface ProjectInfo {
    id: string
    name: string
    root?: string
    db: string
    /** The project this daemon was launched in. */
    current: boolean
    /** The reserved user-level home graph. */
    home: boolean
    /** An engine for this project is open in the daemon. */
    open: boolean
    last_seen?: number
}

/** A registry entry as written by POST /projects. */
export interface ProjectEntry {
    id: string
    name: string
    root: string
    db: string
    last_seen: number
}

export interface PromotionMatch {
    project: string
    id: string
    title: string
    similarity: number
}

/** A Principle/Caution recurring across project graphs — a nomination to
 * promote into the home graph. The user approves; nothing self-applies. */
export interface PromotionCandidate {
    node: GraphNode
    matches: PromotionMatch[]
}

/** One row of the daemon-backed folder picker (GET /fs/dirs). */
export interface FsDir {
    name: string
    path: string
    /** Already carries an .engram graph. */
    engram: boolean
    /** Is a git repository. */
    git: boolean
}

export interface FsListing {
    path: string
    parent: string | null
    home: string | null
    dirs: FsDir[]
}
