<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import SidePanel from '@/components/common/SidePanel.vue'
import { NODE_ACCENT_VAR } from '@/constants/ontology'
import { api } from '@/services/api'
import { useGraphStore } from '@/stores/graph'
import type { AnsweredHint, ClaimReport, GraphNode, PromotionCandidate } from '@/types/graph'

/**
 * The Checkup panel (PLAN §7A): one-click audit passes over the whole graph,
 * powered by the local cortex — zero tokens, fully offline. Every button
 * produces *nominations* into the existing review flow (suspects, selections);
 * nothing here applies a verdict or moves trust.
 */
const store = useGraphStore()
const { nodeList, edgeList } = storeToRefs(store)

const open = ref(false)
const nliReady = ref<boolean | null>(null)

watch(open, async (isOpen) => {
    if (!isOpen) return
    try {
        nliReady.value = (await api.system()).nli
    } catch {
        nliReady.value = null
    }
})

// --- deep sweeps -----------------------------------------------------------

const running = ref<string | null>(null)
const sweepNote = ref<Record<string, string>>({})

async function runSweep(kind: 'conflicts' | 'duplicates'): Promise<void> {
    running.value = kind
    try {
        const sweep =
            kind === 'conflicts' ? await api.auditConflicts() : await api.auditDuplicates()
        const what = kind === 'conflicts' ? 'hidden conflict' : 'duplicate'
        sweepNote.value[kind] = sweep.queued
            ? `${sweep.queued} ${what}${sweep.queued > 1 ? 's' : ''} queued for judgment (${sweep.examined} pairs judged)`
            : `nothing found (${sweep.examined} pairs judged)`
        if (sweep.truncated) sweepNote.value[kind] += ' — budget hit, run again to continue'
        if (sweep.queued) await store.loadSuspects()
    } catch (e) {
        sweepNote.value[kind] = e instanceof Error ? e.message : String(e)
    } finally {
        running.value = null
    }
}

// --- answered problems -------------------------------------------------------

const answered = ref<AnsweredHint[] | null>(null)

async function runAnswered(): Promise<void> {
    running.value = 'answered'
    try {
        answered.value = await api.auditAnswered()
    } catch (e) {
        sweepNote.value.answered = e instanceof Error ? e.message : String(e)
    } finally {
        running.value = null
    }
}

// --- promotion nominations (PLAN §7C) ----------------------------------------

const promotions = ref<PromotionCandidate[] | null>(null)
const promoted = ref(new Set<string>())

async function runPromotions(): Promise<void> {
    running.value = 'promotions'
    try {
        const res = await api.promotions()
        promotions.value = res.candidates
        sweepNote.value.promotions = res.skipped.length
            ? `some projects were skipped: ${res.skipped.join('; ')}`
            : ''
    } catch (e) {
        sweepNote.value.promotions = e instanceof Error ? e.message : String(e)
    } finally {
        running.value = null
    }
}

/** The user's approval: copy the node into the home graph. The project
 * copies stay put — promotion adds a shared source of truth, it never
 * deletes local context. code_refs are dropped: repo paths mean nothing
 * outside their repo. */
async function promote(c: PromotionCandidate): Promise<void> {
    try {
        await api.promoteToHome({
            type: c.node.type,
            title: c.node.title,
            body: c.node.body ?? undefined,
            durability: c.node.durability,
            source: 'user',
            tags: c.node.tags,
        })
        promoted.value.add(c.node.id)
    } catch (e) {
        sweepNote.value.promotions = e instanceof Error ? e.message : String(e)
    }
}

// --- claim check -------------------------------------------------------------

const claim = ref('')
const report = ref<ClaimReport | null>(null)

async function runClaim(): Promise<void> {
    if (!claim.value.trim()) return
    running.value = 'claim'
    report.value = null
    try {
        report.value = await api.checkClaim(claim.value.trim())
    } catch (e) {
        sweepNote.value.claim = e instanceof Error ? e.message : String(e)
    } finally {
        running.value = null
    }
}

// --- structural hygiene (no ML — instant, from the loaded graph) -------------

const active = (n: GraphNode): boolean => n.valid_until == null

/** Decisions with no outgoing `because` — canon without recorded reasons. */
const unjustified = computed(() => {
    const because = new Set(
        edgeList.value.filter((e) => e.type === 'because').map((e) => e.from_id),
    )
    return nodeList.value.filter(
        (n) => active(n) && n.type === 'Decision' && !because.has(n.id),
    )
})

/** Nodes touching no edge at all — islands the graph can't reason about. */
const orphans = computed(() => {
    const linked = new Set(edgeList.value.flatMap((e) => [e.from_id, e.to_id]))
    return nodeList.value.filter((n) => active(n) && !linked.has(n.id))
})

/**
 * Unreachable knowledge: no edges AND no tags — text search is the only road
 * in, so a differently-phrased future query may never find these.
 */
const unreachable = computed(() =>
    orphans.value.filter((n) => !(n.tags && n.tags.length)),
)

const STRUCT_CAP = 8
</script>

<template>
<div class="checkup-root">
    <button
        class="toggle"
        type="button"
        :class="{ active: open }"
        :title="open ? 'Close checkup' : 'Audit the graph with the local cortex — conflicts, duplicates, claims'"
        @click="open = !open"
    >
        Checkup
    </button>

    <SidePanel
        :open="open"
        side="left"
        panel-id="checkup"
        :default-rem="36"
        :min-rem="28"
        :dismiss="() => (open = false)"
        title="Checkup"
        style="--panel-gap: 1.4rem"
    >
        <p v-if="nliReady === false" class="nli-off">
            The local NLI model isn't loaded yet (it downloads on daemon startup, ~35 MB) —
            the sweeps below will report it; structural checks still work.
        </p>

        <section class="block">
            <h3 class="block-title">Deep sweeps — local models, zero tokens</h3>
            <div class="sweep">
                <button
                    class="run"
                    type="button"
                    :disabled="running != null"
                    title="Rescan look-alike pairs and keep only ones the NLI model reads as contradictions — findings land in Review for your judgment"
                    @click="runSweep('conflicts')"
                >
                    {{ running === 'conflicts' ? 'Sweeping…' : 'Find hidden conflicts' }}
                </button>
                <span v-if="sweepNote.conflicts" class="note">{{ sweepNote.conflicts }}</span>
            </div>
            <div class="sweep">
                <button
                    class="run"
                    type="button"
                    :disabled="running != null"
                    title="Find pairs that state the same thing (mutual entailment) — judge them as Replaces in Review to merge histories"
                    @click="runSweep('duplicates')"
                >
                    {{ running === 'duplicates' ? 'Sweeping…' : 'Find duplicates' }}
                </button>
                <span v-if="sweepNote.duplicates" class="note">{{ sweepNote.duplicates }}</span>
            </div>
            <div class="sweep">
                <button
                    class="run"
                    type="button"
                    :disabled="running != null"
                    title="Does any Decision/Resolution/Insight already answer an open Problem? Nominations only — you link and resolve"
                    @click="runAnswered"
                >
                    {{ running === 'answered' ? 'Checking…' : 'Check open problems' }}
                </button>
                <span v-if="sweepNote.answered" class="note">{{ sweepNote.answered }}</span>
            </div>
            <div v-if="answered" class="results">
                <p v-if="answered.length === 0" class="note">no open problem looks answered</p>
                <div v-for="h in answered" :key="h.problem.id + h.candidate.id" class="pair-row">
                    <button class="row-link" type="button" :title="h.problem.title" @click="store.select(h.problem.id)">
                        {{ h.problem.title }}
                    </button>
                    <span class="verb">maybe answered by</span>
                    <button class="row-link" type="button" :title="h.candidate.title" @click="store.select(h.candidate.id)">
                        {{ h.candidate.title }}
                    </button>
                    <span class="pct">{{ Math.round(h.entailment * 100) }}%</span>
                </div>
            </div>
        </section>

        <section class="block">
            <h3 class="block-title">Cross-project — promote shared canon</h3>
            <div class="sweep">
                <button
                    class="run"
                    type="button"
                    :disabled="running != null"
                    title="Find Principles/Cautions that recur in your other projects' graphs — candidates for the shared home graph. Nominations only; you approve each"
                    @click="runPromotions"
                >
                    {{ running === 'promotions' ? 'Scanning…' : 'Find promotion candidates' }}
                </button>
                <span v-if="sweepNote.promotions" class="note">{{ sweepNote.promotions }}</span>
            </div>
            <div v-if="promotions" class="results">
                <p v-if="promotions.length === 0" class="note">
                    nothing recurs across projects that isn't already in the home graph
                </p>
                <div v-for="c in promotions" :key="c.node.id" class="promo-row">
                    <button class="row-link" type="button" :title="c.node.title" @click="store.select(c.node.id)">
                        <span class="dot" :style="{ background: NODE_ACCENT_VAR[c.node.type] }" />
                        {{ c.node.title }}
                    </button>
                    <span class="verb">
                        also in {{ c.matches.map((m) => m.project).filter((v, i, a) => a.indexOf(v) === i).join(', ') }}
                    </span>
                    <button
                        v-if="!promoted.has(c.node.id)"
                        class="promote-btn"
                        type="button"
                        title="Copy this into the shared home graph (project copies stay put)"
                        @click="promote(c)"
                    >
                        Promote to home
                    </button>
                    <span v-else class="note">promoted ✓</span>
                </div>
            </div>
        </section>

        <section class="block">
            <h3 class="block-title">Check a claim against the canon</h3>
            <textarea
                v-model="claim"
                class="claim-input"
                rows="2"
                placeholder="One declarative sentence, e.g. “we store sessions in localStorage”"
                @keydown.enter.exact.prevent="runClaim"
            />
            <button class="run" type="button" :disabled="running != null || !claim.trim()" @click="runClaim">
                {{ running === 'claim' ? 'Checking…' : 'Check' }}
            </button>
            <span v-if="sweepNote.claim" class="note">{{ sweepNote.claim }}</span>
            <div v-if="report" class="results">
                <template v-for="(group, label) in { contradicts: report.contradicts, supports: report.supports, silent: report.silent }" :key="label">
                    <p v-if="group.length" class="group-label" :class="label">{{ label }}</p>
                    <button
                        v-for="v in group"
                        :key="v.id"
                        class="verdict-row"
                        type="button"
                        :title="v.title"
                        @click="store.select(v.id)"
                    >
                        <span class="dot" :style="{ background: NODE_ACCENT_VAR[v.type] }" />
                        <span class="row-title">{{ v.title }}</span>
                        <span class="pct">{{ Math.round(Math.max(v.entailment, v.contradiction, v.neutral) * 100) }}%</span>
                    </button>
                </template>
                <p v-if="!report.contradicts.length && !report.supports.length" class="note">
                    the canon is silent on this — if it matters, it's worth capturing
                </p>
            </div>
        </section>

        <section class="block">
            <h3 class="block-title">Structure — instant, no models</h3>
            <p class="struct-line">
                <b>{{ unjustified.length }}</b> decisions without a recorded reason (<code>because</code>)
            </p>
            <button
                v-for="n in unjustified.slice(0, STRUCT_CAP)"
                :key="n.id"
                class="verdict-row"
                type="button"
                @click="store.select(n.id)"
            >
                <span class="dot" :style="{ background: NODE_ACCENT_VAR[n.type] }" />
                <span class="row-title">{{ n.title }}</span>
            </button>
            <p class="struct-line">
                <b>{{ unreachable.length }}</b> reachability islands (no edges <i>and</i> no tags — only text search finds these)
            </p>
            <button
                v-for="n in unreachable.slice(0, STRUCT_CAP)"
                :key="n.id"
                class="verdict-row"
                type="button"
                @click="store.select(n.id)"
            >
                <span class="dot" :style="{ background: NODE_ACCENT_VAR[n.type] }" />
                <span class="row-title">{{ n.title }}</span>
            </button>
            <p class="struct-line">
                <b>{{ orphans.length }}</b> unconnected nodes (no edges at all)
            </p>
            <button
                v-for="n in orphans.slice(0, STRUCT_CAP)"
                :key="n.id"
                class="verdict-row"
                type="button"
                @click="store.select(n.id)"
            >
                <span class="dot" :style="{ background: NODE_ACCENT_VAR[n.type] }" />
                <span class="row-title">{{ n.title }}</span>
            </button>
        </section>
    </SidePanel>
</div>
</template>

<style scoped>
.toggle {
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

.nli-off {
    padding: 0.8rem 1rem;
    border-radius: var(--radius-md);
    font-size: var(--text-caption);
    color: var(--node-caution);
    background-color: color-mix(in srgb, var(--node-caution) 10%, transparent);
}

.block-title {
    margin-bottom: 0.8rem;
    font-size: var(--text-caption);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-tertiary);
}

.sweep {
    display: flex;
    align-items: center;
    gap: 0.8rem;
    margin-bottom: 0.6rem;
}

.run {
    flex: none;
    padding: 0.6rem 1rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: var(--surface-elevated);
    color: var(--text-primary);
    font-size: var(--text-label);
    font-weight: 600;
    cursor: pointer;
}

.run:disabled {
    opacity: 0.5;
    cursor: default;
}

.run:hover:not(:disabled) {
    background-color: var(--node-hover-surface);
}

.note {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.results {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    margin-top: 0.6rem;
}

.group-label {
    margin-top: 0.4rem;
    font-size: var(--text-caption);
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.05em;
}

.group-label.contradicts {
    color: var(--node-problem);
}

.group-label.supports {
    color: var(--trust-trusted);
}

.group-label.silent {
    color: var(--text-tertiary);
}

.pair-row {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    min-width: 0;
}

.verb {
    flex: none;
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.row-link {
    min-width: 0;
    padding: 0.2rem 0;
    border: none;
    background: transparent;
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    text-align: left;
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.row-link:hover {
    text-decoration: underline;
}

.verdict-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    width: 100%;
    min-width: 0;
    padding: 0.4rem 0.6rem;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    text-align: left;
    cursor: pointer;
}

.verdict-row:hover {
    background-color: var(--interactive-ghost-hover);
}

.dot {
    flex: none;
    width: 0.8rem;
    height: 0.8rem;
    border-radius: var(--radius-full);
}

.row-title {
    min-width: 0;
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.pct {
    flex: none;
    margin-left: auto;
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.claim-input {
    width: 100%;
    margin-bottom: 0.6rem;
    padding: 0.7rem 0.9rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: var(--surface-sunken);
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    font-family: inherit;
    resize: vertical;
}

.struct-line {
    margin: 0.6rem 0 0.3rem;
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
}

.promo-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    min-width: 0;
}

.promo-row .row-link {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex: 1;
}

.promote-btn {
    flex: none;
    margin-left: auto;
    padding: 0.3rem 0.8rem;
    border-radius: var(--radius-full);
    border: 1px solid color-mix(in srgb, var(--interactive-primary) 55%, transparent);
    background-color: color-mix(in srgb, var(--interactive-primary) 12%, transparent);
    color: var(--interactive-primary);
    font-size: var(--text-caption);
    font-weight: 600;
    cursor: pointer;
}

.promote-btn:hover {
    background-color: color-mix(in srgb, var(--interactive-primary) 22%, transparent);
}
</style>
