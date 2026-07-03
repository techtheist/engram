import dagre from '@dagrejs/dagre'
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
function packSkyline(comps: Placed[], width: number): Map<string, XY> {
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
        const w = c.w + COMP_GAP_X
        const h = c.h + COMP_GAP_Y

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
 * Directed layered layout, component-aware. Each connected component flows
 * left→right (a reasoning chain reads well as ranks). Components are then
 * skyline-packed — tallest first, so the main web leads and smaller pieces
 * fill the space to its right. That keeps the "connected node sits to the
 * right" flow while stopping many edgeless/root nodes from marching straight
 * down. Hand-dragged positions always win.
 */
export function layoutGraph(
    nodes: GraphNode[],
    edges: GraphEdge[],
    overrides: Map<string, XY>,
): Map<string, XY> {
    const comps = components(nodes, edges)
        .map((c) => layoutComponent(c, edges))
        .sort((a, b) => b.h - a.h || b.w - a.w)

    /* One card plus its margins ≈ one near-square grid cell, so the field
       width targets a square in cell counts: √cells columns. Each component
       occupies a grid box of cells (dagre's internal spacing included). A web
       taller than the square is fine — the skyline fills the right side. */
    const CELL_W = NODE_W + COMP_GAP_X
    const CELL_H = NODE_H + COMP_GAP_Y
    const widest = comps.reduce((m, c) => Math.max(m, c.w), NODE_W)
    const cells = comps.reduce(
        (sum, c) => sum + Math.ceil(c.w / CELL_W) * Math.ceil(c.h / CELL_H),
        0,
    )
    const width = Math.max(widest + COMP_GAP_X, Math.ceil(Math.sqrt(cells)) * CELL_W)

    const result = packSkyline(comps, width)

    for (const [id, o] of overrides) {
        if (result.has(id)) result.set(id, o)
    }
    return result
}
