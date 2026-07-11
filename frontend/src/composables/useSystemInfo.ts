import { ref } from 'vue'

// Module-scoped so the SettingsMenu trigger and the panel share one state.
const open = ref(false)

export function useSystemInfo() {
    return {
        open,
        show: () => (open.value = true),
        hide: () => (open.value = false),
    }
}
