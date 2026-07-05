<script setup lang="ts">
import { computed, ref } from 'vue'
import { storeToRefs } from 'pinia'
import MarkdownView from '@/components/common/MarkdownView.vue'
import { EDGE_COLOR, NODE_ACCENT_VAR } from '@/constants/ontology'
import { useGraphStore } from '@/stores/graph'
import type { GraphEdge } from '@/types/graph'

const store = useGraphStore()
const { selected, nodes, edgeList } = storeToRefs(store)

const busy = ref(false)
const confirmingDelete = ref(false)

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
}
</script>

<template>
<Transition name="drawer">
    <aside v-if="selected" class="detail glass-panel" :style="{ '--accent': accent }">
        <header class="head">
            <span class="type-pill">{{ selected.type }}</span>
            <button class="close" type="button" aria-label="Close" @click="close">×</button>
        </header>

        <h2 class="title">{{ selected.title }}</h2>

        <div class="badges">
            <span class="badge">{{ selected.durability }}</span>
            <span class="badge">{{ selected.source }}</span>
            <span v-if="selected.status" class="badge">{{ selected.status }}</span>
            <span v-if="trustPct != null" class="badge">trust {{ trustPct }}%</span>
            <span v-if="selected.stale" class="badge stale">stale</span>
            <span v-if="archived" class="badge archived">archived</span>
        </div>

        <MarkdownView v-if="selected.body" :content="selected.body" class="body" />

        <section v-if="selected.code_refs.length" class="block">
            <h3 class="block-title">Code refs</h3>
            <div class="refs">
                <span v-for="codeRef in selected.code_refs" :key="codeRef" class="ref-chip">{{ codeRef }}</span>
            </div>
        </section>

        <section v-if="relations.length" class="block">
            <h3 class="block-title">Connections</h3>
            <ul class="relations">
                <li v-for="r in relations" :key="r.edge.id">
                    <button class="relation" type="button" @click="store.select(r.otherId)">
                        <span class="rel-verb" :style="{ color: EDGE_COLOR[r.edge.type] }">
                            {{ r.dir === 'out' ? '→' : '←' }} {{ r.edge.type }}
                        </span>
                        <span class="rel-target">{{ r.otherTitle }}</span>
                    </button>
                </li>
            </ul>
        </section>

        <dl class="meta">
            <div><dt>Created</dt><dd>{{ fmtDate(selected.created_at) }}</dd></div>
            <div v-if="archived"><dt>Archived</dt><dd>{{ fmtDate(selected.valid_until) }}</dd></div>
        </dl>

        <footer class="actions">
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
    </aside>
</Transition>
</template>

<style scoped>
.detail {
    /* Right-edge drawer: full height below the topbar, adaptive width so it
       fits a narrow side-view window (down to the whole pane on tiny widths). */
    position: absolute;
    top: 6.4rem;
    right: 0;
    bottom: 0;
    z-index: 9;
    display: flex;
    flex-direction: column;
    gap: 1.2rem;
    width: min(40rem, 100vw);
    overflow-y: auto;
    padding: 1.8rem;
    border-top-left-radius: var(--radius-xl);
    border-bottom-left-radius: var(--radius-xl);
    border-left: 3px solid var(--accent);
    box-shadow: var(--shadow-lg);
}

.drawer-enter-active,
.drawer-leave-active {
    transition:
        transform var(--duration-normal) var(--ease-default),
        opacity var(--duration-normal) var(--ease-default);
}

.drawer-enter-from,
.drawer-leave-to {
    transform: translateX(100%);
    opacity: 0;
}

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

.relations {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    list-style: none;
}

.relation {
    display: flex;
    align-items: baseline;
    gap: 0.8rem;
    width: 100%;
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

.rel-verb {
    font-size: var(--text-caption);
    font-weight: 600;
    white-space: nowrap;
}

.rel-target {
    font-size: var(--text-body-sm);
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
