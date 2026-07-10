<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import SidePanel from '@/components/common/SidePanel.vue'
import TagEditor from '@/components/common/TagEditor.vue'
import { ALL_NODE_TYPES, NODE_ACCENT_VAR } from '@/constants/ontology'
import { useGraphStore } from '@/stores/graph'
import type { Durability, NodeType } from '@/types/graph'

/**
 * Manual node creation (PLAN §10 pane CRUD): user parity with Claude's
 * add_note. Created nodes are user-sourced — trusted by construction.
 */
const open = defineModel<boolean>({ required: true })

const store = useGraphStore()
const busy = ref(false)
const error = ref<string | null>(null)

const DURABILITIES: Durability[] = ['stable', 'episodic', 'volatile']

/** Mirrors the MCP server's default durability per type. */
const NATURAL_DURABILITY: Record<NodeType, Durability> = {
    Principle: 'stable',
    Decision: 'stable',
    Caution: 'stable',
    Anchor: 'stable',
    Problem: 'episodic',
    Resolution: 'episodic',
    Insight: 'episodic',
    Intent: 'volatile',
}

const draft = reactive({
    type: 'Decision' as NodeType,
    title: '',
    body: '',
    durability: 'stable' as Durability,
    tags: [] as string[],
})

// Picking a type re-seeds the natural durability; the user can still override.
watch(
    () => draft.type,
    (t) => (draft.durability = NATURAL_DURABILITY[t]),
)

const accent = computed(() => NODE_ACCENT_VAR[draft.type])

function reset(): void {
    draft.type = 'Decision'
    draft.title = ''
    draft.body = ''
    draft.durability = 'stable'
    draft.tags = []
    error.value = null
}

async function save(): Promise<void> {
    busy.value = true
    error.value = null
    try {
        const created = await store.createNode({
            type: draft.type,
            title: draft.title.trim(),
            body: draft.body.trim() || undefined,
            durability: draft.durability,
            // Problems and Intents are live worklist items from birth.
            status: draft.type === 'Problem' || draft.type === 'Intent' ? 'open' : undefined,
            tags: draft.tags,
        })
        open.value = false
        reset()
        store.select(created.id)
    } catch (e) {
        error.value = e instanceof Error ? e.message : String(e)
    } finally {
        busy.value = false
    }
}

function close(): void {
    open.value = false
    error.value = null
}
</script>

<template>
<SidePanel
    :open="open"
    side="right"
    panel-id="create"
    :default-rem="40"
    :min-rem="28"
    :dismiss="close"
    :accent="accent"
    title="New memory"
    :style="{ '--accent': accent }"
>
    <div class="type-row">
        <button
            v-for="t in ALL_NODE_TYPES"
            :key="t"
            class="type-chip"
            type="button"
            :class="{ on: draft.type === t }"
            :style="{ '--chip-accent': NODE_ACCENT_VAR[t] }"
            @click="draft.type = t"
        >
            {{ t }}
        </button>
    </div>

    <input
        v-model="draft.title"
        class="edit-input"
        type="text"
        placeholder="Title"
        aria-label="Title"
    />
    <textarea
        v-model="draft.body"
        class="edit-input edit-body"
        rows="8"
        placeholder="Body (markdown, optional)"
        aria-label="Body (markdown)"
    />

    <label class="edit-label">
        Durability
        <select v-model="draft.durability" class="edit-select">
            <option v-for="d in DURABILITIES" :key="d" :value="d">{{ d }}</option>
        </select>
    </label>

    <label class="edit-label">
        Tags
        <TagEditor v-model="draft.tags" />
    </label>

    <p v-if="error" class="error">{{ error }}</p>

    <footer class="actions">
        <button class="btn primary" type="button" :disabled="busy || !draft.title.trim()" @click="save">
            Create
        </button>
        <button class="btn ghost" type="button" :disabled="busy" @click="close">
            Cancel
        </button>
    </footer>
</SidePanel>
</template>

<style scoped>
.type-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
}

.type-chip {
    --chip-accent: var(--interactive-primary);
    padding: 0.3rem 0.9rem;
    border-radius: var(--radius-full);
    border: 1px solid var(--border-default);
    background-color: transparent;
    color: var(--text-secondary);
    font-size: var(--text-caption);
    font-weight: 600;
    cursor: pointer;
}

.type-chip:hover {
    color: var(--text-primary);
    background-color: var(--interactive-ghost-hover);
}

.type-chip.on {
    color: var(--chip-accent);
    border-color: color-mix(in srgb, var(--chip-accent) 55%, transparent);
    background-color: color-mix(in srgb, var(--chip-accent) 14%, transparent);
}

.edit-input {
    width: 100%;
    padding: 0.7rem 0.9rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: var(--surface-sunken);
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    font-family: inherit;
}

.edit-body {
    resize: vertical;
    font-family: var(--font-mono);
    line-height: var(--leading-normal);
}

.edit-label {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.edit-select {
    padding: 0.5rem 0.7rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: var(--surface-sunken);
    color: var(--text-primary);
    font-size: var(--text-body-sm);
}

.error {
    font-size: var(--text-caption);
    color: var(--node-problem);
}

.actions {
    display: flex;
    gap: 0.6rem;
    margin-top: auto;
    padding-top: 0.6rem;
}

.btn {
    padding: 0.7rem 1.2rem;
    border-radius: var(--radius-md);
    font-size: var(--text-label);
    font-weight: 600;
    cursor: pointer;
    border: 1px solid var(--border-default);
    background-color: var(--surface-elevated);
    color: var(--text-primary);
}

.btn:disabled {
    opacity: 0.5;
    cursor: default;
}

.btn:hover:not(:disabled) {
    background-color: var(--node-hover-surface);
}

.btn.primary {
    border-color: transparent;
    background-color: var(--interactive-primary);
    color: var(--text-inverse);
}

.btn.ghost {
    background-color: transparent;
}
</style>
