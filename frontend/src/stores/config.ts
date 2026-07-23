import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { api } from '@/services/api'
import type { ConfigPreset, GraphConfig, TypeDef, VerbDef, VerbRoles } from '@/types/graph'

/**
 * The per-graph configuration (PLAN §7D): the ontology (types + verbs with
 * role flags), the policy numbers, and the brief composition. The pane
 * renders whatever the config declares — type colors are DERIVED from each
 * type's single hue (light + dark schemes, muted for non-highlight types)
 * and injected as CSS vars, so every scheme stays coherent by construction.
 */
export const useConfigStore = defineStore('config', () => {
    const cfg = ref<GraphConfig | null>(null)
    const presets = ref<ConfigPreset[]>([])

    const types = computed<TypeDef[]>(() => cfg.value?.ontology.types ?? [])
    const verbs = computed<VerbDef[]>(() => cfg.value?.ontology.verbs ?? [])
    const typeNames = computed(() => types.value.map((t) => t.name))
    const verbNames = computed(() => verbs.value.map((v) => v.name))

    function typeDef(name: string): TypeDef | undefined {
        return types.value.find((t) => t.name === name)
    }
    function verbDef(name: string): VerbDef | undefined {
        return verbs.value.find((v) => v.name === name)
    }

    const supersessionVerb = computed(
        () => verbs.value.find((v) => v.roles.supersession)?.name ?? 'replaces',
    )
    const contradictionVerb = computed(
        () => verbs.value.find((v) => v.roles.contradiction)?.name ?? 'conflicts-with',
    )
    const reasonVerb = computed(() => verbs.value.find((v) => v.roles.reason)?.name)
    const worklistTypes = computed(() =>
        types.value.filter((t) => t.roles.worklist).map((t) => t.name),
    )

    /** Below this computed trust a node is stale (policy-tunable). */
    const staleTrust = computed(() => cfg.value?.policy.stale_trust ?? 0.3)

    // ---- derived colors --------------------------------------------------

    function slug(name: string): string {
        return name.toLowerCase().replace(/[^a-z0-9]+/g, '-')
    }

    /** The CSS var reference for a type's accent (fallback: muted slate). */
    function accent(name: string): string {
        return `var(--nt-${slug(name)}, #64748b)`
    }

    /**
     * Hue → concrete colors. Highlightable types get a saturated mid-tone
     * (slightly darker on light themes for contrast); `highlight: false`
     * types (code anchors) render muted — role drives the look, so any
     * ontology's anchor reads as quiet infrastructure.
     */
    function deriveTypeColor(t: TypeDef): { dark: string; light: string } {
        if (!t.roles.highlight) {
            return {
                dark: `hsl(${t.hue} 18% 55%)`,
                light: `hsl(${t.hue} 16% 44%)`,
            }
        }
        return {
            dark: `hsl(${t.hue} 84% 60%)`,
            light: `hsl(${t.hue} 78% 44%)`,
        }
    }

    /** (Re)write the injected stylesheet holding every `--nt-*` var. */
    function injectCss(): void {
        const id = 'engram-ontology-css'
        let el = document.getElementById(id) as HTMLStyleElement | null
        if (!el) {
            el = document.createElement('style')
            el.id = id
            document.head.appendChild(el)
        }
        const dark = types.value
            .map((t) => `--nt-${slug(t.name)}: ${deriveTypeColor(t).dark};`)
            .join(' ')
        const light = types.value
            .map((t) => `--nt-${slug(t.name)}: ${deriveTypeColor(t).light};`)
            .join(' ')
        el.textContent = `:root { ${dark} }\n[data-theme$="light"] { ${light} }`
    }

    /**
     * Edge colors derive from ROLES, not names, so a renamed or custom verb
     * keeps its meaning's color: contradiction red, supersession amber,
     * reason violet, answer green, dependency sky; role-less verbs cycle a
     * small neutral palette (about slate, builds-on indigo in the shipped
     * set).
     */
    const ROLELESS_CYCLE = ['#94a3b8', '#818cf8', '#f472b6', '#2dd4bf']
    const hasRole = (r: VerbRoles) =>
        r.contradiction || r.supersession || r.reason || r.answer || r.dependency
    function edgeColor(verb: string): string {
        const def = verbDef(verb)
        if (!def) return '#94a3b8'
        const r = def.roles
        if (r.contradiction) return '#ef4444'
        if (r.supersession) return '#f59e0b'
        if (r.reason) return '#a78bfa'
        if (r.answer) return '#4ade80'
        if (r.dependency) return '#38bdf8'
        const roleless = verbs.value.filter((v) => !hasRole(v.roles))
        const idx = roleless.findIndex((v) => v.name === verb)
        return ROLELESS_CYCLE[Math.max(idx, 0) % ROLELESS_CYCLE.length] ?? '#94a3b8'
    }

    /** Temporal / weaker / dependency relations draw dashed. */
    function edgeDashed(verb: string): boolean {
        const r = verbDef(verb)?.roles
        return !!r && (r.supersession || r.contradiction || r.dependency)
    }

    /** An unresolved contradiction edge (the panels' shared filter). */
    function isActiveConflict(e: { type: string; status?: string | null }): boolean {
        return e.type === contradictionVerb.value && (e.status == null || e.status === 'active')
    }

    /** The contradiction edge animates — the one relation worth the eye. */
    function edgeAnimated(verb: string): boolean {
        return !!verbDef(verb)?.roles.contradiction
    }

    /** The connect dialog's hint: the verb's worked example from the config. */
    function edgeSentence(verb: string): string {
        return verbDef(verb)?.reads_as ?? ''
    }

    // ---- lifecycle -------------------------------------------------------

    async function load(): Promise<void> {
        cfg.value = await api.config()
        injectCss()
    }

    async function loadPresets(): Promise<void> {
        presets.value = await api.configPresets()
    }

    /** PUT the whole document; the backend validates the hard invariants. */
    async function save(next: GraphConfig): Promise<void> {
        cfg.value = await api.putConfig(next)
        injectCss()
    }

    /** Rename + bulk-retype — the ontology migration gesture. */
    async function renameType(from: string, to: string): Promise<number> {
        const { renamed } = await api.renameType(from, to)
        await load()
        return renamed
    }

    async function renameVerb(from: string, to: string): Promise<number> {
        const { renamed } = await api.renameVerb(from, to)
        await load()
        return renamed
    }

    return {
        cfg,
        presets,
        types,
        verbs,
        typeNames,
        verbNames,
        typeDef,
        verbDef,
        supersessionVerb,
        contradictionVerb,
        reasonVerb,
        worklistTypes,
        staleTrust,
        accent,
        edgeColor,
        deriveTypeColor,
        isActiveConflict,
        edgeDashed,
        edgeAnimated,
        edgeSentence,
        load,
        loadPresets,
        save,
        renameType,
        renameVerb,
    }
})
