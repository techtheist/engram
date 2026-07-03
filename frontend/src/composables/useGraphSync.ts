import { computed, watch } from 'vue'
import { useDocumentVisibility, useIdle, useIntervalFn } from '@vueuse/core'
import { useGraphStore } from '@/stores/graph'

/** How often to reconcile the shared DB while the user is active. */
const POLL_INTERVAL_MS = 3000
/** Treat the user as away after this long with no pointer/keyboard activity. */
const IDLE_AFTER_MS = 60_000

export function useGraphSync(): void {
    const store = useGraphStore()
    const visibility = useDocumentVisibility()
    const { idle } = useIdle(IDLE_AFTER_MS)

    const active = computed(() => visibility.value === 'visible' && !idle.value)

    const { pause, resume } = useIntervalFn(() => store.refresh(), POLL_INTERVAL_MS, {
        immediate: false,
    })

    watch(
        active,
        (isActive) => {
            if (isActive) {
                store.refresh() // catch up now, don't wait a full interval
                resume()
            } else {
                pause()
            }
        },
        { immediate: true },
    )
}
