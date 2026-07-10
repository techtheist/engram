<script setup lang="ts">
import { computed, ref, useTemplateRef } from 'vue'
import { onClickOutside } from '@vueuse/core'
import { DEFAULT_TAGS } from '@/constants/ontology'
import { useGraphStore } from '@/stores/graph'

/**
 * Chip list + combobox for a node's tags: pick from the graph's live
 * vocabulary (plus the recommended defaults while the graph is young) or type
 * a new tag — Enter creates it. Mirrors the backend normalization so the chip
 * shows what will actually be stored.
 */
const tags = defineModel<string[]>({ required: true })

const store = useGraphStore()
const query = ref('')
const open = ref(false)
const root = useTemplateRef<HTMLElement>('root')
onClickOutside(root, () => (open.value = false))

/** Same canonical form the backend applies (store::normalize_tags). */
function normalize(raw: string): string {
    return raw.toLowerCase().split(/\s+/).filter(Boolean).join('-')
}

const suggestions = computed(() => {
    const vocabulary = [...new Set([...store.allTags, ...DEFAULT_TAGS])]
    const q = normalize(query.value)
    return vocabulary
        .filter((t) => !tags.value.includes(t) && (!q || t.includes(q)))
        .slice(0, 8)
})

const creatable = computed(() => {
    const t = normalize(query.value)
    return t && !tags.value.includes(t) && !suggestions.value.includes(t) ? t : null
})

function add(tag: string): void {
    if (!tags.value.includes(tag)) tags.value = [...tags.value, tag]
    query.value = ''
}

function remove(tag: string): void {
    tags.value = tags.value.filter((t) => t !== tag)
}

function onEnter(): void {
    const t = normalize(query.value)
    if (t) add(t)
    else open.value = false
}

function onBackspace(): void {
    if (query.value === '' && tags.value.length) {
        tags.value = tags.value.slice(0, -1)
    }
}
</script>

<template>
<div ref="root" class="tag-editor">
    <div class="field" @click="open = true">
        <span v-for="t in tags" :key="t" class="tag-chip">
            #{{ t }}
            <button class="remove" type="button" :aria-label="`Remove tag ${t}`" @click.stop="remove(t)">
                ×
            </button>
        </span>
        <input
            v-model="query"
            class="input"
            type="text"
            :placeholder="tags.length ? '' : 'Add tags…'"
            aria-label="Add tag"
            @focus="open = true"
            @keydown.enter.prevent="onEnter"
            @keydown.backspace="onBackspace"
            @keydown.escape="open = false"
        />
    </div>

    <div v-if="open && (suggestions.length || creatable)" class="menu glass-panel">
        <button
            v-for="s in suggestions"
            :key="s"
            class="option"
            type="button"
            @click="add(s)"
        >
            #{{ s }}
        </button>
        <button v-if="creatable" class="option create" type="button" @click="add(creatable)">
            Create “#{{ creatable }}”
        </button>
    </div>
</div>
</template>

<style scoped>
.tag-editor {
    position: relative;
}

.field {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.4rem;
    width: 100%;
    min-height: 3.4rem;
    padding: 0.5rem 0.9rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: var(--surface-sunken);
    cursor: text;
}

.tag-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.2rem 0.7rem;
    border-radius: var(--radius-full);
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--interactive-primary);
    background-color: color-mix(in srgb, var(--interactive-primary) 14%, transparent);
    border: 1px solid color-mix(in srgb, var(--interactive-primary) 40%, transparent);
}

.remove {
    border: none;
    background: transparent;
    color: inherit;
    font-size: 1.4rem;
    line-height: 1;
    cursor: pointer;
    opacity: 0.7;
}

.remove:hover {
    opacity: 1;
}

.input {
    flex: 1;
    min-width: 8rem;
    border: none;
    background: transparent;
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    font-family: inherit;
    outline: none;
}

.menu {
    position: absolute;
    top: calc(100% + 0.4rem);
    left: 0;
    right: 0;
    z-index: 30;
    display: flex;
    flex-direction: column;
    max-height: 22rem;
    overflow-y: auto;
    padding: 0.4rem;
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-lg);
}

.option {
    padding: 0.5rem 0.8rem;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-secondary);
    font-size: var(--text-body-sm);
    text-align: left;
    cursor: pointer;
}

.option:hover {
    color: var(--text-primary);
    background-color: var(--interactive-ghost-hover);
}

.option.create {
    color: var(--interactive-primary);
    font-weight: 600;
}
</style>
