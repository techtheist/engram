<script setup lang="ts">
import AutoTextarea from '@/components/common/AutoTextarea.vue'
import { computed, reactive, ref, useTemplateRef, watch } from 'vue'
import { onClickOutside } from '@vueuse/core'
import { storeToRefs } from 'pinia'
import MarkdownView from '@/components/common/MarkdownView.vue'
import SelectMenu from '@/components/common/SelectMenu.vue'
import FormField from '@/components/common/FormField.vue'
import SkeletonBlock from '@/components/common/SkeletonBlock.vue'
import InfoBadge from '@/components/common/InfoBadge.vue'
import NoteText from '@/components/common/NoteText.vue'
import TextField from '@/components/common/TextField.vue'
import UiButton from '@/components/common/UiButton.vue'
import DetailDrawer from '@/components/common/detail/DetailDrawer.vue'
import DetailLink from '@/components/common/detail/DetailLink.vue'
import MetaList from '@/components/common/MetaList.vue'
import MetaRow from '@/components/common/MetaRow.vue'
import PanelSection from '@/components/common/PanelSection.vue'
import DetailTitle from '@/components/common/detail/DetailTitle.vue'
import FieldInput from '@/components/common/FieldInput.vue'
import TagEditor from '@/components/common/TagEditor.vue'
import StandingChip from '@/components/common/StandingChip.vue'
import { useConfigStore } from '@/stores/config'
import { BADGE_TIPS, explainTrust } from '@/constants/trust'
import { api } from '@/services/api'
import { useGraphStore } from '@/stores/graph'
import { useHistoryStore } from '@/stores/history'
import { useTasksStore } from '@/stores/tasks'
import { useLayoutStore } from '@/stores/layout'
import { parseCodeRefs } from '@/utils/codeRefs'
import type { BornIn, EdgeType, FoundBridge, GraphEdge, Standing, TimelineEntry } from '@/types/graph'

const store = useGraphStore()
const config = useConfigStore()
const { selected, detailOpen, nodes, edgeList, driftByNode } = storeToRefs(store)

// --- born-in provenance (0.8.4): the history chip -------------------------

const historyStore = useHistoryStore()
const tasksStore = useTasksStore()
if (!tasksStore.loaded) void tasksStore.load()

/** Open tasks that point at this note (computed — notes store no backlinks). */
const taskRefs = computed(() =>
    selected.value && config.tasksEnabled ? tasksStore.referencing(selected.value.id) : [],
)

function openTask(id: string): void {
    tasksStore.open(id)
    layout.setView('tasks')
}
const layout = useLayoutStore()
const bornIn = ref<BornIn | null>(null)

watch(
    () => selected.value?.id,
    async (id) => {
        bornIn.value = null
        if (!id) return
        try {
            bornIn.value = (await api.bornIn(id)).born_in
        } catch {
            bornIn.value = null
        }
        // Pre-history notes have no born-in edge; the time-window fallback
        // below needs the session lanes to match against.
        if (!bornIn.value && !historyStore.sessions.length) {
            void historyStore.load()
        }
    },
    { immediate: true },
)

// --- grounded answers (0.9.10): the note's standing -----------------------

const standing = ref<Standing | null>(null)

/**
 * Everything the daemon's verdict reads: the node's endorsements and
 * staleness, and the live edges touching it — so pinning, approving, or
 * linking a `replaces` re-reads the verdict at once.
 */
const standingKey = computed(() => {
    const n = selected.value
    if (!n) return null
    const touching = edgeList.value
        .filter((e) => e.from_id === n.id || e.to_id === n.id)
        .map((e) => `${e.id}:${e.status ?? ''}`)
        .sort()
        .join(',')
    return [n.id, n.trust_override, n.approved_at, n.confirmed_at, n.stale, n.valid_until, touching].join('|')
})

const standingPending = ref(false)
/** Which node `standing` describes — a refresh of the same node keeps it. */
let standingFor: string | null = null

watch(
    standingKey,
    async (key) => {
        const id = selected.value?.id
        if (!key || !id) {
            standing.value = null
            standingPending.value = false
            return
        }
        // Hold the line's place while it loads (a skeleton), so it doesn't
        // push the body down when it lands.
        standingPending.value = standingFor !== id
        try {
            const s = await api.standing(id)
            if (selected.value?.id === id) {
                standing.value = s
                standingFor = id
            }
        } catch {
            standing.value = null // an older daemon without the route
        } finally {
            if (selected.value?.id === id) standingPending.value = false
        }
    },
    { immediate: true },
)

// --- found bridges (0.11.2): virtual `about` ties ----------------------------

/**
 * Notes a bridge note ties to this one while no edge joins them — gathered
 * when the view opens (and again when this note's edges change: a new link
 * retires the row). Shown as a virtual `about`, never written to the graph.
 */
const bridges = ref<FoundBridge[]>([])

watch(
    standingKey,
    async (key) => {
        const id = selected.value?.id
        if (!key || !id) {
            bridges.value = []
            return
        }
        try {
            const { bridges: found } = await api.bridges(id)
            if (selected.value?.id === id) bridges.value = found
        } catch {
            bridges.value = [] // an older daemon without the route
        }
    },
    { immediate: true },
)

const BRIDGE_TIP =
    'Found bridge — not an edge. A bridge note ties these two subjects together ' +
    '(“same”, “reuses”, “renamed”…) while nothing in the graph links them. ' +
    'Read from titles: no model judged it, nothing is stored.'

const standingOther = computed(() => {
    const other = standing.value?.other
    return other ? (nodes.value.get(other) ?? null) : null
})

/**
 * The recorded session a pre-history note most plausibly belongs to: its
 * capture time falls inside (or hugs) a session's window. An approximation,
 * offered only within tight slack — a wrong-looking guess is worse than a
 * plain chip.
 */
const timeSession = computed(() => {
    if (bornIn.value || !selected.value) return null
    const t = selected.value.created_at
    let best: { session: string; gap: number } | null = null
    for (const s of historyStore.sessions) {
        const end = s.ended ?? s.started
        if (t < s.started - 600 || t > end + 3600) continue
        const gap = t < s.started ? s.started - t : t > end ? t - end : 0
        if (!best || gap < best.gap) best = { session: s.session, gap }
    }
    return best?.session ?? null
})

/** Jump into the history view: exact birth exchange when born-in exists,
 *  else the session recorded around the note's capture time. */
function openBirth(): void {
    const jump = (session: string, turn: number | null, ts: number | null): void => {
        layout.setView('history')
        void historyStore.open(session, turn, ts)
        // Narrow mode (phone / IDE side pane): the drawer takes the whole
        // width, so left open it would cover the very exchange it linked to.
        if (window.matchMedia('(max-width: 719px)').matches) {
            store.select(null)
        }
    }
    const b = bornIn.value
    if (b) {
        jump(b.session, b.turn ?? null, null)
        return
    }
    if (timeSession.value && selected.value) {
        jump(timeSession.value, null, selected.value.created_at)
    }
}

/** Code refs of this node that no longer exist in the project (drifted). */
const missingRefs = computed(() =>
    selected.value ? (driftByNode.value.get(selected.value.id) ?? []) : [],
)

const busy = ref(false)
const confirmingDelete = ref(false)
// Delete leaves a tombstone-role note by default (0.9.0) — off only when the
// user unticks it, moot when the ontology declares no tombstone type.
const leaveTombstone = ref(true)
const tombstoneReason = ref('')
// 0.9.2: the tombstone carries the removed text (body, tags, code refs) so
// a paraphrase of the buried content still lands on it. Unticked = the
// purge shape: identity and reason only.
const keepRemovedText = ref(true)

// --- trust actions (trust v2: approve ladder, pin, […] menu) ---------------

const menuOpen = ref(false)
const settingTrust = ref(false)
const trustDraft = ref(100)
const menuRoot = useTemplateRef<HTMLElement>('menuRoot')
onClickOutside(menuRoot, () => {
    menuOpen.value = false
    settingTrust.value = false
})

const pinned = computed(() => selected.value?.trust_override != null)

async function trustAction(run: () => Promise<void>): Promise<void> {
    busy.value = true
    try {
        await run()
    } finally {
        busy.value = false
        menuOpen.value = false
        settingTrust.value = false
    }
}

const approve = () => trustAction(() => store.approve(selected.value!.id))
const togglePin = () =>
    trustAction(() => store.pin(selected.value!.id, pinned.value ? null : 1.0))
const confirmStillTrue = () => trustAction(() => store.reconfirm(selected.value!.id))
const revokeApproval = () => trustAction(() => store.revokeApproval(selected.value!.id))
/* A cleared number input yields '' through v-model.number — never coerce
   that to "pin at 0%", which would silently bury the node. */
const trustDraftValid = computed(
    () => typeof trustDraft.value === 'number' && Number.isFinite(trustDraft.value),
)
const applyTrustOverride = () =>
    trustAction(() =>
        store.pin(selected.value!.id, Math.min(100, Math.max(0, trustDraft.value)) / 100),
    )

// --- edit mode (PLAN §10 Phase 1: reclassification / editing UX) ----------

const NODE_TYPES = computed(() => config.typeNames)
const DURABILITIES = ['stable', 'episodic', 'volatile']
const typeOptions = computed(() => NODE_TYPES.value.map((t) => ({ value: t, label: t })))
const durabilityOptions = DURABILITIES.map((d) => ({ value: d, label: d }))
const verbOptions = computed(() =>
    config.verbNames.map((v) => ({ value: v, label: v, color: config.edgeColor(v) })),
)

const editing = ref(false)
const draft = reactive({
    title: '',
    body: '',
    type: '',
    durability: '',
    tags: [] as string[],
    // Raw textarea text, one code ref per line — parsed on save
    // (see parseCodeRefs). Kept as text, not an array, so the user can leave
    // a blank/partial line mid-edit without it being silently dropped.
    codeRefsText: '',
    fields: {} as Record<string, unknown>,
})

/** The custom-field definitions that apply to the node's (or, while editing,
 *  the draft's) type. */
const fieldDefs = computed(() =>
    config.fieldsFor(editing.value ? draft.type : (selected.value?.type ?? '')),
)

/** Read-view rows: declared fields that carry a value on this node. */
const fieldRows = computed(() => {
    const values = selected.value?.fields
    if (!values) return []
    return config
        .fieldsFor(selected.value?.type ?? '')
        .filter((d) => d.name in values)
        .map((d) => ({
            def: d,
            label: d.label !== '' ? d.label : d.name,
            value: values[d.name],
        }))
})

// Selecting another node must never carry a stale draft onto it.
watch(
    () => selected.value?.id,
    () => {
        editing.value = false
        confirmingDelete.value = false
        menuOpen.value = false
        settingTrust.value = false
    },
)

function startEdit(): void {
    if (!selected.value) return
    draft.title = selected.value.title
    draft.body = selected.value.body ?? ''
    draft.type = selected.value.type
    draft.durability = selected.value.durability
    draft.tags = [...selected.value.tags]
    draft.codeRefsText = selected.value.code_refs.join('\n')
    draft.fields = { ...(selected.value.fields ?? {}) }
    editing.value = true
}

async function saveEdit(): Promise<void> {
    if (!selected.value) return
    busy.value = true
    try {
        const patch: Record<string, unknown> = {
            title: draft.title,
            body: draft.body,
            type: draft.type,
            durability: draft.durability,
            tags: draft.tags,
            // Always sent, even empty — an empty list is how a drifted ref
            // gets cleared (PATCH replaces the array wholesale).
            code_refs: parseCodeRefs(draft.codeRefsText),
        }
        // Custom fields ride as a MERGE patch: cleared keys become null
        // (delete), set keys overwrite. Only sent when the graph has defs.
        if (config.cfg?.fields.length) {
            const fields: Record<string, unknown> = {}
            for (const key of Object.keys(selected.value.fields ?? {})) {
                if (draft.fields[key] === undefined) fields[key] = null
            }
            for (const [key, value] of Object.entries(draft.fields)) {
                if (value !== undefined) fields[key] = value
            }
            if (Object.keys(fields).length > 0) patch.fields = fields
        }
        await store.patchNode(selected.value.id, patch)
        editing.value = false
    } finally {
        busy.value = false
    }
}

// --- connection editing (PLAN §10 pane CRUD: retype/delete from the list) --

async function retypeEdge(edge: GraphEdge, picked: string): Promise<void> {
    const type = picked as EdgeType
    if (type === edge.type) return
    busy.value = true
    try {
        await store.retypeEdge(edge.id, type)
    } finally {
        busy.value = false
    }
}

async function removeEdge(edge: GraphEdge): Promise<void> {
    busy.value = true
    try {
        await store.removeEdge(edge.id)
    } finally {
        busy.value = false
    }
}

const accent = computed(() =>
    selected.value ? config.accent(selected.value.type) : '#64748b',
)
const archived = computed(() => selected.value?.valid_until != null)
const trustPct = computed(() =>
    selected.value ? Math.round(selected.value.trust * 100) : null,
)

interface Relation {
    edge: GraphEdge
    dir: 'out' | 'in'
    otherId: string
    otherTitle: string
}

const relations = computed<Relation[]>(() => {
    const id = selected.value?.id
    if (!id) return []
    const out: Relation[] = []
    for (const e of edgeList.value) {
        if (e.from_id === id) {
            out.push({ edge: e, dir: 'out', otherId: e.to_id, otherTitle: title(e.to_id) })
        } else if (e.to_id === id) {
            out.push({ edge: e, dir: 'in', otherId: e.from_id, otherTitle: title(e.from_id) })
        }
    }
    return out
})

function title(id: string): string {
    return nodes.value.get(id)?.title ?? id.slice(0, 8)
}

// --- history (PLAN §10: timeline pane view) --------------------------------

const timeline = ref<TimelineEntry[]>([])

/** Only chain members carry a `replaces` edge — everyone else skips the fetch. */
const inChain = computed(() =>
    relations.value.some((r) => r.edge.type === config.supersessionVerb),
)

watch(
    [() => selected.value?.id, inChain],
    async ([id, chained]) => {
        timeline.value = []
        if (!id || !chained) return
        try {
            const chain = await api.timeline(id)
            // The selection may have moved while the request was in flight.
            if (selected.value?.id === id) timeline.value = chain
        } catch {
            // History is supplementary — a failed fetch just hides the section.
        }
    },
    { immediate: true },
)

function fmtDate(secs: number | null): string {
    if (secs == null) return '—'
    return new Date(secs * 1000).toLocaleString(undefined, {
        dateStyle: 'medium',
        timeStyle: 'short',
    })
}

async function remove(): Promise<void> {
    if (!selected.value) return
    busy.value = true
    try {
        const withTombstone = leaveTombstone.value && config.tombstoneType != null
        await store.remove(selected.value.id, {
            tombstone: withTombstone,
            reason: withTombstone ? tombstoneReason.value.trim() || undefined : undefined,
            keepText: withTombstone ? keepRemovedText.value : undefined,
        })
        confirmingDelete.value = false
        tombstoneReason.value = ''
        keepRemovedText.value = true
    } finally {
        busy.value = false
    }
}

function close(): void {
    store.select(null)
    confirmingDelete.value = false
    editing.value = false
}
</script>

<template>
<DetailDrawer
    :open="selected != null && detailOpen"
    panel-id="detail"
    :kind="selected?.type ?? ''"
    :accent="accent"
    :dismiss="close"
>
    <template v-if="selected && !editing" #title>
        <DetailTitle :text="selected.title" />
    </template>

    <template v-if="selected" #badges>
        <InfoBadge v-if="selected.version" title="Project version this note was captured at (version tracking)">{{ selected.version }}</InfoBadge>
        <InfoBadge :title="BADGE_TIPS.durability[selected.durability]">{{ selected.durability }}</InfoBadge>
        <InfoBadge :title="BADGE_TIPS.source[selected.source]">{{ selected.source }}</InfoBadge>
        <InfoBadge v-if="selected.status" :title="BADGE_TIPS.status[selected.status]">{{ selected.status }}</InfoBadge>
        <InfoBadge v-if="trustPct != null" :title="explainTrust(selected, config.cfg?.policy)">trust {{ trustPct }}%</InfoBadge>
        <InfoBadge v-if="pinned" tone="accent" :title="BADGE_TIPS.pinned">📌 pinned</InfoBadge>
        <InfoBadge v-if="selected.demoted_at != null && !pinned" tone="danger" :title="BADGE_TIPS.demoted">demoted</InfoBadge>
        <InfoBadge v-if="selected.stale" tone="danger" :title="BADGE_TIPS.stale">stale</InfoBadge>
        <InfoBadge
            v-if="missingRefs.length"
            tone="warn"
            title="Some code refs no longer exist in the project — the code moved, this memory may be stale"
        >
            drifted
        </InfoBadge>
        <InfoBadge v-if="archived" tone="muted" title="Superseded or decayed out — kept for history, hidden from retrieval">archived</InfoBadge>
    </template>

    <template v-if="selected">
        <template v-if="editing">
            <TextField v-model="draft.title" class="edit-title" aria-label="Title" />
            <AutoTextarea v-model="draft.body" class="mono" :rows="8" aria-label="Body (markdown)" />
            <div class="edit-row">
                <FormField label="Type">
                    <SelectMenu v-model="draft.type" :options="typeOptions" aria-label="Type" block />
                </FormField>
                <FormField label="Durability">
                    <SelectMenu v-model="draft.durability" :options="durabilityOptions" aria-label="Durability" block />
                </FormField>
            </div>
            <FormField label="Tags">
                <TagEditor v-model="draft.tags" />
            </FormField>
            <FormField
                label="Code refs"
                hint="Repo-relative paths, one per line, no line numbers — drift-checked against the working tree. Leave empty to clear."
            >
                <AutoTextarea
                    v-model="draft.codeRefsText"
                    class="mono small"
                    :rows="4"
                    placeholder="One repo-relative path per line…"
                    aria-label="Code refs"
                />
            </FormField>
            <PanelSection v-if="fieldDefs.length" title="Custom fields">
                <FieldInput
                    v-for="d in fieldDefs"
                    :key="d.name"
                    v-model="draft.fields[d.name]"
                    :def="d"
                />
            </PanelSection>
            <div class="edit-actions">
                <UiButton :disabled="busy || !draft.title.trim()" @click="saveEdit">Save</UiButton>
                <UiButton variant="ghost" :disabled="busy" @click="editing = false">Cancel</UiButton>
            </div>
        </template>

        <NoteText>{{ explainTrust(selected, config.cfg?.policy) }}</NoteText>

        <SkeletonBlock v-if="standingPending" width="70%" height="2rem" />
        <p v-else-if="standing" class="standing-line">
            <StandingChip :standing="standing" />
            <span class="standing-reason">
                <template v-if="standingOther">
                    {{ standing.verdict === 'superseded' ? 'replaced by' : 'in conflict with' }}
                    <button class="standing-other" type="button" @click="store.select(standingOther.id)">
                        {{ standingOther.title }}
                    </button>
                </template>
                <template v-else>{{ standing.reason }}</template>
            </span>
        </p>

        <div v-if="selected.tags.length && !editing" class="tag-row">
            <span v-for="t in selected.tags" :key="t" class="tag-chip">#{{ t }}</span>
        </div>

        <dl v-if="fieldRows.length && !editing" class="fields-view">
            <template v-for="row in fieldRows" :key="row.def.name">
                <dt :title="row.def.name">{{ row.label }}</dt>
                <dd>
                    <a
                        v-if="row.def.kind === 'url' && typeof row.value === 'string'"
                        :href="row.value"
                        target="_blank"
                        rel="noopener noreferrer"
                    >{{ row.value }}</a>
                    <template v-else>{{ row.value }}</template>
                </dd>
            </template>
        </dl>

        <MarkdownView v-if="selected.body && !editing" :content="selected.body" class="body" />

        <PanelSection v-if="selected.code_refs.length" title="Code refs">
            <div class="refs">
                <span
                    v-for="codeRef in selected.code_refs"
                    :key="codeRef"
                    class="ref-chip"
                    :class="{ missing: missingRefs.includes(codeRef) }"
                    :title="missingRefs.includes(codeRef) ? 'This file no longer exists in the project' : undefined"
                >{{ codeRef }}</span>
            </div>
        </PanelSection>

        <PanelSection v-if="relations.length" title="Connections">
            <ul class="relations">
                <DetailLink
                    v-for="r in relations"
                    :key="r.edge.id"
                    :title="r.otherTitle"
                    removable
                    :disabled="busy"
                    :remove-label="`Delete ${r.edge.type} connection`"
                    @open="store.select(r.otherId)"
                    @remove="removeEdge(r.edge)"
                >
                    <template #lead>
                        <span class="rel-dir" :style="{ color: config.edgeColor(r.edge.type) }">
                            {{ r.dir === 'out' ? '→' : '←' }}
                        </span>
                        <SelectMenu
                            size="sm"
                            :model-value="r.edge.type"
                            :options="verbOptions"
                            :disabled="busy"
                            :color="config.edgeColor(r.edge.type)"
                            aria-label="Connection verb"
                            title="Change the connection verb"
                            @update:model-value="retypeEdge(r.edge, $event)"
                        />
                    </template>
                </DetailLink>
            </ul>
            <NoteText>Drag between node handles on the canvas to add a connection.</NoteText>
        </PanelSection>

        <PanelSection v-if="bridges.length" title="Found bridges">
            <ul class="relations">
                <li v-for="b in bridges" :key="b.id" class="bridge-row">
                    <span class="virtual-verb" :title="BRIDGE_TIP">⇢ about</span>
                    <div class="bridge-text">
                        <button class="bridge-target" type="button" @click="store.select(b.id)">
                            {{ b.title }}
                        </button>
                        <button
                            v-if="b.via"
                            class="bridge-via"
                            type="button"
                            :title="`The bridge note: ${b.via.title}`"
                            @click="store.select(b.via.id)"
                        >
                            via {{ b.via.title }}
                        </button>
                        <span v-else class="bridge-via self">named by this note</span>
                    </div>
                </li>
            </ul>
            <NoteText>
                Not in the graph: a bridge note ties these subjects together while no edge does —
                worth a look when facts about them disagree.
            </NoteText>
        </PanelSection>

        <PanelSection v-if="timeline.length > 1" title="History">
            <ol class="timeline">
                <li
                    v-for="entry in timeline"
                    :key="entry.id"
                    class="tl-row"
                    :class="{ current: entry.id === selected.id }"
                >
                    <span class="tl-dot" aria-hidden="true" />
                    <div class="tl-item">
                        <button
                            class="tl-title"
                            type="button"
                            :disabled="entry.id === selected.id"
                            @click="store.select(entry.id)"
                        >
                            {{ entry.title }}
                        </button>
                        <span class="tl-meta">
                            {{ fmtDate(entry.created_at) }}<template v-if="entry.valid_until == null && !entry.replaced_note"> · current</template>
                        </span>
                        <p v-if="entry.replaced_note" class="tl-note">{{ entry.replaced_note }}</p>
                    </div>
                </li>
            </ol>
        </PanelSection>

        <PanelSection title="Provenance">
            <MetaList>
                <MetaRow label="Created">{{ fmtDate(selected.created_at) }} · by {{ selected.source }}</MetaRow>
                <!-- Not every note has an mcp session (pane/HTTP writes carry
                     none) — but born-in provenance can still exist, so the
                     row shows whenever there is anything to link or say. -->
                <MetaRow v-if="selected.session_id || bornIn || timeSession" label="Session">
                    <button
                        v-if="bornIn"
                        class="session-chip session-link"
                        type="button"
                        title="This note was captured during a recorded session — open the exact exchange in the history view"
                        @click="openBirth"
                    >
                        {{ selected.session_id ?? 'recorded session' }} ⤴
                    </button>
                    <button
                        v-else-if="timeSession"
                        class="session-chip session-link"
                        type="button"
                        title="No exact record for this note — open the session recorded around its capture time"
                        @click="openBirth"
                    >
                        {{ selected.session_id ?? 'recorded session' }} ≈⤴
                    </button>
                    <span v-else class="session-chip">{{ selected.session_id }}</span>
                </MetaRow>
                <MetaRow v-if="selected.last_seen != null" label="Last retrieved" :title="BADGE_TIPS.lastSeen">{{ fmtDate(selected.last_seen) }}</MetaRow>
                <MetaRow v-if="selected.confirmed_at != null" label="Confirmed" :title="BADGE_TIPS.confirmed">{{ fmtDate(selected.confirmed_at) }}</MetaRow>
                <MetaRow v-if="selected.approved_at != null" label="Approved" :title="BADGE_TIPS.approved">{{ fmtDate(selected.approved_at) }}</MetaRow>
                <MetaRow v-if="selected.demoted_at != null" label="Demoted" :title="BADGE_TIPS.demoted">{{ fmtDate(selected.demoted_at) }}</MetaRow>
                <MetaRow v-if="archived" label="Archived">{{ fmtDate(selected.valid_until) }}</MetaRow>
                <!-- Computed backlinks (0.10.0): tasks point at notes, notes
                     never store a pointer back. -->
                <MetaRow v-if="taskRefs.length" label="Tasks">
                    <button
                        v-for="t in taskRefs"
                        :key="t.id"
                        class="session-chip session-link"
                        type="button"
                        :title="`Open task: ${t.title}`"
                        @click="openTask(t.id)"
                    >
                        {{ t.title }} ⤴
                    </button>
                </MetaRow>
            </MetaList>
        </PanelSection>
    </template>

    <template v-if="selected" #actions>
        <UiButton
            :disabled="busy"
            :title="selected.approved_at != null
                ? 'Re-approve: restamp the approval, restarting trust at 100%'
                : 'Approve: you vouch for this — trust restarts at 100%'"
            @click="approve"
        >
            {{ selected.approved_at != null ? 'Re-approve' : 'Approve' }}
        </UiButton>
        <UiButton v-if="!editing" variant="ghost" :disabled="busy" @click="startEdit">Edit</UiButton>
        <UiButton
            variant="ghost"
            :disabled="busy"
            :title="pinned
                ? 'Unpin: hand trust back to the model (decay and evidence apply again)'
                : 'Pin: lock trust at 100% forever — for the rare constraint that must never fade'"
            @click="togglePin"
        >
            {{ pinned ? 'Unpin' : '📌 Pin' }}
        </UiButton>

        <div ref="menuRoot" class="menu-wrap">
            <UiButton
                variant="ghost"
                :disabled="busy"
                title="More trust actions"
                aria-label="More trust actions"
                @click="menuOpen = !menuOpen"
            >
                …
            </UiButton>
            <div v-if="menuOpen" class="menu glass-panel">
                <button
                    class="menu-item"
                    type="button"
                    :disabled="busy"
                    title="Stamp this node as verified still true — restarts unapproved trust at 60% and clears any evidence demotion"
                    @click="confirmStillTrue"
                >
                    Confirm still true
                </button>
                <button
                    class="menu-item"
                    type="button"
                    :disabled="busy || (selected.approved_at == null && !pinned)"
                    title="Withdraw your approval and any pin — trust falls back to its last confirmed anchor"
                    @click="revokeApproval"
                >
                    Revoke approval
                </button>
                <button
                    class="menu-item"
                    type="button"
                    :disabled="busy"
                    title="Freeze trust at an exact percentage — a partial pin for knowledge you half-believe"
                    @click="settingTrust = !settingTrust"
                >
                    Set constant trust %
                </button>
                <div v-if="settingTrust" class="trust-set-row">
                    <input
                        v-model.number="trustDraft"
                        class="trust-input"
                        type="number"
                        min="0"
                        max="100"
                        aria-label="Constant trust percentage"
                    />
                    <UiButton :disabled="busy || !trustDraftValid" @click="applyTrustOverride">Set</UiButton>
                </div>
            </div>
        </div>

        <template v-if="confirmingDelete">
            <div v-if="config.tombstoneType" class="tombstone-opt">
                <label class="tombstone-check">
                    <input v-model="leaveTombstone" type="checkbox" :disabled="busy" />
                    Leave a {{ config.tombstoneType }} (records what was removed, so it isn't re-learned)
                </label>
                <input
                    v-if="leaveTombstone"
                    v-model="tombstoneReason"
                    class="tombstone-reason"
                    type="text"
                    placeholder="Why is this being removed? (optional)"
                    :disabled="busy"
                />
                <label
                    v-if="leaveTombstone"
                    class="tombstone-check"
                    title="Carries the body, tags and code refs into the Tombstone, so a rewrite of the removed content is caught — untick to purge the text and keep only what was removed and why"
                >
                    <input v-model="keepRemovedText" type="checkbox" :disabled="busy" />
                    Keep the removed text in the {{ config.tombstoneType }}
                </label>
            </div>
            <UiButton variant="danger" :disabled="busy" @click="remove">Confirm delete</UiButton>
            <UiButton variant="ghost" :disabled="busy" @click="confirmingDelete = false">Cancel</UiButton>
        </template>
        <UiButton
            v-else
            variant="danger-text"
            :disabled="busy"
            title="Hard delete — removes the node and its connections permanently (user-only; the assistant can never do this)"
            @click="confirmingDelete = true"
        >
            Delete
        </UiButton>
    </template>
</DetailDrawer>
</template>

<style scoped>
.mono {
    font-family: var(--font-mono);
}

.mono.small {
    font-size: var(--text-caption);
}

.relations {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    margin: 0;
    padding: 0;
    list-style: none;
}


.edit-row {
    display: flex;
    gap: 1.2rem;
}


.edit-actions {
    display: flex;
    gap: 0.6rem;
}

/* One stale badge across the pane — same shape here, in Review, in the feed. */
.standing-line {
    display: flex;
    align-items: baseline;
    gap: 0.8rem;
    margin-top: 0.6rem;
    font-size: var(--text-caption);
    color: var(--text-secondary);
}

.standing-reason {
    min-width: 0;
}

.standing-other {
    padding: 0;
    border: none;
    background: none;
    color: var(--interactive-primary);
    font: inherit;
    text-align: left;
    cursor: pointer;
}

.standing-other:hover {
    text-decoration: underline;
}


.menu-wrap {
    position: relative;
}

.menu {
    position: absolute;
    bottom: calc(100% + 0.6rem);
    left: 0;
    z-index: 10;
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    min-width: 22rem;
    padding: 0.6rem;
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
}

.menu-item {
    padding: 0.7rem 0.9rem;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    text-align: left;
    cursor: pointer;
}

.menu-item:disabled {
    opacity: 0.45;
    cursor: default;
}

.menu-item:hover:not(:disabled) {
    background-color: var(--interactive-ghost-hover);
}

.trust-set-row {
    display: flex;
    gap: 0.6rem;
    padding: 0.4rem 0.9rem 0.6rem;
}

.trust-input {
    width: 8rem;
    padding: 0.5rem 0.7rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: var(--surface-sunken);
    color: var(--text-primary);
    font-size: var(--text-body-sm);
}

.body {
    font-size: var(--text-body-sm);
    line-height: var(--leading-normal);
}

.refs {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
}

.ref-chip {
    max-width: 100%;
    padding: 0.2rem 0.7rem;
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-secondary);
    background-color: var(--surface-sunken);
    /* Long refs scroll inside the chip instead of widening the drawer. */
    overflow-x: auto;
    white-space: nowrap;
}

.ref-chip.missing {
    color: var(--node-problem);
    background-color: color-mix(in srgb, var(--node-problem) 12%, transparent);
    text-decoration: line-through;
}

.tag-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
}

.tag-chip {
    padding: 0.2rem 0.7rem;
    border-radius: var(--radius-full);
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--interactive-primary);
    background-color: color-mix(in srgb, var(--interactive-primary) 14%, transparent);
    border: 1px solid color-mix(in srgb, var(--interactive-primary) 40%, transparent);
}

.rel-dir {
    flex: none;
    font-size: var(--text-caption);
    font-weight: 600;
}

/* A found bridge's virtual verb: dashed, muted — read, not stored. */
.virtual-verb {
    flex: none;
    padding: 0.1rem 0.5rem;
    border: 1px dashed var(--border-strong);
    border-radius: var(--radius-sm);
    color: var(--text-secondary);
    font-size: var(--text-caption);
    font-weight: 600;
    cursor: help;
}

.bridge-row {
    display: flex;
    gap: 0.6rem;
    align-items: flex-start;
    min-width: 0;
    padding: 0.4rem 0.8rem 0.4rem 0;
}

.bridge-text {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 0;
}

.bridge-target,
.bridge-via {
    display: -webkit-box;
    overflow: hidden;
    padding: 0;
    border: none;
    background: none;
    text-align: left;
    cursor: pointer;
    -webkit-box-orient: vertical;
}

.bridge-target {
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    -webkit-line-clamp: 2;
}

.bridge-via {
    color: var(--text-tertiary);
    font-size: var(--text-caption);
    -webkit-line-clamp: 1;
}

.bridge-via.self {
    cursor: default;
}

.bridge-target:hover,
.bridge-via:not(.self):hover {
    color: var(--interactive-primary);
    text-decoration: underline;
}


.timeline {
    display: flex;
    flex-direction: column;
    list-style: none;
}

.tl-row {
    position: relative;
    display: flex;
    gap: 1rem;
    padding-bottom: 1rem;
}

/* The connecting line between generation dots. */
.tl-row::before {
    content: '';
    position: absolute;
    top: 1.1rem;
    bottom: -0.2rem;
    left: 0.35rem;
    width: 1px;
    background-color: var(--border-subtle);
}

.tl-row:last-child {
    padding-bottom: 0;
}

.tl-row:last-child::before {
    display: none;
}

.tl-dot {
    flex-shrink: 0;
    width: 0.8rem;
    height: 0.8rem;
    margin-top: 0.4rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-full);
    background-color: var(--surface-muted);
}

.tl-row.current .tl-dot {
    border-color: var(--accent);
    background-color: var(--accent);
}

.tl-item {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    min-width: 0;
}

.tl-title {
    padding: 0;
    border: none;
    background: transparent;
    color: var(--text-secondary);
    font-size: var(--text-body-sm);
    text-align: left;
    cursor: pointer;
}

.tl-title:hover:not(:disabled) {
    color: var(--text-primary);
    text-decoration: underline;
}

.tl-row.current .tl-title {
    color: var(--text-primary);
    font-weight: 600;
    cursor: default;
}

.tl-meta {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.tl-note {
    font-size: var(--text-caption);
    font-style: italic;
    color: var(--text-tertiary);
}

.session-chip {
    display: inline-block;
    max-width: 100%;
    padding: 0.1rem 0.5rem;
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
    background-color: var(--surface-sunken);
    overflow-x: auto;
    white-space: nowrap;
    vertical-align: bottom;
}

/* Provenance-backed sessions are a door, not a label. */
.session-link {
    border: none;
    color: var(--interactive-primary, #38bdf8);
    font-size: inherit;
    cursor: pointer;
}

.session-link:hover {
    text-decoration: underline;
}


.fields-view {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.25rem 0.8rem;
    margin: 0;
    font-size: var(--text-caption);
}

.fields-view dt {
    color: var(--text-secondary);
}

.fields-view dd {
    margin: 0;
    color: var(--text-primary);
    overflow-wrap: anywhere;
}

.tombstone-opt {
    flex-basis: 100%;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    margin-bottom: 0.4rem;
}

.tombstone-check {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    font-size: var(--text-caption);
    color: var(--text-secondary);
    cursor: pointer;
}

.tombstone-reason {
    width: 100%;
    padding: 0.4rem 0.6rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md);
    background: var(--surface-sunken);
    color: var(--text-primary);
    font-size: var(--text-caption);
}
</style>
