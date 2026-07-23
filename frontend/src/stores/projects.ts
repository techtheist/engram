import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import { api, setApiProject } from '@/services/api'
import { useGraphStore } from '@/stores/graph'
import { useConfigStore } from '@/stores/config'
import { usePanels } from '@/composables/usePanels'
import type { ProjectInfo } from '@/types/graph'

/**
 * The multi-project layer (PLAN §7C): which graph the pane is looking at.
 * `activeId === null` means the daemon's launch project (the bare routes);
 * anything else scopes every API call under `/projects/{id}`. Switching
 * reloads the graph and re-attaches SSE to that project's channel.
 */
export const useProjectsStore = defineStore('projects', () => {
    const projects = ref<ProjectInfo[]>([])
    const activeId = ref<string | null>(null)
    const error = ref<string | null>(null)
    /** Bumped after every completed switch — the canvas re-fits on it
     * (graph sizes differ wildly between projects). */
    const switchEpoch = ref(0)

    const active = computed<ProjectInfo | null>(() => {
        if (activeId.value == null) return projects.value.find((p) => p.current) ?? null
        return projects.value.find((p) => p.id === activeId.value) ?? null
    })

    const activeName = computed(() => active.value?.name ?? 'this project')

    async function loadProjects(): Promise<void> {
        try {
            projects.value = await api.projects()
            error.value = null
        } catch (e) {
            // A pre-0.6 daemon has no /projects — the switcher just hides.
            projects.value = []
            error.value = e instanceof Error ? e.message : String(e)
        }
    }

    async function switchTo(project: ProjectInfo): Promise<void> {
        const next = project.current ? null : project.id
        if (next === activeId.value) return
        activeId.value = next
        setApiProject(next)
        const graph = useGraphStore()
        // Close every open drawer first: they render the OLD graph's state
        // (a node card, an in-progress settings draft) and would silently
        // present it as the new project's.
        usePanels().closeAll()
        graph.select(null)
        graph.disconnect()
        // Each project's graph carries its own ontology/policy config.
        await Promise.all([graph.load(), useConfigStore().load()])
        graph.connect()
        switchEpoch.value += 1
    }

    async function addByPath(path: string): Promise<void> {
        await api.registerProject(path)
        await loadProjects()
    }

    /** Withdraw awareness only — the project's data stays where it lives. */
    async function unregister(id: string): Promise<void> {
        await api.unregisterProject(id)
        await loadProjects()
    }

    return {
        projects,
        activeId,
        active,
        activeName,
        error,
        switchEpoch,
        loadProjects,
        switchTo,
        addByPath,
        unregister,
    }
})
