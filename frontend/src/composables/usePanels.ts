import { reactive } from 'vue'

export type PanelSide = 'left' | 'right'

interface Claim {
    id: string
    dismiss: () => void
}

// Module-scoped: each screen edge holds at most one open drawer, and the
// user's drag-set widths live for this UI session only (deliberately not
// persisted — a fresh pane starts from the design defaults).
const owners: Record<PanelSide, Claim | null> = { left: null, right: null }
const widths = reactive<Record<string, number>>({})

export function usePanels() {
    return {
        /** Drag-set width in px per panel id; absent until the user resizes. */
        widths,
        /** Opening a drawer closes whatever already occupies its side. */
        claim(side: PanelSide, id: string, dismiss: () => void): void {
            const current = owners[side]
            if (current && current.id !== id) current.dismiss()
            owners[side] = { id, dismiss }
        },
        release(side: PanelSide, id: string): void {
            if (owners[side]?.id === id) owners[side] = null
        },
    }
}
