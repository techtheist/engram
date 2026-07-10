import type { EdgeType, NodeType } from '@/types/graph'

/** CSS var holding each node type's accent (defined theme-independently in tokens.css). */
export const NODE_ACCENT_VAR: Record<NodeType, string> = {
    Principle: 'var(--node-principle)',
    Decision: 'var(--node-decision)',
    Caution: 'var(--node-caution)',
    Problem: 'var(--node-problem)',
    Resolution: 'var(--node-resolution)',
    Insight: 'var(--node-insight)',
    Intent: 'var(--node-intent)',
    Anchor: 'var(--node-anchor)',
}

/** Edge stroke colour per type. Mid-tone hues so they read on light and dark. */
export const EDGE_COLOR: Record<EdgeType, string> = {
    about: '#94a3b8',
    because: '#a78bfa',
    answers: '#4ade80',
    'builds-on': '#818cf8',
    replaces: '#f59e0b',
    'conflicts-with': '#ef4444',
    needs: '#38bdf8',
}

/** Edges drawn dashed (temporal / weaker / dependency relations). */
export const EDGE_DASHED: ReadonlySet<EdgeType> = new Set<EdgeType>([
    'replaces',
    'conflicts-with',
    'needs',
])

/** Edges that animate the dash flow — the "active" relations worth the eye. */
export const EDGE_ANIMATED: ReadonlySet<EdgeType> = new Set<EdgeType>(['conflicts-with'])

export const ALL_EDGE_TYPES: EdgeType[] = [
    'about',
    'because',
    'answers',
    'builds-on',
    'replaces',
    'conflicts-with',
    'needs',
]

/** How each verb completes the sentence "A … B" — the connect dialog's hint. */
export const EDGE_SENTENCE: Record<EdgeType, string> = {
    about: 'concerns this subject',
    because: 'is justified by',
    answers: 'resolves or addresses',
    'builds-on': 'elaborates on',
    replaces: 'supersedes (older kept, marked)',
    'conflicts-with': 'contradicts',
    needs: 'depends on / is blocked by',
}

/**
 * Recommended starting tags (PLAN §10): offered in the editor before the graph
 * has grown its own vocabulary; real usage takes over from there.
 */
export const DEFAULT_TAGS: string[] = ['tech-decision', 'preference', 'process', 'domain']

export const ALL_NODE_TYPES: NodeType[] = [
    'Principle',
    'Decision',
    'Caution',
    'Problem',
    'Resolution',
    'Insight',
    'Intent',
    'Anchor',
]

/** At or above this confidence a node counts as trusted (mirrors backend policy). */
/** Backend trust at/above which a node reads as "trusted" in the UI. */
export const TRUSTED_TRUST = 0.7
/** Mirrors policy::STALE_TRUST — below this the node is stale. */
export const STALE_TRUST = 0.3
