<script setup lang="ts">
import { computed, ref } from 'vue'
import { useConfigStore } from '@/stores/config'
import { useGraphStore } from '@/stores/graph'
import type { EdgeType } from '@/types/graph'

/**
 * Completes a dragged connection into a sentence-shaped edge (PLAN §4.3): the
 * user picks the verb that makes "from → to" read as English — no verb, no
 * edge, and there is deliberately no generic relates-to.
 */
const props = defineProps<{ source: string; target: string }>()
const emit = defineEmits<{ close: [] }>()

const store = useGraphStore()
const config = useConfigStore()
const busy = ref(false)
const error = ref<string | null>(null)
// Drag direction seeds subject → object; the swap fixes a backwards sentence.
const swapped = ref(false)

const fromId = computed(() => (swapped.value ? props.target : props.source))
const toId = computed(() => (swapped.value ? props.source : props.target))

function title(id: string): string {
    return store.nodes.get(id)?.title ?? id
}

async function pick(type: EdgeType): Promise<void> {
    busy.value = true
    error.value = null
    try {
        await store.createEdge({ type, from_id: fromId.value, to_id: toId.value })
        emit('close')
    } catch (e) {
        error.value = e instanceof Error ? e.message : String(e)
    } finally {
        busy.value = false
    }
}
</script>

<template>
<div class="scrim" @click.self="emit('close')">
    <div class="dialog glass-panel" role="dialog" aria-label="Choose the connection type">
        <header class="sentence">
            <span class="endpoint" :title="title(fromId)">{{ title(fromId) }}</span>
            <span class="ellipsis">…</span>
            <span class="endpoint" :title="title(toId)">{{ title(toId) }}</span>
            <button
                class="swap"
                type="button"
                title="Swap direction"
                @click="swapped = !swapped"
            >
                ⇄
            </button>
        </header>

        <p class="hint">Pick the verb that completes the sentence — no fit, no edge.</p>

        <div class="verbs">
            <button
                v-for="t in config.verbNames"
                :key="t"
                class="verb"
                type="button"
                :disabled="busy"
                :style="{ '--verb-color': config.edgeColor(t) }"
                @click="pick(t)"
            >
                <span class="verb-name">{{ t }}</span>
                <span class="verb-hint">{{ config.edgeSentence(t) }}</span>
            </button>
        </div>

        <p v-if="error" class="error">{{ error }}</p>

        <button class="cancel" type="button" :disabled="busy" @click="emit('close')">
            Cancel
        </button>
    </div>
</div>
</template>

<style scoped>
.scrim {
    position: absolute;
    inset: 0;
    z-index: 25;
    display: flex;
    align-items: center;
    justify-content: center;
    background-color: var(--surface-overlay);
}

.dialog {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    width: min(44rem, calc(100vw - 3.2rem));
    padding: 1.8rem;
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-lg);
}

.sentence {
    display: flex;
    align-items: center;
    gap: 0.8rem;
    min-width: 0;
}

.endpoint {
    flex: 1;
    min-width: 0;
    font-size: var(--text-body-sm);
    font-weight: 600;
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.ellipsis {
    flex: none;
    color: var(--text-tertiary);
}

.swap {
    flex: none;
    padding: 0.3rem 0.8rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: transparent;
    color: var(--text-secondary);
    font-size: var(--text-body-sm);
    cursor: pointer;
}

.swap:hover {
    color: var(--text-primary);
    background-color: var(--interactive-ghost-hover);
}

.hint {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.verbs {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(18rem, 1fr));
    gap: 0.5rem;
}

.verb {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    padding: 0.7rem 1rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: transparent;
    text-align: left;
    cursor: pointer;
}

.verb:disabled {
    opacity: 0.5;
    cursor: default;
}

.verb:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--verb-color) 55%, transparent);
    background-color: color-mix(in srgb, var(--verb-color) 10%, transparent);
}

.verb-name {
    font-size: var(--text-label);
    font-weight: 600;
    color: var(--verb-color);
}

.verb-hint {
    font-size: var(--text-caption);
    color: var(--text-secondary);
}

.error {
    font-size: var(--text-caption);
    color: var(--node-problem);
}

.cancel {
    align-self: flex-start;
    padding: 0.5rem 1rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: transparent;
    color: var(--text-secondary);
    font-size: var(--text-caption);
    font-weight: 600;
    cursor: pointer;
}

.cancel:hover:not(:disabled) {
    color: var(--text-primary);
    background-color: var(--interactive-ghost-hover);
}
</style>
