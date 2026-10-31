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
 *   2. pre-recorded — cortex work (claim checks, sweeps) that needs local
 *      models the browser does not have (search is a plain keyword match
 *      over the live graph instead — see `memoryHits`);
 *   3. refused — machine-level gestures (registering a repo, installing the
 *      skill, swapping a model) that only mean something next to a filesystem.
 * Refusals throw a plain-language Error, which the panels already render.
 */
import type { EngramApi, StreamHandlers } from '@/types/api'
import type {
    AgentSettings,
    AuditEntry,
    AuditPage,
    ConfigPreset,
    CrowdingReport,
    ExportGraph,
    GraphConfig,
    GraphNode,
    ModelSelection,
    NewNode,
    SearchHit,
    SystemInfo,
    UndoReport,
} from '@/types/graph'
import * as engine from './engine'
import { queryTerms, matchesAll, snippet, termHits } from './match'
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
    claimReport,
} from './data/canned'

export const API_BASE = ''

/** The demo build's half of the flag in services/http.ts — the topbar's
 *  connection chip reads it to say "demo" instead of the live dot. */
export const isDemo = true

/** Which graph the pane is looking at — the demo's `/projects/{id}` prefix. */
export function setApiProject(id: string | null): void {
    engine.setActiveProject(id === 'home' ? HOME : LAUNCH)
}

/** A gesture that only exists next to a real filesystem. */
/** What undoing a journal row means, in the core's words (engine.rs undo_effect). */
function undoEffect(r: AuditEntry): string {
    const noun = r.entity === 'edge' ? 'link' : 'note'
    if (r.before == null) return `removes the ${noun}`
    if (r.after == null) return `brings the ${noun} back`
    return `restores the ${noun} as it was`
}

/**
 * A dry-run UndoReport over the rows `pick` selects, newest first: a row whose
 * entity a NEWER row outside the selection touched since is skipped with the
 * core's note, everything else would be undone.
 */
function undoPreview(journal: AuditEntry[], pick: (r: AuditEntry) => boolean): UndoReport {
    const undoable = (r: AuditEntry) =>
        (r.entity === 'node' || r.entity === 'edge') && r.action !== 'imported' && (r.before != null || r.after != null)
    const rows = journal.filter((r) => undoable(r) && pick(r)).sort((a, b) => b.seq - a.seq)
    const chosen = new Set(rows.map((r) => r.seq))
    const report: UndoReport = { dry_run: true, undone: [], skipped: [] }
    for (const r of rows) {
        const item = {
            seq: r.seq,
            action: r.action,
            entity: r.entity,
            entity_id: r.entity_id,
            title: r.title,
            effect: undoEffect(r),
        }
        const newer = journal
            .filter((o) => o.seq > r.seq && o.entity_id === r.entity_id && undoable(o) && !chosen.has(o.seq))
            .sort((a, b) => b.seq - a.seq)[0]
        if (newer) {
            report.skipped.push({ ...item, note: `changed since: #${newer.seq} (${newer.action}) is newer — undo it first` })
        } else {
            report.undone.push(item)
        }
    }
    return report
}

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

/** The demo's `~/.engram/settings.json`: one tab-local variable. */
let demoDefaultAgentProject: string | null = null

function agentSettingsView(): AgentSettings {
    const entry = PROJECTS.find((p) => p.id === demoDefaultAgentProject) ?? null
    return {
        default_agent_project: demoDefaultAgentProject,
        default_agent_project_name: entry?.name ?? null,
        default_agent_project_root: entry?.root ?? null,
    }
}

/**
 * Memory search, keyword edition. Current notes only (`valid_until` unset —
 * superseded generations stay out, as in the daemon). Each query term scores
 * by where it lands — title 3, tags 2, body / code refs / custom fields 1 —
 * and the score blends coverage (share of terms found) with that weight, plus
 * a nudge when the whole multi-word query appears as a phrase. Snippets come
 * from the daemon's own snippet port in `match.ts`.
 */
function memoryHits(query: string, limit: number): SearchHit[] {
    const terms = queryTerms(query)
    if (terms.length === 0) return []
    const phrase = query.trim().toLowerCase()
    const tombstoneTypes = new Set(
        engine.state().config.ontology.types.filter((t) => t.roles.tombstone).map((t) => t.name),
    )
    const scored = engine
        .readNodes()
        .filter((n) => n.valid_until == null)
        .flatMap((n) => {
            const body = n.body ?? ''
            const tags = n.tags.join(' ')
            const refs = n.code_refs.join(' ')
            const custom = Object.values(n.fields ?? {})
                .map((v) => (typeof v === 'string' ? v : JSON.stringify(v)))
                .join(' ')
            const where = [
                { text: n.title, weight: 3 },
                { text: tags, weight: 2 },
                { text: body, weight: 1 },
                { text: refs, weight: 1 },
                { text: custom, weight: 1 },
            ]
            let weight = 0
            let found = 0
            for (const term of terms) {
                const w = where.reduce((sum, f) => sum + (termHits(f.text, [term]) ? f.weight : 0), 0)
                weight += w
                if (w > 0) found += 1
            }
            if (found === 0) return []
            const coverage = found / terms.length
            const bonus = terms.length > 1 && `${n.title} ${body}`.toLowerCase().includes(phrase) ? 0.05 : 0
            const score = Math.min(1, 0.6 * coverage + 0.4 * (weight / (terms.length * 8)) + bonus)
            const hit: SearchHit = {
                id: n.id,
                type: n.type,
                title: n.title,
                snippet: snippet([n.title, body, tags, refs, custom], terms, body || n.title),
                score: Math.round(score * 100) / 100,
                durability: n.durability,
                status: n.status,
                created_at: n.created_at,
                standing: engine.standing(n.id),
            }
            if (tombstoneTypes.has(n.type)) hit.tombstone = true
            return [hit]
        })
    return scored.sort((a, b) => b.score - a.score || b.created_at - a.created_at).slice(0, limit)
}

/**
 * Crowding without vectors: the demo has no embedder, so closeness is a
 * blend of what two notes share — title words (Jaccard over words of 4+
 * letters), tags, and a direct link. The real core measures cosine between
 * stored embeddings against the graph's own top 1% of pairs; this keeps the
 * same shape — counts, heat, subjects, reviewed marks — so the
 * pane's heat view and Checkup section have data. Heat is the note's rank
 * among the crowded ones, so the demo shows the whole ramp, yellow to red.
 */
const DEMO_CLOSE = 0.26
const reviewed = new Map<string, Set<string>>()

function jaccard(a: Set<string>, b: Set<string>): number {
    let shared = 0
    for (const w of a) if (b.has(w)) shared++
    const union = a.size + b.size - shared
    return union ? shared / union : 0
}

function demoCrowding(): CrowdingReport {
    const nodes = engine
        .readNodes()
        .filter((n) => n.valid_until == null && n.type !== 'Anchor' && n.type !== 'Tombstone')
    const words = nodes.map((n) => new Set(n.title.toLowerCase().match(/[a-z]{4,}/g) ?? []))
    const tags = nodes.map((n) => new Set(n.tags ?? []))
    const linked = new Set<string>()
    for (const e of engine.state().edges.values()) {
        linked.add(`${e.from_id} ${e.to_id}`)
        linked.add(`${e.to_id} ${e.from_id}`)
    }
    const near: Array<Array<[number, number]>> = nodes.map(() => [])
    for (let i = 0; i < nodes.length; i++) {
        for (let j = i + 1; j < nodes.length; j++) {
            const sim =
                0.55 * jaccard(words[i]!, words[j]!) +
                0.3 * jaccard(tags[i]!, tags[j]!) +
                (linked.has(`${nodes[i]!.id} ${nodes[j]!.id}`) ? 0.15 : 0)
            if (sim >= DEMO_CLOSE) {
                near[i]!.push([j, sim])
                near[j]!.push([i, sim])
            }
        }
    }
    const counts = near.map((l) => l.length)
    // Rank among the crowded notes: the least crowded of them is pale
    // yellow, the most crowded red.
    const crowded = [...new Set(counts.filter((c) => c > 0))].sort((a, b) => a - b)
    const heatOf = (c: number): number =>
        c === 0 ? 0 : crowded.length < 2 ? 1 : 0.1 + (0.9 * crowded.indexOf(c)) / (crowded.length - 1)
    // Subjects: notes joined by chains of close pairs, at least 2% of the
    // notes (5 at the least) — the core also re-cuts an oversized group at
    // stricter cosines; 45 notes never need it, so the demo stops here.
    const parent = nodes.map((_, i) => i)
    const root = (x: number): number => (parent[x] === x ? x : (parent[x] = root(parent[x]!)))
    near.forEach((list, i) => list.forEach(([j]) => (parent[root(i)] = root(j))))
    const groups = new Map<number, number[]>()
    nodes.forEach((_, i) => groups.set(root(i), [...(groups.get(root(i)) ?? []), i]))
    const min = Math.max(5, Math.ceil(nodes.length * 0.02))
    const marks = reviewed.get(engine.activeProject()) ?? new Set<string>()
    const working = engine.state().version
    const day = 86_400
    const now = Date.now() / 1000
    // Names: the core's class TF-IDF, shortened — words and tags a subject
    // shares, by how rare they are in the rest of the graph.
    const terms = nodes.map((_, i) => new Set([...words[i]!, ...tags[i]!]))
    const df = new Map<string, number>()
    for (const t of terms) for (const w of t) df.set(w, (df.get(w) ?? 0) + 1)
    const nameOf = (g: number[]): string | undefined => {
        const count = new Map<string, number>()
        for (const i of g) for (const w of terms[i]!) count.set(w, (count.get(w) ?? 0) + 1)
        const picked: string[] = []
        const ranked = [...count]
            .filter(([, c]) => c >= 2)
            .map(([w, c]) => [w, (c / g.length) * Math.log(nodes.length / df.get(w)!)] as const)
            .sort((a, b) => b[1] - a[1])
        for (const [w] of ranked) {
            if (picked.length === 3) break
            if (!picked.some((p) => p.startsWith(w.slice(0, 5)) || w.startsWith(p.slice(0, 5)))) picked.push(w)
        }
        return picked.length ? picked.join(' · ') : undefined
    }
    const subjects: CrowdingReport['subjects'] = []
    let hidden = 0
    for (const g of groups.values()) {
        if (g.length < min) continue
        // Most central first: summed closeness inside the group.
        const weight = (i: number): number => near[i]!.filter(([j]) => g.includes(j)).reduce((a, [, s]) => a + s, 0)
        const members = [...g].sort((a, b) => weight(b) - weight(a)).slice(0, min * 4)
        const ns = members.map((i) => nodes[i]!)
        if (ns.every((n) => marks.has(n.id))) {
            hidden++
            continue
        }
        const versions = new Set(ns.map((n) => n.version?.replace(/^v/, '')).filter(Boolean)).size
        const settled = ns.filter((n) => n.version !== working && now - n.created_at > 14 * day).length / ns.length
        const times = ns.map((n) => n.created_at)
        subjects.push({
            name: nameOf(members),
            members: ns.map((n) => n.id),
            worth: {
                score: Math.round(ns.length * (1 + Math.log(Math.max(1, versions))) * (0.3 + 0.7 * settled) * 10) / 10,
                versions,
                settled: Math.round(settled * 100) / 100,
                span_days: Math.floor((Math.max(...times) - Math.min(...times)) / day),
            },
        })
    }
    subjects.sort((a, b) => b.worth.score - a.worth.score)
    return {
        band: DEMO_CLOSE,
        size: nodes.length,
        min_size: min,
        nodes: Object.fromEntries(nodes.map((n, i) => [n.id, { crowding: counts[i]!, heat: heatOf(counts[i]!) }])),
        subjects,
        hidden_reviewed: hidden,
        islands: nodes
            .filter((n) => !marks.has(n.id) && !(n.tags ?? []).length && ![...linked].some((k) => k.startsWith(`${n.id} `)))
            .map((n) => n.id),
    }
}

export const api: EngramApi = {
    graph: () => ok({ nodes: engine.readNodes(), edges: [...engine.state().edges.values()] }),

    brief: () => ok(engine.brief()),

    getNode: (id) => ok(engine.readNode(id)),

    createNode: (node) => ok(engine.createNode(node)),

    createEdge: (edge) => ok(engine.createEdge(edge)),

    deleteEdge: (id) => ok(engine.deleteEdge(id)),

    tags: () => ok(engine.tagStats()),

    // A keyword match over the live graph — the cortex (embeddings, reranker)
    // isn't here, so this is the history box's matcher, ranked: see `memoryHits`.
    search: (query: string, limit = 12) => ok(memoryHits(query, limit)),

    searchHistory: (query: string, limit = 8) => {
        const terms = queryTerms(query)
        const hits = HISTORY_SESSIONS.flatMap((s) =>
            (HISTORY_MESSAGES[s.session] ?? [])
                .filter((m) => matchesAll(m.text, terms))
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
        return ok(hits.slice(0, limit))
    },

    reconfirm: (id) => ok(engine.reconfirm(id)),
    approve: (id) => ok(engine.approve(id)),
    revokeApproval: (id) => ok(engine.revokeApproval(id)),
    pin: (id, value) => ok(engine.pin(id, value)),

    patchNode: (id, patch) => ok(engine.patchNode(id, patch)),
    patchEdge: (id, patch) => ok(engine.patchEdge(id, patch)),
    deleteNode: (id, opts) => ok(engine.deleteNode(id, opts)),

    suspects: () => ok(engine.state().suspects),
    scanConflicts: () => ok({ added: engine.scanSuspects() }),

    // Pre-recorded: a plausible judged history for the NLI scoreboard.
    nliAgreement: () =>
        ok({
            judged: 14,
            with_hint: 12,
            hits: 7,
            false_alarms: 2,
            misses: 1,
            passes: 2,
            agreement: 0.75,
        }),

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

    crowding: () => ok(demoCrowding()),

    reviewMarks: (ids, flag = true) => {
        const project = engine.activeProject()
        const marks = reviewed.get(project) ?? new Set<string>()
        for (const id of ids) {
            if (flag) marks.add(id)
            else marks.delete(id)
        }
        reviewed.set(project, marks)
        return ok({ marked: ids, reviewed: flag })
    },

    timeline: (id) => ok(engine.timeline(id)),

    system: () => {
        const p = engine.state(LAUNCH)
        const info: SystemInfo = {
            version: '0.9.7',
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
            // The 0.9.2 process census — the core plus two light MCP bridges,
            // so the demo shows the Processes section populated.
            processes: {
                core: {
                    pid: 4821,
                    version: '0.9.7',
                    started_at: Math.floor(Date.now() / 1000) - 7326,
                    home: '/Users/you/.engram',
                },
                clients: [
                    {
                        lease_id: 'lease-lantern',
                        pid: 5140,
                        kind: 'mcp-bridge',
                        project: 'lantern',
                        root: '/Users/you/code/lantern',
                        connected_at: Math.floor(Date.now() / 1000) - 3540,
                        last_seen: Math.floor(Date.now() / 1000) - 12,
                    },
                    {
                        lease_id: 'lease-fieldnotes',
                        pid: 6022,
                        kind: 'mcp-bridge',
                        project: 'fieldnotes',
                        root: '/Users/you/code/fieldnotes',
                        connected_at: Math.floor(Date.now() / 1000) - 190,
                        last_seen: Math.floor(Date.now() / 1000) - 65,
                    },
                ],
            },
            models_state: { state: 'loaded', since: Math.floor(Date.now() / 1000) - 7326 },
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

    standing: (id: string) => ok(engine.standing(id)),

    config: () => ok(engine.state().config),
    putConfig: (cfg: GraphConfig) => {
        engine.state().config = cfg
        engine.record('updated', 'graph', 'config', 'graph config', null, null)
        return ok(cfg)
    },
    configPresets: () => ok(presets as unknown as ConfigPreset[]),
    applyPreset: (id: string) => {
        const preset = (presets as unknown as ConfigPreset[]).find((p) => p.id === id)
        if (!preset) return Promise.reject(new Error(`unknown preset "${id}"`))
        const cfg = engine.state().config
        cfg.ontology = structuredClone(preset.config.ontology)
        engine.record('updated', 'graph', 'config', 'graph config', null, null)
        return ok(cfg)
    },

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
    renameField: (from, to) => {
        const p = engine.state()
        let renamed = 0
        for (const [id, n] of p.nodes) {
            const f = n.fields as Record<string, unknown> | null | undefined
            if (f && from in f) {
                const next = { ...f, [to]: f[from] }
                delete next[from]
                p.nodes.set(id, { ...n, fields: next })
                renamed += 1
            }
        }
        const d = (p.config.fields ?? []).find((x) => x.name === from)
        if (d) d.name = to
        return ok({ renamed })
    },

    installSkill: () => unavailable('Installing the capture skill'),

    models: () => ok(models as unknown as ModelSelection),
    applyModel: () => unavailable('Downloading and swapping a model'),

    encryption: () =>
        ok({
            graph: { desired: false, state: 'plaintext' as const, job: { running: false } },
            history: { desired: true, state: null, job: { running: false } },
        }),
    setEncryption: () => unavailable('Encrypting or decrypting a store'),

    // Machine-level settings, served from a tab-local variable: the control
    // works in the demo, nothing persists past the tab.
    settings: () => ok(agentSettingsView()),
    saveSettings: (defaultAgentProject) => {
        if (defaultAgentProject == null || defaultAgentProject === '' || defaultAgentProject === 'home') {
            demoDefaultAgentProject = null
        } else {
            const entry = PROJECTS.find(
                (p) => !p.home && (p.id === defaultAgentProject || p.name === defaultAgentProject),
            )
            if (!entry) throw new Error(`unknown project '${defaultAgentProject}'`)
            demoDefaultAgentProject = entry.id
        }
        return ok(agentSettingsView())
    },

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

    /**
     * Undo (0.9.10) previews in the demo exactly as the core words it — the
     * dry run the pane shows before anything changes — but applying it needs
     * the daemon: the demo's journal is rebuilt from the seed, not replayed.
     */
    auditUndo: (seq: number, dryRun = true) =>
        dryRun ? ok(undoPreview(engine.state().journal, (r) => r.seq === seq)) : unavailable('Undo'),
    auditUndoSession: (sessionId: string, dryRun = true) =>
        dryRun
            ? ok(undoPreview(engine.state().journal, (r) => !!sessionId && r.session_id === sessionId))
            : unavailable('Undo'),

    // Tasks (0.10.0): real — served by the in-tab engine.
    tasks: () => ok(engine.listTasks()),
    createTask: (task) => ok(engine.createTask(task)),
    patchTask: (id, patch) => ok(engine.patchTask(id, patch)),
    deleteTask: (id) => {
        engine.deleteTask(id)
        return ok(undefined)
    },
    orderTasks: (ids) => {
        engine.orderTasks(ids)
        return ok(undefined)
    },

    exportGraph: () => {
        const g: ExportGraph = {
            version: 1,
            nodes: engine.readNodes(),
            edges: [...engine.state().edges.values()],
            tasks: engine.state().tasks,
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
