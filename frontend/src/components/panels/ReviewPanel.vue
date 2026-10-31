<script setup lang="ts">
import NoteText from '@/components/common/NoteText.vue'
import InfoBadge from '@/components/common/InfoBadge.vue'
import PanelSection from '@/components/common/PanelSection.vue'
import UiButton from '@/components/common/UiButton.vue'
import { computed, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import SidePanel from '@/components/common/SidePanel.vue'
import { api } from '@/services/api'
import { onProjectSwitch } from '@/composables/onProjectSwitch'
import { useConfigStore } from '@/stores/config'
import { useCrowdingStore } from '@/stores/crowding'
import { useGraphStore } from '@/stores/graph'
import type { GraphEdge, GraphNode } from '@/types/graph'

/**
 * The transparency surface for silent writes (PLAN §6A): what Claude added
 * recently, what is still provisional (approve → trusted), and which
 * conflicts are unresolved (resolve / dismiss). Rows focus the node on the
 * canvas, so this drawer and the right-hand NodeDetail work as a pair.
 * Rows carry their crowding subject's name (0.11.0), and provisional notes
 * group by subject, so a pile on one subject is judged together.
 */
const store = useGraphStore()
const config = useConfigStore()
const { nodeList, edgeList, nodes, suspects, drift } = storeToRefs(store)

const open = ref(false)
const busyId = ref<string | null>(null)
const decayIds = ref<Set<string>>(new Set())
const scanning = ref(false)
const scanNote = ref<string | null>(null)

const RECENT_COUNT = 8

const crowding = useCrowdingStore()
/** An open panel reads crowding, so the store keeps it fresh while open. */
watch(open, (isOpen) => {
    crowding.readers += isOpen ? 1 : -1
})

/** Note id → the name of the open subject it belongs to. */
const subjectOf = computed(() => {
    const names = new Map<string, string>()
    for (const s of crowding.report?.subjects ?? []) {
        if (s.name) for (const id of s.members) names.set(id, s.name)
    }
    return names
})

/** A pair's subject: the one both ends share, else either end's. */
function pairSubject(a: string, b: string): string | undefined {
    return subjectOf.value.get(a) ?? subjectOf.value.get(b)
}

const active = (n: GraphNode): boolean => n.valid_until == null

const conflicts = computed(() =>
    edgeList.value.filter(
        (e) => config.isActiveConflict(e),
    ),
)

// Needs a human eye: never-approved Claude nodes, plus anything whose
// computed trust has gone stale (stale first, then newest). A pin is an
// endorsement at least as strong as approval — pinned nodes are reviewed.
const provisional = computed(() =>
    nodeList.value
        .filter(
            (n) =>
                active(n) &&
                ((n.source !== 'user' && n.approved_at == null && n.trust_override == null) ||
                    n.stale),
        )
        .sort((a, b) => Number(b.stale) - Number(a.stale) || b.created_at - a.created_at),
)

/** Provisional notes by subject — named piles first, biggest first; the
 *  rest in one unnamed group, in the order above. */
const provisionalGroups = computed(() => {
    const groups = new Map<string, GraphNode[]>()
    for (const n of provisional.value) {
        const name = subjectOf.value.get(n.id) ?? ''
        groups.set(name, [...(groups.get(name) ?? []), n])
    }
    const loose = groups.get('') ?? []
    groups.delete('')
    const named = [...groups].filter(([, ns]) => ns.length > 1)
    for (const [, ns] of [...groups].filter(([, ns]) => ns.length === 1)) loose.unshift(...ns)
    named.sort((a, b) => b[1].length - a[1].length)
    const out = named.map(([name, notes]) => ({ name, notes }))
    if (loose.length) {
        loose.sort((a, b) => Number(b.stale) - Number(a.stale) || b.created_at - a.created_at)
        out.push({ name: '', notes: loose })
    }
    return out
})

const recent = computed(() =>
    [...nodeList.value]
        .filter(active)
        .sort((a, b) => b.created_at - a.created_at)
        .slice(0, RECENT_COUNT),
)

const attention = computed(
    () =>
        conflicts.value.length +
        provisional.value.length +
        suspects.value.length +
        drift.value.length,
)

watch(open, async (isOpen) => {
    if (!isOpen) return
    void store.loadDrift() // files move on disk without any graph event
    try {
        const preview = await api.decayPreview()
        decayIds.value = new Set(preview.ids)
    } catch {
        decayIds.value = new Set()
    }
})

// The decay preview and the scan report were computed on the other graph.
onProjectSwitch(() => {
    decayIds.value = new Set()
    scanNote.value = null
    busyId.value = null
})

function title(id: string): string {
    return nodes.value.get(id)?.title ?? id
}

function accent(n: GraphNode): string {
    return config.accent(n.type)
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
const approveAll = (name: string, notes: GraphNode[]) =>
    run(`subject:${name}`, async () => {
        for (const n of notes) await store.approve(n.id)
    })
const settleConflict = (e: GraphEdge, status: 'resolved' | 'dismissed') =>
    run(e.id, () => store.setEdgeStatus(e.id, status))
const judge = (id: string, verdict: 'conflict' | 'replaces' | 'dismiss') =>
    run(id, () => store.resolveSuspect(id, verdict))

async function scanNow(): Promise<void> {
    scanning.value = true
    scanNote.value = null
    try {
        const added = await store.scanConflicts()
        scanNote.value = added ? `${added} new suspect${added > 1 ? 's' : ''}` : 'nothing new'
    } catch (e) {
        scanNote.value = e instanceof Error ? e.message : String(e)
    } finally {
        scanning.value = false
    }
}
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

    <SidePanel
        :open="open"
        side="left"
        panel-id="review"
        :default-rem="42"
        :min-rem="28"
        :dismiss="() => (open = false)"
        title="Review"
        style="--panel-gap: 1.4rem"
    >
        <template #actions>
            <NoteText v-if="scanNote" as="span">{{ scanNote }}</NoteText>
            <UiButton
                size="sm"
                :disabled="scanning"
                title="Sweep the graph for unlinked look-alike pairs"
                @click="scanNow"
            >
                {{ scanning ? 'Scanning…' : 'Scan now' }}
            </UiButton>
        </template>

        <PanelSection v-if="suspects.length" title="Suspected conflicts — needs judgment">
            <div v-for="s in suspects" :key="s.id" class="conflict">
                <div class="row pair">
                    <button class="row-link" type="button" :title="s.a.title" @click="store.select(s.a.id)">
                        {{ s.a.title }}
                    </button>
                    <span class="verb">vs</span>
                    <button class="row-link" type="button" :title="s.b.title" @click="store.select(s.b.id)">
                        {{ s.b.title }}
                    </button>
                    <span class="trust">{{ Math.round(s.similarity * 100) }}%</span>
                    <span v-if="pairSubject(s.a.id, s.b.id)" class="subject-chip" title="The crowded subject these notes belong to">{{ pairSubject(s.a.id, s.b.id) }}</span>
                    <span
                        v-if="s.nli_label"
                        class="nli-hint"
                        :class="s.nli_label"
                        :title="s.nli_label === 'inherited'
                            ? `Inherited (${Math.round((s.nli_score ?? 0) * 100)}%): the newer note contradicts or replaces something the other builds on, needs, or holds because of — check whether it still stands`
                            : `Local NLI hint (${Math.round((s.nli_score ?? 0) * 100)}%)${s.nli_direction ? ` — negation likely on the ${s.nli_direction} side` : ''} — a suggestion for your judgment, never a verdict`"
                    >{{ s.nli_label }}{{ s.nli_direction ? ` · ${s.nli_direction} negates` : '' }}</span>
                </div>
                <div class="row-actions">
                    <UiButton size="sm" :disabled="busyId === s.id" title="They contradict — record a conflicts-with edge" @click="judge(s.id, 'conflict')">
                        Conflict
                    </UiButton>
                    <UiButton size="sm" :disabled="busyId === s.id" title="The newer supersedes — records replaces and archives the older" @click="judge(s.id, 'replaces')">
                        Replaces
                    </UiButton>
                    <UiButton size="sm" variant="ghost" :disabled="busyId === s.id" title="Fine together — never raised again" @click="judge(s.id, 'dismiss')">
                        Dismiss
                    </UiButton>
                </div>
            </div>
        </PanelSection>

        <PanelSection v-if="conflicts.length" title="Unresolved conflicts">
            <div v-for="e in conflicts" :key="e.id" class="conflict">
                <div class="row pair">
                    <button class="row-link" type="button" :title="title(e.from_id)" @click="store.select(e.from_id)">
                        {{ title(e.from_id) }}
                    </button>
                    <span class="verb">conflicts with</span>
                    <button class="row-link" type="button" :title="title(e.to_id)" @click="store.select(e.to_id)">
                        {{ title(e.to_id) }}
                    </button>
                    <span v-if="pairSubject(e.from_id, e.to_id)" class="subject-chip" title="The crowded subject these notes belong to">{{ pairSubject(e.from_id, e.to_id) }}</span>
                </div>
                <div class="row-actions">
                    <UiButton size="sm" :disabled="busyId === e.id" @click="settleConflict(e, 'resolved')">
                        Resolve
                    </UiButton>
                    <UiButton size="sm" variant="ghost" :disabled="busyId === e.id" @click="settleConflict(e, 'dismissed')">
                        Dismiss
                    </UiButton>
                </div>
            </div>
        </PanelSection>

        <PanelSection v-if="drift.length" title="Drifted code refs — the code moved, the memory didn't">
            <div v-for="d in drift" :key="d.id" class="conflict">
                <button class="row" type="button" :title="d.title" @click="store.select(d.id)">
                    <span class="dot" :style="{ background: config.accent(d.type) }" />
                    <span class="row-title">{{ d.title }}</span>
                </button>
                <div class="missing-refs">
                    <span
                        v-for="r in d.missing"
                        :key="r"
                        class="missing-ref"
                        title="This file no longer exists in the project"
                    >
                        {{ r }}
                    </span>
                </div>
            </div>
        </PanelSection>

        <PanelSection v-if="provisional.length" title="Needs review — approve what's right">
            <template v-for="g in provisionalGroups" :key="g.name">
                <div v-if="g.name" class="subject-head">
                    <span class="subject-chip" title="A crowded subject: these notes sit close together in vector space">{{ g.name }}</span>
                    <span class="date">{{ g.notes.length }} notes</span>
                    <span class="grow" />
                    <UiButton
                        size="sm"
                        variant="ghost"
                        :disabled="busyId != null"
                        title="Approve every provisional note of this subject"
                        @click="approveAll(g.name, g.notes)"
                    >
                        {{ busyId === `subject:${g.name}` ? 'Approving…' : 'Approve all' }}
                    </UiButton>
                </div>
                <div v-for="n in g.notes" :key="n.id" class="item" :class="{ grouped: g.name }">
                    <button class="row" type="button" @click="store.select(n.id)">
                        <span class="dot" :style="{ background: accent(n) }" />
                        <span class="row-title">{{ n.title }}</span>
                        <InfoBadge v-if="decayIds.has(n.id)" pill tone="warn" title="Will be archived by the next decay pass">decaying</InfoBadge>
                        <InfoBadge v-if="n.stale" pill tone="danger">stale</InfoBadge>
                        <span class="trust">{{ Math.round(n.trust * 100) }}%</span>
                    </button>
                    <UiButton size="sm" :disabled="busyId === n.id" @click="approve(n)">
                        Approve
                    </UiButton>
                </div>
            </template>
        </PanelSection>

        <PanelSection v-if="recent.length" title="Recently added">
            <div v-for="n in recent" :key="n.id" class="item">
                <button class="row" type="button" @click="store.select(n.id)">
                    <span class="dot" :style="{ background: accent(n) }" />
                    <span class="row-title">{{ n.title }}</span>
                    <span class="date">{{ fmtDate(n.created_at) }}</span>
                </button>
            </div>
        </PanelSection>

        <NoteText v-if="!attention && !recent.length" tone="empty">Nothing to review.</NoteText>
    </SidePanel>
</div>
</template>

<style scoped>
.toggle {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.6rem 1.2rem;
    border-radius: var(--radius-control);
    border: 1px solid var(--border-default);
    background-color: var(--surface-glass);
    backdrop-filter: var(--glass-backdrop);
    color: var(--control-text);
    font-size: var(--text-label);
    font-weight: 600;
    cursor: pointer;
}

.toggle:hover,
.toggle.active {
    color: var(--text-primary);
    background-image: linear-gradient(var(--node-hover-surface), var(--node-hover-surface));
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

/* A pair row is a plain container; each endpoint is its own link so both
   nodes of a conflict/suspect are reachable. */
.row.pair {
    cursor: default;
}

.row-link {
    flex: 1;
    min-width: 0;
    padding: 0;
    border: none;
    background: transparent;
    font-size: var(--text-body-sm);
    color: var(--text-primary);
    text-align: left;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
}

.row-link:hover {
    text-decoration: underline;
    color: var(--interactive-primary);
}

.subject-head {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.6rem 0.7rem 0.2rem;
}

.item.grouped {
    padding-left: 1.2rem;
}

.subject-chip {
    flex: none;
    max-width: 24rem;
    overflow: hidden;
    padding: 0.1rem 0.6rem;
    border-radius: var(--radius-full);
    border: 1px solid color-mix(in srgb, rgb(249 115 22) 40%, transparent);
    background-color: color-mix(in srgb, rgb(249 115 22) 12%, transparent);
    color: var(--text-secondary);
    font-size: var(--text-caption);
    font-weight: 600;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.grow {
    flex: 1;
}

.verb {
    flex: none;
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--edge-conflicts, var(--node-problem));
    white-space: nowrap;
}

.trust,
.date {
    flex: none;
    margin-left: auto;
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.row-actions {
    display: flex;
    gap: 0.4rem;
    padding-left: 0.7rem;
}

.missing-refs {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
    padding-left: 0.7rem;
}

.missing-ref {
    padding: 0.1rem 0.6rem;
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--node-problem);
    background-color: color-mix(in srgb, var(--node-problem) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--node-problem) 35%, transparent);
}

.nli-hint {
    flex: none;
    padding: 0.1rem 0.6rem;
    border-radius: var(--radius-full);
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--text-tertiary);
    background-color: var(--surface-muted);
}

.nli-hint.contradiction {
    color: var(--node-problem);
    background-color: color-mix(in srgb, var(--node-problem) 14%, transparent);
}

.nli-hint.entailment {
    color: var(--trust-trusted);
    background-color: color-mix(in srgb, var(--trust-trusted) 14%, transparent);
}

/* 0.9.9: queued because the newer note contradicts something this one
   builds on / needs / holds because of — a review, not a direct clash. */
.nli-hint.inherited {
    color: var(--node-caution);
    background-color: color-mix(in srgb, var(--node-caution) 14%, transparent);
}
</style>
