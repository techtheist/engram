import { defineStore } from 'pinia'
import { ref, watch } from 'vue'

export type LayoutMode = 'skyline' | 'nebula' | 'archipelago' | 'orbit'

export interface LayoutOption {
    id: LayoutMode
    label: string
    hint: string
}

/**
 * The four arrangements of the canvas. Skyline is the default: reasoning
 * chains read left→right in layers, components packed into rows. The other
 * three trade the grid for structure: Nebula is one global physics cloud,
 * Archipelago runs the same physics per community so clusters become
 * separated islands, Orbit is geometric — hubs with their satellites in
 * rings. All physics modes share a flow force: a source drifts left of its
 * target, matching the out-right / in-left node handles.
 */
export const LAYOUTS: LayoutOption[] = [
    { id: 'skyline', label: 'Skyline', hint: 'layered left→right, packed rows' },
    { id: 'nebula', label: 'Nebula', hint: 'one force-directed cloud' },
    { id: 'archipelago', label: 'Archipelago', hint: 'community islands, physics inside' },
    { id: 'orbit', label: 'Orbit', hint: 'hubs with satellites in rings' },
]

const STORAGE_KEY = 'engram.layout'
const DEFAULT_LAYOUT: LayoutMode = 'skyline'

function initialLayout(): LayoutMode {
    const saved = localStorage.getItem(STORAGE_KEY)
    return LAYOUTS.some((l) => l.id === saved) ? (saved as LayoutMode) : DEFAULT_LAYOUT
}

export const useLayoutStore = defineStore('layout', () => {
    const current = ref<LayoutMode>(initialLayout())

    function set(id: LayoutMode): void {
        if (LAYOUTS.some((l) => l.id === id)) current.value = id
    }

    watch(current, (id) => localStorage.setItem(STORAGE_KEY, id))

    return { current, layouts: LAYOUTS, set }
})
