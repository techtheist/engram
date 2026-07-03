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
export const TRUSTED_CONFIDENCE = 0.7
