<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { api } from '@/services/api'
import { useProjectsStore } from '@/stores/projects'
import { useGraphSettings } from '@/composables/useGraphSettings'
import type { HistoryStatus } from '@/types/graph'

/**
 * The recording announcement (0.8.4): the FIRST time recorded sessions
 * exist for a project the pane says so, once, with the off-switch one click
 * away. Recording is opt-in, so this fires after the user's own enable
 * gesture — a receipt, not a surprise. Dismissal is remembered per project;
 * the settings section remains the permanent home of the knobs.
 */
const projects = useProjectsStore()
const settings = useGraphSettings()

const status = ref<HistoryStatus | null>(null)
const dismissed = ref(true)

const ackKey = computed(() => `engram-history-ack:${projects.active?.id ?? 'local'}`)

async function check(): Promise<void> {
    dismissed.value = localStorage.getItem(ackKey.value) === '1'
    if (dismissed.value) return
    try {
        status.value = await api.historyStatus()
    } catch {
        status.value = null
    }
}

const show = computed(
    () =>
        !dismissed.value &&
        status.value?.open === true &&
        (status.value?.stats?.nodes ?? 0) > 0,
)

function dismiss(): void {
    localStorage.setItem(ackKey.value, '1')
    dismissed.value = true
}

function openSettings(): void {
    dismiss()
    settings.show()
}

onMounted(check)
watch(() => projects.active?.id, check)
</script>

<template>
<Transition name="fade">
    <div v-if="show" class="notice glass-panel" role="status">
        <p class="text">
            Recording session history — {{ status?.stats?.nodes ?? 0 }} nodes indexed from
            your coding-assistant transcripts. Hidden from the graph; searchable only as a
            labeled fall-through.
        </p>
        <div class="row">
            <button class="mini" type="button" @click="openSettings">Settings</button>
            <button class="mini quiet" type="button" @click="dismiss">Got it</button>
        </div>
    </div>
</Transition>
</template>

<style scoped>
.notice {
    position: absolute;
    right: 16px;
    bottom: 16px;
    z-index: 30;
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-width: 340px;
    padding: 12px 14px;
    border-radius: 10px;
}

.text {
    margin: 0;
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
}

.row {
    display: flex;
    gap: 8px;
}

.mini {
    padding: 4px 10px;
    border: 1px solid var(--border-default, rgb(148 163 184 / 35%));
    border-radius: 6px;
    background: transparent;
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    cursor: pointer;
}

.mini.quiet {
    border-color: transparent;
    color: var(--text-secondary);
}
</style>
