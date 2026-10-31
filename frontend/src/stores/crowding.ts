import { defineStore, storeToRefs } from 'pinia'
import { computed, ref, watch } from 'vue'
import { api } from '@/services/api'
import { onProjectSwitch } from '@/composables/onProjectSwitch'
import { useGraphStore } from '@/stores/graph'
import type { CrowdingReport } from '@/types/graph'

export { heatColor } from '@/utils/heat'

const HEAT_KEY = 'engram.heat'

function initialHeat(): boolean {
    try {
        return localStorage.getItem(HEAT_KEY) === '1'
    } catch {
        return false
    }
}

/**
 * Crowding (0.11.0): the graph view's heat toggle and Checkup's sleep queue
 * read one report. It loads while someone looks at it (heat on, or Checkup
 * asked) and follows the graph: a write changes crowding, so a changed note
 * list reloads it, debounced so a burst of writes costs one request.
 */
export const useCrowdingStore = defineStore('crowding', () => {
    const graph = useGraphStore()
    const { nodeList } = storeToRefs(graph)

    const heatOn = ref(initialHeat())
    const report = ref<CrowdingReport | null>(null)
    const loading = ref(false)
    const error = ref<string | null>(null)
    /** Checkup's open section counts as a reader too. */
    const readers = ref(0)

    const active = computed(() => heatOn.value || readers.value > 0)

    async function load(): Promise<void> {
        loading.value = true
        try {
            report.value = await api.crowding()
            error.value = null
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e)
        } finally {
            loading.value = false
        }
    }

    let timer: ReturnType<typeof setTimeout> | null = null
    function reloadSoon(): void {
        if (timer) clearTimeout(timer)
        timer = setTimeout(() => {
            timer = null
            if (active.value) void load()
        }, 600)
    }

    watch(active, (on) => {
        if (on) void load()
    }, { immediate: true })
    watch(nodeList, reloadSoon)
    onProjectSwitch(() => {
        report.value = null
        if (active.value) void load()
    })

    function toggleHeat(): void {
        heatOn.value = !heatOn.value
        try {
            localStorage.setItem(HEAT_KEY, heatOn.value ? '1' : '0')
        } catch {
            /* a private window keeps it for this tab only */
        }
    }

    /** Mark a cluster reviewed (kept as it is); the queue drops it. */
    async function markReviewed(ids: string[]): Promise<void> {
        await api.reviewMarks(ids, true)
        await load()
    }

    function heatOf(id: string): number | null {
        return report.value?.nodes[id]?.heat ?? null
    }

    return { heatOn, report, loading, error, readers, toggleHeat, load, markReviewed, heatOf }
})
