import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import { api, setApiProject } from '@/services/api'
import { useGraphStore } from '@/stores/graph'
import { useConfigStore } from '@/stores/config'
import { usePanels } from '@/composables/usePanels'
import type { ProjectInfo } from '@/types/graph'

declare global {
    interface Window {
        /** The IDE deep link, injected by the VSCode webview host — its
         *  bundled SPA has no real URL to carry a `?project=` query, so the
         *  workspace folder name rides a global instead. */
        __ENGRAM_PROJECT__?: string
    }
}

/** The remembered selection — localStorage, like the theme and layout keys
 *  (the mechanism already proven inside the JetBrains JCEF browser). */
const STORAGE_KEY = 'engram.project'

/** The registry mints project names as kebab slugs of the repo folder name
 *  (registry.rs `unique_slug`) — normalize a raw IDE folder name the same
 *  way so a deep link for "MyProject" finds the project named "myproject". */
function slug(s: string): string {
    let out = ''
    for (const c of s) {
        if (/[a-z0-9]/i.test(c)) out += c.toLowerCase()
        else if (out && !out.endsWith('-')) out += '-'
    }
    return out.replace(/-+$/, '')
}

/** Last path segment of a project root, tolerant of either separator. */
function rootBasename(root: string | undefined): string | null {
    if (!root) return null
    const parts = root.split(/[/\\]/).filter(Boolean)
    return parts[parts.length - 1] ?? null
}

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

    /** Match a `?project=` deep-link value: exact registry name, then the
     *  slug of the raw value, then the basename of a project's root path.
     *  Unknown → null (the caller falls back silently). */
    function matchDeepLink(raw: string): ProjectInfo | null {
        const want = raw.trim()
        if (!want) return null
        const wanted = slug(want)
        const list = projects.value
        const hit =
            list.find((p) => p.name === want) ??
            list.find((p) => wanted !== '' && p.name === wanted) ??
            list.find((p) => {
                const base = rootBasename(p.root)
                return base != null && wanted !== '' && slug(base) === wanted
            })
        return hit ?? null
    }

    /**
     * Boot-time selection, before the first graph load: the `?project=` deep
     * link from an IDE wins (and updates the memory), then the remembered
     * selection — both validated against the live project list, falling back
     * to the launch graph silently when the value is unknown. Sets the API
     * scope directly instead of `switchTo`'s reload dance: nothing is loaded
     * yet — App.vue loads config + graph right after.
     */
    async function restore(): Promise<void> {
        await loadProjects()
        if (!projects.value.length) return
        const param =
            new URLSearchParams(window.location.search).get('project') ??
            window.__ENGRAM_PROJECT__
        let target: ProjectInfo | null = null
        if (param != null) {
            target = matchDeepLink(param)
            if (target) localStorage.setItem(STORAGE_KEY, target.id)
        }
        if (!target) {
            const stored = localStorage.getItem(STORAGE_KEY)
            if (stored != null) target = projects.value.find((p) => p.id === stored) ?? null
        }
        if (!target || target.current) return // the launch graph is the default scope
        activeId.value = target.id
        setApiProject(target.id)
    }

    async function switchTo(project: ProjectInfo): Promise<void> {
        // Remember the pick across reloads (registry ids are stable).
        localStorage.setItem(STORAGE_KEY, project.id)
        const next = project.current ? null : project.id
        if (next === activeId.value) return
        activeId.value = next
        setApiProject(next)
        const graph = useGraphStore()
        // Close every open drawer first: they render the OLD graph's state
        // (a node card, an in-progress settings draft) and would silently
        // present it as the new project's.
        usePanels().closeAll()
        // Filters, selection and queues describe the graph we are leaving;
        // component-level state (search, trails, sweep results) resets off
        // `switchEpoch` via the onProjectSwitch composable.
        graph.resetView()
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
        restore,
        switchTo,
        addByPath,
        unregister,
    }
})
