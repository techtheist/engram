<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import SidePanel from '@/components/common/SidePanel.vue'
import { useGraphSettings } from '@/composables/useGraphSettings'
import { humanDays, pct } from '@/constants/trust'
import { useConfigStore } from '@/stores/config'
import { useGraphStore } from '@/stores/graph'
import { api } from '@/services/api'
import type { Durability, GraphConfig, TypeDef, VerbDef } from '@/types/graph'

/**
 * Settings → Graph settings: the ontology redactor (PLAN §7D stage 4).
 * Everything the engine knows about meaning, editable — node types (name,
 * hue, thought, durability, role flags, brief section), edge verbs (name,
 * example, role flags with exactly one supersession + one contradiction),
 * the policy numbers with plain-word explanations rendered from the live
 * values, and the brief composition. Renames go through the bulk-retype
 * endpoints so stored knowledge follows; everything else is one PUT.
 */
const { open, hide } = useGraphSettings()
const config = useConfigStore()
const graph = useGraphStore()

const draft = ref<GraphConfig | null>(null)
const error = ref<string | null>(null)
const notice = ref<string | null>(null)
const busy = ref(false)

function clone(cfg: GraphConfig): GraphConfig {
    return JSON.parse(JSON.stringify(cfg)) as GraphConfig
}

function resetDraft(): void {
    draft.value = config.cfg ? clone(config.cfg) : null
    error.value = null
}

watch(open, (isOpen) => {
    if (isOpen) {
        void config
            .load()
            .then(() => config.loadPresets())
            .then(resetDraft)
            .catch((e) => (error.value = e instanceof Error ? e.message : String(e)))
        void loadVersion()
    }
})

const dirty = computed(
    () =>
        draft.value != null &&
        config.cfg != null &&
        JSON.stringify(draft.value) !== JSON.stringify(config.cfg),
)

async function save(): Promise<void> {
    if (!draft.value) return
    busy.value = true
    error.value = null
    notice.value = null
    try {
        draft.value.ontology.preset = presetStillMatches() ? draft.value.ontology.preset : 'custom'
        await config.save(draft.value)
        resetDraft()
        flash('Saved — the next brief and every write run on the new rules.')
    } catch (e) {
        error.value = shortHttpError(e)
    } finally {
        busy.value = false
    }
}

/** Keep the provenance label honest: any ontology edit makes it "custom". */
function presetStillMatches(): boolean {
    const shelf = config.presets.find((p) => p.id === draft.value?.ontology.preset)
    return (
        !!shelf &&
        JSON.stringify(shelf.config.ontology) === JSON.stringify(draft.value?.ontology)
    )
}

/** The backend's 400 carries the violated invariant — show it, not the URL. */
function shortHttpError(e: unknown): string {
    const raw = e instanceof Error ? e.message : String(e)
    const tail = raw.split('→').pop() ?? raw
    return tail.replace(/^\s*\d+\s*/, '').trim() || raw
}

let flashTimer: ReturnType<typeof setTimeout> | undefined
function flash(msg: string): void {
    notice.value = msg
    clearTimeout(flashTimer)
    flashTimer = setTimeout(() => (notice.value = null), 6000)
}

// ---- presets ---------------------------------------------------------------

const nodeCount = computed(() => graph.nodeList.length)

async function applyPreset(id: string): Promise<void> {
    const preset = config.presets.find((p) => p.id === id)
    if (!preset) return
    if (
        nodeCount.value > 0 &&
        !window.confirm(
            `Apply the "${preset.name}" ontology? Types already holding nodes cannot be dropped — ` +
                'on a non-empty graph this only works when the type names line up (or after retyping). ' +
                'Policy and brief settings reset to the preset too.',
        )
    ) {
        return
    }
    busy.value = true
    error.value = null
    try {
        await config.save(clone(preset.config))
        resetDraft()
        flash(`Preset "${preset.name}" applied.`)
    } catch (e) {
        error.value = shortHttpError(e)
    } finally {
        busy.value = false
    }
}

// ---- types -----------------------------------------------------------------

const DURABILITIES: Durability[] = ['stable', 'episodic', 'volatile']

/** Which type/verb is mid-rename (renames bypass the draft: they bulk-retype). */
const renaming = ref<{ kind: 'type' | 'verb'; from: string; to: string } | null>(null)

function startRename(kind: 'type' | 'verb', from: string): void {
    renaming.value = { kind, from, to: from }
}

async function commitRename(): Promise<void> {
    const r = renaming.value
    if (!r || r.to.trim() === '' || r.to === r.from) {
        renaming.value = null
        return
    }
    busy.value = true
    error.value = null
    try {
        const renamed =
            r.kind === 'type'
                ? await config.renameType(r.from, r.to.trim())
                : await config.renameVerb(r.from, r.to.trim())
        resetDraft()
        flash(
            `Renamed ${r.from} → ${r.to.trim()} — ${renamed} stored ${
                r.kind === 'type' ? 'node' : 'edge'
            }${renamed === 1 ? '' : 's'} followed.`,
        )
        renaming.value = null
        void graph.refresh()
    } catch (e) {
        error.value = shortHttpError(e)
    } finally {
        busy.value = false
    }
}

function countNodes(type: string): number {
    return graph.nodeList.filter((n) => n.type === type).length
}

function addType(): void {
    if (!draft.value) return
    draft.value.ontology.types.push({
        name: nextName('NewType', draft.value.ontology.types.map((t) => t.name)),
        hue: Math.floor(Math.random() * 360),
        thought: 'what this type captures',
        durability: 'episodic',
        roles: { worklist: false, anchor: false, rank_prior: 0, highlight: true, versioned: true },
        brief: { show: false, cap: 8, excerpt: 140 },
    })
}

function removeType(t: TypeDef): void {
    if (!draft.value) return
    const used = countNodes(t.name)
    if (used > 0) {
        error.value = `"${t.name}" still has ${used} node${used === 1 ? '' : 's'} — rename it into another type instead (bulk retype), then remove.`
        return
    }
    draft.value.ontology.types = draft.value.ontology.types.filter((x) => x !== t)
}

function nextName(base: string, taken: string[]): string {
    if (!taken.includes(base)) return base
    let i = 2
    while (taken.includes(`${base}${i}`)) i += 1
    return `${base}${i}`
}

/** Live swatch for the hue slider — the canvas's own derivation. */
function swatch(t: TypeDef): string {
    return config.deriveTypeColor(t).dark
}

// ---- verbs -----------------------------------------------------------------

function addVerb(): void {
    if (!draft.value) return
    draft.value.ontology.verbs.push({
        name: nextName('relates', draft.value.ontology.verbs.map((v) => v.name)),
        reads_as: 'A relates B',
        roles: {
            supersession: false,
            contradiction: false,
            reason: false,
            answer: false,
            dependency: false,
        },
    })
}

function removeVerb(v: VerbDef): void {
    if (!draft.value) return
    if (v.roles.supersession || v.roles.contradiction) {
        error.value = `"${v.name}" carries the ${v.roles.supersession ? 'supersession' : 'contradiction'} role — move the role to another verb first (exactly one must exist).`
        return
    }
    const used = graph.edgeList.filter((e) => e.type === v.name).length
    if (used > 0) {
        error.value = `"${v.name}" still has ${used} edge${used === 1 ? '' : 's'} — rename it into another verb instead.`
        return
    }
    draft.value.ontology.verbs = draft.value.ontology.verbs.filter((x) => x !== v)
}

/** Exactly-one semantics: picking a carrier clears the flag everywhere else. */
function setRoleCarrier(role: 'supersession' | 'contradiction', verb: VerbDef): void {
    if (!draft.value) return
    for (const v of draft.value.ontology.verbs) v.roles[role] = v === verb
}

// ---- version tracking -------------------------------------------------------

const currentVersion = ref<string>('')
const savedVersion = ref<string | null>(null)

async function loadVersion(): Promise<void> {
    try {
        const v = await api.getVersion()
        savedVersion.value = v.current
        currentVersion.value = v.current ?? ''
    } catch {
        savedVersion.value = null
    }
}

async function applyVersion(): Promise<void> {
    busy.value = true
    error.value = null
    try {
        const next = currentVersion.value.trim() || null
        await api.putVersion(next)
        savedVersion.value = next
        flash(next ? `Current working version set to ${next}.` : 'Current version cleared.')
    } catch (e) {
        error.value = shortHttpError(e)
    } finally {
        busy.value = false
    }
}

// ---- assistant skill (re)install -------------------------------------------

const skillVariant = ref('relaxed')
const customOntology = computed(
    () => draft.value != null && draft.value.ontology.preset !== 'engram',
)

async function installSkill(): Promise<void> {
    busy.value = true
    error.value = null
    try {
        const res = await api.installSkill(skillVariant.value)
        flash(
            !res.installed
                ? (res.note ?? 'The skill folder is a symlink — left untouched.')
                : res.generated
                  ? `Generated a skill from this graph's ontology and installed it to ${res.path}.`
                  : `Installed the shipped '${res.variant}' skill to ${res.path}.`,
        )
    } catch (e) {
        error.value = shortHttpError(e)
    } finally {
        busy.value = false
    }
}

// ---- plain-word policy explanations ---------------------------------------

const policyWords = computed(() => {
    const p = draft.value?.policy
    if (!p) return []
    return [
        `A fresh assistant note starts at ${pct(p.trust_created)} trust; a deliberate edit or "confirm still true" lifts it to ${pct(p.trust_confirmed)}; your approval sets it to ${pct(p.trust_approved)}.`,
        `Unapproved episodic notes fade toward ${pct(p.trust_floor)} over ${humanDays(p.episodic_window_days)}; volatile ones over ${humanDays(p.volatile_window_days)}. Approved notes settle at ${pct(p.trust_approved_floor)} over ${humanDays(p.approved_window_days)}. Stable knowledge never fades with time — only judged contradictions demote it.`,
        `Below ${pct(p.stale_trust)} a note reads as stale; once it has been stale ${humanDays(p.decay_ttl_days)}, the decay pass archives it (assistant-authored, unapproved, unpinned only).`,
        `Writes ${pct(p.duplicate_similarity)} similar to an existing same-type note merge instead of duplicating; ${pct(p.conflict_suspect_similarity)}–${pct(p.duplicate_similarity)} pairs queue as suspected conflicts; anything above ${pct(p.warn_similarity)} near contradicted or superseded knowledge warns the writer. The NLI sweep only queues pairs it is ${pct(p.nli_sweep_min_confidence)} sure about.`,
    ]
})

</script>

<template>
<SidePanel
    :open="open"
    side="left"
    panel-id="graph-settings"
    :default-rem="46"
    :min-rem="32"
    :dismiss="hide"
    title="Graph settings"
    style="--panel-gap: 1.2rem"
>
    <template #actions>
        <button
            v-if="dirty"
            class="bar-btn ghost"
            type="button"
            :disabled="busy"
            @click="resetDraft"
        >
            Revert
        </button>
        <button class="bar-btn" type="button" :disabled="!dirty || busy" @click="save">
            Save
        </button>
    </template>

    <p v-if="error" class="state error">{{ error }}</p>
    <p v-else-if="notice" class="state ok">{{ notice }}</p>

    <template v-if="draft">
        <section class="block">
            <h3 class="block-title">Ontology presets</h3>
            <p class="hint">
                A preset replaces the whole configuration — types, verbs, policy, brief. The
                current shape is <strong>{{ draft.ontology.preset }}</strong>.
            </p>
            <div v-for="p in config.presets" :key="p.id" class="preset-line">
                <div class="preset-text">
                    <span class="preset-name">{{ p.name }}</span>
                    <span class="preset-desc">{{ p.description }}</span>
                </div>
                <button
                    class="mini"
                    type="button"
                    :disabled="busy || p.id === draft.ontology.preset"
                    @click="applyPreset(p.id)"
                >
                    {{ p.id === draft.ontology.preset ? 'active' : 'apply' }}
                </button>
            </div>
        </section>

        <section class="block">
            <h3 class="block-title">Node types</h3>
            <p class="hint">
                The name is what the assistant writes; the roles are what the engine does with
                it. Renaming bulk-retypes every stored node — nothing is lost.
            </p>
            <article
                v-for="t in draft.ontology.types"
                :key="t.name"
                class="card"
                :style="{ '--check-accent': swatch(t) }"
            >
                <header class="card-head">
                    <span class="swatch" :style="{ background: swatch(t) }" />
                    <template v-if="renaming?.kind === 'type' && renaming.from === t.name">
                        <input
                            v-model="renaming.to"
                            class="edit-input rename-input"
                            type="text"
                            :aria-label="`New name for ${t.name}`"
                            @keydown.enter="commitRename"
                            @keydown.escape="renaming = null"
                        />
                        <button class="mini" type="button" :disabled="busy" @click="commitRename">
                            rename {{ countNodes(t.name) ? `(${countNodes(t.name)} nodes follow)` : '' }}
                        </button>
                        <button class="mini ghost" type="button" @click="renaming = null">cancel</button>
                    </template>
                    <template v-else>
                        <span class="card-name">{{ t.name }}</span>
                        <span v-if="countNodes(t.name)" class="count">{{ countNodes(t.name) }} nodes</span>
                        <span class="spacer" />
                        <button
                            class="mini ghost"
                            type="button"
                            :disabled="dirty || busy"
                            :title="dirty ? 'Save or revert your edits first — renames apply immediately' : 'Rename and bulk-retype stored nodes'"
                            @click="startRename('type', t.name)"
                        >
                            rename
                        </button>
                        <button class="mini ghost danger" type="button" @click="removeType(t)">
                            remove
                        </button>
                    </template>
                </header>

                <label class="row-label">
                    Hue
                    <input
                        v-model.number="t.hue"
                        class="hue-slider"
                        type="range"
                        min="0"
                        max="359"
                        :aria-label="`${t.name} hue`"
                    />
                    <span class="hue-value">{{ t.hue }}°</span>
                </label>

                <label class="row-label">
                    Thought
                    <input
                        v-model="t.thought"
                        class="edit-input grow"
                        type="text"
                        placeholder="e.g. &quot;we chose this, for a reason&quot;"
                        :aria-label="`${t.name} thought`"
                    />
                </label>

                <div class="row-label">
                    Durability
                    <select v-model="t.durability" class="edit-select" :aria-label="`${t.name} durability`">
                        <option v-for="d in DURABILITIES" :key="d" :value="d">{{ d }}</option>
                    </select>
                    <span class="spacer" />
                    Rank prior
                    <input
                        v-model.number="t.roles.rank_prior"
                        class="edit-input num"
                        type="number"
                        step="0.01"
                        min="0"
                        max="0.5"
                        :aria-label="`${t.name} rank prior`"
                    />
                </div>

                <div class="checks">
                    <label class="check" title="Open/resolved lifecycle: lives in the worklist, never decays while open">
                        <input v-model="t.roles.worklist" type="checkbox" /> worklist
                    </label>
                    <label class="check" title="A code subject: carries code refs, excluded from the conflict scan, renders muted">
                        <input v-model="t.roles.anchor" type="checkbox" /> anchor
                    </label>
                    <label class="check" title="Off renders this type muted (gray-toned) everywhere">
                        <input v-model="t.roles.highlight" type="checkbox" /> highlight
                    </label>
                    <label
                        v-if="draft.versioning.enabled"
                        class="check"
                        title="Stamp new nodes of this type with the current working version (off for types that transcend releases)"
                    >
                        <input v-model="t.roles.versioned" type="checkbox" /> versioned
                    </label>
                    <span class="spacer" />
                    <label class="check" title="Give this type its own canon section in the brief">
                        <input v-model="t.brief.show" type="checkbox" /> brief section
                    </label>
                    <template v-if="t.brief.show">
                        <label class="check">
                            cap
                            <input v-model.number="t.brief.cap" class="edit-input num" type="number" min="0" max="100" :aria-label="`${t.name} brief cap`" />
                        </label>
                        <label class="check">
                            excerpt
                            <input v-model.number="t.brief.excerpt" class="edit-input num" type="number" min="20" max="2000" :aria-label="`${t.name} brief excerpt chars`" />
                        </label>
                    </template>
                </div>
            </article>
            <button class="mini add" type="button" @click="addType">+ add type</button>
        </section>

        <section class="block">
            <h3 class="block-title">Edge verbs</h3>
            <p class="hint">
                A triple must read as English — "A {{ config.supersessionVerb }} B". Exactly one
                verb supersedes and exactly one contradicts; those two make the graph active.
            </p>
            <article
                v-for="v in draft.ontology.verbs"
                :key="v.name"
                class="card"
                :style="{ '--check-accent': config.edgeColor(v.name) }"
            >
                <header class="card-head">
                    <template v-if="renaming?.kind === 'verb' && renaming.from === v.name">
                        <input
                            v-model="renaming.to"
                            class="edit-input rename-input"
                            type="text"
                            :aria-label="`New name for ${v.name}`"
                            @keydown.enter="commitRename"
                            @keydown.escape="renaming = null"
                        />
                        <button class="mini" type="button" :disabled="busy" @click="commitRename">rename</button>
                        <button class="mini ghost" type="button" @click="renaming = null">cancel</button>
                    </template>
                    <template v-else>
                        <span class="card-name mono">{{ v.name }}</span>
                        <span class="spacer" />
                        <button
                            class="mini ghost"
                            type="button"
                            :disabled="dirty || busy"
                            :title="dirty ? 'Save or revert your edits first — renames apply immediately' : 'Rename and retype stored edges'"
                            @click="startRename('verb', v.name)"
                        >
                            rename
                        </button>
                        <button class="mini ghost danger" type="button" @click="removeVerb(v)">remove</button>
                    </template>
                </header>
                <label class="row-label">
                    Reads as
                    <input
                        v-model="v.reads_as"
                        class="edit-input grow"
                        type="text"
                        placeholder="Decision because Principle"
                        :aria-label="`${v.name} example`"
                    />
                </label>
                <div class="checks">
                    <label class="check" title="Creating it archives the older endpoint and chains history — exactly one verb carries this">
                        <input
                            type="radio"
                            name="role-supersession"
                            :checked="v.roles.supersession"
                            @change="setRoleCarrier('supersession', v)"
                        />
                        supersedes
                    </label>
                    <label class="check" title="A judged one demotes the older claim's trust and feeds the conflict worklist — exactly one verb carries this">
                        <input
                            type="radio"
                            name="role-contradiction"
                            :checked="v.roles.contradiction"
                            @change="setRoleCarrier('contradiction', v)"
                        />
                        contradicts
                    </label>
                    <label class="check" title="The reason edge — its absence on reasoning nodes is what the checkup flags">
                        <input v-model="v.roles.reason" type="checkbox" /> reason
                    </label>
                    <label class="check" title="Closes worklist nodes (Resolution answers Problem)">
                        <input v-model="v.roles.answer" type="checkbox" /> answer
                    </label>
                    <label class="check" title="A live dependency / blocker">
                        <input v-model="v.roles.dependency" type="checkbox" /> dependency
                    </label>
                </div>
            </article>
            <button class="mini add" type="button" @click="addVerb">+ add verb</button>
        </section>

        <section class="block">
            <h3 class="block-title">Trust &amp; decay policy</h3>
            <div class="grid">
                <label>start trust <input v-model.number="draft.policy.trust_created" class="edit-input num" type="number" step="0.05" min="0" max="1" /></label>
                <label>confirmed <input v-model.number="draft.policy.trust_confirmed" class="edit-input num" type="number" step="0.05" min="0" max="1" /></label>
                <label>approved <input v-model.number="draft.policy.trust_approved" class="edit-input num" type="number" step="0.05" min="0" max="1" /></label>
                <label>approved floor <input v-model.number="draft.policy.trust_approved_floor" class="edit-input num" type="number" step="0.05" min="0" max="1" /></label>
                <label>floor <input v-model.number="draft.policy.trust_floor" class="edit-input num" type="number" step="0.01" min="0" max="1" /></label>
                <label>stale below <input v-model.number="draft.policy.stale_trust" class="edit-input num" type="number" step="0.05" min="0" max="1" /></label>
                <label>episodic days <input v-model.number="draft.policy.episodic_window_days" class="edit-input num" type="number" min="1" max="36500" /></label>
                <label>volatile days <input v-model.number="draft.policy.volatile_window_days" class="edit-input num" type="number" min="1" max="36500" /></label>
                <label>approved days <input v-model.number="draft.policy.approved_window_days" class="edit-input num" type="number" min="1" max="36500" /></label>
                <label>decay TTL days <input v-model.number="draft.policy.decay_ttl_days" class="edit-input num" type="number" min="1" max="36500" /></label>
                <label>duplicate ≥ <input v-model.number="draft.policy.duplicate_similarity" class="edit-input num" type="number" step="0.01" min="0" max="1" /></label>
                <label>suspect ≥ <input v-model.number="draft.policy.conflict_suspect_similarity" class="edit-input num" type="number" step="0.01" min="0" max="1" /></label>
                <label>warn ≥ <input v-model.number="draft.policy.warn_similarity" class="edit-input num" type="number" step="0.01" min="0" max="1" /></label>
                <label>NLI gate ≥ <input v-model.number="draft.policy.nli_sweep_min_confidence" class="edit-input num" type="number" step="0.05" min="0" max="1" /></label>
            </div>
            <p v-for="(line, i) in policyWords" :key="i" class="hint words">{{ line }}</p>
        </section>

        <section class="block">
            <h3 class="block-title">Brief composition</h3>
            <div class="grid">
                <label>budget (chars) <input v-model.number="draft.brief.total_chars" class="edit-input num wide" type="number" min="1000" max="200000" /></label>
                <label>home reserve <input v-model.number="draft.brief.home_reserve" class="edit-input num wide" type="number" min="0" max="200000" /></label>
            </div>
            <div class="checks">
                <label class="check"><input v-model="draft.brief.tags.show" type="checkbox" /> tags</label>
                <label class="check"><input v-model="draft.brief.conflicts.show" type="checkbox" /> conflicts</label>
                <label class="check"><input v-model="draft.brief.suspects.show" type="checkbox" /> suspects</label>
                <label class="check"><input v-model="draft.brief.recent.show" type="checkbox" /> recent</label>
                <label class="check"><input v-model="draft.brief.open.show" type="checkbox" /> open work</label>
                <label class="check" title="Teach this graph's ontology at the top of every brief — for customized ontologies the assistant's skill can't know">
                    <input v-model="draft.brief.ontology.show" type="checkbox" /> teach ontology
                </label>
            </div>
            <div class="grid">
                <label>tags cap <input v-model.number="draft.brief.tags.cap" class="edit-input num" type="number" min="0" max="100" /></label>
                <label>suspects cap <input v-model.number="draft.brief.suspects.cap" class="edit-input num" type="number" min="0" max="100" /></label>
                <label>recent cap <input v-model.number="draft.brief.recent.cap" class="edit-input num" type="number" min="0" max="100" /></label>
                <label>recent excerpt <input v-model.number="draft.brief.recent.excerpt" class="edit-input num" type="number" min="20" max="2000" /></label>
                <label>open cap <input v-model.number="draft.brief.open.cap" class="edit-input num" type="number" min="0" max="100" /></label>
                <label>open excerpt <input v-model.number="draft.brief.open.excerpt" class="edit-input num" type="number" min="20" max="2000" /></label>
            </div>
            <p class="hint">
                Per-type canon sections (Principles, Decisions, …) are configured on each type
                above. The worklist section shows every type carrying the worklist role.
            </p>
        </section>

        <section class="block">
            <h3 class="block-title">Version tracking</h3>
            <div class="checks">
                <label class="check" title="Stamp every new version-bound note with the graph's current working version; the brief announces it and set_version (MCP) moves it">
                    <input v-model="draft.versioning.enabled" type="checkbox" /> track versions
                    (save to apply)
                </label>
            </div>
            <div v-if="draft.versioning.enabled" class="skill-row">
                <input
                    v-model="currentVersion"
                    class="edit-input grow"
                    type="text"
                    placeholder="current working version — v0.7.0, 26.7.23, …"
                    aria-label="Current working version"
                />
                <button class="mini" type="button" :disabled="busy" @click="applyVersion">
                    {{ currentVersion.trim() ? 'Set' : 'Clear' }}
                </button>
                <span v-if="savedVersion" class="count">now: {{ savedVersion }}</span>
            </div>
            <p v-if="draft.versioning.enabled" class="hint">
                Which types carry the stamp is per-type — the "versioned" check on each type
                card above. Assistants manage the version via the <span class="mono">set_version</span>
                MCP tool; switch history lives in the audit journal.
            </p>
        </section>

        <section class="block">
            <h3 class="block-title">Assistant skill</h3>
            <p class="hint">
                The capture skill teaches the assistant this graph's vocabulary.
                <template v-if="customOntology">
                    This ontology is customized, so the installed file is <strong>generated</strong>
                    from it — reinstall after reshaping types or verbs.
                </template>
                <template v-else>
                    This graph runs the shipped ontology, so the canonical skill text installs
                    verbatim.
                </template>
            </p>
            <div class="skill-row">
                <select v-model="skillVariant" class="edit-select" aria-label="Skill capture intensity">
                    <option value="relaxed">relaxed — capture sparingly</option>
                    <option value="normal">normal — the middle ground</option>
                    <option value="aggressive">aggressive — maximum capture</option>
                </select>
                <button class="mini" type="button" :disabled="busy" @click="installSkill">
                    Install into project
                </button>
            </div>
            <p class="hint">
                Writes <span class="mono">.claude/skills/engram/SKILL.md</span> in the project's
                repository. Symlinked skill folders are left untouched.
            </p>
        </section>
    </template>
</SidePanel>
</template>

<style scoped>
.state {
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
}

.state.error {
    color: #ef4444;
}

.state.ok {
    color: var(--interactive-primary);
}

.block {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
}

.block-title {
    font-size: var(--text-caption);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-tertiary);
}

.hint {
    margin: 0;
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.hint.words {
    color: var(--text-secondary);
}

.preset-line {
    display: flex;
    align-items: center;
    gap: 0.8rem;
}

.preset-text {
    display: flex;
    flex: 1;
    flex-direction: column;
}

.preset-name {
    font-size: var(--text-body-sm);
    font-weight: 600;
    color: var(--text-primary);
}

.preset-desc {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.card {
    display: flex;
    flex-direction: column;
    gap: 0.55rem;
    padding: 0.7rem 0.8rem;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md, 8px);
    background: var(--surface-muted);
}

.card-head {
    display: flex;
    align-items: center;
    gap: 0.6rem;
}

.card-name {
    font-size: var(--text-body-sm);
    font-weight: 600;
    color: var(--text-primary);
}

.count {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.spacer {
    flex: 1;
}

.swatch {
    width: 0.9rem;
    height: 0.9rem;
    border-radius: 50%;
    flex-shrink: 0;
}

.row-label {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.hue-slider {
    flex: 1;
    accent-color: var(--interactive-primary);
}

.hue-value {
    min-width: 2.6rem;
    text-align: right;
    font-family: var(--font-mono);
}

.edit-input,
.edit-select {
    padding: 0.25rem 0.5rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-sm, 6px);
    background: var(--surface-base);
    font-size: var(--text-body-sm);
    color: var(--text-primary);
}

.edit-input.grow {
    flex: 1;
}

.edit-input.num {
    width: 4.6rem;
}

.edit-input.num.wide {
    width: 6.5rem;
}

.rename-input {
    flex: 1;
}

.checks {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.4rem 0.9rem;
}

.check {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: var(--text-caption);
    color: var(--text-secondary);
    cursor: pointer;
}

.grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(11rem, 1fr));
    gap: 0.45rem 0.9rem;
}

.skill-row {
    display: flex;
    align-items: center;
    gap: 0.8rem;
}

.grid > label {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    font-size: var(--text-caption);
    color: var(--text-secondary);
}

.mini {
    padding: 0.2rem 0.6rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-full, 999px);
    background: none;
    font-size: var(--text-caption);
    color: var(--text-secondary);
    cursor: pointer;
}

.mini:disabled {
    opacity: 0.5;
    cursor: default;
}

.mini:hover:not(:disabled) {
    background: var(--interactive-ghost-hover);
    color: var(--text-primary);
}

.mini.ghost {
    border-color: transparent;
}

.mini.danger:hover:not(:disabled) {
    color: #ef4444;
}

.mini.add {
    align-self: flex-start;
}

.bar-btn {
    padding: 0.3rem 0.9rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-full, 999px);
    background: var(--interactive-primary);
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--text-inverse);
    cursor: pointer;
}

.bar-btn:disabled {
    opacity: 0.45;
    cursor: default;
}

.bar-btn.ghost {
    background: none;
    color: var(--text-secondary);
}

.mono {
    font-family: var(--font-mono);
}
</style>
