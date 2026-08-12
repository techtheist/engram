<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import MarkdownView from '@/components/common/MarkdownView.vue'
import { useConfigStore } from '@/stores/config'
import { useHistoryStore } from '@/stores/history'
import type { GraphConfig, HistorySession } from '@/types/graph'

/**
 * The history view (0.8.4, plan §5): recorded coding-assistant sessions as
 * lanes on the left, and one conversation on a vertical time axis — user
 * turns offset left, assistant turns offset right, so a glance reads as a
 * dialogue. History is records, not knowledge: no trust, no edges, no
 * editing — read and delete are the only gestures.
 */
const history = useHistoryStore()
const config = useConfigStore()
const { sessions, active, messages, focusTurn, focusTs, loading, error } = storeToRefs(history)

const thread = ref<HTMLElement | null>(null)
/** The message a jump centers on: exact turn (born-in) or nearest-by-time
 *  (pre-history notes, where no exact record exists). */
const focusId = ref<string | null>(null)

onMounted(() => {
    void history.load()
    if (!config.cfg) void config.load().catch(() => undefined)
})

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
    thread.value
        .querySelector(`[data-mid="${focusId.value}"]`)
        ?.scrollIntoView({ block: 'center' })
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
    if (
        !window.confirm(
            `Delete the recorded session "${s.title}" (${s.messages} messages)? ` +
                'Its transcript path is excluded from future indexing. This cannot be undone.',
        )
    ) {
        return
    }
    await history.deleteSession(s.session)
}
</script>

<template>
<div v-if="recordingOff && sessions.length === 0" class="history">
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
        <p v-if="enableError" class="empty error">{{ enableError }}</p>
        <button class="mini accent" type="button" :disabled="enabling || !config.cfg" @click="enableRecording">
            {{ enabling ? 'Turning on…' : 'Turn on recording' }}
        </button>
    </section>
</div>
<div v-else class="history" :class="{ reading: !!active }">
    <aside class="lanes glass-panel">
        <header class="lanes-head">
            <h2 class="lanes-title">Session history</h2>
            <span class="count">{{ sessions.length }}</span>
        </header>
        <div v-if="recordingOff" class="off-banner">
            Recording is off — new sessions aren't ingested.
            <button class="mini" type="button" :disabled="enabling" @click="enableRecording">
                Turn on
            </button>
        </div>
        <p v-if="error" class="empty error">{{ error }}</p>
        <p v-else-if="!loading && sessions.length === 0" class="empty">
            Nothing recorded yet. The daemon indexes coding-assistant transcripts in the
            background — sessions appear here as they're found. Knobs live in Settings →
            Session history.
        </p>
        <ul class="lane-list">
            <li v-for="s in sessions" :key="s.session">
                <button
                    class="lane"
                    :class="{ active: active?.session === s.session }"
                    type="button"
                    @click="history.open(s.session)"
                >
                    <span class="lane-title">{{ s.title }}</span>
                    <span class="lane-meta">
                        <span v-if="s.harness" class="chip">{{ s.harness }}</span>
                        {{ day(s.started) }} · {{ s.messages }} turns
                    </span>
                </button>
            </li>
        </ul>
    </aside>

    <section ref="thread" class="thread">
        <p v-if="!active" class="empty pick">Pick a session to read it.</p>
        <template v-else>
            <header class="thread-head glass-panel">
                <button
                    class="mini back"
                    type="button"
                    aria-label="Back to sessions"
                    @click="backToLanes"
                >
                    ←
                </button>
                <div class="thread-id">
                    <h2 class="thread-title">{{ active.title }}</h2>
                    <p class="thread-meta">
                        <span v-if="active.harness" class="chip">{{ active.harness }}</span>
                        {{ day(active.started) }} · {{ messages.length }} turns
                        <span v-if="active.version" class="chip quiet">v{{ active.version }}</span>
                    </p>
                </div>
                <button class="mini danger" type="button" @click="remove(active)">
                    Delete session
                </button>
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
    inset: var(--topbar-height, 56px) 0 0;
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
    border-radius: 12px;
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
    border-radius: 8px;
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
    gap: 6px;
    align-items: center;
    font-size: var(--text-caption, 11px);
    color: var(--text-secondary);
}

.chip {
    padding: 1px 6px;
    border-radius: 999px;
    background: rgb(56 189 248 / 15%);
    font-size: var(--text-caption, 11px);
    color: var(--text-secondary);
}

.chip.quiet {
    background: rgb(148 163 184 / 15%);
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
    border-radius: 12px;
}

.turn.user .bubble {
    border-top-left-radius: 4px;
}

.turn.assistant .bubble {
    border-top-right-radius: 4px;
}

.turn.focus .bubble {
    outline: 2px solid var(--interactive-primary, #38bdf8);
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

.empty {
    padding: 12px;
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
}

.empty.error {
    color: #ef4444;
}

.empty.pick {
    margin-top: 20vh;
    text-align: center;
}

.mini {
    padding: 4px 10px;
    border: 1px solid rgb(148 163 184 / 35%);
    border-radius: 6px;
    background: transparent;
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    cursor: pointer;
}

.mini.danger {
    border-color: rgb(239 68 68 / 45%);
    color: #ef4444;
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
    border-radius: 12px;
}

.off-title {
    margin: 0;
    font-size: var(--text-heading, 18px);
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

.mini.accent {
    border-color: var(--interactive-primary, #38bdf8);
    color: var(--interactive-primary, #38bdf8);
}

.off-banner {
    display: flex;
    gap: 8px;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 8px;
    padding: 8px 10px;
    border: 1px solid rgb(148 163 184 / 25%);
    border-radius: 8px;
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
</style>
