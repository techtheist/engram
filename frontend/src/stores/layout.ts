import { defineStore } from 'pinia'
import { ref, watch } from 'vue'

export type LayoutMode = 'skyline' | 'archipelago' | 'orbit'

export interface LayoutOption {
    id: LayoutMode
    label: string
    hint: string
}

/**
 * The three arrangements of the canvas (a fourth, Nebula — one global
 * physics cloud — was removed in 0.9.0: an unstructured cloud never read
 * well; `initialLayout` falls a saved choice back to the default). Skyline
 * is the default: reasoning chains read left→right in layers, components
 * packed into rows. The other two trade the grid for structure: Archipelago
 * runs physics per community so clusters become separated islands, Orbit is
 * geometric — hubs with their satellites in rings. Physics shares a flow
 * force: a source drifts left of its target, matching the out-right /
 * in-left node handles.
 */
export const LAYOUTS: LayoutOption[] = [
    { id: 'skyline', label: 'Skyline', hint: 'layered left→right, packed rows' },
    { id: 'archipelago', label: 'Archipelago', hint: 'community islands, physics inside' },
    { id: 'orbit', label: 'Orbit', hint: 'hubs with satellites in rings' },
]

const STORAGE_KEY = 'engram.layout'
const DEFAULT_LAYOUT: LayoutMode = 'skyline'

/** The two screens: the spatial graph canvas, or the vertical card feed. */
export type ViewMode = 'graph' | 'feed' | 'history'
const VIEW_KEY = 'engram.view'

function initialLayout(): LayoutMode {
    const saved = localStorage.getItem(STORAGE_KEY)
    return LAYOUTS.some((l) => l.id === saved) ? (saved as LayoutMode) : DEFAULT_LAYOUT
}

/** #feed / #graph deep-links win over the remembered choice. */
function initialView(): ViewMode {
    if (window.location.hash === '#feed') return 'feed'
    if (window.location.hash === '#graph') return 'graph'
    return localStorage.getItem(VIEW_KEY) === 'feed' ? 'feed' : 'graph'
}

export const useLayoutStore = defineStore('layout', () => {
    const current = ref<LayoutMode>(initialLayout())
    const view = ref<ViewMode>(initialView())

    function set(id: LayoutMode): void {
        if (LAYOUTS.some((l) => l.id === id)) current.value = id
    }

    function setView(v: ViewMode): void {
        view.value = v
    }

    watch(current, (id) => localStorage.setItem(STORAGE_KEY, id))
    watch(view, (v) => localStorage.setItem(VIEW_KEY, v))

    return { current, layouts: LAYOUTS, set, view, setView }
})
