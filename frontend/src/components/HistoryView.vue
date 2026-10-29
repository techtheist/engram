<script setup lang="ts">
import InfoBadge from '@/components/common/InfoBadge.vue'
import NoteText from '@/components/common/NoteText.vue'
import UiButton from '@/components/common/UiButton.vue'
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import MarkdownView from '@/components/common/MarkdownView.vue'
import { useConfigStore } from '@/stores/config'
import { useHistoryStore } from '@/stores/history'
import { useLayoutStore } from '@/stores/layout'
import type { GraphConfig, HistorySession } from '@/types/graph'
import { confirmDialog } from '@/composables/useConfirm'

/**
 * The history view (0.8.4, plan §5): recorded coding-assistant sessions as
 * lanes on the left, and one conversation on a vertical time axis — user
 * turns offset left, assistant turns offset right, so a glance reads as a
 * dialogue. History is records, not knowledge: no trust, no edges, no
 * editing — read and delete are the only gestures.
 */
const history = useHistoryStore()
const layout = useLayoutStore()
const config = useConfigStore()
const { sessions, visibleSessions, active, messages, focusTurn, focusTs, loading, error } = storeToRefs(history)

const thread = ref<HTMLElement | null>(null)
/** The message a jump centers on: exact turn (born-in) or nearest-by-time
 *  (pre-history notes, where no exact record exists). */
const focusId = ref<string | null>(null)

onMounted(() => {
    void history.load()
    if (!config.cfg) void config.load().catch(() => undefined)
})

// Kept alive under other screens (App): refresh the lanes on each return.
watch(
    () => layout.view,
    (view) => {
        if (view === 'history') void history.load()
    },
)

// Recording is opt-in (0.8.4): when it's off, this view is where the switch
// (and the explanation of what it does) lives.
const recordingOff = computed(() => config.cfg != null && !config.cfg.history.enabled)
const enabling = ref(false)
const enableError = ref<string | null>(null)

async function enableRecording(): Promise<void> {
    if (!config.cfg) return
    enabling.value = true
    enableError.value = null
    try {
        const next = JSON.parse(JSON.stringify(config.cfg)) as GraphConfig
        next.history.enabled = true
        await config.save(next)
        await history.load()
    } catch (e) {
        enableError.value = e instanceof Error ? e.message : String(e)
    } finally {
        enabling.value = false
    }
}

watch(messages, async () => {
    focusId.value = null
    if (focusTurn.value != null) {
        focusId.value =
            messages.value.find((m) => m.turn === focusTurn.value)?.message_id ?? null
    } else if (focusTs.value != null && messages.value.length) {
        const ts = focusTs.value
        focusId.value = messages.value.reduce((best, m) =>
            Math.abs(m.timestamp - ts) < Math.abs(best.timestamp - ts) ? m : best,
        ).message_id
    }
    await nextTick()
    if (!focusId.value || !thread.value) return
    // Scroll the thread itself, vertically only: scrollIntoView would also
    // scroll the app sideways while this screen is still sliding in.
    const el = thread.value.querySelector<HTMLElement>(`[data-mid="${focusId.value}"]`)
    if (!el) return
    const box = thread.value.getBoundingClientRect()
    const at = el.getBoundingClientRect()
    thread.value.scrollTop += at.top - box.top - (box.height - at.height) / 2
})

function day(ts: number): string {
    return new Date(ts * 1000).toLocaleDateString(undefined, {
        month: 'short',
        day: 'numeric',
    })
}

function clock(ts: number): string {
    return new Date(ts * 1000).toLocaleTimeString(undefined, {
        hour: '2-digit',
        minute: '2-digit',
    })
}

/** Narrow mode (phone / IDE side pane): one column at a time — the thread
 *  takes the whole width and this returns to the lane list. */
function backToLanes(): void {
    active.value = null
    messages.value = []
}

async function remove(s: HistorySession): Promise<void> {
    const ok = await confirmDialog({
        title: 'Delete this recorded session?',
        message:
            `"${s.title}" and its ${s.messages} messages leave session history, and its transcript ` +
            'is skipped from now on. This cannot be undone.',
        confirmLabel: 'Delete session',
        danger: true,
    })
    if (!ok) return
    await history.deleteSession(s.session)
}
</script>

<template>
<div v-if="recordingOff && sessions.length === 0" class="history screen-content">
    <section class="off-hero glass-panel">
        <h2 class="off-title">Session history is off</h2>
        <p class="off-text">
            When recording is on, the daemon indexes your coding-assistant
            conversations (Claude Code, Codex, opencode, Kilo, Gemini CLI,
            Antigravity) for this project into a separate local store —
            <strong>never</strong> mixed with the curated graph.
        </p>
        <ul class="off-list">
            <li>Browse any past session here, as a readable dialogue.</li>
            <li>Notes link back to the exchange they were born in.</li>
            <li>
                Search can fall through to recordings — only as a labeled section, and
                only when curated memory likely doesn't hold the answer.
            </li>
            <li>
                Everything is sealed at rest with a key in your OS keystore — macOS may
                ask for keychain access once recording starts.
            </li>
            <li>
                Every knob (per-harness toggles, ignored paths, per-session delete,
                wipe) lives in Settings → Session history.
            </li>
        </ul>
        <NoteText v-if="enableError" tone="error">{{ enableError }}</NoteText>
        <UiButton size="sm" variant="accent" :disabled="enabling || !config.cfg" @click="enableRecording">
            {{ enabling ? 'Turning on…' : 'Turn on recording' }}
        </UiButton>
    </section>
</div>
<div v-else class="history screen-content" :class="{ reading: !!active }">
    <aside class="lanes glass-panel">
        <header class="lanes-head">
            <h2 class="lanes-title">Session history</h2>
            <span class="count">{{ visibleSessions.length }}</span>
        </header>
        <div v-if="recordingOff" class="off-banner">
            Recording is off — new sessions aren't ingested.
            <UiButton size="sm" :disabled="enabling" @click="enableRecording">
                Turn on
            </UiButton>
        </div>
        <NoteText v-if="error" tone="error">{{ error }}</NoteText>
        <NoteText v-else-if="!loading && sessions.length === 0" tone="empty">
            Nothing recorded yet. The daemon indexes coding-assistant transcripts in the
            background — sessions appear here as they're found. Knobs live in Settings →
            Session history.
        </NoteText>
        <ul class="lane-list">
            <li v-for="s in visibleSessions" :key="s.session">
                <button
                    class="lane"
                    :class="{ active: active?.session === s.session }"
                    type="button"
                    @click="history.open(s.session)"
                >
                    <span class="lane-title">{{ s.title }}</span>
                    <span class="lane-meta">
                        <InfoBadge v-if="s.harness" pill>{{ s.harness }}</InfoBadge>
                        {{ day(s.started) }} · {{ s.messages }} turns
                    </span>
                </button>
            </li>
        </ul>
    </aside>

    <section ref="thread" class="thread">
        <NoteText v-if="!active" tone="empty">Pick a session to read it.</NoteText>
        <template v-else>
            <header class="thread-head glass-panel">
                <UiButton
                    size="sm" class="back"
                    aria-label="Back to sessions"
                    @click="backToLanes"
                >
                    ←
                </UiButton>
                <div class="thread-id">
                    <h2 class="thread-title">{{ active.title }}</h2>
                    <p class="thread-meta">
                        <InfoBadge v-if="active.harness" pill>{{ active.harness }}</InfoBadge>
                        {{ day(active.started) }} · {{ messages.length }} turns
                        <InfoBadge v-if="active.version" pill tone="muted">v{{ active.version }}</InfoBadge>
                    </p>
                </div>
                <UiButton size="sm" variant="danger" @click="remove(active)">
                    Delete session
                </UiButton>
            </header>
            <ol class="turns">
                <li
                    v-for="m in messages"
                    :key="m.message_id"
                    class="turn"
                    :class="[m.role, { focus: m.message_id === focusId }]"
                    :data-mid="m.message_id"
                >
                    <div class="bubble glass-panel">
                        <span class="who">{{ m.role }} · {{ clock(m.timestamp) }}</span>
                        <!-- Recorded turns are markdown in the wild (assistants
                             write it, users paste it); sanitized like note
                             bodies. -->
                        <MarkdownView class="text" :content="m.text" />
                    </div>
                </li>
            </ol>
        </template>
    </section>
</div>
</template>

<style scoped>
.history {
    position: absolute;
    inset: 5.6rem 0 0; /* just under the floating top bar */
    display: grid;
    grid-template-columns: 300px 1fr;
    gap: 12px;
    padding: 12px;
    overflow: hidden;
}

.lanes {
    display: flex;
    flex-direction: column;
    min-width: 0; /* grid item: nowrap lane titles must not blow the track */
    min-height: 0;
    padding: 12px;
    border-radius: var(--radius-lg);
}

.lanes-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    margin-bottom: 8px;
}

.lanes-title {
    margin: 0;
    font-size: var(--text-body);
    font-weight: 600;
}

.count {
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
}

.lane-list {
    margin: 0;
    padding: 0;
    overflow-y: auto;
    list-style: none;
}

.lane {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    padding: 8px 10px;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    color: inherit;
    text-align: left;
    cursor: pointer;
}

.lane:hover {
    background: rgb(148 163 184 / 12%);
}

.lane.active {
    background: rgb(148 163 184 / 20%);
}

.lane-title {
    overflow: hidden;
    font-size: var(--text-body-sm);
    text-overflow: ellipsis;
    white-space: nowrap;
}

.lane-meta {
    display: flex;
    flex-wrap: wrap; /* sliver panes: the date line wraps, the chip stays whole */
    gap: 6px;
    align-items: center;
    font-size: var(--text-caption, 11px);
    color: var(--text-secondary);
}

.thread {
    min-width: 0;
    min-height: 0;
    padding: 0 8px 24px;
    overflow-y: auto;
}

.thread-head {
    position: sticky;
    top: 0;
    z-index: 2;
    display: flex;
    gap: 10px;
    align-items: center;
    margin-bottom: 12px;
    padding: 10px 14px;
    border-radius: 10px;
}

/* The way back to the lane list — only exists in narrow single-column mode. */
.back {
    display: none;
    flex: none;
}

.thread-id {
    flex: 1;
    min-width: 0;
}

.thread-title {
    margin: 0;
    overflow: hidden;
    font-size: var(--text-body);
    font-weight: 600;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.thread-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 8px;
    align-items: center;
    margin: 2px 0 0;
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
}

/* The zigzag: user turns hug the left edge, assistant turns the right —
   the vertical axis IS time. */
.turns {
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-width: 900px;
    margin: 0 auto;
    padding: 0;
    list-style: none;
}

.turn {
    display: flex;
}

.turn.user {
    justify-content: flex-start;
}

.turn.assistant {
    justify-content: flex-end;
}

.bubble {
    max-width: 72%;
    padding: 10px 12px;
    border-radius: var(--radius-lg);
}

.turn.user .bubble {
    border-top-left-radius: 4px;
}

.turn.assistant .bubble {
    border-top-right-radius: 4px;
}

.turn.focus .bubble {
    outline: 2px solid var(--interactive-primary);
}

.who {
    display: block;
    margin-bottom: 4px;
    font-size: var(--text-caption, 11px);
    color: var(--text-secondary);
    text-transform: capitalize;
}

.text {
    font-size: var(--text-body-sm);
    overflow-wrap: anywhere;
}

/* Rendered markdown inside a bubble: keep code blocks from forcing the
   bubble past its 72% lane. */
.text :deep(pre) {
    max-width: 100%;
    overflow-x: auto;
}

.pick {
    margin-top: 20vh;
    text-align: center;
}

/* Recording off: the opt-in hero (what turning it on does) and the slim
   lane banner when old recordings exist but ingestion is paused. */
.off-hero {
    display: flex;
    flex-direction: column;
    gap: 12px;
    grid-column: 1 / -1;
    align-items: flex-start;
    max-width: 560px;
    height: fit-content;
    margin: 14vh auto 0;
    padding: 22px 26px;
    border-radius: var(--radius-lg);
}

.off-title {
    margin: 0;
    font-size: 1.8rem;
    font-weight: 600;
}

.off-text {
    margin: 0;
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
}

.off-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0;
    padding-left: 18px;
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
}

.off-banner {
    display: flex;
    gap: 8px;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 8px;
    padding: 8px 10px;
    border: 1px solid rgb(148 163 184 / 25%);
    border-radius: var(--radius-md);
    font-size: var(--text-caption, 11px);
    color: var(--text-secondary);
}

/* Narrow mode — a phone or an IDE side pane: one column at a time. The lane
   list IS the screen until a session opens; the thread then takes the full
   width with ← back in its header. */
@media (width <= 719px) {
    .history {
        grid-template-columns: 1fr;
        gap: 0;
        padding: 8px;
    }

    .thread {
        padding: 0 0 24px;
    }

    .history.reading .lanes {
        display: none;
    }

    .history:not(.reading) .thread {
        display: none;
    }

    .back {
        display: inline-flex;
    }

    /* At phone widths the delete button may wrap under the title row. */
    .thread-head {
        flex-wrap: wrap;
    }

    .bubble {
        max-width: 90%;
    }

    .off-hero {
        margin-top: 6vh;
        padding: 18px 16px;
    }
}

/* Sliver panes: the top bar wraps onto two rows (App.vue) — start below it. */
@media (width <= 250px) {
    .history {
        inset: 9.4rem 0 0;
    }
}
</style>
