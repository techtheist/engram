/**
 * The mock daemon: the demo build's answer to `@/services/api`.
 *
 * `vite.config.ts` aliases that specifier to this file when `ENGRAM_DEMO=1`,
 * so every store, panel and canvas in the pane runs untouched — they cannot
 * tell they are talking to a Map instead of a Rust process. The type
 * annotation is the contract: `EngramApi` is derived from the real HTTP
 * client, so adding an endpoint there breaks the demo build until it is
 * answered for here.
 *
 * Three kinds of method live below:
 *   1. real — served by the in-tab engine, mutations and all;
 *   2. pre-recorded — cortex work (search, claim checks, sweeps) that needs
 *      local models the browser does not have;
 *   3. refused — machine-level gestures (registering a repo, installing the
 *      skill, swapping a model) that only mean something next to a filesystem.
 * Refusals throw a plain-language Error, which the panels already render.
 */
import type { EngramApi, StreamHandlers } from '@/types/api'
import type {
    AuditPage,
    ConfigPreset,
    ExportGraph,
    GraphConfig,
    GraphNode,
    ModelSelection,
    NewNode,
    SystemInfo,
} from '@/types/graph'
import * as engine from './engine'
import { HOME, LAUNCH } from './engine'
import presets from './data/presets.json'
import models from './data/models.json'
import {
    ANSWERED_HINTS,
    BORN_IN,
    DRIFT,
    FS_LISTING,
    HISTORY_MESSAGES,
    HISTORY_SESSIONS,
    PROJECTS,
    SEARCH_HITS,
    claimReport,
} from './data/canned'

export const API_BASE = ''

/** Which graph the pane is looking at — the demo's `/projects/{id}` prefix. */
export function setApiProject(id: string | null): void {
    engine.setActiveProject(id === 'home' ? HOME : LAUNCH)
}

/** A gesture that only exists next to a real filesystem. */
function unavailable(what: string): never {
    throw new Error(`${what} needs the local daemon — this is the browser demo. Run engram-alpha serve to do it for real.`)
}

/**
 * Every answer takes a beat. Not decoration: the pane is written against a
 * backend that is a process away, and components that re-fit the canvas or
 * clear a drawer after a load assume their await actually yields. Resolving
 * synchronously reorders those effects and the demo drifts from the product.
 */
const LATENCY_MS = 40
const ok = <T>(value: T): Promise<T> =>
    new Promise((resolve) => setTimeout(() => resolve(value), LATENCY_MS))

export const api: EngramApi = {
    graph: () => ok({ nodes: engine.readNodes(), edges: [...engine.state().edges.values()] }),

    brief: () => ok(engine.brief()),

    getNode: (id) => ok(engine.readNode(id)),

    createNode: (node) => ok(engine.createNode(node)),

    createEdge: (edge) => ok(engine.createEdge(edge)),

    deleteEdge: (id) => ok(engine.deleteEdge(id)),

    tags: () => ok(engine.tagStats()),

    // Pre-recorded: ranking is the cortex's job and the cortex isn't here.
    search: () => ok(SEARCH_HITS),

    searchHistory: (query: string) => {
        const q = query.toLowerCase()
        const hits = HISTORY_SESSIONS.flatMap((s) =>
            (HISTORY_MESSAGES[s.session] ?? [])
                .filter((m) => m.text.toLowerCase().includes(q))
                .map((m) => ({
                    message_id: m.message_id,
                    session: s.session,
                    session_title: s.title,
                    harness: s.harness,
                    role: m.role,
                    turn: m.turn,
                    timestamp: m.timestamp,
                    snippet: m.text.slice(0, 240),
                    score: 0.7,
                })),
        )
        return ok(hits.slice(0, 8))
    },

    reconfirm: (id) => ok(engine.reconfirm(id)),
    approve: (id) => ok(engine.approve(id)),
    revokeApproval: (id) => ok(engine.revokeApproval(id)),
    pin: (id, value) => ok(engine.pin(id, value)),

    patchNode: (id, patch) => ok(engine.patchNode(id, patch)),
    patchEdge: (id, patch) => ok(engine.patchEdge(id, patch)),
    deleteNode: (id) => ok(engine.deleteNode(id)),

    suspects: () => ok(engine.state().suspects),
    scanConflicts: () => ok({ added: engine.scanSuspects() }),

    // Pre-recorded: NLI verdicts need the local model.
    checkClaim: (text) => ok(claimReport(text)),

    auditConflicts: () =>
        ok({ queued: engine.scanSuspects(), examined: engine.state().nodes.size, truncated: false }),
    auditDuplicates: () =>
        ok({ queued: 0, examined: engine.state().nodes.size, truncated: false }),
    auditAnswered: () => ok(ANSWERED_HINTS),
    auditStale: () =>
        ok(
            engine
                .readNodes()
                .filter((n) => n.stale && n.valid_until == null)
                .slice(0, 6)
                .map((n) => ({
                    node: { id: n.id, type: n.type, title: n.title },
                    trust: n.trust,
                    verdict: n.code_refs.length ? 'reconfirm' : 'isolated',
                    evidence: null,
                    score: 0.42,
                })),
        ),

    drift: () => ok(engine.activeProject() === LAUNCH ? DRIFT : []),

    timeline: (id) => ok(engine.timeline(id)),

    system: () => {
        const p = engine.state(LAUNCH)
        const info: SystemInfo = {
            version: '0.8.1',
            daemon: { pid: 4821, uptime_secs: 7326, repo_root: '/Users/you/code/lantern' },
            store: {
                db: '/Users/you/code/lantern/.engram/graph.tepin',
                size_bytes: 2_310_144,
                backend: 'tepindb',
                nodes: p.nodes.size,
                edges: p.edges.size,
                embedded: p.nodes.size,
                journal_mode: '',
                integrity_ok: true,
                embed_composition: 3,
                embed_composition_current: true,
            },
            model_cached: true,
            reranker: true,
            nli: true,
            models: [
                {
                    name: 'bge-small-en-v1.5',
                    role: 'embeddings — recall (384-dim vectors, hybrid search)',
                    path: '~/.cache/engram/bge-small-en-v1.5',
                    active: true,
                },
                {
                    name: 'jina-reranker-v1-turbo-en',
                    role: 'reranker — search precision (cross-encoder)',
                    path: '~/.cache/engram/jina-reranker-v1-turbo-en',
                    active: true,
                },
                {
                    name: 'deberta-v3-small-tasksource-nli',
                    role: 'NLI — logic (conflict hints, claim checks, Checkup sweeps)',
                    path: '~/.cache/engram/deberta-v3-small-tasksource-nli',
                    active: true,
                },
            ],
            model_selection: true,
            wiring: [
                { agent: 'claude', wired: true, prerename: false },
                { agent: 'codex', wired: true, prerename: false },
                { agent: 'gemini', wired: false, prerename: false },
            ],
        }
        return ok(info)
    },

    /** Canned history: two recorded Lantern sessions the decisions were born in. */
    historyStatus: () =>
        ok({
            enabled: engine.state().config.history?.enabled ?? true,
            open: true,
            search_fallthrough: engine.state().config.history?.search_fallthrough ?? true,
            stats: {
                backend: 'tepindb',
                nodes:
                    HISTORY_SESSIONS.length +
                    Object.values(HISTORY_MESSAGES).reduce((n, m) => n + m.length, 0),
                edges: Object.values(HISTORY_MESSAGES).reduce((n, m) => n + m.length, 0),
                embedded:
                    HISTORY_SESSIONS.length +
                    Object.values(HISTORY_MESSAGES).reduce((n, m) => n + m.length, 0),
            },
        }),
    historyReset: () => {
        HISTORY_SESSIONS.length = 0
        return ok({ reset: true })
    },
    historySessions: () => ok({ sessions: [...HISTORY_SESSIONS] }),
    historyMessages: (sid: string) =>
        ok({ session: sid, messages: HISTORY_MESSAGES[sid] ?? [] }),
    historyDeleteSession: (sid: string) => {
        const i = HISTORY_SESSIONS.findIndex((s) => s.session === sid)
        const removed = i >= 0 ? 1 + (HISTORY_MESSAGES[sid]?.length ?? 0) : 0
        if (i >= 0) HISTORY_SESSIONS.splice(i, 1)
        return ok({ removed })
    },
    bornIn: (id: string) => ok({ born_in: BORN_IN[id] ?? null }),

    config: () => ok(engine.state().config),
    putConfig: (cfg: GraphConfig) => {
        engine.state().config = cfg
        engine.record('updated', 'graph', 'config', 'graph config', null, null)
        return ok(cfg)
    },
    configPresets: () => ok(presets as unknown as ConfigPreset[]),

    getVersion: () => ok({ enabled: true, current: engine.state().version }),
    putVersion: (version) => {
        const p = engine.state()
        const previous = p.version
        p.version = version
        return ok({ previous, current: version })
    },

    /** Rename + bulk retype, for real: every stored row moves with the name. */
    renameType: (from, to) => {
        const p = engine.state()
        let renamed = 0
        for (const [id, n] of p.nodes) {
            if (n.type === from) {
                p.nodes.set(id, { ...n, type: to })
                renamed += 1
            }
        }
        const t = p.config.ontology.types.find((x) => x.name === from)
        if (t) t.name = to
        return ok({ renamed })
    },
    renameVerb: (from, to) => {
        const p = engine.state()
        let renamed = 0
        for (const [id, e] of p.edges) {
            if (e.type === from) {
                p.edges.set(id, { ...e, type: to })
                renamed += 1
            }
        }
        const v = p.config.ontology.verbs.find((x) => x.name === from)
        if (v) v.name = to
        return ok({ renamed })
    },

    installSkill: () => unavailable('Installing the capture skill'),

    models: () => ok(models as unknown as ModelSelection),
    applyModel: () => unavailable('Downloading and swapping a model'),

    projects: () => ok(PROJECTS),
    registerProject: () => unavailable('Registering a project'),
    unregisterProject: () => unavailable('Unregistering a project'),
    fsDirs: () => ok(FS_LISTING),

    promotions: () => {
        const pick = (title: string, type: string): GraphNode =>
            engine.readNodes(LAUNCH).find((n) => n.title.startsWith(title) && n.type === type)!
        const local = pick('A library never leaves the machine', 'Principle')
        const candidates = local
            ? [
                  {
                      node: local,
                      matches: [
                          { project: 'atlas', id: 'x1', title: 'No user data leaves the device', similarity: 0.89 },
                          { project: 'home', id: 'x2', title: 'Local-first by default on every project', similarity: 0.86 },
                      ],
                  },
              ]
            : []
        return ok({ candidates, skipped: ['atlas'] })
    },
    promoteToHome: (node: NewNode) => ok(engine.createNode(node, HOME)),

    resolveSuspect: (id, verdict) => ok({ edge: engine.resolveSuspect(id, verdict) }),

    decay: (ttlDays) => {
        const ids = engine.runDecay(ttlDays)
        return ok({ archived: ids.length, ids })
    },
    decayPreview: (ttlDays = 14) => {
        const ids = engine.decayCandidates(ttlDays).map((n) => n.id)
        return ok({ archived: ids.length, ids })
    },

    audit: (limit = 50, before?: number) => {
        const rows = [...engine.state().journal].sort((a, b) => b.seq - a.seq)
        const from = before != null ? rows.filter((r) => r.seq < before) : rows
        const page: AuditPage = { entries: from.slice(0, limit), total: rows.length }
        return ok(page)
    },

    exportGraph: () => {
        const g: ExportGraph = {
            version: 1,
            nodes: engine.readNodes(),
            edges: [...engine.state().edges.values()],
        }
        return ok(g)
    },

    /** Import works fully: drop a real exported graph in and inspect it here. */
    importGraph: (graph: ExportGraph) => {
        const p = engine.state()
        for (const n of graph.nodes) p.nodes.set(n.id, n)
        for (const e of graph.edges) p.edges.set(e.id, e)
        engine.record('imported', 'graph', 'import', `${graph.nodes.length} nodes`, null, null)
        return ok({ nodes: graph.nodes.length, edges: graph.edges.length })
    },

    /** The daemon's SSE channel, played by an in-page emitter. */
    subscribe(handlers: StreamHandlers): () => void {
        const off = engine.onChange(handlers.message)
        // The demo is never "offline" — the backend is three feet away.
        queueMicrotask(() => handlers.open())
        return off
    },
}
