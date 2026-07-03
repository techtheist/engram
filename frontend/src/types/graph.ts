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
    confidence: number | null
    code_refs: string[]
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

export interface SearchHit {
    id: string
    type: NodeType
    title: string
    snippet: string
    score: number
    durability: Durability
    status: NodeStatus | null
}
