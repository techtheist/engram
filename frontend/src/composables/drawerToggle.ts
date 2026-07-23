import { ref } from 'vue'

/**
 * One left-edge drawer's shared open state: module-scoped so the SettingsMenu
 * trigger and the panel component see the same ref. Every drawer composable
 * is an instance of this.
 */
export function makeDrawerToggle() {
    const open = ref(false)
    return {
        open,
        show: () => (open.value = true),
        hide: () => (open.value = false),
    }
}
