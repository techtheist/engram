<script setup lang="ts">
import { ref, watch } from 'vue'
import MarkdownView from '@/components/common/MarkdownView.vue'
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
<Transition name="drawer-left">
    <aside v-if="open" class="panel glass-panel">
        <header class="head">
            <h2 class="heading">Memory Lens</h2>
            <button class="close" type="button" aria-label="Close" @click="hide">×</button>
        </header>

        <p class="hint">
            What your assistant receives from <code>brief</code> at session start.
        </p>

        <p v-if="loading" class="state">Loading brief…</p>
        <p v-else-if="error" class="state error">{{ error }}</p>
        <MarkdownView v-else-if="content" :content="content" class="content" />
    </aside>
</Transition>
</template>

<style scoped>
.panel {
    /* Left-edge drawer, mirroring NodeDetail on the right. */
    position: fixed;
    top: 6.4rem;
    left: 0;
    bottom: 0;
    /* Above the topbar's stacking context (z-10) — the Review drawer lives
       inside it, and the lens must not open hidden behind that drawer. */
    z-index: 11;
    display: flex;
    flex-direction: column;
    gap: 1rem;
    width: min(44rem, 100vw);
    overflow-y: auto;
    padding: 1.8rem;
    border-top-right-radius: var(--radius-xl);
    border-bottom-right-radius: var(--radius-xl);
    box-shadow: var(--shadow-lg);
}

.drawer-left-enter-active,
.drawer-left-leave-active {
    transition:
        transform var(--duration-normal) var(--ease-default),
        opacity var(--duration-normal) var(--ease-default);
}

.drawer-left-enter-from,
.drawer-left-leave-to {
    transform: translateX(-100%);
    opacity: 0;
}

.head {
    display: flex;
    align-items: center;
    justify-content: space-between;
}

.heading {
    font-size: var(--text-h3);
    font-weight: 700;
    color: var(--text-primary);
}

.close {
    border: none;
    background: transparent;
    color: var(--text-tertiary);
    font-size: 2.4rem;
    line-height: 1;
    cursor: pointer;
}

.close:hover {
    color: var(--text-primary);
}

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
