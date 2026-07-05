<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { api } from '@/services/api'
import { NODE_ACCENT_VAR } from '@/constants/ontology'
import { useGraphStore } from '@/stores/graph'
import type { GraphEdge, GraphNode } from '@/types/graph'

/**
 * The transparency surface for silent writes (PLAN §6A): what Claude added
 * recently, what is still provisional (approve → trusted), and which
 * conflicts are unresolved (resolve / dismiss). Rows focus the node on the
 * canvas, so this drawer and the right-hand NodeDetail work as a pair.
 */
const store = useGraphStore()
const { nodeList, edgeList, nodes } = storeToRefs(store)

const open = ref(false)
const busyId = ref<string | null>(null)
const decayIds = ref<Set<string>>(new Set())

const RECENT_COUNT = 8

const active = (n: GraphNode): boolean => n.valid_until == null

const conflicts = computed(() =>
    edgeList.value.filter(
        (e) => e.type === 'conflicts-with' && (e.status == null || e.status === 'active'),
    ),
)

// Needs a human eye: never-approved Claude nodes, plus anything whose
// computed trust has gone stale (stale first, then newest).
const provisional = computed(() =>
    nodeList.value
        .filter((n) => active(n) && ((n.source === 'claude' && n.approved_at == null) || n.stale))
        .sort((a, b) => Number(b.stale) - Number(a.stale) || b.created_at - a.created_at),
)

const recent = computed(() =>
    [...nodeList.value]
        .filter(active)
        .sort((a, b) => b.created_at - a.created_at)
        .slice(0, RECENT_COUNT),
)

const attention = computed(() => conflicts.value.length + provisional.value.length)

watch(open, async (isOpen) => {
    if (!isOpen) return
    try {
        const preview = await api.decayPreview()
        decayIds.value = new Set(preview.ids)
    } catch {
        decayIds.value = new Set()
    }
})

function title(id: string): string {
    return nodes.value.get(id)?.title ?? id
}

function accent(n: GraphNode): string {
    return NODE_ACCENT_VAR[n.type]
}

function fmtDate(secs: number): string {
    return new Date(secs * 1000).toLocaleDateString(undefined, {
        month: 'short',
        day: 'numeric',
    })
}

async function run(id: string, action: () => Promise<void>): Promise<void> {
    busyId.value = id
    try {
        await action()
    } finally {
        busyId.value = null
    }
}

const approve = (n: GraphNode) => run(n.id, () => store.approve(n.id))
const settleConflict = (e: GraphEdge, status: 'resolved' | 'dismissed') =>
    run(e.id, () => store.setEdgeStatus(e.id, status))
</script>

<template>
<div class="review-root">
    <button
        class="toggle"
        type="button"
        :class="{ active: open }"
        :title="open ? 'Close review' : 'Review recent & provisional memory'"
        @click="open = !open"
    >
        Review
        <span v-if="attention" class="count">{{ attention }}</span>
    </button>

    <Transition name="drawer-left">
        <aside v-if="open" class="panel glass-panel">
            <header class="head">
                <h2 class="heading">Review</h2>
                <button class="close" type="button" aria-label="Close" @click="open = false">×</button>
            </header>

            <section v-if="conflicts.length" class="block">
                <h3 class="block-title">Unresolved conflicts</h3>
                <div v-for="e in conflicts" :key="e.id" class="conflict">
                    <button class="row" type="button" @click="store.select(e.from_id)">
                        <span class="row-title">{{ title(e.from_id) }}</span>
                        <span class="verb">conflicts with</span>
                        <span class="row-title">{{ title(e.to_id) }}</span>
                    </button>
                    <div class="row-actions">
                        <button class="mini" type="button" :disabled="busyId === e.id" @click="settleConflict(e, 'resolved')">
                            Resolve
                        </button>
                        <button class="mini ghost" type="button" :disabled="busyId === e.id" @click="settleConflict(e, 'dismissed')">
                            Dismiss
                        </button>
                    </div>
                </div>
            </section>

            <section v-if="provisional.length" class="block">
                <h3 class="block-title">Needs review — approve what's right</h3>
                <div v-for="n in provisional" :key="n.id" class="item">
                    <button class="row" type="button" @click="store.select(n.id)">
                        <span class="dot" :style="{ background: accent(n) }" />
                        <span class="row-title">{{ n.title }}</span>
                        <span v-if="decayIds.has(n.id)" class="stale" title="Will be archived by the next decay pass">decaying</span>
                        <span v-if="n.stale" class="stale-badge">stale</span>
                        <span class="trust">{{ Math.round(n.trust * 100) }}%</span>
                    </button>
                    <button class="mini" type="button" :disabled="busyId === n.id" @click="approve(n)">
                        Approve
                    </button>
                </div>
            </section>

            <section v-if="recent.length" class="block">
                <h3 class="block-title">Recently added</h3>
                <div v-for="n in recent" :key="n.id" class="item">
                    <button class="row" type="button" @click="store.select(n.id)">
                        <span class="dot" :style="{ background: accent(n) }" />
                        <span class="row-title">{{ n.title }}</span>
                        <span class="date">{{ fmtDate(n.created_at) }}</span>
                    </button>
                </div>
            </section>

            <p v-if="!attention && !recent.length" class="empty">Nothing to review.</p>
        </aside>
    </Transition>
</div>
</template>

<style scoped>
.toggle {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.6rem 1.2rem;
    border-radius: var(--radius-full);
    border: 1px solid var(--border-default);
    background-color: var(--surface-glass);
    backdrop-filter: var(--glass-backdrop);
    color: var(--text-secondary);
    font-size: var(--text-label);
    font-weight: 600;
    cursor: pointer;
}

.toggle:hover,
.toggle.active {
    color: var(--text-primary);
    background-color: var(--node-hover-surface);
}

.count {
    min-width: 1.8rem;
    padding: 0.1rem 0.5rem;
    border-radius: var(--radius-full);
    background-color: var(--interactive-primary);
    color: var(--text-inverse);
    font-size: var(--text-caption);
    text-align: center;
}

.panel {
    /* Left-edge drawer, mirroring NodeDetail on the right. */
    position: fixed;
    top: 6.4rem;
    left: 0;
    bottom: 0;
    z-index: 9;
    display: flex;
    flex-direction: column;
    gap: 1.4rem;
    width: min(36rem, 100vw);
    overflow-y: auto;
    padding: 1.8rem;
    border-top-right-radius: var(--radius-xl);
    border-bottom-right-radius: var(--radius-xl);
    box-shadow: var(--shadow-lg);
}

.drawer-left-enter-active,
.drawer-left-leave-active {
    transition:
        transform var(--duration-normal) var(--ease-default),
        opacity var(--duration-normal) var(--ease-default);
}

.drawer-left-enter-from,
.drawer-left-leave-to {
    transform: translateX(-100%);
    opacity: 0;
}

.head {
    display: flex;
    align-items: center;
    justify-content: space-between;
}

.heading {
    font-size: var(--text-h3);
    font-weight: 700;
    color: var(--text-primary);
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

.block-title {
    margin-bottom: 0.6rem;
    font-size: var(--text-caption);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-tertiary);
}

.conflict {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    padding-bottom: 0.6rem;
}

.item {
    display: flex;
    align-items: center;
    gap: 0.4rem;
}

.row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex: 1;
    min-width: 0;
    padding: 0.5rem 0.7rem;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    text-align: left;
    cursor: pointer;
}

.row:hover {
    background-color: var(--interactive-ghost-hover);
}

.dot {
    flex: none;
    width: 0.8rem;
    height: 0.8rem;
    border-radius: var(--radius-full);
}

.row-title {
    font-size: var(--text-body-sm);
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.verb {
    flex: none;
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--edge-conflicts, var(--node-problem));
    white-space: nowrap;
}

.stale-badge {
    padding: 0.1rem 0.6rem;
    border-radius: var(--radius-sm);
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--node-problem);
    background-color: color-mix(in srgb, var(--node-problem) 14%, transparent);
    border: 1px solid color-mix(in srgb, var(--node-problem) 40%, transparent);
}

.trust,
.date {
    flex: none;
    margin-left: auto;
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.stale {
    flex: none;
    padding: 0.1rem 0.5rem;
    border-radius: var(--radius-sm);
    font-size: var(--text-caption);
    color: var(--node-problem);
    background-color: color-mix(in srgb, var(--node-problem) 14%, transparent);
}

.row-actions {
    display: flex;
    gap: 0.4rem;
    padding-left: 0.7rem;
}

.mini {
    flex: none;
    padding: 0.35rem 0.9rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: var(--surface-elevated);
    color: var(--text-primary);
    font-size: var(--text-caption);
    font-weight: 600;
    cursor: pointer;
}

.mini:disabled {
    opacity: 0.5;
    cursor: default;
}

.mini:hover:not(:disabled) {
    background-color: var(--node-hover-surface);
}

.mini.ghost {
    background-color: transparent;
    color: var(--text-secondary);
}

.empty {
    font-size: var(--text-body-sm);
    color: var(--text-tertiary);
}
</style>
