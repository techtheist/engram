<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import HueRail from '@/components/common/HueRail.vue'
import SegmentedControl from '@/components/common/SegmentedControl.vue'
import SidePanel from '@/components/common/SidePanel.vue'
import StepperInput from '@/components/common/StepperInput.vue'
import ToggleChip from '@/components/common/ToggleChip.vue'
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

const DURABILITY_OPTIONS: { value: Durability; label: string }[] = [
    { value: 'episodic', label: 'Episodic' },
    { value: 'stable', label: 'Stable' },
    { value: 'volatile', label: 'Volatile' },
]

const SKILL_OPTIONS = [
    { value: 'relaxed', label: 'relaxed' },
    { value: 'normal', label: 'normal' },
    { value: 'aggressive', label: 'aggressive' },
]

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

const deliveryWords = computed(() => {
    const p = draft.value?.policy
    if (!p) return []
    return [
        `Search hits scoring under ${pct(p.delivery_floor)} are trimmed before delivery — the benchmark put the edge of the recall-free zone there, so the weak tail stops spending your assistant's attention. When even the best hit stays under ${pct(p.weak_evidence_top)}, the reply says "likely not in memory" — the candidates still arrive, never cut, just labeled — and an empty result says "no memory on this" instead of guessing. All of it applies only while a reranker is loaded: the calibrated scale is the reranker's.`,
        p.knee_cliff != null
            ? `Knee trim is on: when the delivered scores fall by ${pct(p.knee_cliff)} or more in one step, everything past that cliff is tail and stays home. Measured recall-free from 100 to 2000 notes while the answer's share of delivered tokens roughly quadruples on big graphs.`
            : `Knee trim is off: only the fixed floor trims the tail.`,
        p.auto_tune
            ? `Auto-tune is on: past 200 notes and 20 judged look-alike pairs this graph refits its suspect threshold from your own verdicts, and past 50 notes it recalibrates the weak-evidence line from ${p.weak_line_probes} phantom probes (questions about invented subjects that cannot be in memory) at q${Math.round(p.weak_line_quantile * 100)} of what they still score. Both run at session boundaries; every adjustment lands in the audit journal.`
            : `Auto-tune is off: the suspect threshold and the weak-evidence line stay exactly where you set them.`,
    ]
})

const kneeOn = computed({
    get: () => draft.value?.policy.knee_cliff != null,
    set: (v: boolean) => {
        if (draft.value) draft.value.policy.knee_cliff = v ? 0.25 : null
    },
})
const kneeCliff = computed({
    get: () => draft.value?.policy.knee_cliff ?? 0.25,
    set: (v: number) => {
        if (draft.value) draft.value.policy.knee_cliff = v
    },
})

</script>

<template>
<SidePanel
    :open="open"
    side="left"
    panel-id="graph-settings"
    :default-rem="42"
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
                    <HueRail v-model="t.hue" :aria-label="`${t.name} hue`" />
                </label>

                <label class="row-label">
                    <input
                        v-model="t.thought"
                        class="edit-input grow"
                        type="text"
                        placeholder="the thought this type captures — e.g. &quot;we chose this, for a reason&quot;"
                        :aria-label="`${t.name} thought`"
                    />
                </label>

                <div class="row-label">
                    Durability
                    <SegmentedControl
                        v-model="t.durability"
                        :options="DURABILITY_OPTIONS"
                        :aria-label="`${t.name} durability`"
                    />
                    <span class="spacer" />
                    Rank prior
                    <StepperInput
                        v-model="t.roles.rank_prior"
                        :step="0.01"
                        :max="0.5"
                        :decimals="2"
                        :aria-label="`${t.name} rank prior`"
                    />
                </div>

                <div class="checks">
                    <ToggleChip
                        v-model="t.roles.worklist"
                        label="worklist"
                        title="Open/resolved lifecycle: lives in the worklist, never decays while open"
                    />
                    <ToggleChip
                        v-model="t.roles.anchor"
                        label="anchor"
                        title="A code subject: carries code refs, excluded from the conflict scan, renders muted"
                    />
                    <ToggleChip
                        v-model="t.roles.highlight"
                        label="highlight"
                        title="Off renders this type muted (gray-toned) everywhere"
                    />
                    <ToggleChip
                        v-if="draft.versioning.enabled"
                        v-model="t.roles.versioned"
                        label="versioned"
                        title="Stamp new nodes of this type with the current working version (off for types that transcend releases)"
                    />
                    <ToggleChip
                        v-model="t.brief.show"
                        label="brief section"
                        title="Give this type its own canon section in the brief"
                    />
                    <template v-if="t.brief.show">
                        <span class="check">
                            cap
                            <StepperInput v-model="t.brief.cap" :max="100" :aria-label="`${t.name} brief cap`" />
                        </span>
                        <span class="check">
                            excerpt
                            <StepperInput
                                v-model="t.brief.excerpt"
                                :min="20"
                                :max="2000"
                                :step="10"
                                :aria-label="`${t.name} brief excerpt chars`"
                            />
                        </span>
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
                    <ToggleChip
                        :model-value="v.roles.supersession"
                        label="supersedes"
                        title="Creating it archives the older endpoint and chains history — exactly one verb carries this (click to move the role here)"
                        @update:model-value="setRoleCarrier('supersession', v)"
                    />
                    <ToggleChip
                        :model-value="v.roles.contradiction"
                        label="contradicts"
                        title="A judged one demotes the older claim's trust and feeds the conflict worklist — exactly one verb carries this (click to move the role here)"
                        @update:model-value="setRoleCarrier('contradiction', v)"
                    />
                    <ToggleChip
                        v-model="v.roles.reason"
                        label="reason"
                        title="The reason edge — its absence on reasoning nodes is what the checkup flags"
                    />
                    <ToggleChip
                        v-model="v.roles.answer"
                        label="answer"
                        title="Closes worklist nodes (Resolution answers Problem)"
                    />
                    <ToggleChip
                        v-model="v.roles.dependency"
                        label="dependency"
                        title="A live dependency / blocker"
                    />
                </div>
            </article>
            <button class="mini add" type="button" @click="addVerb">+ add verb</button>
        </section>

        <section class="block">
            <h3 class="block-title">Trust &amp; decay policy</h3>
            <div class="grid">
                <label>start trust <StepperInput v-model="draft.policy.trust_created" :step="0.05" :max="1" aria-label="start trust" /></label>
                <label>confirmed <StepperInput v-model="draft.policy.trust_confirmed" :step="0.05" :max="1" aria-label="confirmed trust" /></label>
                <label>approved <StepperInput v-model="draft.policy.trust_approved" :step="0.05" :max="1" aria-label="approved trust" /></label>
                <label>approved floor <StepperInput v-model="draft.policy.trust_approved_floor" :step="0.05" :max="1" aria-label="approved floor" /></label>
                <label>floor <StepperInput v-model="draft.policy.trust_floor" :step="0.01" :max="1" aria-label="trust floor" /></label>
                <label>stale below <StepperInput v-model="draft.policy.stale_trust" :step="0.05" :max="1" aria-label="stale threshold" /></label>
                <label>episodic days <StepperInput v-model="draft.policy.episodic_window_days" :min="1" :max="36500" aria-label="episodic window days" /></label>
                <label>volatile days <StepperInput v-model="draft.policy.volatile_window_days" :min="1" :max="36500" aria-label="volatile window days" /></label>
                <label>approved days <StepperInput v-model="draft.policy.approved_window_days" :min="1" :max="36500" aria-label="approved window days" /></label>
                <label>decay TTL days <StepperInput v-model="draft.policy.decay_ttl_days" :min="1" :max="36500" aria-label="decay TTL days" /></label>
                <label>duplicate ≥ <StepperInput v-model="draft.policy.duplicate_similarity" :step="0.01" :max="1" aria-label="duplicate similarity" /></label>
                <label>suspect ≥ <StepperInput v-model="draft.policy.conflict_suspect_similarity" :step="0.01" :max="1" aria-label="suspect similarity" /></label>
                <label>warn ≥ <StepperInput v-model="draft.policy.warn_similarity" :step="0.01" :max="1" aria-label="warn similarity" /></label>
                <label>NLI gate ≥ <StepperInput v-model="draft.policy.nli_sweep_min_confidence" :step="0.05" :max="1" aria-label="NLI gate" /></label>
            </div>
            <p v-for="(line, i) in policyWords" :key="i" class="hint words">{{ line }}</p>
        </section>

        <section class="block">
            <h3 class="block-title">Calibrated delivery</h3>
            <div class="grid">
                <label>delivery floor <StepperInput v-model="draft.policy.delivery_floor" :step="0.01" :max="1" aria-label="delivery floor" /></label>
                <label>weak below <StepperInput v-model="draft.policy.weak_evidence_top" :step="0.01" :max="1" aria-label="weak evidence line" /></label>
                <label v-if="kneeOn">knee cliff ≥ <StepperInput v-model="kneeCliff" :step="0.05" :max="1" aria-label="knee cliff" /></label>
                <label>weak-line q <StepperInput v-model="draft.policy.weak_line_quantile" :step="0.05" :max="1" aria-label="weak-line quantile" /></label>
            </div>
            <div class="checks">
                <ToggleChip
                    v-model="kneeOn"
                    label="knee trim"
                    title="Cut delivery at the largest relative drop in the score curve — the cliff between the relevance head and the noise tail. Measured recall-free at every graph size"
                />
                <ToggleChip
                    v-model="draft.policy.auto_tune"
                    label="auto-tune"
                    title="At session boundaries the graph refits its conflict threshold from your judgment history and its weak-evidence line from phantom probes — journaled, and off means never"
                />
            </div>
            <p v-for="(line, i) in deliveryWords" :key="i" class="hint words">{{ line }}</p>
        </section>

        <section class="block">
            <h3 class="block-title">Brief composition</h3>
            <div class="grid">
                <label>budget (chars) <StepperInput v-model="draft.brief.total_chars" :min="1000" :max="200000" :step="1000" aria-label="brief budget chars" /></label>
                <label>home reserve <StepperInput v-model="draft.brief.home_reserve" :max="200000" :step="500" aria-label="home reserve chars" /></label>
            </div>
            <div class="checks">
                <ToggleChip v-model="draft.brief.tags.show" label="tags" />
                <ToggleChip v-model="draft.brief.conflicts.show" label="conflicts" />
                <ToggleChip v-model="draft.brief.suspects.show" label="suspects" />
                <ToggleChip v-model="draft.brief.recent.show" label="recent" />
                <ToggleChip v-model="draft.brief.open.show" label="open work" />
                <ToggleChip
                    v-model="draft.brief.ontology.show"
                    label="teach ontology"
                    title="Teach this graph's ontology at the top of every brief — for customized ontologies the assistant's skill can't know"
                />
            </div>
            <div class="grid">
                <label>tags cap <StepperInput v-model="draft.brief.tags.cap" :max="100" aria-label="tags cap" /></label>
                <label>suspects cap <StepperInput v-model="draft.brief.suspects.cap" :max="100" aria-label="suspects cap" /></label>
                <label>recent cap <StepperInput v-model="draft.brief.recent.cap" :max="100" aria-label="recent cap" /></label>
                <label>recent excerpt <StepperInput v-model="draft.brief.recent.excerpt" :min="20" :max="2000" :step="10" aria-label="recent excerpt" /></label>
                <label>open cap <StepperInput v-model="draft.brief.open.cap" :max="100" aria-label="open cap" /></label>
                <label>open excerpt <StepperInput v-model="draft.brief.open.excerpt" :min="20" :max="2000" :step="10" aria-label="open excerpt" /></label>
            </div>
            <p class="hint">
                Per-type canon sections (Principles, Decisions, …) are configured on each type
                above. The worklist section shows every type carrying the worklist role.
            </p>
        </section>

        <section class="block">
            <h3 class="block-title">Version tracking</h3>
            <div class="checks">
                <ToggleChip
                    v-model="draft.versioning.enabled"
                    label="track versions"
                    title="Stamp every new version-bound note with the graph's current working version; the brief announces it and set_version (MCP) moves it"
                />
                <span class="check">(save to apply)</span>
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
                <SegmentedControl
                    v-model="skillVariant"
                    :options="SKILL_OPTIONS"
                    aria-label="Skill capture intensity"
                />
                <button class="mini" type="button" :disabled="busy" @click="installSkill">
                    Install into project
                </button>
            </div>
            <p class="hint">
                relaxed captures sparingly · normal is the middle ground · aggressive is
                maximum capture.
            </p>
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
    gap: 0.7rem;
    padding: 0.9rem 1rem;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md, 8px);
    background-color: var(--surface-muted);

    /* The type's own hue marks the card's spine — a gradient, not a border,
       so it follows the rounded corners instead of cutting at the curve.
       Sized to the strip: full-width gradient textures bleed a 1px wrap
       line at the far edge on transformed/fractional-scale surfaces. */
    background-image: linear-gradient(
        90deg,
        var(--check-accent, var(--border-strong)) 0,
        var(--check-accent, var(--border-strong)) 0.3rem,
        color-mix(in srgb, var(--check-accent, var(--border-strong)) calc(8% * var(--accent-wash, 1)), transparent) 0.3rem,
        transparent 100%
    );
    background-repeat: no-repeat;
    background-size: 5rem 100%;
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

.edit-input {
    padding: 0.5rem 0.9rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md);
    background: var(--surface-sunken);
    font-size: var(--text-body-sm);
    color: var(--text-primary);
}

.edit-input.grow {
    flex: 1;
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
    grid-template-columns: repeat(auto-fill, minmax(16rem, 1fr));
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
