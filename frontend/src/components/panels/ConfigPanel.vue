<script setup lang="ts">
import NoteText from '@/components/common/NoteText.vue'
import TextField from '@/components/common/TextField.vue'
import PanelSection from '@/components/common/PanelSection.vue'
import UiButton from '@/components/common/UiButton.vue'
import AutoTextarea from '@/components/common/AutoTextarea.vue'
import { computed, ref, watch } from 'vue'
import HueRail from '@/components/common/HueRail.vue'
import SegmentedControl from '@/components/common/SegmentedControl.vue'
import SelectMenu from '@/components/common/SelectMenu.vue'
import SidePanel from '@/components/common/SidePanel.vue'
import StepperInput from '@/components/common/StepperInput.vue'
import ToggleChip from '@/components/common/ToggleChip.vue'
import ToggleSwitch from '@/components/common/ToggleSwitch.vue'
import { useGraphSettings } from '@/composables/useGraphSettings'
import { humanDays, pct } from '@/constants/trust'
import { useConfigStore } from '@/stores/config'
import { useGraphStore } from '@/stores/graph'
import { api } from '@/services/api'
import type { Durability, FieldDef, GraphConfig, HistoryStatus, TypeDef, VerbDef } from '@/types/graph'
import { confirmDialog } from '@/composables/useConfirm'

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
        void loadHistory()
    }
})

// ---- session history (0.8.4) ----------------------------------------------

const historyStatus = ref<HistoryStatus | null>(null)

async function loadHistory(): Promise<void> {
    try {
        historyStatus.value = await api.historyStatus()
    } catch {
        historyStatus.value = null
    }
}

/** The exclude-path editor lives behind a button, in a small modal —
 *  one absolute path per line. Edits land in the draft; Save persists. */
const showExcludeEditor = ref(false)
const excludePathsText = computed({
    get: () => draft.value?.history.exclude_paths.join('\n') ?? '',
    set: (text: string) => {
        if (!draft.value) return
        draft.value.history.exclude_paths = text
            .split('\n')
            .map((l) => l.trim())
            .filter((l) => l.length > 0)
    },
})

// The main agent roster (harness.rs AGENTS), one toggle each.
const HARNESS_LABELS: Array<{ key: keyof GraphConfig['history']['harnesses']; label: string }> = [
    { key: 'claude_code', label: 'Claude Code' },
    { key: 'codex', label: 'Codex' },
    { key: 'opencode', label: 'opencode' },
    { key: 'kilo', label: 'Kilo' },
    { key: 'gemini', label: 'Gemini CLI' },
    { key: 'antigravity', label: 'Antigravity' },
    { key: 'bob', label: 'Bob' },
]

/** Wholesale delete: user-only, like curated hard delete. */
async function wipeHistory(): Promise<void> {
    const n = historyStatus.value?.stats?.nodes ?? 0
    const ok = await confirmDialog({
        title: "Delete this project's session history?",
        message:
            `Every recorded session${n ? ` (${n} nodes)` : ''} goes. The curated graph is untouched. ` +
            'Transcripts on disk are indexed again from scratch only while recording stays on. ' +
            'This cannot be undone.',
        confirmLabel: 'Delete history',
        danger: true,
    })
    if (!ok) return
    busy.value = true
    error.value = null
    try {
        await api.historyReset()
        await loadHistory()
        flash('Session history wiped — the store re-seeds empty.')
    } catch (e) {
        error.value = shortHttpError(e)
    } finally {
        busy.value = false
    }
}

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
        !(await confirmDialog({
            title: `Apply the "${preset.name}" ontology?`,
            message:
                'Types that already hold notes cannot be dropped, so on a non-empty graph this only ' +
                'works when the type names line up (or after retyping). Policy, brief, version ' +
                'tracking and custom fields are kept.',
            confirmLabel: 'Apply ontology',
        }))
    ) {
        return
    }
    busy.value = true
    error.value = null
    try {
        await config.applyPreset(preset.id)
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
const FIELD_KIND_OPTIONS = ['text', 'number', 'bool', 'date', 'enum', 'url'].map((k) => ({ value: k, label: k }))
const CANON_ORDER_OPTIONS = [
    { value: 'endorsed', label: 'endorsed — pinned, approved, newest' },
    { value: 'connected', label: 'connected — trust × links' },
]

const SKILL_OPTIONS = [
    { value: 'relaxed', label: 'relaxed' },
    { value: 'normal', label: 'normal' },
    { value: 'aggressive', label: 'aggressive' },
]

/** Which type/verb is mid-rename (renames bypass the draft: they bulk-retype). */
const renaming = ref<{ kind: 'type' | 'verb' | 'field'; from: string; to: string } | null>(null)

function startRename(kind: 'type' | 'verb' | 'field', from: string): void {
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
                : r.kind === 'verb'
                  ? await config.renameVerb(r.from, r.to.trim())
                  : await config.renameField(r.from, r.to.trim())
        resetDraft()
        const what = r.kind === 'type' ? 'node' : r.kind === 'verb' ? 'edge' : 'node value'
        flash(
            `Renamed ${r.from} → ${r.to.trim()} — ${renamed} stored ${what}${renamed === 1 ? '' : 's'} followed.`,
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
        roles: {
            worklist: false,
            anchor: false,
            rank_prior: 0,
            highlight: true,
            versioned: true,
            tombstone: false,
        },
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

function addField(): void {
    if (!draft.value) return
    draft.value.fields.push({
        name: nextName('new_field', draft.value.fields.map((f) => f.name)),
        label: '',
        kind: 'text',
        values: [],
        required: false,
        applies_to: [],
        indexed: false,
        show_in_brief: false,
    })
}

function removeField(f: FieldDef): void {
    if (!draft.value) return
    // No in-use guard: values on stored nodes stay inert (schemaless) — the
    // definition is what's removed. Say so instead of silently proceeding.
    const used = graph.nodeList.filter((n) => n.fields && f.name in n.fields).length
    if (used > 0) {
        flash(
            `"${f.name}" removed from the config — ${used} node${used === 1 ? '' : 's'} still carry a value; it stays stored but hidden until re-declared.`,
        )
    }
    draft.value.fields = draft.value.fields.filter((x) => x !== f)
}

/** The enum vocabulary as one editable comma-joined line. */
function valuesLine(f: FieldDef): string {
    return f.values.join(', ')
}
function setValuesLine(f: FieldDef, line: string): void {
    f.values = line
        .split(',')
        .map((v) => v.trim())
        .filter((v) => v.length > 0)
}

function toggleAppliesTo(f: FieldDef, type: string): void {
    if (f.applies_to.includes(type)) {
        f.applies_to = f.applies_to.filter((t) => t !== type)
    } else {
        f.applies_to = [...f.applies_to, type]
    }
}

/** The built-in node fields, for the reference block — the whole picture
 *  beside the custom ones. */
const BUILT_IN_FIELDS: Array<{ name: string; kind: string; what: string }> = [
    { name: 'id', kind: 'id', what: 'stable node id, assigned at creation' },
    { name: 'type', kind: 'enum', what: "one of this graph's node types" },
    { name: 'title', kind: 'text', what: 'one-sentence summary — the claim itself' },
    { name: 'body', kind: 'text', what: 'the reasoning; markdown' },
    { name: 'durability', kind: 'enum', what: 'stable | episodic | volatile — how knowledge ages' },
    { name: 'source', kind: 'enum', what: 'user | claude — who wrote it' },
    { name: 'tags', kind: 'list', what: 'free-form slice labels' },
    { name: 'code_refs', kind: 'list', what: 'repo-relative paths, drift-checked' },
    { name: 'status', kind: 'enum', what: 'open | resolved | obsolete on worklist types' },
    { name: 'created_at', kind: 'date', what: 'when the knowledge was captured' },
    { name: 'confirmed_at', kind: 'date', what: 'last deliberate re-validation' },
    { name: 'approved_at', kind: 'date', what: 'last explicit user approval' },
    { name: 'valid_until', kind: 'date', what: 'set = archived (superseded or decayed)' },
    { name: 'version', kind: 'text', what: 'project version captured at (version tracking)' },
    { name: 'session_id', kind: 'text', what: 'the session that wrote it' },
    { name: 'trust', kind: 'number', what: 'computed at read time — never stored' },
]

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
/** The inherits role's value when unset — mirrors VerbDef::inherits in
 *  engram-core (dependency and reason verbs, and builds-on, inherit). */
function derivedInherits(v: VerbDef): boolean {
    return v.roles.dependency || v.roles.reason || v.name === 'builds-on'
}

function inheritsOf(v: VerbDef): boolean {
    return v.roles.inherits ?? derivedInherits(v)
}

/** An explicit value is stored only where it overrides the derived one;
 *  clicking back to the default returns the verb to "auto". */
function setInherits(v: VerbDef, on: boolean): void {
    v.roles.inherits = on === derivedInherits(v) ? null : on
}

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
        `Writes ${pct(p.duplicate_similarity)} similar to an existing same-type note merge instead of duplicating; ${pct(p.conflict_suspect_similarity)}–${pct(p.duplicate_similarity)} pairs queue as suspected conflicts; anything above ${pct(p.warn_similarity)} near contradicted or superseded knowledge warns the writer. The NLI sweep only queues pairs it is ${pct(p.nli_sweep_min_confidence)} sure about.${p.conflict_nli_gate != null ? ` Below the similarity floor, a pair whose titles the logic layer reads as contradicting at ${pct(p.conflict_nli_gate)} or more — and that name the same subject — still queues.` : ' The title-contradiction path is off (gate 0).'}`,
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
        p.session_diversity_demote > 0
            ? `Session-diverse delivery is on: when more strong candidates survive the floor than fit the list, each extra hit from a session already holding a slot is demoted ${p.session_diversity_demote} rank position${p.session_diversity_demote === 1 ? '' : 's'} — one session's restatements stop crowding out other sessions' evidence. Measured recall-free at the shipped value; scores and the verdict are untouched.`
            : `Session-diverse delivery is off: the result list is cut purely by rank, however many hits share a session.`,
        p.twin_trust_order != null
            ? `Endorsed twins go first: when two delivered hits are ${pct(p.twin_trust_order)} alike or more — near-identical notes — the one you pinned or approved ranks above the other, whatever its capture date or type; between two you endorsed alike, the one endorsed last. Nothing else moves.`
            : `Twin order is off: between two near-identical notes, capture date and type decide the order, not your endorsements.`,
    ]
})

// Brief shape (0.9.9): both keys are optional on the wire — a daemon older
// than the pane never sends them — so the controls read the defaults.
const briefBodies = computed({
    get: () => draft.value?.brief.bodies ?? true,
    set: (v: boolean) => {
        if (draft.value) draft.value.brief.bodies = v
    },
})
const canonOrder = computed({
    get: () => draft.value?.brief.canon_order ?? 'endorsed',
    set: (v: string) => {
        if (draft.value) draft.value.brief.canon_order = v === 'connected' ? 'connected' : 'endorsed'
    },
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
// The title-contradiction gate: a null gate is the path switched off, and
// the stepper shows that as 0 — a gate of zero would queue every judged pair,
// so zero has no other meaning to lose.
const nliGate = computed({
    get: () => draft.value?.policy.conflict_nli_gate ?? 0,
    set: (v: number) => {
        if (draft.value) draft.value.policy.conflict_nli_gate = v > 0 ? v : null
    },
})
// Twin order: null is off, shown as 0 — a cosine band of zero would make
// every pair of hits "twins", so zero has no other meaning to lose.
const twinOrder = computed({
    get: () => draft.value?.policy.twin_trust_order ?? 0,
    set: (v: number) => {
        if (draft.value) draft.value.policy.twin_trust_order = v > 0 ? v : null
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
        <UiButton v-if="dirty" size="sm" variant="ghost" :disabled="busy" @click="resetDraft">Revert</UiButton>
        <UiButton size="sm" variant="primary" :disabled="!dirty || busy" @click="save">Save</UiButton>
    </template>

    <NoteText v-if="error" tone="error">{{ error }}</NoteText>
    <NoteText v-else-if="notice" tone="ok">{{ notice }}</NoteText>

    <template v-if="draft">
        <PanelSection title="Ontology presets">
            <NoteText>
                A preset replaces the whole configuration — types, verbs, policy, brief. The
                current shape is <strong>{{ draft.ontology.preset }}</strong>.
            </NoteText>
            <div v-for="p in config.presets" :key="p.id" class="preset-line">
                <div class="preset-text">
                    <span class="preset-name">{{ p.name }}</span>
                    <span class="preset-desc">{{ p.description }}</span>
                </div>
                <UiButton
                    size="sm"
                    :disabled="busy || p.id === draft.ontology.preset"
                    @click="applyPreset(p.id)"
                >
                    {{ p.id === draft.ontology.preset ? 'active' : 'apply' }}
                </UiButton>
            </div>
        </PanelSection>

        <PanelSection title="Node types">
            <NoteText>
                The name is what the assistant writes; the roles are what the engine does with
                it. Renaming bulk-retypes every stored node — nothing is lost.
            </NoteText>
            <article
                v-for="t in draft.ontology.types"
                :key="t.name"
                class="card"
                :style="{ '--check-accent': swatch(t) }"
            >
                <header class="card-head">
                    <span class="swatch" :style="{ background: swatch(t) }" />
                    <template v-if="renaming?.kind === 'type' && renaming.from === t.name">
                        <TextField
                            v-model="renaming.to"
                            :aria-label="`New name for ${t.name}`"
                            class="rename-input"
                            @keydown.enter="commitRename" @keydown.escape="renaming = null"
                        />
                        <UiButton size="sm" :disabled="busy" @click="commitRename">
                            rename {{ countNodes(t.name) ? `(${countNodes(t.name)} nodes follow)` : '' }}
                        </UiButton>
                        <UiButton size="sm" variant="ghost" @click="renaming = null">cancel</UiButton>
                    </template>
                    <template v-else>
                        <span class="card-name">{{ t.name }}</span>
                        <span v-if="countNodes(t.name)" class="count">{{ countNodes(t.name) }} nodes</span>
                        <span class="spacer" />
                        <UiButton
                            size="sm" variant="ghost"
                            :disabled="dirty || busy"
                            :title="dirty ? 'Save or revert your edits first — renames apply immediately' : 'Rename and bulk-retype stored nodes'"
                            @click="startRename('type', t.name)"
                        >
                            rename
                        </UiButton>
                        <UiButton size="sm" variant="danger" @click="removeType(t)">
                            remove
                        </UiButton>
                    </template>
                </header>

                <label class="row-label">
                    Hue
                    <HueRail v-model="t.hue" :aria-label="`${t.name} hue`" />
                </label>

                <label class="row-label">
                    <TextField
                        v-model="t.thought"
                        placeholder="the thought this type captures — e.g. &quot;we chose this, for a reason&quot;"
                        :aria-label="`${t.name} thought`" class="grow"
                    />
                </label>

                <div class="row-label wrap">
                    <span class="row-group">
                        Durability
                        <SegmentedControl
                            v-model="t.durability"
                            :options="DURABILITY_OPTIONS"
                            :aria-label="`${t.name} durability`"
                        />
                    </span>
                    <span class="spacer" />
                    <span class="row-group">
                        Rank prior
                        <StepperInput
                            v-model="t.roles.rank_prior"
                            :step="0.01"
                            :max="0.5"
                            :decimals="2"
                            :aria-label="`${t.name} rank prior`"
                        />
                    </span>
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
                        v-model="t.roles.tombstone"
                        label="tombstone"
                        title="A deletion marker: records deliberately removed knowledge so it isn't re-learned; sits out the conflict scan; delete offers to mint one"
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
                </div>

                <div v-if="t.brief.show" class="checks brief-nums">
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
                </div>
            </article>
            <UiButton size="sm" class="add" @click="addType">+ add type</UiButton>
        </PanelSection>

        <PanelSection title="Edge verbs">
            <NoteText>
                A triple must read as English — "A {{ config.supersessionVerb }} B". Exactly one
                verb supersedes and exactly one contradicts; those two make the graph active.
            </NoteText>
            <article
                v-for="v in draft.ontology.verbs"
                :key="v.name"
                class="card"
                :style="{ '--check-accent': config.edgeColor(v.name) }"
            >
                <header class="card-head">
                    <template v-if="renaming?.kind === 'verb' && renaming.from === v.name">
                        <TextField
                            v-model="renaming.to"
                            :aria-label="`New name for ${v.name}`"
                            class="rename-input"
                            @keydown.enter="commitRename" @keydown.escape="renaming = null"
                        />
                        <UiButton size="sm" :disabled="busy" @click="commitRename">rename</UiButton>
                        <UiButton size="sm" variant="ghost" @click="renaming = null">cancel</UiButton>
                    </template>
                    <template v-else>
                        <span class="card-name mono">{{ v.name }}</span>
                        <span class="spacer" />
                        <UiButton
                            size="sm" variant="ghost"
                            :disabled="dirty || busy"
                            :title="dirty ? 'Save or revert your edits first — renames apply immediately' : 'Rename and retype stored edges'"
                            @click="startRename('verb', v.name)"
                        >
                            rename
                        </UiButton>
                        <UiButton size="sm" variant="danger" @click="removeVerb(v)">remove</UiButton>
                    </template>
                </header>
                <label class="row-label">
                    Reads as
                    <TextField
                        v-model="v.reads_as"
                        placeholder="Decision because Principle"
                        :aria-label="`${v.name} example`" class="grow"
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
                    <ToggleChip
                        :model-value="inheritsOf(v)"
                        :label="v.roles.inherits == null ? 'inherits · auto' : 'inherits'"
                        :title="`When a note contradicts or supersedes the target, the source inherits the contradiction and is queued for review against it. ${v.roles.inherits == null ? 'Auto: follows the reason and dependency roles (and builds-on) — click to override.' : 'Set explicitly — click back to the default to return to auto.'}`"
                        @update:model-value="setInherits(v, $event)"
                    />
                </div>
            </article>
            <UiButton size="sm" class="add" @click="addVerb">+ add verb</UiButton>
        </PanelSection>

        <PanelSection title="Custom fields">
            <NoteText>
                Your own first-class values on every note — enforced at write time for every
                surface (assistant writes included), beside the built-ins below. A
                <span class="mono">required</span> field refuses writes that omit it;
                <span class="mono">indexed</span> puts the value into search.
            </NoteText>
            <details class="builtin-ref">
                <summary>Built-in fields (the whole picture)</summary>
                <table class="builtin-table">
                    <tbody>
                        <tr v-for="b in BUILT_IN_FIELDS" :key="b.name">
                            <td class="mono">{{ b.name }}</td>
                            <td class="mono dim">{{ b.kind }}</td>
                            <td>{{ b.what }}</td>
                        </tr>
                    </tbody>
                </table>
                <NoteText>
                    Custom field names can't shadow any of these — Save refuses the collision.
                </NoteText>
            </details>
            <article v-for="f in draft.fields" :key="f.name" class="card">
                <header class="card-head">
                    <template v-if="renaming?.kind === 'field' && renaming.from === f.name">
                        <TextField
                            v-model="renaming.to"
                            :aria-label="`New name for ${f.name}`"
                            class="rename-input"
                            @keydown.enter="commitRename" @keydown.escape="renaming = null"
                        />
                        <UiButton size="sm" :disabled="busy" @click="commitRename">rename</UiButton>
                        <UiButton size="sm" variant="ghost" @click="renaming = null">cancel</UiButton>
                    </template>
                    <template v-else>
                        <span class="card-name mono">{{ f.name }}</span>
                        <span class="spacer" />
                        <UiButton
                            size="sm" variant="ghost"
                            :disabled="dirty || busy"
                            :title="dirty ? 'Save or revert your edits first — renames apply immediately' : 'Rename and move every stored value with it'"
                            @click="startRename('field', f.name)"
                        >
                            rename
                        </UiButton>
                        <UiButton size="sm" variant="danger" @click="removeField(f)">remove</UiButton>
                    </template>
                </header>
                <div class="field-rows">
                    <label class="row-label">
                        Label
                        <TextField
                            v-model="f.label"
                            :placeholder="f.name"
                            :aria-label="`${f.name} display label`" class="grow"
                        />
                    </label>
                    <label class="row-label">
                        Kind
                        <SelectMenu v-model="f.kind" :options="FIELD_KIND_OPTIONS" :aria-label="`${f.name} kind`" />
                    </label>
                    <label v-if="f.kind === 'enum'" class="row-label">
                        Values
                        <TextField
                            :model-value="valuesLine(f)"
                            class="grow"
                            placeholder="low, medium, high"
                            :aria-label="`${f.name} enum values`"
                            @change="setValuesLine(f, ($event.target as HTMLInputElement).value)"
                        />
                    </label>
                </div>
                <div class="checks">
                    <ToggleChip
                        v-model="f.required"
                        label="required"
                        title="Writes of applicable types that omit this field are refused"
                    />
                    <ToggleChip
                        v-model="f.indexed"
                        label="indexed"
                        title="The value joins search — embedded with the note and in the keyword channel"
                    />
                    <ToggleChip
                        v-model="f.show_in_brief"
                        label="in brief"
                        title="Render name: value on this note's brief lines"
                    />
                </div>
                <div class="checks applies">
                    <span class="dim applies-label">applies to{{ f.applies_to.length === 0 ? ' (all types)' : '' }}:</span>
                    <ToggleChip
                        v-for="t in draft.ontology.types"
                        :key="t.name"
                        :model-value="f.applies_to.includes(t.name)"
                        :label="t.name"
                        :title="`Limit ${f.name} to ${t.name} notes — no selection = every type`"
                        @update:model-value="toggleAppliesTo(f, t.name)"
                    />
                </div>
            </article>
            <UiButton size="sm" class="add" @click="addField">+ add field</UiButton>
        </PanelSection>

        <PanelSection title="Trust &amp; decay policy">
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
                <label>title contradiction ≥ <StepperInput v-model="nliGate" :step="0.05" :max="1" aria-label="title contradiction gate (0 = off)" /></label>
            </div>
            <NoteText v-for="(line, i) in policyWords" :key="i" tone="body">{{ line }}</NoteText>
        </PanelSection>

        <PanelSection title="Calibrated delivery">
            <div class="grid">
                <label>delivery floor <StepperInput v-model="draft.policy.delivery_floor" :step="0.01" :max="1" aria-label="delivery floor" /></label>
                <label>weak below <StepperInput v-model="draft.policy.weak_evidence_top" :step="0.01" :max="1" aria-label="weak evidence line" /></label>
                <label v-if="kneeOn">knee cliff ≥ <StepperInput v-model="kneeCliff" :step="0.05" :max="1" aria-label="knee cliff" /></label>
                <label>weak-line q <StepperInput v-model="draft.policy.weak_line_quantile" :step="0.05" :max="1" aria-label="weak-line quantile" /></label>
                <label>session demote <StepperInput v-model="draft.policy.session_diversity_demote" :step="1" :max="50" aria-label="session diversity demotion" /></label>
                <label>endorsed twins ≥ <StepperInput v-model="twinOrder" :step="0.01" :max="1" aria-label="twin order band (0 = off)" /></label>
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
            <NoteText v-for="(line, i) in deliveryWords" :key="i" tone="body">{{ line }}</NoteText>
        </PanelSection>

        <PanelSection title="Brief composition">
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
                <ToggleChip
                    v-model="briefBodies"
                    label="note bodies"
                    title="Entries carry a body excerpt after the title. Off = titles only: worth it when titles are already whole claims, so the same budget holds more entries"
                />
            </div>
            <div class="grid">
                <label class="full-row">
                    canon order
                    <SelectMenu v-model="canonOrder" :options="CANON_ORDER_OPTIONS" aria-label="canon order" />
                </label>
            </div>
            <div class="grid">
                <label>tags cap <StepperInput v-model="draft.brief.tags.cap" :max="100" aria-label="tags cap" /></label>
                <label>suspects cap <StepperInput v-model="draft.brief.suspects.cap" :max="100" aria-label="suspects cap" /></label>
                <label>recent cap <StepperInput v-model="draft.brief.recent.cap" :max="100" aria-label="recent cap" /></label>
                <label>recent excerpt <StepperInput v-model="draft.brief.recent.excerpt" :min="20" :max="2000" :step="10" aria-label="recent excerpt" /></label>
                <label>open cap <StepperInput v-model="draft.brief.open.cap" :max="100" aria-label="open cap" /></label>
                <label>open excerpt <StepperInput v-model="draft.brief.open.excerpt" :min="20" :max="2000" :step="10" aria-label="open excerpt" /></label>
            </div>
            <NoteText>
                Per-type canon sections (Principles, Decisions, …) are configured on each type
                above. The worklist section shows every type carrying the worklist role.
            </NoteText>
        </PanelSection>

        <PanelSection title="Tasks">
            <ToggleSwitch
                :model-value="draft.tasks?.enabled !== false"
                label="Keep a task list"
                title="The working list beside the graph: the Tasks screen, the brief's Tasks section and the assistant's task tools"
                @update:model-value="draft.tasks = { enabled: $event }"
            />
            <NoteText>
                Off hides the Tasks screen and the brief section; the assistant's task tools then
                answer that tasks are off and it works without them. Stored tasks are kept and
                return when you turn this back on.
            </NoteText>
            <template v-if="draft.tasks?.enabled !== false && draft.brief.tasks">
                <div class="checks">
                    <ToggleChip
                        v-model="draft.brief.tasks.show"
                        label="in the brief"
                        title="The ## Tasks section: the active working list, titles only, right after the current cycle"
                    />
                </div>
                <div v-if="draft.brief.tasks.show" class="grid">
                    <label>tasks cap <StepperInput v-model="draft.brief.tasks.cap" :max="100" aria-label="tasks cap" /></label>
                    <label>recently done <StepperInput v-model="draft.brief.tasks.recent_done" :max="20" aria-label="recently done tasks" /></label>
                    <label title="Sessions that briefed without an open task changing before it is marked idle (0 = never)">idle after (sessions) <StepperInput v-model="draft.brief.tasks.idle_sessions" :max="100" aria-label="idle after sessions" /></label>
                </div>
            </template>
        </PanelSection>

        <PanelSection title="Version tracking">
            <div class="checks">
                <ToggleChip
                    v-model="draft.versioning.enabled"
                    label="track versions"
                    title="Stamp every new version-bound note with the graph's current working version; the brief announces it and set_version (MCP) moves it"
                />
                <span class="check">(save to apply)</span>
            </div>
            <div v-if="draft.versioning.enabled" class="skill-row">
                <TextField
                    v-model="currentVersion"
                    placeholder="current working version — v0.7.0, 26.7.23, …"
                    aria-label="Current working version" class="grow"
                />
                <UiButton size="sm" :disabled="busy" @click="applyVersion">
                    {{ currentVersion.trim() ? 'Set' : 'Clear' }}
                </UiButton>
                <span v-if="savedVersion" class="count">now: {{ savedVersion }}</span>
            </div>
            <NoteText v-if="draft.versioning.enabled">
                Which types carry the stamp is per-type — the "versioned" check on each type
                card above. Assistants manage the version via the <span class="mono">set_version</span>
                MCP tool; switch history lives in the audit journal.
            </NoteText>
        </PanelSection>

        <PanelSection title="Session history">
            <NoteText>
                Opt-in: when recording is on, the daemon indexes your coding-assistant
                conversations into a separate store, sealed at rest — searchable only as
                a labeled fall-through when curated memory likely doesn't hold the
                answer, and browsable in the history view. It never mixes with the
                curated graph. Sealing keeps a key in your OS keystore, so macOS may ask
                for keychain access when it starts.
            </NoteText>
            <div class="switch-rows">
                <ToggleSwitch
                    v-model="draft.history.enabled"
                    label="Record sessions"
                    title="Harvest this project's transcripts into the history layer; off = the store closes and the backlog waits"
                />
                <ToggleSwitch
                    v-model="draft.history.search_fallthrough"
                    :disabled="!draft.history.enabled"
                    label="History in search"
                    title="Allow recorded dialogue to appear as the labeled fall-through section when the confidence verdict says the answer is likely not in curated memory"
                />
                <span class="check">(save to apply)</span>
            </div>
            <div v-if="draft.history.enabled" class="checks">
                <span class="check">record from:</span>
                <ToggleChip
                    v-for="h in HARNESS_LABELS"
                    :key="h.key"
                    v-model="draft.history.harnesses[h.key]"
                    :label="h.label"
                    :title="`Ingest ${h.label} transcripts`"
                />
            </div>
            <div v-if="draft.history.enabled" class="skill-row">
                <UiButton size="sm" @click="showExcludeEditor = true">
                    Set ignored paths…
                </UiButton>
                <span class="check">
                    {{ draft.history.exclude_paths.length || 'no' }} path{{
                        draft.history.exclude_paths.length === 1 ? '' : 's'
                    }} excluded
                </span>
            </div>
            <NoteText v-if="historyStatus">
                <template v-if="historyStatus.stats">
                    Recorded so far: {{ historyStatus.stats.nodes }} nodes
                    ({{ historyStatus.stats.embedded }} embedded) in
                    <span class="mono">.engram/history.tepin</span>.
                </template>
                <template v-else-if="!historyStatus.open">
                    The history store is closed{{ historyStatus.enabled ? ' (opening on next write)' : ' — recording is off' }}.
                </template>
            </NoteText>
            <div class="skill-row">
                <UiButton size="sm" variant="danger" :disabled="busy" @click="wipeHistory">
                    Delete all history
                </UiButton>
                <span class="check">removes history.tepin — curated memory is untouched</span>
            </div>
            <Teleport to="body">
                <div
                    v-if="showExcludeEditor && draft"
                    class="modal-backdrop"
                    @click.self="showExcludeEditor = false"
                >
                    <div class="modal glass-panel" role="dialog" aria-label="Ignored paths">
                        <h4 class="modal-title">Ignored paths</h4>
                        <NoteText>
                            The harvester never reads these — one absolute path per line, a
                            file or a whole directory. Deleting a recorded session adds its
                            transcript here automatically. Save the settings to apply.
                        </NoteText>
                        <AutoTextarea
                            v-model="excludePathsText"
                            class="grow"
                            :rows="8"
                            placeholder="/Users/you/private-repo"
                            aria-label="History exclude paths"
                        />
                        <div class="modal-actions">
                            <UiButton size="sm" @click="showExcludeEditor = false">
                                Done
                            </UiButton>
                        </div>
                    </div>
                </div>
            </Teleport>
        </PanelSection>

        <PanelSection title="Assistant skill">
            <NoteText>
                The capture skill teaches the assistant this graph's vocabulary.
                <template v-if="customOntology">
                    This ontology is customized, so the installed file is <strong>generated</strong>
                    from it — reinstall after reshaping types or verbs.
                </template>
                <template v-else>
                    This graph runs the shipped ontology, so the canonical skill text installs
                    verbatim.
                </template>
            </NoteText>
            <div class="skill-row">
                <SegmentedControl
                    v-model="skillVariant"
                    :options="SKILL_OPTIONS"
                    aria-label="Skill capture intensity"
                />
                <UiButton size="sm" :disabled="busy" @click="installSkill">
                    Install into project
                </UiButton>
            </div>
            <NoteText>
                relaxed captures sparingly · normal is the middle ground · aggressive is
                maximum capture.
            </NoteText>
            <NoteText>
                Writes <span class="mono">.claude/skills/engram/SKILL.md</span> in the project's
                repository. Symlinked skill folders are left untouched.
            </NoteText>
        </PanelSection>
    </template>
</SidePanel>
</template>

<style scoped>

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

/* A row whose controls can't all share one line at panel width wraps
   instead of pushing the trailing control out of the card. */
.row-label.wrap {
    flex-wrap: wrap;
    row-gap: 0.5rem;
}

.row-group {
    display: inline-flex;
    align-items: center;
    gap: 0.6rem;
    white-space: nowrap;
}

.grow {
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

.brief-nums {
    padding-left: 0.15rem;
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

/* A lone control in its own grid spans the row instead of sitting in one
   auto-fill column (half the panel at the default width). */
.grid > label.full-row {
    grid-column: 1 / -1;
}

.mini.danger:hover:not(:disabled) {
    color: var(--node-problem);
}

.mini.add {
    align-self: flex-start;
}

.mono {
    font-family: var(--font-mono);
}
/* Session history: the group togglers as switch rows, the path editor as a
   small modal instead of an always-there textarea. */
.switch-rows {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 18px;
    align-items: center;
}

.modal-backdrop {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--surface-overlay);
}

.modal {
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: min(560px, calc(100vw - 48px));
    padding: 16px 18px;
    border-radius: var(--radius-lg);
}

.modal-title {
    margin: 0;
    font-size: var(--text-body);
    font-weight: 600;
}

.modal-actions {
    display: flex;
    justify-content: flex-end;
}

.builtin-ref {
    margin: 0.4rem 0 0.8rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md);
    padding: 0.4rem 0.7rem;
}

.builtin-ref summary {
    cursor: pointer;
    font-size: var(--text-caption);
    color: var(--text-secondary);
}

.builtin-table {
    width: 100%;
    margin-top: 0.5rem;
    border-collapse: collapse;
    font-size: var(--text-caption);
}

.builtin-table td {
    padding: 0.2rem 0.6rem 0.2rem 0;
    vertical-align: top;
    color: var(--text-secondary);
}

.builtin-table td:first-child {
    color: var(--text-primary);
    white-space: nowrap;
}

.field-rows {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
}

.dim {
    color: var(--text-secondary);
}

.applies-label {
    font-size: var(--text-caption);
    align-self: center;
}
</style>
