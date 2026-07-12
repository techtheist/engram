// Plain-words rendering of the backend trust model (engram-core policy.rs).
// Trust v2 rules, mirrored here for display only — the number itself always
// comes computed from the backend:
//  - override (pin) short-circuits everything
//  - anchor: approved (100%) > confirmed (60%) > created (50%)
//  - stable holds flat until contradicting evidence demotes it
//  - episodic decays over ~6 months, volatile over ~1 month
//  - open Problems/Intents never decay while open
//  - retrieval (last_seen) never moves trust — it only proves findability

import type { GraphNode } from '@/types/graph'

function pct(v: number): string {
    return `${Math.round(v * 100)}%`
}

function ago(secs: number): string {
    const days = Math.floor((Date.now() / 1000 - secs) / 86400)
    if (days <= 0) return 'today'
    if (days === 1) return 'yesterday'
    if (days < 60) return `${days} days ago`
    if (days < 365 * 2) return `${Math.round(days / 30)} months ago`
    return `${Math.round(days / 365)} years ago`
}

/** The anchor a node's trust currently stands on. */
function anchor(n: GraphNode): { word: string; ts: number; start: string } {
    if (n.approved_at != null) return { word: 'Approved', ts: n.approved_at, start: '100%' }
    if (n.confirmed_at != null) return { word: 'Confirmed', ts: n.confirmed_at, start: '60%' }
    return { word: 'Created', ts: n.created_at, start: '50%' }
}

/**
 * One or two sentences answering "why is trust this number" — shown under the
 * badges and as the trust badge's tooltip. Every state the model can be in
 * has a human explanation; if this ever says something the backend doesn't
 * do, that's a bug worth a report.
 */
export function explainTrust(n: GraphNode): string {
    if (n.trust_override != null) {
        return n.trust_override >= 1
            ? 'Pinned: trust is locked at 100%. It never decays or auto-archives; contradicting evidence still surfaces for review but cannot demote it. Unpin to hand it back to the model.'
            : `Trust is manually locked at ${pct(n.trust_override)}. Decay is off until the override is cleared.`
    }
    const a = anchor(n)
    if (n.status === 'open') {
        return `Held at ${a.start} while open — worklist items are never buried by age.`
    }
    if (n.durability === 'stable') {
        if (n.demoted_at != null) {
            return `A judged conflict landed ${ago(n.demoted_at)} — trust has been falling from ${a.start} since. Editing or approving the node restores it; so does dismissing the conflict.`
        }
        return `${a.word} ${ago(a.ts)}, holding at ${a.start}: stable knowledge does not decay with time — only contradicting evidence lowers it.`
    }
    if (n.approved_at != null) {
        return `Approved ${ago(n.approved_at)}, now ${pct(n.trust)}: approved knowledge decays slowly to a 20% floor over about a year — re-approve to reset it.`
    }
    const window = n.durability === 'volatile' ? 'about a month' : 'about six months'
    return `${a.word} ${ago(a.ts)}, now ${pct(n.trust)}: ${n.durability} notes decay over ${window}. Retrieval alone never refreshes this — confirm, edit, or approve the node to reset it.`
}

/** Tooltip copy for the recurring badges, shared by the card and the canvas. */
export const BADGE_TIPS = {
    pinned: 'User-pinned: constant trust, exempt from decay, auto-archive, and evidence demotion',
    stale: 'Trust decayed below 30% — verify before relying; confirm or approve if still true',
    user: 'User-authored — approved by construction',
    trusted: 'Approved or recently confirmed',
    provisional: 'Claude-authored, not yet approved — earns trust through confirmation',
    demoted:
        'A judged conflict started this node’s decay — edit or approve the node (or dismiss the conflict) to restore it',
    durability: {
        stable: 'Stable: holds trust flat — loses it only to contradicting evidence, never to time',
        episodic: 'Episodic: decays over ~6 months unless confirmed, approved, or pinned',
        volatile: 'Volatile: decays over ~1 month — short-lived working context',
    } as Record<string, string>,
    source: {
        user: 'Authored by you — approved from the start',
        claude: 'Authored by the assistant — starts provisional',
    } as Record<string, string>,
    status: {
        open: 'Open: on the live worklist — never decays while open',
        resolved: 'Resolved: the question this raised has been answered',
        obsolete: 'Obsolete: no longer applicable',
    } as Record<string, string>,
    lastSeen:
        'Last time retrieval surfaced this node (search or brief). Observability only — it never affects trust.',
    confirmed:
        'Last deliberate act that vouched for this node (an edit or "Confirm still true") — the trust anchor when unapproved.',
    approved: 'Explicit approval — trust restarted at 100%',
}
