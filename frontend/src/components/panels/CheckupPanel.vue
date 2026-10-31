<script setup lang="ts">
import NoteText from '@/components/common/NoteText.vue'
import UiButton from '@/components/common/UiButton.vue'
import PanelSection from '@/components/common/PanelSection.vue'
import AutoTextarea from '@/components/common/AutoTextarea.vue'
import { computed, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import SidePanel from '@/components/common/SidePanel.vue'
import { useConfigStore } from '@/stores/config'
import { api } from '@/services/api'
import { onProjectSwitch } from '@/composables/onProjectSwitch'
import { useGraphStore } from '@/stores/graph'
import { useCrowdingStore } from '@/stores/crowding'
import type {
    AnsweredHint,
    AuditSweep,
    ClaimReport,
    GraphNode,
    NliAgreement,
    PromotionCandidate,
    StaleTriage,
} from '@/types/graph'

/**
 * The Checkup panel (PLAN §7A): one-click audit passes over the whole graph,
 * powered by the local cortex — zero tokens, fully offline. Every button
 * produces *nominations* into the existing review flow (suspects, selections);
 * nothing here applies a verdict or moves trust.
 */
const store = useGraphStore()
const config = useConfigStore()
const { nodeList, edgeList, driftByNode } = storeToRefs(store)

const open = ref(false)
const crowding = useCrowdingStore()
const nliReady = ref<boolean | null>(null)
const agreement = ref<NliAgreement | null>(null)

watch(open, async (isOpen) => {
    if (!isOpen) return
    try {
        nliReady.value = (await api.system()).nli
    } catch {
        nliReady.value = null
    }
    try {
        agreement.value = await api.nliAgreement()
    } catch {
        agreement.value = null
    }
})

// --- crowded subjects: the sleep queue (0.11.0) -----------------------------

/** An open panel reads crowding, so the store keeps it fresh while open. */
watch(open, (isOpen) => {
    crowding.readers += isOpen ? 1 : -1
})

const byId = computed(() => new Map(nodeList.value.map((n) => [n.id, n])))
const expanded = ref<string | null>(null)
const keeping = ref<string | null>(null)
const SUBJECT_CAP = 8

/** Subjects whose notes this pane has loaded, most central first. */
const crowded = computed(() =>
    (crowding.report?.subjects ?? [])
        .map((s) => ({
            name: s.name,
            worth: s.worth,
            members: s.members.map((id) => byId.value.get(id)).filter((n): n is GraphNode => n != null),
        }))
        .filter((s) => s.members.length > 1),
)

async function keepSubject(members: GraphNode[]): Promise<void> {
    keeping.value = members[0]!.id
    try {
        await crowding.markReviewed(members.map((n) => n.id))
    } catch (e) {
        sweepNote.value.crowded = e instanceof Error ? e.message : String(e)
    } finally {
        keeping.value = null
    }
}

// --- deep sweeps -----------------------------------------------------------

const running = ref<string | null>(null)
const sweepNote = ref<Record<string, string>>({})

const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? '' : 's'}`

/**
 * Why a sweep came back empty (0.9.11). A graph whose close pairs are all
 * linked or already judged is healthy, and "0 pairs judged" alone read like
 * broken embeddings.
 */
function emptySweep(kind: 'conflicts' | 'duplicates', sweep: AuditSweep): string {
    if (sweep.already_linked == null) return `nothing found (${sweep.examined} pairs judged)`
    const linked = sweep.already_linked
    const raised = sweep.already_raised ?? 0
    const inactive = sweep.inactive ?? 0
    const remembered = sweep.already_judged ?? 0
    const passed = linked + raised + inactive
    if (sweep.examined === 0 && passed === 0 && remembered === 0) {
        return 'nothing to compare — no two notes are close enough to be a pair'
    }
    const parts: string[] = []
    if (sweep.examined > 0) {
        const none = kind === 'conflicts' ? 'none contradicts' : 'none restates the other'
        parts.push(`${plural(sweep.examined, 'pair')} judged, ${none}`)
    }
    if (passed > 0) {
        const why = [
            linked && `${linked} linked`,
            raised && `${raised} already raised`,
            inactive && `${inactive} archived or anchored`,
        ].filter(Boolean)
        parts.push(`${plural(passed, 'close pair')} already handled (${why.join(', ')})`)
    }
    if (remembered > 0) parts.push(`${plural(remembered, 'pair')} judged on earlier runs`)
    return `nothing new — ${parts.join('; ')}`
}

async function runSweep(kind: 'conflicts' | 'duplicates'): Promise<void> {
    running.value = kind
    try {
        const sweep =
            kind === 'conflicts' ? await api.auditConflicts() : await api.auditDuplicates()
        const what = kind === 'conflicts' ? 'hidden conflict' : 'duplicate'
        sweepNote.value[kind] = sweep.queued
            ? `${sweep.queued} ${what}${sweep.queued > 1 ? 's' : ''} queued for judgment (${sweep.examined} pairs judged)`
            : emptySweep(kind, sweep)
        if (sweep.truncated) {
            // Since 0.9.11 the core remembers what it judged, so "again"
            // continues; an older core re-reads the same pairs every run.
            sweepNote.value[kind] +=
                sweep.already_judged == null
                    ? ' — budget hit, run again to continue'
                    : ' — budget hit, run again to judge the next pairs'
        }
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

// --- stale triage (0.7.0 NLI sweep) ------------------------------------------

const staleTriage = ref<StaleTriage[] | null>(null)

const TRIAGE_WORDS: Record<string, string> = {
    reconfirm: 'still backed by',
    contradicted: 'disputed by',
    isolated: 'nothing current speaks to it',
}

async function runStaleTriage(): Promise<void> {
    running.value = 'stale'
    try {
        staleTriage.value = await api.auditStale()
    } catch (e) {
        sweepNote.value.stale = e instanceof Error ? e.message : String(e)
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

/** Every report on this panel names nodes of the graph we are leaving. */
onProjectSwitch(() => {
    expanded.value = null
    nliReady.value = null
    agreement.value = null
    sweepNote.value = {}
    answered.value = null
    staleTriage.value = null
    promotions.value = null
    promoted.value = new Set()
    claim.value = ''
    report.value = null
})

// --- structural hygiene (no ML — instant, from the loaded graph) -------------

const active = (n: GraphNode): boolean => n.valid_until == null

/** Decisions with no outgoing `because` — canon without recorded reasons. */
const unjustified = computed(() => {
    const because = new Set(
        edgeList.value.filter((e) => e.type === (config.reasonVerb ?? 'because')).map((e) => e.from_id),
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
 * in, so a differently-phrased future query may never find these. The core
 * lists them too (a sleep's islands queue), without the reviewed ones.
 */
const unreachable = computed(() =>
    crowding.report
        ? crowding.report.islands.map((id) => byId.value.get(id)).filter((n): n is GraphNode => n != null)
        : orphans.value.filter((n) => !(n.tags && n.tags.length)),
)

async function keepIslands(): Promise<void> {
    keeping.value = 'islands'
    try {
        await crowding.markReviewed(unreachable.value.map((n) => n.id))
    } catch (e) {
        sweepNote.value.crowded = e instanceof Error ? e.message : String(e)
    } finally {
        keeping.value = null
    }
}

/** Nodes carrying at least one path-shaped code_ref that no longer exists —
 *  open one from here to fix it by hand in the edit drawer. */
const drifted = computed(() => nodeList.value.filter((n) => active(n) && driftByNode.value.has(n.id)))

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
        :default-rem="42"
        :min-rem="28"
        :dismiss="() => (open = false)"
        title="Checkup"
        style="--panel-gap: 1.4rem"
    >
        <p v-if="nliReady === false" class="nli-off">
            The local NLI model isn't loaded yet (it downloads on daemon startup, 27 MB) —
            the sweeps below will report it; structural checks still work.
        </p>

        <PanelSection title="Crowded subjects — the sleep queue">
            <NoteText v-if="!crowding.report && crowding.loading">measuring crowding…</NoteText>
            <NoteText v-else-if="crowding.error" tone="error">{{ crowding.error }}</NoteText>
            <template v-else-if="crowding.report">
                <p class="struct-line">
                    <b>{{ crowded.length }}</b> subject{{ crowded.length === 1 ? '' : 's' }} of {{ crowding.report.min_size }}+ close notes
                    <span class="verb">(close = cosine ≥ {{ crowding.report.band.toFixed(2) }}, this graph's top 1% of pairs; a subject is 2% of the notes or more)</span>
                    <template v-if="crowding.report.hidden_reviewed">
                        · {{ crowding.report.hidden_reviewed }} reviewed and hidden
                    </template>
                </p>
                <NoteText>
                    Piles of notes on one subject crowd each other out of search. Ask your
                    assistant to <b>sleep</b> (<code>/engram-sleep</code> in Claude Code): it
                    writes one state-of note per subject, merges restatements and links what
                    replaced what. Settled history spread over many releases comes first. Keep a
                    subject here to drop it from the queue until most of its notes are new or changed. The
                    flame on the graph's controls shows crowding on the cards.
                </NoteText>
                <div v-for="{ name, members, worth } in crowded.slice(0, SUBJECT_CAP)" :key="members[0]!.id" class="cluster">
                    <div class="cluster-head">
                        <button
                            class="row-link cluster-title"
                            type="button"
                            :title="members[0]!.title"
                            :aria-expanded="expanded === members[0]!.id"
                            @click="expanded = expanded === members[0]!.id ? null : members[0]!.id"
                        >
                            <span class="dot" :style="{ background: config.accent(members[0]!.type) }" />
                            <span class="row-title">{{ name ?? members[0]!.title }}</span>
                        </button>
                        <span
                            class="verb"
                            :title="`worth ${worth.score}: ${worth.versions} versions over ${worth.span_days} days, ${Math.round(worth.settled * 100)}% settled history`"
                        >{{ members.length }} notes · {{ worth.versions }} versions</span>
                        <UiButton
                            size="sm"
                            :disabled="keeping != null"
                            title="Mark these notes reviewed: the subject leaves the queue until most of its notes are new or changed"
                            @click="keepSubject(members)"
                        >
                            {{ keeping === members[0]!.id ? 'Keeping…' : 'Keep' }}
                        </UiButton>
                    </div>
                    <div v-if="expanded === members[0]!.id" class="cluster-members">
                        <button
                            v-for="n in members"
                            :key="n.id"
                            class="verdict-row"
                            type="button"
                            :title="n.title"
                            @click="store.select(n.id)"
                        >
                            <span class="dot" :style="{ background: config.accent(n.type) }" />
                            <span class="row-title">{{ n.title }}</span>
                            <span class="pct">{{ crowding.report.nodes[n.id]?.crowding ?? 0 }}</span>
                        </button>
                    </div>
                </div>
                <NoteText v-if="crowded.length > SUBJECT_CAP">
                    …and {{ crowded.length - SUBJECT_CAP }} more — a sleep pages through them all
                </NoteText>
                <NoteText v-if="sweepNote.crowded" as="span">{{ sweepNote.crowded }}</NoteText>
            </template>
        </PanelSection>

        <PanelSection title="Deep sweeps — local models, zero tokens">
            <div class="sweep">
                <UiButton 
                    :disabled="running != null"
                    title="Rescan look-alike pairs and keep only ones the NLI model reads as contradictions — findings land in Review for your judgment"
                    @click="runSweep('conflicts')"
                >
                    {{ running === 'conflicts' ? 'Sweeping…' : 'Find hidden conflicts' }}
                </UiButton>
                <NoteText v-if="sweepNote.conflicts" as="span">{{ sweepNote.conflicts }}</NoteText>
            </div>
            <div class="sweep">
                <UiButton 
                    :disabled="running != null"
                    title="Find pairs that state the same thing (mutual entailment) — judge them as Replaces in Review to merge histories"
                    @click="runSweep('duplicates')"
                >
                    {{ running === 'duplicates' ? 'Sweeping…' : 'Find duplicates' }}
                </UiButton>
                <NoteText v-if="sweepNote.duplicates" as="span">{{ sweepNote.duplicates }}</NoteText>
            </div>
            <div class="sweep">
                <UiButton 
                    :disabled="running != null"
                    title="Does any Decision/Resolution/Insight already answer an open Problem? Nominations only — you link and resolve"
                    @click="runAnswered"
                >
                    {{ running === 'answered' ? 'Checking…' : 'Check open problems' }}
                </UiButton>
                <NoteText v-if="sweepNote.answered" as="span">{{ sweepNote.answered }}</NoteText>
            </div>
            <div v-if="answered" class="results">
                <NoteText v-if="answered.length === 0">no open problem looks answered</NoteText>
                <div v-for="h in answered" :key="h.problem.id + h.candidate.id" class="pair-row">
                    <button class="row-link" type="button" :title="h.problem.title" @click="store.select(h.problem.id)">
                        {{ h.problem.title }}
                    </button>
                    <span class="verb">{{
                        h.existing_link
                            ? `linked (${h.existing_link}) — maybe answers too:`
                            : 'maybe answered by'
                    }}</span>
                    <button class="row-link" type="button" :title="h.candidate.title" @click="store.select(h.candidate.id)">
                        {{ h.candidate.title }}
                    </button>
                    <span class="pct">{{ Math.round(h.entailment * 100) }}%</span>
                </div>
            </div>
            <div class="sweep">
                <UiButton 
                    :disabled="running != null"
                    title="What does the live canon suggest for each stale note — confirm it, judge it as a conflict, or archive it?"
                    @click="runStaleTriage"
                >
                    {{ running === 'stale' ? 'Triaging…' : 'Triage stale notes' }}
                </UiButton>
                <NoteText v-if="sweepNote.stale" as="span">{{ sweepNote.stale }}</NoteText>
            </div>
            <div v-if="staleTriage" class="results">
                <NoteText v-if="staleTriage.length === 0">no stale notes to triage</NoteText>
                <div v-for="t in staleTriage" :key="t.node.id" class="pair-row">
                    <button class="row-link" type="button" :title="t.node.title" @click="store.select(t.node.id)">
                        {{ t.node.title }}
                    </button>
                    <span class="verb">{{ TRIAGE_WORDS[t.verdict] ?? t.verdict }}</span>
                    <button
                        v-if="t.evidence"
                        class="row-link"
                        type="button"
                        :title="t.evidence.title"
                        @click="store.select(t.evidence.id)"
                    >
                        {{ t.evidence.title }}
                    </button>
                    <span v-if="t.score" class="pct">{{ Math.round(t.score * 100) }}%</span>
                </div>
            </div>
        </PanelSection>

        <PanelSection v-if="agreement && agreement.judged > 0" title="NLI scoreboard — models nominate, people judge">
            <NoteText v-if="agreement.with_hint === 0">
                none of the {{ agreement.judged }} judged pairs carried an NLI hint —
                the scoreboard fills in as hinted suspects get judged in Review
            </NoteText>
            <template v-else>
                <p class="struct-line">
                    the model's hint matched your verdict on
                    <b>{{ Math.round((agreement.agreement ?? 0) * 100) }}%</b>
                    of {{ agreement.with_hint }} judged pair{{ agreement.with_hint > 1 ? 's' : '' }}
                </p>
                <p class="struct-line">
                    <b>{{ agreement.hits }}</b> flagged as contradiction and confirmed ·
                    <b>{{ agreement.false_alarms }}</b> flagged but dismissed ·
                    <b>{{ agreement.misses }}</b> unflagged yet confirmed ·
                    <b>{{ agreement.passes }}</b> unflagged and dismissed
                </p>
                <NoteText v-if="agreement.judged > agreement.with_hint">
                    {{ agreement.judged - agreement.with_hint }} more judged
                    pair{{ agreement.judged - agreement.with_hint > 1 ? 's' : '' }} carried no hint
                    (judged before the NLI layer, or the model wasn't loaded)
                </NoteText>
            </template>
        </PanelSection>

        <PanelSection title="Cross-project — promote shared canon">
            <div class="sweep">
                <UiButton 
                    :disabled="running != null"
                    title="Find Principles/Cautions that recur in your other projects' graphs — candidates for the shared home graph. Nominations only; you approve each"
                    @click="runPromotions"
                >
                    {{ running === 'promotions' ? 'Scanning…' : 'Find promotion candidates' }}
                </UiButton>
                <NoteText v-if="sweepNote.promotions" as="span">{{ sweepNote.promotions }}</NoteText>
            </div>
            <div v-if="promotions" class="results">
                <NoteText v-if="promotions.length === 0">
                    nothing recurs across projects that isn't already in the home graph
                </NoteText>
                <div v-for="c in promotions" :key="c.node.id" class="promo-row">
                    <button class="row-link" type="button" :title="c.node.title" @click="store.select(c.node.id)">
                        <span class="dot" :style="{ background: config.accent(c.node.type) }" />
                        {{ c.node.title }}
                    </button>
                    <span class="verb">
                        also in {{ c.matches.map((m) => m.project).filter((v, i, a) => a.indexOf(v) === i).join(', ') }}
                    </span>
                    <UiButton
                        v-if="!promoted.has(c.node.id)"
                        size="sm"
                        variant="accent"
                        title="Copy this into the shared home graph (project copies stay put)"
                        @click="promote(c)"
                    >
                        Promote to home
                    </UiButton>
                    <NoteText v-else as="span">promoted ✓</NoteText>
                </div>
            </div>
        </PanelSection>

        <PanelSection title="Check a claim against the canon">
            <AutoTextarea
                v-model="claim"
                class="claim-input"
                :rows="2"
                placeholder="One declarative sentence, e.g. “we store sessions in localStorage”"
                @keydown.enter.exact.prevent="runClaim"
            />
            <UiButton :disabled="running != null || !claim.trim()" @click="runClaim">
                {{ running === 'claim' ? 'Checking…' : 'Check' }}
            </UiButton>
            <NoteText v-if="sweepNote.claim" as="span">{{ sweepNote.claim }}</NoteText>
            <div v-if="report" class="results">
                <template
                    v-for="(group, label) in {
                        contradicts: report.contradicts,
                        retracted: report.retracted ?? [],
                        supports: report.supports,
                        silent: report.silent,
                    }"
                    :key="label"
                >
                    <p
                        v-if="group.length"
                        class="group-label"
                        :class="label"
                        :title="label === 'retracted' ? 'A Tombstone says this was deliberately removed — not live canon' : undefined"
                    >
                        {{ label }}
                    </p>
                    <button
                        v-for="v in group"
                        :key="v.id"
                        class="verdict-row"
                        type="button"
                        :title="v.title"
                        @click="store.select(v.id)"
                    >
                        <span class="dot" :style="{ background: config.accent(v.type) }" />
                        <span class="row-title">{{ v.title }}</span>
                        <span class="pct">{{ Math.round(Math.max(v.entailment, v.contradiction, v.neutral) * 100) }}%</span>
                    </button>
                </template>
                <NoteText v-if="!report.contradicts.length && !report.supports.length && !report.retracted?.length">
                    the canon is silent on this — if it matters, it's worth capturing
                </NoteText>
            </div>
        </PanelSection>

        <PanelSection title="Structure — instant, no models">
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
                <span class="dot" :style="{ background: config.accent(n.type) }" />
                <span class="row-title">{{ n.title }}</span>
            </button>
            <p class="struct-line">
                <b>{{ unreachable.length }}</b> reachability islands (no edges <i>and</i> no tags — only text search finds these; a
                <b>sleep</b> links, tags or buries them)
                <UiButton
                    v-if="crowding.report && unreachable.length"
                    size="sm"
                    variant="ghost"
                    :disabled="keeping != null"
                    title="Mark every island reviewed: they leave this list and the sleep queue until edited"
                    @click="keepIslands"
                >
                    {{ keeping === 'islands' ? 'Keeping…' : 'Keep all' }}
                </UiButton>
            </p>
            <button
                v-for="n in unreachable.slice(0, STRUCT_CAP)"
                :key="n.id"
                class="verdict-row"
                type="button"
                @click="store.select(n.id)"
            >
                <span class="dot" :style="{ background: config.accent(n.type) }" />
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
                <span class="dot" :style="{ background: config.accent(n.type) }" />
                <span class="row-title">{{ n.title }}</span>
            </button>
            <p class="struct-line">
                <b>{{ drifted.length }}</b> notes with drifted code refs (the path no longer exists — open one to fix it in the edit drawer)
            </p>
            <button
                v-for="n in drifted.slice(0, STRUCT_CAP)"
                :key="n.id"
                class="verdict-row"
                type="button"
                :title="`Missing: ${(driftByNode.get(n.id) ?? []).join(', ')}`"
                @click="store.select(n.id)"
            >
                <span class="dot" :style="{ background: config.accent(n.type) }" />
                <span class="row-title">{{ n.title }}</span>
            </button>
        </PanelSection>
    </SidePanel>
</div>
</template>

<style scoped>
.toggle {
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

.nli-off {
    padding: 0.8rem 1rem;
    border-radius: var(--radius-md);
    font-size: var(--text-caption);
    color: var(--node-caution);
    background-color: color-mix(in srgb, var(--node-caution) 10%, transparent);
}

.sweep {
    display: flex;
    align-items: center;
    gap: 0.8rem;
    margin-bottom: 0.6rem;
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

.group-label.retracted {
    color: var(--text-secondary);
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
}

.struct-line {
    margin: 0.6rem 0 0.3rem;
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
}

.cluster {
    margin-top: 0.4rem;
}

.cluster-head {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    min-width: 0;
}

.cluster-title {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex: 1;
}

.cluster-members {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    margin: 0.2rem 0 0.4rem 1.4rem;
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

</style>
