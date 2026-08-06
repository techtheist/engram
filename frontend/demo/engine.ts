/**
 * A memory engine that fits in a browser tab.
 *
 * The GitHub Pages demo has no daemon behind it, so this file stands in for
 * one: it holds the graph in memory, computes trust the way
 * `engram-core::policy` does, keeps an audit journal, and emits the same
 * change frames the daemon's SSE stream sends. Everything a visitor does —
 * create, edit, link, approve, pin, judge a conflict, delete — is a real
 * mutation against a real (if small) engine.
 *
 * What it deliberately does NOT do is pretend to run the cortex. Embeddings,
 * the reranker and the NLI model are 100+ MB of local inference; in the demo,
 * search and claim checks return fixed, hand-written payloads (see
 * `data/canned.ts`) and the banner says so. Faking a score is the one lie a
 * memory tool cannot afford.
 *
 * State lives in sessionStorage: edits survive a reload, and closing the tab
 * throws them away.
 */
import type {
    AuditEntry,
    Durability,
    GraphConfig,
    GraphEdge,
    GraphNode,
    NewEdge,
    NewNode,
    NodeStatus,
    PolicyConfig,
    Source,
    SuspectView,
    TimelineEntry,
} from '@/types/graph'
import rawConfig from './data/config.json'
import { HOME_EDGES, HOME_NODES, LANTERN_EDGES, LANTERN_NODES } from './data/lantern'
import type { SeedEdge, SeedNode } from './data/lantern'

/** The launch project's key; '' is what the pane calls the unprefixed graph. */
export const LAUNCH = ''
export const HOME = 'home'

const STORAGE_KEY = 'engram-demo/v1'
const DAY = 86_400

export const nowSecs = (): number => Math.floor(Date.now() / 1000)

/**
 * A stable 12-character id from an authoring key — same shape as the
 * backend's (lowercase base36), so ids look like ids in the pane and the
 * canned payloads can name a node without tracking a literal.
 */
export function synthId(key: string): string {
    let h1 = 0x811c9dc5
    let h2 = 0x01000193
    for (let i = 0; i < key.length; i++) {
        h1 = Math.imul(h1 ^ key.charCodeAt(i), 0x01000193) >>> 0
        h2 = Math.imul(h2 + key.charCodeAt(i) + i, 0x85ebca6b) >>> 0
    }
    const s = (h1.toString(36) + h2.toString(36)).replace(/[^a-z0-9]/g, '')
    return `00${s}`.padEnd(12, '0').slice(0, 12)
}

let idCounter = 0
/** Ids for things created during the session — visibly fresh, never colliding. */
function freshId(): string {
    idCounter += 1
    return synthId(`session:${idCounter}:${Date.now()}`)
}

export interface ProjectState {
    nodes: Map<string, GraphNode>
    edges: Map<string, GraphEdge>
    suspects: SuspectView[]
    journal: AuditEntry[]
    seq: number
    version: string | null
    config: GraphConfig
}

// ---------------------------------------------------------------------------
// Trust — a faithful port of engram-core::policy::trust
// ---------------------------------------------------------------------------

function ramp(start: number, floor: number, window: number, age: number): number {
    if (age <= 0) return start
    if (age >= window) return floor
    return start - (start - floor) * (age / window)
}

function provisionalWindow(d: Durability, p: PolicyConfig): number {
    return d === 'volatile' ? p.volatile_window_days * DAY : p.episodic_window_days * DAY
}

/**
 * Computed at read time from the timestamps, exactly like the daemon: there
 * is no stored confidence to go out of date, and `last_seen` is not an input
 * — retrieval is observability, not evidence.
 */
export function computeTrust(n: GraphNode, p: PolicyConfig, now: number): number {
    if (n.trust_override != null) return Math.min(Math.max(n.trust_override, 0), 1)
    let start: number
    let anchor: number
    let floor: number
    let window: number
    if (n.approved_at != null) {
        start = p.trust_approved
        anchor = n.approved_at
        floor = p.trust_approved_floor
        window = p.approved_window_days * DAY
    } else if (n.confirmed_at != null) {
        start = p.trust_confirmed
        anchor = n.confirmed_at
        floor = p.trust_floor
        window = provisionalWindow(n.durability, p)
    } else {
        start = p.trust_created
        anchor = n.created_at
        floor = p.trust_floor
        window = provisionalWindow(n.durability, p)
    }
    // An open Problem/Intent is live work — age never buries it.
    if (n.status === 'open') return start
    if (n.durability === 'stable') {
        // Stable knowledge doesn't rot with time; only evidence moves it, and
        // then the ramp runs from the evidence, not from the anchor.
        if (n.demoted_at == null) return start
        return ramp(start, floor, window, now - Math.max(n.demoted_at, anchor))
    }
    return ramp(start, floor, window, now - anchor)
}

/** Stamp the read-time fields onto a stored node — what every GET returns. */
export function materialize(n: GraphNode, cfg: GraphConfig, now = nowSecs()): GraphNode {
    const trust = computeTrust(n, cfg.policy, now)
    return { ...n, trust, stale: trust < cfg.policy.stale_trust }
}

// ---------------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------------

function seedNode(s: SeedNode, now: number): GraphNode {
    const at = (d: number | undefined): number | null => (d == null ? null : now - d * DAY)
    return {
        id: synthId(s.key),
        type: s.type,
        title: s.title,
        body: s.body ?? null,
        durability: s.durability,
        source: s.source,
        session_id: s.session ?? (s.source === 'claude' ? 'demo-session' : null),
        created_at: now - s.days * DAY,
        valid_from: null,
        valid_until: at(s.archived),
        status: s.status ?? null,
        last_seen: null,
        confirmed_at: at(s.confirmed),
        approved_at: at(s.approved),
        demoted_at: at(s.demoted),
        trust_override: s.pin ?? null,
        trust: 0,
        stale: false,
        code_refs: s.code_refs ?? [],
        tags: s.tags ?? [],
        version: s.version ?? null,
    }
}

function seedEdge(s: SeedEdge, now: number): GraphEdge {
    return {
        id: synthId(`edge:${s.type}:${s.from}:${s.to}`),
        type: s.type,
        from_id: synthId(s.from),
        to_id: synthId(s.to),
        source: s.source,
        created_at: now - s.days * DAY,
        confidence: null,
        strength: null,
        note: s.note ?? null,
        valid_from: null,
        valid_until: null,
        status: s.status ?? null,
    }
}

/**
 * The journal the graph would have if these notes had been written as they
 * happened: one `created` row per node, newest last, plus the approvals and
 * confirmations that followed. Rebuilt from the seed rather than stored, so
 * it always lines up with the graph the visitor is looking at.
 */
function seedJournal(nodes: GraphNode[], edges: GraphEdge[], version: string | null): AuditEntry[] {
    const rows: Omit<AuditEntry, 'seq'>[] = []
    const origin = (s: Source): string => (s === 'user' ? 'pane' : 'mcp')
    for (const n of nodes) {
        rows.push({
            ts: n.created_at,
            action: 'created',
            entity: 'node',
            entity_id: n.id,
            title: n.title,
            before: null,
            after: { type: n.type, title: n.title, durability: n.durability },
            origin: origin(n.source),
            session_id: n.session_id,
            cwd: '/Users/you/code/lantern',
            pid: 4821,
            version: n.version ?? version,
        })
        if (n.confirmed_at != null) {
            rows.push({
                ts: n.confirmed_at,
                action: 'updated',
                entity: 'node',
                entity_id: n.id,
                title: n.title,
                before: { confirmed_at: null },
                after: { confirmed_at: n.confirmed_at },
                origin: origin(n.source),
                session_id: n.session_id,
                cwd: '/Users/you/code/lantern',
                pid: 4821,
                version: n.version ?? version,
            })
        }
        if (n.approved_at != null) {
            rows.push({
                ts: n.approved_at,
                action: 'approved',
                entity: 'node',
                entity_id: n.id,
                title: n.title,
                before: { approved_at: null },
                after: { approved_at: n.approved_at },
                origin: 'pane',
                session_id: null,
                cwd: '/Users/you/code/lantern',
                pid: 4821,
                version: n.version ?? version,
            })
        }
        if (n.valid_until != null) {
            rows.push({
                ts: n.valid_until,
                action: 'archived',
                entity: 'node',
                entity_id: n.id,
                title: n.title,
                before: { valid_until: null },
                after: { valid_until: n.valid_until },
                origin: 'daemon',
                session_id: null,
                cwd: '/Users/you/code/lantern',
                pid: 4821,
                version: n.version ?? version,
            })
        }
    }
    for (const e of edges) {
        rows.push({
            ts: e.created_at,
            action: 'created',
            entity: 'edge',
            entity_id: e.id,
            title: e.type,
            before: null,
            after: { type: e.type, from_id: e.from_id, to_id: e.to_id },
            origin: origin(e.source),
            session_id: null,
            cwd: '/Users/you/code/lantern',
            pid: 4821,
            version,
        })
    }
    rows.sort((a, b) => a.ts - b.ts)
    return rows.map((r, i) => ({ ...r, seq: i + 1 }))
}

/** Two candidate pairs the scan queued and nobody has judged yet. */
function seedSuspects(now: number): SuspectView[] {
    const end = (key: string, type: string, title: string) => ({ id: synthId(key), type, title })
    return [
        {
            id: synthId('suspect:1'),
            similarity: 0.91,
            created_at: now - 2 * DAY,
            nli_label: 'contradiction',
            nli_score: 0.86,
            nli_direction: 'newer',
            a: end(
                'i-device-mode',
                'Insight',
                'People scroll on phones and paginate on desktop — the mode is a device fact, not a preference',
            ),
            b: end('d-keyboard-first', 'Decision', 'Keyboard-first: every reading action has a binding before it has a button'),
        },
        {
            id: synthId('suspect:2'),
            similarity: 0.88,
            created_at: now - 5 * DAY,
            nli_label: 'entailment',
            nli_score: 0.79,
            a: end('i-nobody-searched', 'Insight', 'Every note we lost was one nobody searched for before writing the second copy'),
            b: end('d-search-before-write', 'Decision', 'Search memory before writing to it, and read the write verdict'),
        },
    ]
}

function buildProject(
    seedNodes: SeedNode[],
    seedEdges: SeedEdge[],
    version: string | null,
    withSuspects: boolean,
): ProjectState {
    const now = nowSecs()
    const nodes = seedNodes.map((s) => seedNode(s, now))
    const edges = seedEdges.map((s) => seedEdge(s, now))
    return {
        nodes: new Map(nodes.map((n) => [n.id, n])),
        edges: new Map(edges.map((e) => [e.id, e])),
        suspects: withSuspects ? seedSuspects(now) : [],
        journal: seedJournal(nodes, edges, version),
        seq: 0,
        version,
        config: structuredClone(rawConfig) as unknown as GraphConfig,
    }
}

function freshWorld(): Map<string, ProjectState> {
    const world = new Map<string, ProjectState>()
    world.set(LAUNCH, buildProject(LANTERN_NODES, LANTERN_EDGES, '0.5.0', true))
    world.set(HOME, buildProject(HOME_NODES, HOME_EDGES, null, false))
    for (const p of world.values()) p.seq = p.journal.length
    return world
}

// ---------------------------------------------------------------------------
// The world, its persistence, and its change stream
// ---------------------------------------------------------------------------

type Frame = { type: string; data: unknown }
type Listener = (raw: string) => void

let world = freshWorld()
let active = LAUNCH
const listeners = new Set<Listener>()

export function setActiveProject(id: string | null): void {
    active = id ?? LAUNCH
}
export function activeProject(): string {
    return active
}

export function state(project = active): ProjectState {
    return world.get(project) ?? world.get(LAUNCH)!
}

export function onChange(fn: Listener): () => void {
    listeners.add(fn)
    return () => listeners.delete(fn)
}

/**
 * Broadcast a change frame — byte-identical in shape to the daemon's SSE
 * payload, so `stores/graph.ts` can't tell the difference. Frames for a
 * project the pane isn't looking at are dropped, the way per-project SSE
 * channels do it.
 */
function emit(type: string, data: unknown, project = active): void {
    if (project !== active) return
    const raw = JSON.stringify({ type, data } satisfies Frame)
    for (const fn of listeners) fn(raw)
}

function persist(): void {
    try {
        const dump: Record<string, unknown> = {}
        for (const [id, p] of world) {
            dump[id] = {
                nodes: [...p.nodes.values()],
                edges: [...p.edges.values()],
                suspects: p.suspects,
                journal: p.journal,
                seq: p.seq,
                version: p.version,
                config: p.config,
            }
        }
        sessionStorage.setItem(STORAGE_KEY, JSON.stringify(dump))
    } catch {
        /* private mode, quota, whatever — the demo just stops surviving reloads */
    }
}

export function restore(): void {
    try {
        const raw = sessionStorage.getItem(STORAGE_KEY)
        if (!raw) return
        const dump = JSON.parse(raw) as Record<string, Record<string, unknown>>
        const next = new Map<string, ProjectState>()
        for (const [id, p] of Object.entries(dump)) {
            next.set(id, {
                nodes: new Map((p.nodes as GraphNode[]).map((n) => [n.id, n])),
                edges: new Map((p.edges as GraphEdge[]).map((e) => [e.id, e])),
                suspects: p.suspects as SuspectView[],
                journal: p.journal as AuditEntry[],
                seq: p.seq as number,
                version: (p.version ?? null) as string | null,
                config: p.config as GraphConfig,
            })
        }
        if (next.has(LAUNCH)) world = next
    } catch {
        /* a corrupt dump is not worth a broken demo — start clean */
    }
}

/** Throw the session's edits away and rebuild the shipped graph. */
export function resetWorld(): void {
    world = freshWorld()
    try {
        sessionStorage.removeItem(STORAGE_KEY)
    } catch {
        /* ignore */
    }
}

// ---------------------------------------------------------------------------
// Journal
// ---------------------------------------------------------------------------

export function record(
    action: string,
    entity: 'node' | 'edge' | 'graph',
    entityId: string,
    title: string | null,
    before: Record<string, unknown> | null,
    after: Record<string, unknown> | null,
    project = active,
): void {
    const p = state(project)
    p.seq += 1
    p.journal.push({
        seq: p.seq,
        ts: nowSecs(),
        action,
        entity,
        entity_id: entityId,
        title,
        before,
        after,
        // Everything the visitor does is a pane gesture — which is the honest
        // label, and also the one the audit filter is most interesting on.
        origin: 'pane',
        session_id: null,
        cwd: '/Users/you/code/lantern',
        pid: 4821,
        version: p.version,
    })
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

export function readNodes(project = active): GraphNode[] {
    const p = state(project)
    const now = nowSecs()
    return [...p.nodes.values()].map((n) => materialize(n, p.config, now))
}

export function readNode(id: string, project = active): GraphNode {
    const p = state(project)
    const n = p.nodes.get(id)
    if (!n) throw new Error(`GET /nodes/${id} → 404 not found`)
    return materialize(n, p.config, nowSecs())
}

function put(n: GraphNode, project = active): GraphNode {
    const p = state(project)
    p.nodes.set(n.id, n)
    const out = materialize(n, p.config, nowSecs())
    emit('node_updated', out, project)
    persist()
    return out
}

export function createNode(input: NewNode, project = active): GraphNode {
    const p = state(project)
    const now = nowSecs()
    const node: GraphNode = {
        id: freshId(),
        type: input.type,
        title: input.title,
        body: input.body ?? null,
        durability: input.durability,
        source: input.source,
        session_id: null,
        created_at: now,
        valid_from: null,
        valid_until: null,
        status: (input.status ?? null) as NodeStatus | null,
        last_seen: null,
        confirmed_at: null,
        approved_at: null,
        demoted_at: null,
        trust_override: null,
        trust: 0,
        stale: false,
        code_refs: input.code_refs ?? [],
        tags: input.tags ?? [],
        version: p.version,
    }
    p.nodes.set(node.id, node)
    record('created', 'node', node.id, node.title, null, { type: node.type, title: node.title }, project)
    const out = materialize(node, p.config, now)
    emit('node_added', out, project)
    persist()
    return out
}

export function patchNode(id: string, patch: Record<string, unknown>, project = active): GraphNode {
    const p = state(project)
    const prev = p.nodes.get(id)
    if (!prev) throw new Error(`PATCH /nodes/${id} → 404 not found`)
    const next: GraphNode = { ...prev, ...(patch as Partial<GraphNode>) }
    // An edit is a deliberate act — that, not being read, is what restores trust.
    next.confirmed_at = nowSecs()
    const before: Record<string, unknown> = {}
    const after: Record<string, unknown> = {}
    for (const k of Object.keys(patch)) {
        before[k] = (prev as unknown as Record<string, unknown>)[k]
        after[k] = (next as unknown as Record<string, unknown>)[k]
    }
    record('updated', 'node', id, next.title, before, after, project)
    return put(next, project)
}

export function deleteNode(id: string, project = active): void {
    const p = state(project)
    const prev = p.nodes.get(id)
    if (!prev) return
    p.nodes.delete(id)
    for (const [eid, e] of [...p.edges]) {
        if (e.from_id === id || e.to_id === id) p.edges.delete(eid)
    }
    p.suspects = p.suspects.filter((s) => s.a.id !== id && s.b.id !== id)
    record('deleted', 'node', id, prev.title, { title: prev.title }, null, project)
    emit('node_deleted', { id }, project)
    persist()
}

export function createEdge(input: NewEdge, project = active): GraphEdge {
    const p = state(project)
    if (!p.nodes.has(input.from_id) || !p.nodes.has(input.to_id)) {
        throw new Error('POST /edges → 400 both endpoints must exist')
    }
    const edge: GraphEdge = {
        id: freshId(),
        type: input.type,
        from_id: input.from_id,
        to_id: input.to_id,
        source: input.source,
        created_at: nowSecs(),
        confidence: null,
        strength: null,
        note: input.note ?? null,
        valid_from: null,
        valid_until: null,
        status: null,
    }
    p.edges.set(edge.id, edge)
    record('created', 'edge', edge.id, edge.type, null, { type: edge.type }, project)
    emit('edge_added', edge, project)
    // Supersession retires the older endpoint wherever the edge comes from —
    // a `replaces` written by hand archives, exactly like a judged verdict.
    if (isSupersession(edge.type, project)) archive(edge.to_id, project)
    persist()
    return edge
}

export function patchEdge(id: string, patch: Record<string, unknown>, project = active): GraphEdge {
    const p = state(project)
    const prev = p.edges.get(id)
    if (!prev) throw new Error(`PATCH /edges/${id} → 404 not found`)
    const next = { ...prev, ...(patch as Partial<GraphEdge>) }
    p.edges.set(id, next)
    record('updated', 'edge', id, next.type, { type: prev.type, status: prev.status }, patch, project)
    emit('edge_updated', next, project)
    persist()
    return next
}

export function deleteEdge(id: string, project = active): void {
    const p = state(project)
    const prev = p.edges.get(id)
    if (!prev) return
    p.edges.delete(id)
    record('deleted', 'edge', id, prev.type, { type: prev.type }, null, project)
    emit('edge_deleted', { id }, project)
    persist()
}

function isSupersession(verb: string, project = active): boolean {
    return state(project).config.ontology.verbs.find((v) => v.name === verb)?.roles.supersession === true
}

function contradictionVerb(project = active): string {
    return (
        state(project).config.ontology.verbs.find((v) => v.roles.contradiction)?.name ??
        'conflicts-with'
    )
}

function supersessionVerb(project = active): string {
    return state(project).config.ontology.verbs.find((v) => v.roles.supersession)?.name ?? 'replaces'
}

function archive(id: string, project = active): void {
    const p = state(project)
    const n = p.nodes.get(id)
    if (!n || n.valid_until != null) return
    const next = { ...n, valid_until: nowSecs() }
    record('archived', 'node', id, n.title, { valid_until: null }, { valid_until: next.valid_until }, project)
    put(next, project)
}

export function reconfirm(id: string, project = active): GraphNode {
    const p = state(project)
    const n = p.nodes.get(id)
    if (!n) throw new Error(`POST /nodes/${id}/reconfirm → 404 not found`)
    const next = { ...n, confirmed_at: nowSecs(), demoted_at: null }
    record('updated', 'node', id, n.title, { confirmed_at: n.confirmed_at }, { confirmed_at: next.confirmed_at }, project)
    return put(next, project)
}

export function approve(id: string, project = active): GraphNode {
    const p = state(project)
    const n = p.nodes.get(id)
    if (!n) throw new Error(`POST /nodes/${id}/approve → 404 not found`)
    const next = { ...n, approved_at: nowSecs(), demoted_at: null }
    record('approved', 'node', id, n.title, { approved_at: n.approved_at }, { approved_at: next.approved_at }, project)
    return put(next, project)
}

export function revokeApproval(id: string, project = active): GraphNode {
    const p = state(project)
    const n = p.nodes.get(id)
    if (!n) throw new Error(`DELETE /nodes/${id}/approve → 404 not found`)
    const next = { ...n, approved_at: null, trust_override: null }
    record('unapproved', 'node', id, n.title, { approved_at: n.approved_at }, { approved_at: null }, project)
    return put(next, project)
}

export function pin(id: string, value: number | null, project = active): GraphNode {
    const p = state(project)
    const n = p.nodes.get(id)
    if (!n) throw new Error(`POST /nodes/${id}/pin → 404 not found`)
    const next = { ...n, trust_override: value }
    record(
        value == null ? 'unpinned' : 'pinned',
        'node',
        id,
        n.title,
        { trust_override: n.trust_override },
        { trust_override: value },
        project,
    )
    return put(next, project)
}

/** The `replaces` chain this node belongs to, oldest generation first. */
export function timeline(id: string, project = active): TimelineEntry[] {
    const p = state(project)
    const back = new Map<string, { from: string; note: string | null }>()
    for (const e of p.edges.values()) {
        if (isSupersession(e.type, project)) back.set(e.to_id, { from: e.from_id, note: e.note })
    }
    const forward = new Map<string, string>()
    for (const [to, { from }] of back) forward.set(from, to)

    // Walk to the oldest generation, then forward through the whole chain.
    let oldest = id
    while (forward.has(oldest)) oldest = forward.get(oldest)!
    const chain: TimelineEntry[] = []
    let cursor: string | undefined = oldest
    while (cursor) {
        const n = p.nodes.get(cursor)
        if (!n) break
        const replacedBy = back.get(cursor)
        chain.push({
            id: n.id,
            type: n.type,
            title: n.title,
            created_at: n.created_at,
            valid_until: n.valid_until ?? undefined,
            replaced_note: replacedBy?.note ?? undefined,
        })
        cursor = replacedBy?.from
    }
    return chain.length > 1 ? chain : []
}

/**
 * Judge a suspected pair. `conflict` records the contradiction and demotes
 * the older side; `replaces` retires it; `dismiss` drops the candidate. The
 * engine never picks — this only happens because somebody clicked.
 */
export function resolveSuspect(
    id: string,
    verdict: 'conflict' | 'replaces' | 'dismiss',
    project = active,
): GraphEdge | null {
    const p = state(project)
    const suspect = p.suspects.find((s) => s.id === id)
    p.suspects = p.suspects.filter((s) => s.id !== id)
    if (!suspect || verdict === 'dismiss') {
        emit('suspects_changed', {}, project)
        persist()
        return null
    }
    const verb = verdict === 'conflict' ? contradictionVerb(project) : supersessionVerb(project)
    const edge = createEdge(
        { type: verb, from_id: suspect.a.id, to_id: suspect.b.id, source: 'user' },
        project,
    )
    if (verdict === 'conflict') {
        const older = p.nodes.get(suspect.b.id)
        if (older) put({ ...older, demoted_at: nowSecs() }, project)
        patchEdge(edge.id, { status: 'active' }, project)
    }
    emit('suspects_changed', {}, project)
    persist()
    return edge
}

/**
 * A candidate sweep with no encoder behind it: token overlap over
 * title + body, which is a crude stand-in for the daemon's embedding
 * similarity but queues plausible pairs so the judging gesture is live.
 */
export function scanSuspects(project = active): number {
    const p = state(project)
    const live = readNodes(project).filter((n) => n.valid_until == null)
    const anchorTypes = new Set(
        p.config.ontology.types.filter((t) => t.roles.anchor).map((t) => t.name),
    )
    const tokens = (n: GraphNode): Set<string> =>
        new Set(
            `${n.title} ${n.body ?? ''}`
                .toLowerCase()
                .split(/[^a-z0-9]+/)
                .filter((w) => w.length > 4),
        )
    const linked = new Set<string>()
    for (const e of p.edges.values()) {
        linked.add(`${e.from_id}|${e.to_id}`)
        linked.add(`${e.to_id}|${e.from_id}`)
    }
    const judged = new Set(p.suspects.map((s) => `${s.a.id}|${s.b.id}`))
    const candidates: SuspectView[] = []
    for (let i = 0; i < live.length; i++) {
        for (let j = i + 1; j < live.length; j++) {
            const a = live[i]!
            const b = live[j]!
            if (anchorTypes.has(a.type) || anchorTypes.has(b.type)) continue
            const key = `${a.id}|${b.id}`
            if (linked.has(key) || judged.has(key)) continue
            const ta = tokens(a)
            const tb = tokens(b)
            let shared = 0
            for (const t of ta) if (tb.has(t)) shared += 1
            const jaccard = shared / (ta.size + tb.size - shared || 1)
            if (jaccard < 0.22) continue
            candidates.push({
                id: freshId(),
                similarity: Math.min(0.99, 0.6 + jaccard),
                created_at: nowSecs(),
                nli_label: 'neutral',
                a: { id: a.id, type: a.type, title: a.title },
                b: { id: b.id, type: b.type, title: b.title },
            })
        }
    }
    candidates.sort((x, y) => y.similarity - x.similarity)
    const added = candidates.slice(0, 5)
    p.suspects = [...p.suspects, ...added]
    emit('suspects_changed', {}, project)
    persist()
    return added.length
}

/** What the decay pass would archive: stale, decaying, past its TTL. */
export function decayCandidates(ttlDays: number, project = active): GraphNode[] {
    const cutoff = nowSecs() - ttlDays * DAY
    return readNodes(project).filter(
        (n) =>
            n.valid_until == null &&
            n.durability !== 'stable' &&
            n.status !== 'open' &&
            n.trust_override == null &&
            n.approved_at == null &&
            n.stale &&
            (n.confirmed_at ?? n.created_at) < cutoff,
    )
}

export function runDecay(ttlDays: number, project = active): string[] {
    const ids = decayCandidates(ttlDays, project).map((n) => n.id)
    for (const id of ids) archive(id, project)
    return ids
}

export function tagStats(project = active): { tag: string; count: number; last_used: number }[] {
    const seen = new Map<string, { count: number; last_used: number }>()
    for (const n of state(project).nodes.values()) {
        for (const t of n.tags) {
            const prev = seen.get(t) ?? { count: 0, last_used: 0 }
            seen.set(t, { count: prev.count + 1, last_used: Math.max(prev.last_used, n.created_at) })
        }
    }
    return [...seen.entries()]
        .map(([tag, v]) => ({ tag, ...v }))
        .sort((a, b) => b.last_used - a.last_used)
}

/**
 * The session brief, generated from whatever the graph currently holds — so
 * a visitor who adds a note and reopens the Memory lens sees their own note
 * in it. Same sections the daemon composes, minus the token budgeting.
 */
export function brief(project = active): string {
    const p = state(project)
    const now = nowSecs()
    const live = readNodes(project).filter((n) => n.valid_until == null)
    const byId = new Map(live.map((n) => [n.id, n]))
    const out: string[] = ['# Engram brief']
    if (p.version) {
        out.push(
            `Current working version: ${p.version} — new notes are stamped with it; call \`set_version\` when the project moves on.`,
        )
    }
    const tags = tagStats(project).slice(0, 7).map((t) => t.tag)
    if (tags.length) {
        out.push(`Recent tags (reuse before inventing new ones): ${tags.join(', ')}`)
    }

    const conflicts = [...p.edges.values()].filter(
        (e) => e.type === contradictionVerb(project) && (e.status == null || e.status === 'active'),
    )
    if (conflicts.length) {
        out.push('', '## Unresolved conflicts')
        for (const e of conflicts) {
            const a = byId.get(e.from_id)
            const b = byId.get(e.to_id)
            if (!a || !b) continue
            out.push(`- "${a.title}" [${a.type} ${a.id}] conflicts with "${b.title}" [${b.type} ${b.id}]`)
        }
    }

    if (p.suspects.length) {
        out.push('', '## Suspected conflicts (unjudged — resolve these)')
        for (const s of p.suspects.slice(0, 8)) {
            out.push(`- "${s.a.title}" vs "${s.b.title}" (similarity ${s.similarity.toFixed(2)})`)
        }
    }

    const open = live.filter((n) => n.status === 'open').sort((a, b) => b.created_at - a.created_at)
    if (open.length) {
        out.push('', '## Open work')
        for (const n of open.slice(0, 10)) out.push(`- ${n.title} [${n.type} ${n.id}]`)
    }

    const excerpt = (n: GraphNode, len: number): string =>
        n.body ? ` — ${n.body.slice(0, len)}${n.body.length > len ? '…' : ''}` : ''

    for (const type of ['Principle', 'Decision', 'Caution']) {
        const rows = live
            .filter((n) => n.type === type)
            .sort((a, b) => b.trust - a.trust || b.created_at - a.created_at)
            .slice(0, 8)
        if (!rows.length) continue
        out.push('', `## ${type}s`)
        for (const n of rows) {
            const pinned = n.trust_override != null ? ' PINNED' : ''
            out.push(`- ${n.title} [${n.type} ${n.id}${pinned}]${excerpt(n, 140)}`)
        }
    }

    const recent = live
        .filter((n) => now - n.created_at < 45 * DAY)
        .sort((a, b) => b.created_at - a.created_at)
        .slice(0, 7)
    if (recent.length) {
        out.push('', '## Recently added')
        for (const n of recent) out.push(`- ${n.title} [${n.type} ${n.id}]${excerpt(n, 100)}`)
    }

    out.push(
        '',
        '---',
        '_Demo brief: composed in your browser from the graph above. The real one is assembled by the daemon and injected at session start._',
    )
    return out.join('\n')
}

restore()
