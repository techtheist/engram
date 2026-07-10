<script setup lang="ts">
import { ref, watch } from 'vue'
import { watchDebounced } from '@vueuse/core'
import DOMPurify from 'dompurify'
import { api } from '@/services/api'
import { useGraphStore } from '@/stores/graph'
import { NODE_ACCENT_VAR } from '@/constants/ontology'
import type { SearchHit } from '@/types/graph'

/** FTS5 wraps matches in <mark>; keep only that tag, drop any stored markup. */
const safeSnippet = (s: string): string => DOMPurify.sanitize(s, { ALLOWED_TAGS: ['mark'] })

const store = useGraphStore()

const query = ref('')
const hits = ref<SearchHit[]>([])
const open = ref(false)
const searching = ref(false)
const error = ref<string | null>(null)

watchDebounced(
    query,
    async (q) => {
        const term = q.trim()
        if (term.length < 2) {
            hits.value = []
            open.value = false
            return
        }
        searching.value = true
        error.value = null
        try {
            hits.value = await api.search(term)
            open.value = true
        } catch (e) {
            error.value = e instanceof Error ? e.message : String(e)
            hits.value = []
        } finally {
            searching.value = false
        }
    },
    { debounce: 250, maxWait: 1000 },
)

function pick(hit: SearchHit): void {
    store.select(hit.id)
    open.value = false
    query.value = ''
}

function clear(): void {
    query.value = ''
    hits.value = []
    open.value = false
}

// Re-open the result list when focus returns and there are stale hits.
watch(query, (q) => {
    if (!q) open.value = false
})
</script>

<template>
<div class="search">
    <div class="field glass-panel">
        <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="11" cy="11" r="7" fill="none" stroke="currentColor" stroke-width="2" />
            <line x1="16.5" y1="16.5" x2="21" y2="21" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
        </svg>
        <input
            v-model="query"
            class="input"
            type="search"
            placeholder="Search memory…"
            aria-label="Search the graph"
            @focus="open = hits.length > 0"
        />
        <button v-if="query" class="clear" type="button" aria-label="Clear" @click="clear">×</button>
    </div>

    <ul v-if="open && hits.length" class="results glass-panel">
        <li v-for="hit in hits" :key="hit.id">
            <button class="result" type="button" @click="pick(hit)">
                <span class="type-dot" :style="{ backgroundColor: NODE_ACCENT_VAR[hit.type] }" />
                <span class="result-text">
                    <span class="result-title">{{ hit.title }}</span>
                    <!-- snippet sanitized to <mark>-only above -->
                    <!-- eslint-disable-next-line vue/no-v-html -->
                    <span class="result-snippet" v-html="safeSnippet(hit.snippet)" />
                </span>
                <span class="result-type">{{ hit.type }}</span>
            </button>
        </li>
    </ul>

    <p v-else-if="open && !searching && query.trim().length >= 2" class="empty glass-panel">
        No matches.
    </p>
    <p v-if="error" class="empty glass-panel error">{{ error }}</p>
</div>
</template>

<style scoped>
.search {
    position: relative;
    width: 36rem;
    max-width: 100%;
}

.field {
    display: flex;
    align-items: center;
    gap: 0.8rem;
    padding: 0 1rem;
    border-radius: var(--radius-lg);
}

.icon {
    width: 1.7rem;
    height: 1.7rem;
    flex-shrink: 0;
    color: var(--text-tertiary);
}

.input {
    flex: 1;
    /* Without this the input's min-content width pushes it out of the pill
       when the pane gets narrow (IDE side panels). */
    min-width: 0;
    border: none;
    background: transparent;
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    padding: 1rem 0;
    outline: none;
}

.input::placeholder {
    color: var(--text-tertiary);
}

.input::-webkit-search-cancel-button {
    display: none;
}

.clear {
    border: none;
    background: transparent;
    color: var(--text-tertiary);
    font-size: 2rem;
    line-height: 1;
    cursor: pointer;
    padding: 0 0.4rem;
}

.clear:hover {
    color: var(--text-primary);
}

.results {
    position: absolute;
    top: calc(100% + 0.6rem);
    left: 0;
    right: 0;
    z-index: 20;
    max-height: 42rem;
    overflow-y: auto;
    padding: 0.5rem;
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
    list-style: none;
}

.result {
    display: flex;
    align-items: flex-start;
    gap: 0.9rem;
    width: 100%;
    padding: 0.9rem 1rem;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    text-align: left;
    cursor: pointer;
}

.result:hover {
    background-color: var(--interactive-ghost-hover);
}

.type-dot {
    width: 0.9rem;
    height: 0.9rem;
    margin-top: 0.4rem;
    border-radius: var(--radius-full);
    flex-shrink: 0;
}

.result-text {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    min-width: 0;
    flex: 1;
}

.result-title {
    font-size: var(--text-body-sm);
    font-weight: 600;
    color: var(--text-primary);
}

.result-snippet {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.result-snippet :deep(mark) {
    background: transparent;
    color: var(--interactive-primary-hover);
    font-weight: 600;
}

.result-type {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
    flex-shrink: 0;
}

.empty {
    position: absolute;
    top: calc(100% + 0.6rem);
    left: 0;
    right: 0;
    z-index: 20;
    padding: 1rem 1.2rem;
    border-radius: var(--radius-lg);
    font-size: var(--text-body-sm);
    color: var(--text-tertiary);
}

.empty.error {
    color: var(--node-problem);
}
</style>
