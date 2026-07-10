<script setup lang="ts">
import { ref, watch } from 'vue'
import MarkdownView from '@/components/common/MarkdownView.vue'
import SidePanel from '@/components/common/SidePanel.vue'
import { api } from '@/services/api'
import { useMemoryLens } from '@/composables/useMemoryLens'

/**
 * Memory Lens (PLAN §10): shows exactly what the assistant receives from the
 * memory. First view: the `brief` session-start digest, rendered as markdown.
 */
const { open, hide } = useMemoryLens()

const content = ref<string | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)

watch(open, async (isOpen) => {
    if (!isOpen) return
    loading.value = true
    error.value = null
    try {
        content.value = await api.brief()
    } catch (e) {
        error.value = e instanceof Error ? e.message : String(e)
    } finally {
        loading.value = false
    }
})
</script>

<template>
<SidePanel
    :open="open"
    side="left"
    panel-id="lens"
    :default-rem="44"
    :min-rem="30"
    :dismiss="hide"
    title="Memory Lens"
    style="--panel-gap: 1rem"
>
    <p class="hint">
        What your assistant receives from <code>brief</code> at session start.
    </p>

    <p v-if="loading" class="state">Loading brief…</p>
    <p v-else-if="error" class="state error">{{ error }}</p>
    <MarkdownView v-else-if="content" :content="content" class="content" />
</SidePanel>
</template>

<style scoped>
.hint {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.hint code {
    font-family: var(--font-mono);
    background-color: var(--surface-sunken);
    padding: 0.1rem 0.4rem;
    border-radius: var(--radius-sm);
}

.state {
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
}

.state.error {
    color: var(--node-problem);
}

.content {
    font-size: var(--text-body-sm);
    line-height: var(--leading-normal);
}

/* The brief is heading-structured; MarkdownView only styles body elements. */
.content :deep(h1) {
    font-size: var(--text-h3);
    font-weight: 700;
    color: var(--text-primary);
    margin-bottom: 0.8rem;
}

.content :deep(h2) {
    font-size: var(--text-body);
    font-weight: 600;
    color: var(--text-primary);
    margin: 1.2rem 0 0.6rem;
}
</style>
