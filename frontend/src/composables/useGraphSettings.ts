import { makeDrawerToggle } from '@/composables/drawerToggle'

// Module-scoped singleton: trigger and panel share one open state.
const toggle = makeDrawerToggle()

export function useGraphSettings() {
    return toggle
}
