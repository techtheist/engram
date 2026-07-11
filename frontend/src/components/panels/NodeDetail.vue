<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import MarkdownView from '@/components/common/MarkdownView.vue'
import SidePanel from '@/components/common/SidePanel.vue'
import TagEditor from '@/components/common/TagEditor.vue'
import { ALL_EDGE_TYPES, EDGE_COLOR, NODE_ACCENT_VAR } from '@/constants/ontology'
import { api } from '@/services/api'
import { useGraphStore } from '@/stores/graph'
import type { EdgeType, GraphEdge, TimelineEntry } from '@/types/graph'

const store = useGraphStore()
const { selected, nodes, edgeList, driftByNode } = storeToRefs(store)

/** Code refs of this node that no longer exist in the project (drifted). */
const missingRefs = computed(() =>
    selected.value ? (driftByNode.value.get(selected.value.id) ?? []) : [],
)

const busy = ref(false)
const confirmingDelete = ref(false)

// --- edit mode (PLAN §10 Phase 1: reclassification / editing UX) ----------

const NODE_TYPES = Object.keys(NODE_ACCENT_VAR)
const DURABILITIES = ['stable', 'episodic', 'volatile']

const editing = ref(false)
const draft = reactive({ title: '', body: '', type: '', durability: '', tags: [] as string[] })

// Selecting another node must never carry a stale draft onto it.
watch(
    () => selected.value?.id,
    () => {
        editing.value = false
        confirmingDelete.value = false
    },
)

function startEdit(): void {
    if (!selected.value) return
    draft.title = selected.value.title
    draft.body = selected.value.body ?? ''
    draft.type = selected.value.type
    draft.durability = selected.value.durability
    draft.tags = [...selected.value.tags]
    editing.value = true
}

async function saveEdit(): Promise<void> {
    if (!selected.value) return
    busy.value = true
    try {
        await store.patchNode(selected.value.id, {
            title: draft.title,
            body: draft.body,
            type: draft.type,
            durability: draft.durability,
            tags: draft.tags,
        })
        editing.value = false
    } finally {
        busy.value = false
    }
}

// --- connection editing (PLAN §10 pane CRUD: retype/delete from the list) --

async function retypeEdge(edge: GraphEdge, event: Event): Promise<void> {
    const type = (event.target as HTMLSelectElement).value as EdgeType
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
    selected.value ? NODE_ACCENT_VAR[selected.value.type] : 'var(--node-anchor)',
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
const inChain = computed(() => relations.value.some((r) => r.edge.type === 'replaces'))

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

async function reconfirm(): Promise<void> {
    if (!selected.value) return
    busy.value = true
    try {
        await store.reconfirm(selected.value.id)
    } finally {
        busy.value = false
    }
}

async function remove(): Promise<void> {
    if (!selected.value) return
    busy.value = true
    try {
        await store.remove(selected.value.id)
        confirmingDelete.value = false
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
<SidePanel
    :open="selected != null"
    side="right"
    panel-id="detail"
    :default-rem="40"
    :min-rem="28"
    :dismiss="close"
    :accent="accent"
    :style="{ '--accent': accent }"
>
    <template #header>
        <header v-if="selected" class="head">
            <span class="type-pill">{{ selected.type }}</span>
            <button class="close" type="button" aria-label="Close" @click="close">×</button>
        </header>
    </template>

    <template v-if="selected">
        <template v-if="editing">
            <input v-model="draft.title" class="edit-input" type="text" aria-label="Title" />
            <textarea v-model="draft.body" class="edit-input edit-body" rows="8" aria-label="Body (markdown)" />
            <div class="edit-row">
                <label class="edit-label">
                    Type
                    <select v-model="draft.type" class="edit-select">
                        <option v-for="t in NODE_TYPES" :key="t" :value="t">{{ t }}</option>
                    </select>
                </label>
                <label class="edit-label">
                    Durability
                    <select v-model="draft.durability" class="edit-select">
                        <option v-for="d in DURABILITIES" :key="d" :value="d">{{ d }}</option>
                    </select>
                </label>
            </div>
            <label class="edit-label">
                Tags
                <TagEditor v-model="draft.tags" />
            </label>
            <div class="edit-actions">
                <button class="btn" type="button" :disabled="busy || !draft.title.trim()" @click="saveEdit">
                    Save
                </button>
                <button class="btn ghost" type="button" :disabled="busy" @click="editing = false">
                    Cancel
                </button>
            </div>
        </template>

        <h2 v-if="!editing" class="title">{{ selected.title }}</h2>

        <div class="badges">
            <span class="badge">{{ selected.durability }}</span>
            <span class="badge">{{ selected.source }}</span>
            <span v-if="selected.status" class="badge">{{ selected.status }}</span>
            <span v-if="trustPct != null" class="badge">trust {{ trustPct }}%</span>
            <span v-if="selected.stale" class="badge stale">stale</span>
            <span
                v-if="missingRefs.length"
                class="badge drifted"
                title="Some code refs no longer exist in the project — the code moved, this memory may be stale"
            >drifted</span>
            <span v-if="archived" class="badge archived">archived</span>
        </div>

        <div v-if="selected.tags.length && !editing" class="tag-row">
            <span v-for="t in selected.tags" :key="t" class="tag-chip">#{{ t }}</span>
        </div>

        <MarkdownView v-if="selected.body && !editing" :content="selected.body" class="body" />

        <section v-if="selected.code_refs.length" class="block">
            <h3 class="block-title">Code refs</h3>
            <div class="refs">
                <span
                    v-for="codeRef in selected.code_refs"
                    :key="codeRef"
                    class="ref-chip"
                    :class="{ missing: missingRefs.includes(codeRef) }"
                    :title="missingRefs.includes(codeRef) ? 'This file no longer exists in the project' : undefined"
                >{{ codeRef }}</span>
            </div>
        </section>

        <section v-if="relations.length" class="block">
            <h3 class="block-title">Connections</h3>
            <ul class="relations">
                <li v-for="r in relations" :key="r.edge.id" class="relation-row">
                    <span class="rel-dir" :style="{ color: EDGE_COLOR[r.edge.type] }">
                        {{ r.dir === 'out' ? '→' : '←' }}
                    </span>
                    <select
                        class="rel-select"
                        :value="r.edge.type"
                        :disabled="busy"
                        :style="{ color: EDGE_COLOR[r.edge.type] }"
                        title="Change the connection verb"
                        @change="retypeEdge(r.edge, $event)"
                    >
                        <option v-for="t in ALL_EDGE_TYPES" :key="t" :value="t">{{ t }}</option>
                    </select>
                    <button class="relation" type="button" @click="store.select(r.otherId)">
                        <span class="rel-target">{{ r.otherTitle }}</span>
                    </button>
                    <button
                        class="rel-delete"
                        type="button"
                        :disabled="busy"
                        :aria-label="`Delete ${r.edge.type} connection`"
                        title="Delete this connection"
                        @click="removeEdge(r.edge)"
                    >
                        ×
                    </button>
                </li>
            </ul>
            <p class="rel-hint">Drag between node handles on the canvas to add a connection.</p>
        </section>

        <section v-if="timeline.length > 1" class="block">
            <h3 class="block-title">History</h3>
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
        </section>

        <section class="block">
            <h3 class="block-title">Provenance</h3>
            <dl class="meta">
                <div><dt>Created</dt><dd>{{ fmtDate(selected.created_at) }} · by {{ selected.source }}</dd></div>
                <div v-if="selected.session_id">
                    <dt>Session</dt>
                    <dd><span class="session-chip">{{ selected.session_id }}</span></dd>
                </div>
                <div v-if="selected.last_seen != null" title="Last time retrieval surfaced this node (search hit or brief)">
                    <dt>Last seen</dt><dd>{{ fmtDate(selected.last_seen) }}</dd>
                </div>
                <div v-if="selected.approved_at != null">
                    <dt>Approved</dt><dd>{{ fmtDate(selected.approved_at) }}</dd>
                </div>
                <div v-if="archived"><dt>Archived</dt><dd>{{ fmtDate(selected.valid_until) }}</dd></div>
            </dl>
        </section>

        <footer class="actions">
            <button v-if="!editing" class="btn ghost" type="button" :disabled="busy" @click="startEdit">
                Edit
            </button>
            <button class="btn ghost" type="button" :disabled="busy" @click="reconfirm">
                Reconfirm
            </button>
            <template v-if="confirmingDelete">
                <button class="btn danger" type="button" :disabled="busy" @click="remove">
                    Confirm delete
                </button>
                <button class="btn ghost" type="button" :disabled="busy" @click="confirmingDelete = false">
                    Cancel
                </button>
            </template>
            <button v-else class="btn ghost danger-text" type="button" :disabled="busy" @click="confirmingDelete = true">
                Delete
            </button>
        </footer>
    </template>
</SidePanel>
</template>

<style scoped>
.head {
    display: flex;
    align-items: center;
    justify-content: space-between;
}

.type-pill {
    padding: 0.3rem 0.9rem;
    border-radius: var(--radius-md);
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--accent);
    background-color: color-mix(in srgb, var(--accent) 16%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent);
}

.close {
    border: none;
    background: transparent;
    color: var(--text-tertiary);
    font-size: 2.4rem;
    line-height: 1;
    cursor: pointer;
}

.close:hover {
    color: var(--text-primary);
}

.title {
    font-size: var(--text-h2);
    font-weight: 700;
    line-height: var(--leading-tight);
    color: var(--text-primary);
}

.edit-input {
    width: 100%;
    padding: 0.7rem 0.9rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: var(--surface-sunken);
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    font-family: inherit;
}

.edit-body {
    resize: vertical;
    font-family: var(--font-mono);
    line-height: var(--leading-normal);
}

.edit-row {
    display: flex;
    gap: 1.2rem;
}

.edit-label {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 0.3rem;
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.edit-select {
    padding: 0.5rem 0.7rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: var(--surface-sunken);
    color: var(--text-primary);
    font-size: var(--text-body-sm);
}

.edit-actions {
    display: flex;
    gap: 0.6rem;
}

.badges {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
}

.badge {
    padding: 0.3rem 0.7rem;
    border-radius: var(--radius-md);
    font-size: var(--text-caption);
    color: var(--text-secondary);
    background-color: var(--surface-muted);
}

.badge.archived {
    color: var(--text-tertiary);
}

.badge.stale {
    color: var(--node-problem);
    background-color: color-mix(in srgb, var(--node-problem) 14%, transparent);
}

.badge.drifted {
    color: var(--node-caution);
    background-color: color-mix(in srgb, var(--node-caution) 14%, transparent);
}

.body {
    font-size: var(--text-body-sm);
    line-height: var(--leading-normal);
}

.block-title {
    margin-bottom: 0.6rem;
    font-size: var(--text-caption);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-tertiary);
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

.relations {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    list-style: none;
}

.relation-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
}

.rel-dir {
    flex: none;
    font-size: var(--text-caption);
    font-weight: 600;
}

.rel-select {
    flex: none;
    padding: 0.3rem 0.4rem;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border-subtle);
    background-color: transparent;
    font-size: var(--text-caption);
    font-weight: 600;
    cursor: pointer;
}

.rel-delete {
    flex: none;
    border: none;
    background: transparent;
    color: var(--text-tertiary);
    font-size: 1.6rem;
    line-height: 1;
    cursor: pointer;
}

.rel-delete:hover:not(:disabled) {
    color: var(--node-problem);
}

.rel-hint {
    margin-top: 0.5rem;
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.relation {
    display: flex;
    align-items: baseline;
    gap: 0.8rem;
    flex: 1;
    min-width: 0;
    padding: 0.6rem 0.8rem;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    text-align: left;
    cursor: pointer;
}

.relation:hover {
    background-color: var(--interactive-ghost-hover);
}

.rel-target {
    font-size: var(--text-body-sm);
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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

.meta {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    font-size: var(--text-caption);
}

.meta div {
    display: flex;
    gap: 0.8rem;
}

.meta dt {
    width: 7rem;
    color: var(--text-tertiary);
}

.meta dd {
    color: var(--text-secondary);
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

.actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.6rem;
    margin-top: auto;
    padding-top: 0.6rem;
}

.btn {
    padding: 0.7rem 1.2rem;
    border-radius: var(--radius-md);
    font-size: var(--text-label);
    font-weight: 600;
    cursor: pointer;
    border: 1px solid var(--border-default);
    background-color: var(--surface-elevated);
    color: var(--text-primary);
}

.btn:disabled {
    opacity: 0.5;
    cursor: default;
}

.btn:hover:not(:disabled) {
    background-color: var(--node-hover-surface);
}

.btn.ghost {
    background-color: transparent;
}

.btn.danger {
    border-color: var(--node-problem);
    background-color: color-mix(in srgb, var(--node-problem) 16%, transparent);
    color: var(--node-problem);
}

.btn.danger-text {
    color: var(--node-problem);
    border-color: transparent;
}
</style>
