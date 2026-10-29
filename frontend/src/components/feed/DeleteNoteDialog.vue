<script setup lang="ts">
import AutoTextarea from '@/components/common/AutoTextarea.vue'
import ModalDialog from '@/components/common/ModalDialog.vue'
import NoteText from '@/components/common/NoteText.vue'
import ToggleSwitch from '@/components/common/ToggleSwitch.vue'
import UiButton from '@/components/common/UiButton.vue'
import { computed, ref } from 'vue'
import { useConfigStore } from '@/stores/config'
import { useGraphStore } from '@/stores/graph'
import type { GraphNode } from '@/types/graph'

/**
 * The Feed's delete (0.10.3, issue #14): the user's quick way to drop notes
 * mid-review, with the choice the drawer offers — delete outright, or leave a
 * Tombstone (an optional reason, and the removed text kept by default) so the
 * assistant doesn't learn it again. The last choice is remembered per viewer.
 */
const props = defineProps<{ node: GraphNode }>()
const emit = defineEmits<{ close: [] }>()

const store = useGraphStore()
const config = useConfigStore()

const CHOICE_KEY = 'engram.feed.delete-tombstone'
function savedChoice(): boolean {
    try {
        return localStorage.getItem(CHOICE_KEY) === '1'
    } catch {
        return false
    }
}

const tombstoneType = computed(() => config.tombstoneType ?? null)
const tombstone = ref(tombstoneType.value != null && savedChoice())
const reason = ref('')
const keepText = ref(true)
const busy = ref(false)
const error = ref<string | null>(null)

const connections = computed(() => {
    let n = 0
    for (const e of store.edges.values()) {
        if (e.from_id === props.node.id || e.to_id === props.node.id) n++
    }
    return n
})

const confirmLabel = computed(() => (tombstone.value ? `Delete, leave a ${tombstoneType.value}` : 'Delete'))

async function confirm(): Promise<void> {
    if (busy.value) return
    busy.value = true
    error.value = null
    try {
        if (tombstoneType.value != null) {
            try {
                localStorage.setItem(CHOICE_KEY, tombstone.value ? '1' : '0')
            } catch {
                // storage blocked: the choice just isn't remembered
            }
        }
        await store.remove(
            props.node.id,
            tombstone.value
                ? { tombstone: true, reason: reason.value.trim() || undefined, keepText: keepText.value }
                : undefined,
        )
        emit('close')
    } catch (e) {
        error.value = e instanceof Error ? e.message : String(e)
    } finally {
        busy.value = false
    }
}

/** ⌘/Ctrl+Enter confirms from anywhere in the dialog, the reason field included. */
function onKey(e: KeyboardEvent): void {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
        e.preventDefault()
        void confirm()
    }
}
</script>

<template>
<ModalDialog title="Delete this note?" tone="danger" :locked="busy" width="50rem" @close="emit('close')">
    <div class="del" @keydown="onKey">
        <div class="victim">
            <span class="victim-dot" :style="{ background: config.accent(node.type) }" />
            <span class="victim-type">{{ node.type }}</span>
            <span class="victim-title">{{ node.title }}</span>
        </div>
        <NoteText tone="body">
            It goes from the graph for good, with
            {{ connections === 1 ? 'its 1 connection' : `its ${connections} connections` }}.
            Only you can do this. The assistant has no delete.
        </NoteText>

        <div v-if="tombstoneType" class="choices" role="radiogroup" aria-label="How to delete">
            <button
                class="choice"
                type="button"
                role="radio"
                :aria-checked="!tombstone"
                :class="{ on: !tombstone }"
                :disabled="busy"
                :data-autofocus="!tombstone ? '' : undefined"
                @click="tombstone = false"
            >
                <span class="choice-name">Just delete</span>
                <span class="choice-hint">Nothing is left behind. The assistant may learn it again later.</span>
            </button>
            <button
                class="choice"
                type="button"
                role="radio"
                :aria-checked="tombstone"
                :class="{ on: tombstone }"
                :disabled="busy"
                :data-autofocus="tombstone ? '' : undefined"
                @click="tombstone = true"
            >
                <span class="choice-name">Leave a {{ tombstoneType }}</span>
                <span class="choice-hint">Records what was removed and why, so it isn't learned again.</span>
            </button>
        </div>

        <div v-if="tombstone" class="tomb">
            <label class="tomb-label" for="delete-reason">Tombstone note</label>
            <AutoTextarea
                id="delete-reason"
                v-model="reason"
                :rows="2"
                :max-rows="6"
                placeholder="Why is this being removed? (optional)"
                :disabled="busy"
            />
            <ToggleSwitch
                v-model="keepText"
                label="Keep the removed text"
                title="Carries the body, tags and code refs into the Tombstone, so a rewrite of the removed content is caught"
                :disabled="busy"
            />
            <NoteText>
                {{ keepText
                    ? 'A rewrite of this note will be caught and flagged.'
                    : 'Only the title and your note are kept; the text is purged.' }}
            </NoteText>
        </div>

        <NoteText v-if="error" tone="error">{{ error }}</NoteText>
    </div>

    <template #actions>
        <UiButton variant="ghost" :disabled="busy" @click="emit('close')">Cancel</UiButton>
        <UiButton variant="danger" :disabled="busy" @click="confirm">
            {{ busy ? 'Deleting…' : confirmLabel }}
        </UiButton>
    </template>
</ModalDialog>
</template>

<style scoped>
.del {
    display: flex;
    flex-direction: column;
    gap: 1.2rem;
}

.victim {
    display: flex;
    align-items: baseline;
    gap: 0.7rem;
    min-width: 0;
    padding: 1rem 1.2rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: var(--surface-elevated);
}

.victim-dot {
    flex: none;
    width: 0.8rem;
    height: 0.8rem;
    border-radius: 50%;
    transform: translateY(-0.05rem);
}

.victim-type {
    flex: none;
    font-size: var(--text-caption);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-tertiary);
}

.victim-title {
    min-width: 0;
    font-size: var(--text-body-sm);
    font-weight: 600;
    color: var(--text-primary);
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
}

.choices {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(18rem, 1fr));
    gap: 0.8rem;
}

.choice {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    padding: 1rem 1.2rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: transparent;
    text-align: left;
    cursor: pointer;
    transition: border-color 0.12s ease, background-color 0.12s ease;
}

.choice:disabled {
    cursor: default;
}

.choice:hover:not(:disabled, .on) {
    background-color: var(--interactive-ghost-hover);
}

.choice.on {
    border-color: color-mix(in srgb, var(--node-problem) 60%, transparent);
    background-color: color-mix(in srgb, var(--node-problem) 9%, transparent);
}

.choice-name {
    font-size: var(--text-label);
    font-weight: 600;
    color: var(--text-primary);
}

.choice-hint {
    font-size: var(--text-caption);
    line-height: 1.45;
    color: var(--text-secondary);
}

.tomb {
    display: flex;
    flex-direction: column;
    gap: 0.8rem;
}

.tomb-label {
    font-size: var(--text-label);
    font-weight: 600;
    color: var(--text-secondary);
}
</style>
