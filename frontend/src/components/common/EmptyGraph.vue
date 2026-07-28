<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { storeToRefs } from 'pinia'
import { useConfigStore } from '@/stores/config'
import type { GraphConfig } from '@/types/graph'

/**
 * The cold-start card, shared by both screens (the canvas overlay and the
 * feed's empty state) so an empty graph says its piece exactly once.
 *
 * It teaches the one gesture that fills a graph from what already exists —
 * the digest skill — and offers the one choice that is cheap NOW and
 * expensive later: which ontology this graph speaks. An empty graph is the
 * only moment a preset can be applied with nothing to retype.
 */
const config = useConfigStore()
const { presets } = storeToRefs(config)

const current = computed(() => config.cfg?.ontology.preset ?? 'engram')
const choice = ref(current.value)
const busy = ref(false)
const note = ref<string | null>(null)

onMounted(async () => {
    // A pre-0.7 daemon has no /config/presets — the row just stays hidden.
    await config.loadPresets().catch(() => undefined)
    choice.value = current.value
})

/** The current shape may be hand-edited ("custom") — keep it selectable. */
const options = computed(() => {
    const list = presets.value.map((p) => ({ id: p.id, name: p.name, description: p.description }))
    if (!list.some((p) => p.id === current.value)) {
        list.unshift({ id: current.value, name: `${current.value} (current)`, description: '' })
    }
    return list
})

const chosen = computed(() => options.value.find((p) => p.id === choice.value))
const dirty = computed(() => choice.value !== current.value)

async function apply(): Promise<void> {
    const preset = presets.value.find((p) => p.id === choice.value)
    if (!preset) return
    busy.value = true
    note.value = null
    try {
        // No confirmation and no retype worry: nothing is stored yet.
        // JSON round-trip, not structuredClone — the preset came out of a
        // Pinia store, so it is a reactive Proxy and structuredClone throws.
        await config.save(JSON.parse(JSON.stringify(preset.config)) as GraphConfig)
        note.value = 'Saved'
    } catch (e) {
        note.value = e instanceof Error ? e.message : String(e)
    } finally {
        busy.value = false
    }
}
</script>

<template>
<div class="empty-graph">
    <p class="empty-title">No memory yet</p>
    <p>
        This graph fills as your assistant works — decisions and their reasons, cautions
        that bit you, problems and how they were solved.
    </p>
    <p>
        <strong>Fast start:</strong> run <code>/engram:digest</code>, or ask your assistant
        to <em>“digest this project”</em>. It reads the working tree and its history and
        captures the canon that is already there in one pass, for you to review here.
    </p>

    <div v-if="options.length" class="ontology">
        <label class="ontology-label" for="empty-ontology">Ontology</label>
        <select id="empty-ontology" v-model="choice" class="ontology-select">
            <option v-for="p in options" :key="p.id" :value="p.id">{{ p.name }}</option>
        </select>
        <button v-if="dirty" class="save" type="button" :disabled="busy" @click="apply">
            {{ busy ? 'Saving…' : 'Save' }}
        </button>
        <span v-if="note" class="note">{{ note }}</span>
    </div>
    <p v-if="chosen?.description" class="ontology-desc">{{ chosen.description }}</p>
</div>
</template>

<style scoped>
.empty-graph {
    display: flex;
    flex-direction: column;
    gap: 0.8rem;
    max-width: 44rem;
    text-align: left;
}

.empty-title {
    font-size: var(--text-h3);
    font-weight: 600;
    color: var(--text-primary);
}

.empty-graph em {
    color: var(--text-primary);
    font-style: italic;
}

.empty-graph code {
    padding: 0.1rem 0.4rem;
    border-radius: var(--radius-sm);
    background: var(--surface-sunken);
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-primary);
}

.ontology {
    display: flex;
    align-items: center;
    gap: 0.8rem;
    flex-wrap: wrap;
    margin-top: 0.4rem;
}

.ontology-label {
    font-size: var(--text-caption);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-tertiary);
}

.ontology-select {
    padding: 0.4rem 0.8rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md);
    background: var(--surface-elevated);
    color: var(--text-primary);
    font: inherit;
    font-size: var(--text-body-sm);
    cursor: pointer;
}

.save {
    padding: 0.4rem 1.2rem;
    border: 1px solid transparent;
    border-radius: var(--radius-full);
    background: var(--interactive-primary);
    color: var(--text-inverse);
    font-size: var(--text-caption);
    font-weight: 600;
    cursor: pointer;
}

.save:disabled {
    opacity: 0.5;
    cursor: default;
}

.save:hover:not(:disabled) {
    background: var(--interactive-primary-hover);
}

.note {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.ontology-desc {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}
</style>
