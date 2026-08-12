import { defineStore } from 'pinia'
import { ref } from 'vue'
import { api } from '@/services/api'
import { onProjectSwitch } from '@/composables/onProjectSwitch'
import type { HistoryMessage, HistorySession } from '@/types/graph'

/**
 * The history view's state (0.8.4): recorded sessions as lanes, one open
 * conversation, and the focus handle a born-in chip jumps in on. Kept apart
 * from the graph store on purpose — history is a separate layer everywhere,
 * the pane included.
 */
export const useHistoryStore = defineStore('history', () => {
    const sessions = ref<HistorySession[]>([])
    const active = ref<HistorySession | null>(null)
    const messages = ref<HistoryMessage[]>([])
    /** Turn to center/highlight after a born-in jump; consumed by the view. */
    const focusTurn = ref<number | null>(null)
    /** Fallback focus for pre-history notes: land on the message nearest
     *  this unix timestamp (no exact record exists to point at). */
    const focusTs = ref<number | null>(null)
    const loading = ref(false)
    const error = ref<string | null>(null)

    async function load(): Promise<void> {
        loading.value = true
        error.value = null
        try {
            sessions.value = (await api.historySessions()).sessions
            // Keep the selection alive across reloads; drop it if deleted.
            if (active.value && !sessions.value.some((s) => s.session === active.value?.session)) {
                active.value = null
                messages.value = []
            }
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e)
        } finally {
            loading.value = false
        }
    }

    async function open(
        sessionId: string,
        turn: number | null = null,
        ts: number | null = null,
    ): Promise<void> {
        focusTurn.value = turn
        focusTs.value = ts
        if (!sessions.value.length) await load()
        active.value = sessions.value.find((s) => s.session === sessionId) ?? null
        messages.value = []
        if (!active.value) return
        try {
            messages.value = (await api.historyMessages(sessionId)).messages
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e)
        }
    }

    // Sessions belong to the graph they were recorded in: a project switch
    // drops everything (stale lanes would feed NodeDetail's time-window
    // fallback wrong sessions) and reloads the new project's lanes.
    onProjectSwitch(() => {
        sessions.value = []
        active.value = null
        messages.value = []
        focusTurn.value = null
        focusTs.value = null
        error.value = null
        void load()
    })

    async function deleteSession(sessionId: string): Promise<void> {
        await api.historyDeleteSession(sessionId)
        if (active.value?.session === sessionId) {
            active.value = null
            messages.value = []
        }
        await load()
    }

    return {
        sessions,
        active,
        messages,
        focusTurn,
        focusTs,
        loading,
        error,
        load,
        open,
        deleteSession,
    }
})
