import { reactive } from 'vue'

export type PanelSide = 'left' | 'right'

interface Claim {
    id: string
    dismiss: () => void
    /** Content floor in px — how far this drawer can be pushed. */
    minPx: number
    /** Width in px while the user has never dragged the grip. */
    defaultPx: number
}

// Module-scoped: each screen edge holds at most one open drawer, and the
// user's drag-set widths live for this UI session only (deliberately not
// persisted — a fresh pane starts from the design defaults).
const owners: Record<PanelSide, Claim | null> = { left: null, right: null }
const widths = reactive<Record<string, number>>({})

function widthOf(c: Claim): number {
    return widths[c.id] ?? c.defaultPx
}

export function usePanels() {
    return {
        /** Drag-set width in px per panel id; absent until the user resizes. */
        widths,
        /** Opening a drawer closes whatever already occupies its side. */
        claim(side: PanelSide, next: Claim): void {
            const current = owners[side]
            if (current && current.id !== next.id) current.dismiss()
            owners[side] = next
        },
        release(side: PanelSide, id: string): void {
            if (owners[side]?.id === id) owners[side] = null
        },
        /**
         * Dynamic center: drawers have no fixed midline cap — instead, when
         * both edges are open and would overlap, the drawer opposite
         * `activeSide` gives way down to its content floor, and only then
         * does the active one stop growing. Called on open and on drag.
         */
        reconcile(activeSide: PanelSide): void {
            const active = owners[activeSide]
            const passive = owners[activeSide === 'left' ? 'right' : 'left']
            if (!active || !passive) return
            const vw = window.innerWidth
            if (widthOf(active) + widthOf(passive) <= vw) return
            widths[passive.id] = Math.round(Math.max(passive.minPx, vw - widthOf(active)))
            if (widthOf(active) + widthOf(passive) > vw) {
                widths[active.id] = Math.round(Math.max(active.minPx, vw - widthOf(passive)))
            }
        },
        /**
         * Force-close both edges. A project switch calls this: drawers hold
         * per-graph state (a node card, the settings draft), and carrying
         * them across graphs shows the previous project's truth as the new
         * one's.
         */
        closeAll(): void {
            for (const side of ['left', 'right'] as const) {
                owners[side]?.dismiss()
                owners[side] = null
            }
        },
    }
}
