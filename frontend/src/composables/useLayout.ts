import dagre from '@dagrejs/dagre'
import {
    forceCollide,
    forceLink,
    forceManyBody,
    forceSimulation,
    forceX,
    forceY,
    type SimulationLinkDatum,
    type SimulationNodeDatum,
} from 'd3-force'
import type { LayoutMode } from '@/stores/layout'
import type { GraphEdge, GraphNode } from '@/types/graph'

export interface XY {
    x: number
    y: number
}

const NODE_W = 340
const NODE_H = 150
const NODE_SEP = 100 // vertical gap between nodes within a dagre rank
const RANK_SEP = 80 // horizontal gap between dagre ranks — matched to COMP_GAP_X
const COMP_GAP_X = 80 // horizontal gap between packed components
const COMP_GAP_Y = 90 // vertical gap between packed rows

/**
 * Split the nodes into connected components (an edgeless node is its own
 * singleton component) via union-find.
 */
function components(nodes: GraphNode[], edges: GraphEdge[]): GraphNode[][] {
    const parent = new Map<string, string>()
    for (const n of nodes) parent.set(n.id, n.id)

    const find = (x: string): string => {
        let r = x
        while (parent.get(r) !== r) r = parent.get(r)!
        return r
    }
    for (const e of edges) {
        if (parent.has(e.from_id) && parent.has(e.to_id)) parent.set(find(e.from_id), find(e.to_id))
    }

    const groups = new Map<string, GraphNode[]>()
    for (const n of nodes) {
        const root = find(n.id)
        let arr = groups.get(root)
        if (!arr) {
            arr = []
            groups.set(root, arr)
        }
        arr.push(n)
    }
    return [...groups.values()]
}

interface Placed {
    positions: Map<string, XY>
    w: number
    h: number
}

/** Lay out one component (dagre, L→R), with positions normalized to (0,0). */
function layoutComponent(comp: GraphNode[], edges: GraphEdge[]): Placed {
    const positions = new Map<string, XY>()
    if (comp.length === 1) {
        positions.set(comp[0]!.id, { x: 0, y: 0 })
        return { positions, w: NODE_W, h: NODE_H }
    }

    const ids = new Set(comp.map((n) => n.id))
    const g = new dagre.graphlib.Graph()
    g.setGraph({ rankdir: 'LR', nodesep: NODE_SEP, ranksep: RANK_SEP })
    g.setDefaultEdgeLabel(() => ({}))
    for (const n of comp) g.setNode(n.id, { width: NODE_W, height: NODE_H })
    for (const e of edges) {
        if (ids.has(e.from_id) && ids.has(e.to_id)) g.setEdge(e.from_id, e.to_id)
    }
    dagre.layout(g)

    let minX = Infinity
    let minY = Infinity
    let maxX = -Infinity
    let maxY = -Infinity
    for (const n of comp) {
        const dn = g.node(n.id)
        const x = dn.x - NODE_W / 2
        const y = dn.y - NODE_H / 2
        positions.set(n.id, { x, y })
        minX = Math.min(minX, x)
        minY = Math.min(minY, y)
        maxX = Math.max(maxX, x + NODE_W)
        maxY = Math.max(maxY, y + NODE_H)
    }
    for (const [id, p] of positions) positions.set(id, { x: p.x - minX, y: p.y - minY })
    return { positions, w: maxX - minX, h: maxY - minY }
}

/** A horizontal span of the packing skyline; everything below `y` is occupied. */
interface Segment {
    x: number
    w: number
    y: number
}

/**
 * Skyline packing: components (tallest first) land at the lowest, then
 * leftmost, point of the skyline where they fit inside `width`. Unlike shelf
 * rows — whose height is set by their tallest member — this fills the space
 * to the *right* of a tall linked web with stacked smaller components instead
 * of opening a fresh row beneath it.
 */
function packSkyline(
    comps: Placed[],
    width: number,
    gapX = COMP_GAP_X,
    gapY = COMP_GAP_Y,
): Map<string, XY> {
    const result = new Map<string, XY>()
    let skyline: Segment[] = [{ x: 0, w: width, y: 0 }]

    const yAt = (x: number, w: number): number => {
        let y = 0
        for (const s of skyline) {
            if (s.x + s.w <= x || s.x >= x + w) continue
            y = Math.max(y, s.y)
        }
        return y
    }

    for (const c of comps) {
        const w = c.w + gapX
        const h = c.h + gapY

        let best = { x: 0, y: yAt(0, w) }
        for (const s of skyline) {
            if (s.x + w > width) continue
            const y = yAt(s.x, w)
            if (y < best.y || (y === best.y && s.x < best.x)) best = { x: s.x, y }
        }

        for (const [id, p] of c.positions) result.set(id, { x: best.x + p.x, y: best.y + p.y })

        // Carve the covered span out of the skyline and cap it at the new top.
        const next: Segment[] = []
        for (const s of skyline) {
            const cutStart = Math.max(s.x, best.x)
            const cutEnd = Math.min(s.x + s.w, best.x + w)
            if (cutEnd <= cutStart) {
                next.push(s)
                continue
            }
            if (s.x < cutStart) next.push({ x: s.x, w: cutStart - s.x, y: s.y })
            if (s.x + s.w > cutEnd) next.push({ x: cutEnd, w: s.x + s.w - cutEnd, y: s.y })
        }
        next.push({ x: best.x, w, y: best.y + h })
        next.sort((a, b) => a.x - b.x)
        skyline = next
    }
    return result
}

/**
 * Skyline: directed layered layout, component-aware. Each connected component
 * flows left→right (a reasoning chain reads well as ranks). Components are
 * then skyline-packed — tallest first, so the main web leads and smaller
 * pieces fill the space to its right. That keeps the "connected node sits to
 * the right" flow while stopping many edgeless/root nodes from marching
 * straight down.
 */
function layoutSkyline(
    nodes: GraphNode[],
    edges: GraphEdge[],
): Map<string, XY> {
    const comps = components(nodes, edges)
        .map((c) => layoutComponent(c, edges))
        .sort((a, b) => b.h - a.h || b.w - a.w)
    return packSkyline(comps, fieldWidth(comps, COMP_GAP_X, COMP_GAP_Y))
}

/**
 * Recommended field width: one card plus its margins ≈ one near-square grid
 * cell, so the field targets a square in cell counts — √cells columns. A box
 * taller than the square is fine; the skyline fills the space beside it.
 */
function fieldWidth(comps: Placed[], gapX: number, gapY: number): number {
    const cellW = NODE_W + gapX
    const cellH = NODE_H + gapY
    const widest = comps.reduce((m, c) => Math.max(m, c.w), NODE_W)
    const cells = comps.reduce(
        (sum, c) => sum + Math.ceil(c.w / cellW) * Math.ceil(c.h / cellH),
        0,
    )
    return Math.max(widest + gapX, Math.ceil(Math.sqrt(cells)) * cellW)
}

/** Shift positions to a (0,0) origin and measure the box — one island. */
function normalized(positions: Map<string, XY>): Placed {
    let minX = Infinity
    let minY = Infinity
    let maxX = -Infinity
    let maxY = -Infinity
    for (const p of positions.values()) {
        minX = Math.min(minX, p.x)
        minY = Math.min(minY, p.y)
        maxX = Math.max(maxX, p.x + NODE_W)
        maxY = Math.max(maxY, p.y + NODE_H)
    }
    const out = new Map<string, XY>()
    for (const [id, p] of positions) out.set(id, { x: p.x - minX, y: p.y - minY })
    return { positions: out, w: maxX - minX, h: maxY - minY }
}

// --- Nebula: force-directed particle cloud --------------------------------

interface NebulaNode extends SimulationNodeDatum {
    id: string
}

/** Half-diagonal of a card + margin: circles that cover the whole rectangle,
 * so no two cards overlap at any approach angle. */
const COLLIDE_R = 195
/** Resting edge length — roughly one card width plus a gap. */
const LINK_DISTANCE = 430
/** How hard an edge pulls toward its resting length. Tuned with FLOW_STRENGTH
 * on the dogfood graph: 0.8/0.7 keeps ~87% left→right flow while pulling
 * singly-connected nodes ~10% closer to their cluster. */
const BOND_STRENGTH = 0.8
const CHARGE = -900
/** Beyond this, repulsion stops — far components drift, they don't explode. */
const CHARGE_MAX_DISTANCE = 2200
const CENTER_PULL = 0.03
/** d3's default alpha schedule converges in ~300 ticks; run it synchronously. */
const NEBULA_TICKS = 300
/** Flow gravity: how far right of its source a target wants to sit, and how
 * hard the nudge is. Node handles are out-right / in-left, so edges read
 * left→right when this wins. */
const FLOW_GAP = 300
/** Balanced against BOND_STRENGTH (see there); zero card overlaps. */
const FLOW_STRENGTH = 0.7

/**
 * Directed-flow gravity: every edge nudges its source to the left of its
 * target, so the layered reading order emerges *inside* the physics instead
 * of being imposed as ranks. Registered after forceLink, which has already
 * resolved the links' endpoints into node objects.
 */
function forceFlow(links: SimulationLinkDatum<NebulaNode>[]) {
    function force(alpha: number): void {
        for (const l of links) {
            const s = l.source
            const t = l.target
            if (typeof s !== 'object' || typeof t !== 'object') continue
            const err = (t.x ?? 0) - (s.x ?? 0) - FLOW_GAP
            if (err >= 0) continue
            const push = err * FLOW_STRENGTH * alpha
            s.vx = (s.vx ?? 0) + push / 2
            t.vx = (t.vx ?? 0) - push / 2
        }
    }
    force.initialize = (): void => {}
    return force
}

/**
 * The shared physics: edges pull like springs, nodes repel like charged
 * particles and collide on their bounding circles, flow gravity drifts
 * sources left of targets, and an asymmetric center pull squeezes the cloud
 * toward the same recommended box shape Skyline targets (√n columns of card
 * cells) without imposing any grid. `pin` fixes nodes in place (fx/fy); the
 * cloud settles around them. Deterministic: d3-force's phyllotaxis seeding
 * and LCG jiggle, no Math.random.
 */
function simulate(
    nodes: GraphNode[],
    edges: GraphEdge[],
    pin?: Map<string, XY>,
): Map<string, XY> {
    if (nodes.length === 0) return new Map()

    const simNodes: NebulaNode[] = nodes.map((n) => {
        const o = pin?.get(n.id)
        return o ? { id: n.id, x: o.x, y: o.y, fx: o.x, fy: o.y } : { id: n.id }
    })
    const ids = new Set(nodes.map((n) => n.id))
    const links: SimulationLinkDatum<NebulaNode>[] = edges
        .filter((e) => ids.has(e.from_id) && ids.has(e.to_id))
        .map((e) => ({ source: e.from_id, target: e.to_id }))

    const cellW = NODE_W + COMP_GAP_X
    const cellH = NODE_H + COMP_GAP_Y
    const cols = Math.max(1, Math.ceil(Math.sqrt(nodes.length)))
    const rows = Math.max(1, Math.ceil(nodes.length / cols))
    const aspect = (cols * cellW) / (rows * cellH)

    const sim = forceSimulation(simNodes)
        .force(
            'link',
            forceLink<NebulaNode, SimulationLinkDatum<NebulaNode>>(links)
                .id((n) => n.id)
                .distance(LINK_DISTANCE)
                .strength(BOND_STRENGTH),
        )
        .force('charge', forceManyBody().strength(CHARGE).distanceMax(CHARGE_MAX_DISTANCE))
        .force('collide', forceCollide(COLLIDE_R).strength(0.9))
        .force('flow', forceFlow(links))
        // The stronger vertical pull flattens the cloud toward the target
        // box's aspect (card cells are wider than tall).
        .force('x', forceX(0).strength(CENTER_PULL))
        .force('y', forceY(0).strength(Math.min(CENTER_PULL * aspect, 0.12)))
        .stop()
    for (let i = 0; i < NEBULA_TICKS; i++) sim.tick()

    const result = new Map<string, XY>()
    for (const n of simNodes) result.set(n.id, { x: n.x ?? 0, y: n.y ?? 0 })
    return result
}

/** Nebula: one global physics cloud. Hand-placed nodes are pinned. */
function layoutNebula(
    nodes: GraphNode[],
    edges: GraphEdge[],
    overrides: Map<string, XY>,
): Map<string, XY> {
    return simulate(nodes, edges, overrides)
}

// --- Archipelago: community islands, physics inside ------------------------

/** Components bigger than this get split further by community. */
const COMMUNITY_MIN = 12
const LP_ROUNDS = 10
/** Water between islands — wider than the in-island spacing on purpose. */
const ISLAND_GAP_X = 234
const ISLAND_GAP_Y = 252

/**
 * Label propagation, deterministic (fixed sweep order, lexicographic tie
 * break, no randomness) and anchor-seeded: an Anchor never adopts a
 * neighbor's label, so `about` neighborhoods condense around their anchors.
 */
function communities(comp: GraphNode[], edges: GraphEdge[]): GraphNode[][] {
    const ids = new Set(comp.map((n) => n.id))
    const adj = new Map<string, string[]>(comp.map((n) => [n.id, []]))
    for (const e of edges) {
        if (!ids.has(e.from_id) || !ids.has(e.to_id)) continue
        adj.get(e.from_id)!.push(e.to_id)
        adj.get(e.to_id)!.push(e.from_id)
    }
    const label = new Map(comp.map((n) => [n.id, n.id]))
    const order = [...comp].sort((a, b) => a.id.localeCompare(b.id))
    for (let round = 0; round < LP_ROUNDS; round++) {
        let changed = false
        for (const n of order) {
            if (n.type === 'Anchor') continue
            const counts = new Map<string, number>()
            for (const m of adj.get(n.id)!) {
                const l = label.get(m)!
                counts.set(l, (counts.get(l) ?? 0) + 1)
            }
            let best = label.get(n.id)!
            let bestCount = 0
            for (const [l, c] of [...counts].sort((a, b) => a[0].localeCompare(b[0]))) {
                if (c > bestCount) {
                    best = l
                    bestCount = c
                }
            }
            if (best !== label.get(n.id)) {
                label.set(n.id, best)
                changed = true
            }
        }
        if (!changed) break
    }
    const groups = new Map<string, GraphNode[]>()
    for (const n of comp) {
        const l = label.get(n.id)!
        let arr = groups.get(l)
        if (!arr) {
            arr = []
            groups.set(l, arr)
        }
        arr.push(n)
    }
    return [...groups.values()]
}

interface Island {
    nodes: GraphNode[]
    placed: Placed
}

/**
 * Pack order by affinity: seed with the biggest island, then repeatedly take
 * the island with the most edges into what's already placed — the skyline
 * packer places consecutive islands near each other, so linked islands land
 * as neighbors and cross-water edges stay short. Islands with no external
 * edges (orphan singletons included) score zero forever and sink to the end
 * instead of wedging between linked clusters.
 */
function affinityOrder(islands: Island[], edges: GraphEdge[]): Placed[] {
    const islandOf = new Map<string, number>()
    islands.forEach((isl, i) => {
        for (const n of isl.nodes) islandOf.set(n.id, i)
    })
    const m = islands.length
    const aff: number[][] = Array.from({ length: m }, () => new Array<number>(m).fill(0))
    for (const e of edges) {
        const a = islandOf.get(e.from_id)
        const b = islandOf.get(e.to_id)
        if (a == null || b == null || a === b) continue
        aff[a]![b]!++
        aff[b]![a]!++
    }
    const area = islands.map((i) => i.placed.w * i.placed.h)
    const remaining = new Set(islands.map((_, i) => i))
    const order: number[] = []
    while (remaining.size) {
        let best = -1
        let bestScore = -1
        for (const i of remaining) {
            let score = 0
            for (const j of order) score += aff[i]![j]!
            if (score > bestScore || (score === bestScore && area[i]! > area[best]!)) {
                best = i
                bestScore = score
            }
        }
        order.push(best)
        remaining.delete(best)
    }
    return order.map((i) => islands[i]!.placed)
}

/**
 * Archipelago: clusters become separated islands so their members rest
 * against siblings only. Connected components — and communities inside the
 * big ones — each run the Nebula physics alone, then the island boxes are
 * skyline-packed with wide water between them, in affinity order so islands
 * that share edges sit close. Edges between islands stretch across the
 * water: those are the graph's weak ties, worth seeing.
 */
function layoutArchipelago(nodes: GraphNode[], edges: GraphEdge[]): Map<string, XY> {
    const groups: GraphNode[][] = []
    for (const comp of components(nodes, edges)) {
        if (comp.length > COMMUNITY_MIN) groups.push(...communities(comp, edges))
        else groups.push(comp)
    }
    const islands: Island[] = groups.map((g) => ({
        nodes: g,
        placed:
            g.length === 1
                ? {
                      positions: new Map([[g[0]!.id, { x: 0, y: 0 }]]),
                      w: NODE_W,
                      h: NODE_H,
                  }
                : normalized(simulate(g, edges)),
    }))
    const ordered = affinityOrder(islands, edges)
    return packSkyline(
        ordered,
        fieldWidth(ordered, ISLAND_GAP_X, ISLAND_GAP_Y),
        ISLAND_GAP_X,
        ISLAND_GAP_Y,
    )
}

// --- Orbit: hubs with satellites in rings -----------------------------------

/** Non-anchor nodes with at least this many edges also count as hubs. */
const HUB_MIN_DEGREE = 4
const RING_R0 = 420
/** Radial gap between rings. ≥380 keeps adjacent-ring cards from touching on
 * the horizontal flanks: closer than a card width there forces the vertical
 * offset past a card height. */
const RING_STEP = 380
/** Arc length reserved per satellite card on a ring. */
const SAT_ARC = NODE_W + 60

/**
 * Orbit: geometric hub-and-spoke. Anchors (and any node with enough edges)
 * become suns; every other node joins the system of its *nearest* hub —
 * assigned by multi-source BFS, so a chain hanging off a satellite still
 * belongs to its cluster and lands on an outer ring instead of drifting off
 * as a leftover. Only truly hubless components keep the dagre treatment.
 * Systems are packed like islands, in affinity order. Deterministic
 * throughout.
 */
function layoutOrbit(nodes: GraphNode[], edges: GraphEdge[]): Map<string, XY> {
    const ids = new Set(nodes.map((n) => n.id))
    const byId = new Map(nodes.map((n) => [n.id, n]))
    const adj = new Map<string, string[]>(nodes.map((n) => [n.id, []]))
    for (const e of edges) {
        if (!ids.has(e.from_id) || !ids.has(e.to_id)) continue
        adj.get(e.from_id)!.push(e.to_id)
        adj.get(e.to_id)!.push(e.from_id)
    }
    const degree = (id: string): number => adj.get(id)!.length

    const hubs = nodes
        .filter((n) => n.type === 'Anchor' || degree(n.id) >= HUB_MIN_DEGREE)
        .sort((a, b) => degree(b.id) - degree(a.id) || a.id.localeCompare(b.id))
    const hubIds = new Set(hubs.map((h) => h.id))

    // Multi-source BFS from the hubs (biggest first, so equidistant nodes
    // resolve toward the bigger hub): owner = whose system a node joins,
    // hops = its ring band. Direct neighbors orbit close, tails further out.
    const owner = new Map<string, string>()
    const hops = new Map<string, number>()
    const queue: string[] = []
    for (const h of hubs) {
        owner.set(h.id, h.id)
        hops.set(h.id, 0)
        queue.push(h.id)
    }
    for (let head = 0; head < queue.length; head++) {
        const cur = queue[head]!
        for (const m of [...adj.get(cur)!].sort()) {
            if (owner.has(m)) continue
            owner.set(m, owner.get(cur)!)
            hops.set(m, hops.get(cur)! + 1)
            queue.push(m)
        }
    }

    const system = new Map<string, string[]>(hubs.map((h) => [h.id, []]))
    const leftovers: GraphNode[] = []
    for (const n of nodes) {
        if (hubIds.has(n.id)) continue
        const o = owner.get(n.id)
        if (o) system.get(o)!.push(n.id)
        else leftovers.push(n)
    }

    const islands: Island[] = []
    for (const h of hubs) {
        // Nearest first: inner rings hold the hub's direct neighborhood.
        const sats = system
            .get(h.id)!
            .sort((a, b) => hops.get(a)! - hops.get(b)! || a.localeCompare(b))
        const positions = new Map<string, XY>([[h.id, { x: 0, y: 0 }]])
        let i = 0
        for (let ring = 0; i < sats.length; ring++) {
            const r = RING_R0 + ring * RING_STEP
            const count = Math.min(
                Math.max(1, Math.floor((2 * Math.PI * r) / SAT_ARC)),
                sats.length - i,
            )
            for (let k = 0; k < count; k++, i++) {
                // Stagger successive rings so satellites interleave.
                const angle = (k / count) * 2 * Math.PI + ring * 0.35
                positions.set(sats[i]!, { x: r * Math.cos(angle), y: r * Math.sin(angle) })
            }
        }
        const members = [h.id, ...sats].map((id) => byId.get(id)!)
        islands.push({ nodes: members, placed: normalized(positions) })
    }

    if (leftovers.length) {
        const leftIds = new Set(leftovers.map((n) => n.id))
        const leftEdges = edges.filter((e) => leftIds.has(e.from_id) && leftIds.has(e.to_id))
        for (const comp of components(leftovers, leftEdges)) {
            islands.push({ nodes: comp, placed: layoutComponent(comp, leftEdges) })
        }
    }

    const ordered = affinityOrder(islands, edges)
    return packSkyline(
        ordered,
        fieldWidth(ordered, ISLAND_GAP_X, ISLAND_GAP_Y),
        ISLAND_GAP_X,
        ISLAND_GAP_Y,
    )
}

/**
 * Lay the graph out in the chosen mode — Skyline (layered, packed; the
 * default), Nebula (one physics cloud), Archipelago (community islands), or
 * Orbit (hub-and-spoke rings). Hand-dragged positions always win in all.
 */
export function layoutGraph(
    nodes: GraphNode[],
    edges: GraphEdge[],
    overrides: Map<string, XY>,
    mode: LayoutMode = 'skyline',
): Map<string, XY> {
    const result =
        mode === 'nebula'
            ? layoutNebula(nodes, edges, overrides)
            : mode === 'archipelago'
              ? layoutArchipelago(nodes, edges)
              : mode === 'orbit'
                ? layoutOrbit(nodes, edges)
                : layoutSkyline(nodes, edges)

    for (const [id, o] of overrides) {
        if (result.has(id)) result.set(id, o)
    }
    return result
}
