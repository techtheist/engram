// Mirrors the engram-core wire types (serde output of Node / Edge).

/**
 * Open names since 0.7 (PLAN §7D ontology-as-data): the graph's config
 * defines which type and verb names exist — the pane renders whatever the
 * config declares, so these are strings, not closed unions.
 */
export type NodeType = string
export type EdgeType = string

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
    /** Project version this node was captured at (version tracking). */
    version?: string | null
    /** Custom field values (0.9.0), keyed by the config's FieldDef names. */
    fields?: Record<string, unknown> | null
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
    /** Custom field values (0.9.0). */
    fields?: Record<string, unknown>
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
    /** The working list beside the graph (0.10.0); absent when empty. */
    tasks?: Task[]
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
    /** For contradiction hints: "newer" | "older" — which side likely carries the negation. */
    nli_direction?: string
    a: SuspectEndpoint
    b: SuspectEndpoint
}

export interface SuspectEndpoint {
    id: string
    type: NodeType
    title: string
}

/**
 * The "models nominate, people judge" loop, scored: over judged suspect
 * pairs that carried an NLI hint, how often the hint matched the verdict.
 */
export interface NliAgreement {
    judged: number
    with_hint: number
    /** Hint said contradiction, judge confirmed the pair. */
    hits: number
    /** Hint said contradiction, judge dismissed the pair. */
    false_alarms: number
    /** Hint said entailment/neutral, judge confirmed anyway. */
    misses: number
    /** Hint said entailment/neutral, judge dismissed. */
    passes: number
    /** (hits + passes) / with_hint; absent until a hinted pair is judged. */
    agreement?: number
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
/** The core daemon's own entry in the process census (0.8.8+). */
export interface ProcessCore {
    pid: number
    version: string
    /** Unix seconds the core started — uptime is now minus this. */
    started_at: number
    /** The machine home (`~/.engram`) this core serves. */
    home: string
}

/** One live MCP session's binding (the in-session rebind census). */
export interface SessionBinding {
    session_id: string
    /** Resolved project id ('home' for the home graph). */
    project: string
    name: string
    since: number
    last_seen: number
    /** The end client's name as its bridge relayed it ('Windsurf',
     *  'claude-code'). Absent on cores before 0.9.8. */
    client?: string
    /** Which rung of the binding ladder bound the session: 'roots', 'cwd',
     *  'default-project', 'home' (read-only until the agent binds it), 'db',
     *  'brief' (the agent bound it itself). Absent on cores before 0.9.8. */
    bound_by?: string
}

/** One registered light client (an MCP bridge holding a lease on the core). */
export interface ProcessClient {
    lease_id: string
    pid: number
    /** What kind of light process this is, e.g. 'mcp-bridge'. */
    kind: string
    /** The MCP client behind the bridge (clientInfo.name from its stdio
     *  initialize, e.g. 'claude-code', 'mcp-go'). Absent from older bridges
     *  — the row renders without it. */
    client?: string
    /** The project id the lease is bound to. */
    project: string
    /** Absolute project root — the folder is the client's primary identity. */
    root: string
    connected_at: number
    last_seen: number
}

/** The machine-level settings (`GET/POST /settings`, core daemons only).
 *  A 404 means an older or non-core daemon — the pane hides the control. */
export interface AgentSettings {
    /** Registered project id an otherwise-unbindable agent session binds;
     *  null = the home graph (the default behavior). */
    default_agent_project: string | null
    /** The project's current name/root, resolved daemon-side (null when the
     *  setting is unset or the project left the registry). */
    default_agent_project_name?: string | null
    default_agent_project_root?: string | null
    /** At-rest encryption switches (0.9.0); absent on older daemons. */
    encrypt_graph?: boolean
    encrypt_history?: boolean
}

/** One store's side of `GET /encryption` (0.9.0). */
export interface EncryptionSide {
    /** The machine-global switch. */
    desired: boolean
    /** The store's own recorded state; null while a migration runs (the job
     *  carries the story) or the store is closed. */
    state: 'plaintext' | 'sealing' | 'sealed' | 'unsealing' | null
    job: {
        running: boolean
        done?: number
        total?: number
        error?: string | null
    }
}

/** `GET /encryption`: the two switches, each store's state, job progress. */
export interface EncryptionStatus {
    graph: EncryptionSide
    history: EncryptionSide
}

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
    /**
     * The process census (0.8.8 process-model refactor): the core plus every
     * connected light client. Absent on older daemons — the pane hides the
     * Processes section rather than breaking against a 0.8.7 core.
     */
    processes?: {
        core: ProcessCore
        clients: ProcessClient[]
        /** Live MCP session bindings (in-session rebind: 0.8.9 set_project, brief(project) since 0.8.11): where each session
         *  IS right now, which after an in-session rebind is not what its
         *  bridge's launch lease says. Absent on older cores. */
        sessions?: SessionBinding[]
    }
    /** Whether the cortex models are resident ('loaded') or idle-unloaded
     *  ('unloaded_idle'), and since when. Absent on older daemons. */
    models_state?: {
        state: string
        since: number
    }
}

/** One provisionable cortex model (PLAN §7A model selection). */
export interface ModelSpec {
    name: string
    base_url: string
    model_file: string
    dim?: number | null
    pooling?: string | null
    /** One line the picker shows: what this model trades (0.9.9). */
    description?: string | null
    /** NLI only: "laya" = a Laya typed-decision model directory. */
    layout?: string | null
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
    /** created | updated | approved | unapproved | pinned | unpinned | demoted | archived | deleted | imported | merged */
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
    /** Rows one operation wrote (a merge, a delete and its links, an undo)
     *  share this; undoing any of them undoes the whole operation (0.9.10). */
    op_id?: string | null
    /** The `undone` row that reverted this one, while that undo stands. */
    undone_by?: number | null
}

/** One journal row an undo handled (0.9.10). */
export interface UndoItem {
    seq: number
    action: string
    entity: string
    entity_id: string
    title: string | null
    /** "removes the note", "restores the note as it was", … */
    effect: string
    /** Why it was skipped, or what else the undo touched. */
    note?: string | null
}

/** What an undo did — or, on a dry run, would do. */
export interface UndoReport {
    dry_run: boolean
    undone: UndoItem[]
    skipped: UndoItem[]
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
    /** When the node was captured — carried so a hit can be dated or time-ordered without a second read (0.8.7). */
    created_at: number
    /** The hit plays the graph's tombstone role — a removal record, not live canon; present only when true (0.9.4). */
    tombstone?: boolean
    /** How far the hit can be relied on (0.9.10) — absent from older daemons. */
    standing?: Standing
}

/**
 * Grounded answers (0.9.10): one verdict on how far a note can be relied on,
 * composed by the daemon from the tombstone role, a live `replaces`, an open
 * judged conflict, staleness and the trust ladder — worst first.
 */
export interface Standing {
    verdict: 'tombstone' | 'superseded' | 'contested' | 'stale' | 'canon' | 'confirmed' | 'unverified'
    /** What it means for the reader, in plain words. */
    reason: string
    /** The note that replaces or contests this one. */
    other?: string
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
    /**
     * Tombstone-role hits (0.9.2): the claim lands on knowledge a person
     * deliberately removed. Sorted out by role before the NLI runs — the
     * retrieval score rides in `entailment`, the other columns are zero.
     * Omitted from the wire when empty.
     */
    retracted?: ClaimVerdict[]
    silent: ClaimVerdict[]
}

/**
 * Crowding (0.11.0, GET /crowding): how many notes sit inside this graph's
 * close band around each note (the cosine only its own top 1% of pairs
 * reach), heat 0..1 against the graph's crowded end, and the unreviewed
 * subjects — the sleep queue — most worth consolidating first.
 */
/** Why a subject ranks where it does: size × (1 + ln versions) × (0.3 + 0.7 × settled). */
export interface SubjectWorth {
    score: number
    versions: number
    /** Share of members from a past version and older than two weeks. */
    settled: number
    span_days: number
}

export interface CrowdingReport {
    band: number
    size: number
    /** A subject's size floor on this graph (2% of its notes, at least 5). */
    min_size: number
    nodes: Record<string, { crowding: number; heat: number }>
    /** Unreviewed subjects, most worth consolidating first; members most central first. */
    /** `name`: two or three terms its titles and tags share and the rest of the graph rarely uses. */
    subjects: { name?: string; members: string[]; worth: SubjectWorth }[]
    hidden_reviewed: number
    /** Unreviewed islands: notes no edge and no tag reaches. */
    islands: string[]
}

/** POST /review/marks — the ids that were (un)marked. */
export interface ReviewMarkResult {
    marked: string[]
    reviewed: boolean
}

/** What a Checkup sweep did (POST /audit/conflicts | /audit/duplicates). */
export interface AuditSweep {
    queued: number
    examined: number
    truncated: boolean
    /**
     * Close pairs the sweep passed over, each counted once (0.9.11) — absent
     * from older daemons.
     */
    already_linked?: number
    already_raised?: number
    inactive?: number
    /**
     * Pairs an earlier run judged and passed — unchanged notes, same judge
     * (0.9.11): read once, never again, so a truncated sweep continues.
     */
    already_judged?: number
}

/** A nomination that an open Problem may already be answered (POST /audit/answered). */
export interface AnsweredHint {
    problem: SuspectEndpoint
    candidate: SuspectEndpoint
    entailment: number
    /** Already connected by this (non-answer) verb — ranked at a penalty. */
    existing_link?: string
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

// ---------------------------------------------------------------------------
// Per-graph configuration (PLAN §7D) — mirrors engram_core::config.
// ---------------------------------------------------------------------------

export interface TypeRoles {
    /** Open/resolved lifecycle: lives in the worklist, never decays while open. */
    worklist: boolean
    /** A code subject: carries code_refs, excluded from the conflict scan. */
    anchor: boolean
    /** Ranking prior added in search's trust boost (0 = none). */
    rank_prior: number
    /** May the pane accent/highlight this type (false renders muted). */
    highlight: boolean
    /** Binds to the current working version when tracking is on. */
    versioned: boolean
    /** A deletion marker: records deliberately removed knowledge (0.9.0). */
    tombstone: boolean
}

export interface BriefSectionCfg {
    show: boolean
    cap: number
    excerpt: number
}

export interface BriefToggle {
    show: boolean
    cap: number
}

export interface TypeDef {
    name: string
    /** The one color input (0..360) — everything else is derived. */
    hue: number
    /** The thought this type captures — the teaching line. */
    thought: string
    durability: Durability
    roles: TypeRoles
    /** This type's canon section in the brief. */
    brief: BriefSectionCfg
}

/** What a custom field's value must be (0.9.0). */
export type FieldKind = 'text' | 'number' | 'bool' | 'date' | 'enum' | 'url'

/** One user-defined custom field on notes — mirrors engram_core::config::FieldDef. */
export interface FieldDef {
    /** Storage + wire key: lowercase snake_case, unique, never a built-in name. */
    name: string
    /** Pane display label; empty = render the name. */
    label: string
    kind: FieldKind
    /** The allowed values when kind is 'enum'. */
    values: string[]
    /** Refuse writes of applicable types that omit this field. */
    required: boolean
    /** Type names this field applies to; empty = every type. */
    applies_to: string[]
    /** Join the search index (embedding + keyword; tepin-backed graphs). */
    indexed: boolean
    /** Render `name: value` on this note's brief lines. */
    show_in_brief: boolean
}

export interface VerbRoles {
    /** Creating it archives the older endpoint and chains history. */
    supersession: boolean
    /** A judged one demotes trust and feeds the conflict worklist. */
    contradiction: boolean
    /** The reason-edge (its absence is what the checkup flags). */
    reason: boolean
    /** Closes worklist nodes. */
    answer: boolean
    /** A live dependency. */
    dependency: boolean
    /** The source holds what the target holds; unset = derived (0.9.9). */
    inherits?: boolean | null
}

export interface VerbDef {
    name: string
    /** A worked example ("Decision because Principle"). */
    reads_as: string
    roles: VerbRoles
}

export interface OntologyConfig {
    preset: string
    types: TypeDef[]
    verbs: VerbDef[]
}

export interface PolicyConfig {
    trust_created: number
    trust_confirmed: number
    trust_approved: number
    trust_approved_floor: number
    trust_floor: number
    stale_trust: number
    episodic_window_days: number
    volatile_window_days: number
    approved_window_days: number
    decay_ttl_days: number
    duplicate_similarity: number
    conflict_suspect_similarity: number
    conflict_nli_gate: number | null
    warn_similarity: number
    nli_sweep_min_confidence: number
    /** Post-rerank hits under this score are trimmed before delivery. */
    delivery_floor: number
    knee_cliff: number | null
    weak_line_quantile: number
    weak_line_probes: number
    /** Top-score under which the search verdict is "weak". */
    weak_evidence_top: number
    /** Let a mature graph refit its conflict threshold from judged history. */
    auto_tune: boolean
    /**
     * Rank positions each additional same-session hit is demoted at the
     * delivery cut — session-diverse delivery. 0 disables.
     */
    session_diversity_demote: number
    /** Endorsed-first order among near-identical hits at or above this
     *  cosine; null = off (0.9.10). */
    twin_trust_order?: number | null
}

export interface BriefConfig {
    total_chars: number
    tags: BriefToggle
    conflicts: BriefToggle
    suspects: BriefToggle
    recent: BriefSectionCfg
    open: BriefSectionCfg
    home_reserve: number
    /** Teach the graph's ontology at the top of the brief. */
    ontology: BriefToggle
    /** Notes left for the next session (tag `handoff`), placed first. */
    handoff?: BriefSectionCfg
    /** Open work stamped with the working version (0.9.9). */
    cycle?: BriefSectionCfg
    /** Entries carry a body excerpt; false = titles only (0.9.9). */
    bodies?: boolean
    /** Canon section order: 'endorsed' (default) or 'connected' (0.9.9). */
    canon_order?: 'endorsed' | 'connected'
    /** The `## Tasks` section (0.10.0). */
    tasks?: BriefTasksCfg
}

/** The brief's task section (0.10.0). */
export interface BriefTasksCfg {
    show: boolean
    /** Most active top-level tasks listed (a group counts once). */
    cap: number
    /** Recently done titles riding along (0 = none). */
    recent_done: number
    /** Briefed sessions without a change before an open task reads idle (0 = never). */
    idle_sessions: number
}

/** Per-harness ingestion switches for the history layer (0.8.4). */
export interface HarnessToggles {
    claude_code: boolean
    codex: boolean
    gemini: boolean
    opencode: boolean
    kilo: boolean
    antigravity: boolean
    /** IBM Bob — one switch for both surfaces (IDE + BobShell). */
    bob: boolean
}

/** The history layer's knobs — every one a pane gesture. */
export interface HistoryConfig {
    enabled: boolean
    harnesses: HarnessToggles
    exclude_paths: string[]
    /** May history answer as the sectioned search fall-through. */
    search_fallthrough: boolean
}

/** One recorded session (GET /history/sessions). */
export interface HistorySession {
    node_id: string
    session: string
    title: string
    harness?: string
    started: number
    ended?: number
    messages: number
    version?: string
}

/** One recorded turn (GET /history/sessions/{sid}). */
export interface HistoryMessage {
    message_id: string
    role: string
    turn?: number
    timestamp: number
    text: string
}

/** One history-layer search hit (GET /search?scope=history). */
export interface HistoryHit {
    message_id: string
    session: string
    session_title: string
    harness?: string
    role: string
    turn?: number
    timestamp: number
    snippet: string
    score: number
    /** Earlier statements of the same thing, newest-first — present only under `order: "recent"` (0.8.7). */
    prior?: HistoryHit[]
}

/** Where a curated node was born (GET /nodes/{id}/born-in). */
export interface BornIn {
    session: string
    message_id: string
    timestamp: number
    turn?: number
}

/** GET /history — the settings section's status line. */
export interface HistoryStatus {
    enabled: boolean
    open: boolean
    search_fallthrough: boolean
    stats: { backend: string; nodes: number; edges: number; embedded: number } | null
}

export interface GraphConfig {
    ontology: OntologyConfig
    policy: PolicyConfig
    brief: BriefConfig
    versioning: { enabled: boolean }
    history: HistoryConfig
    /** User-defined custom fields on notes (0.9.0). */
    fields: FieldDef[]
    /** The working list beside the graph (0.10.0); absent = on. */
    tasks?: { enabled: boolean }
}

/** One curated ontology template (GET /config/presets). */
export interface ConfigPreset {
    id: string
    name: string
    description: string
    config: GraphConfig
}

/** A stale node triaged against the live canon (POST /audit/stale). */
export interface StaleTriage {
    node: SuspectEndpoint
    trust: number
    /** reconfirm | contradicted | isolated */
    verdict: string
    evidence?: SuspectEndpoint | null
    score: number
}

// ---------------------------------------------------------------------------
// Tasks (0.10.0): the working list beside the graph — never searched, never
// knowledge. One level of groups; rules settle on read and on set_version.
// ---------------------------------------------------------------------------

export type TaskStatus = 'active' | 'delayed' | 'backlog' | 'done' | 'dropped'

export interface Task {
    id: string
    title: string
    body?: string
    status: TaskStatus
    /** The group this task belongs to (one level only). */
    parent?: string
    /** Memory node ids this task is about — one-way. */
    refs?: string[]
    /** Canonical wake rule: a version, `>version`, an ISO instant, or `node:<id>`. */
    wake?: string
    /** Canonical close rule, same forms. */
    close?: string
    position: number
    version?: string
    closed_version?: string
    created_at: number
    updated_at: number
    closed_at?: number
    session_id?: string
    touched_session?: string
    origin?: string
    /** The last automatic change and why. */
    auto?: string
}

/** A task as listings return it. */
export interface TaskView extends Task {
    /** Groups: [closed children, all children]. */
    progress?: [number, number]
    /** Open task created under an older working version. */
    carried_from?: string
    idle_sessions?: number
}

export interface NewTask {
    title: string
    body?: string
    status?: TaskStatus
    parent?: string
    refs?: string[]
    wake?: string
    close?: string
}

/** A pane patch: absent = unchanged; '' on body/parent/wake/close clears. */
export interface TaskPatch {
    title?: string
    body?: string
    status?: TaskStatus
    parent?: string
    refs?: string[]
    wake?: string
    close?: string
    position?: number
    progress?: string
}

export interface TaskWrite {
    id?: string
    created: boolean
    matched?: { id: string; title: string; status: TaskStatus }
    auto_close?: string
    error?: string
}
