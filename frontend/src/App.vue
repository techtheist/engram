<script setup lang="ts">
import { onBeforeUnmount, onMounted } from 'vue'
import { storeToRefs } from 'pinia'
import GraphCanvas from '@/components/GraphCanvas.vue'
import EngramMark from '@/components/common/EngramMark.vue'
import SearchBar from '@/components/panels/SearchBar.vue'
import FilterMenu from '@/components/panels/FilterMenu.vue'
import HealthStrip from '@/components/panels/HealthStrip.vue'
import MemoryLensPanel from '@/components/panels/MemoryLensPanel.vue'
import NodeDetail from '@/components/panels/NodeDetail.vue'
import ReviewPanel from '@/components/panels/ReviewPanel.vue'
import SettingsMenu from '@/components/panels/SettingsMenu.vue'
import { useGraphStore } from '@/stores/graph'
import { useThemeStore } from '@/stores/theme'
import { useGraphSync } from '@/composables/useGraphSync'

useThemeStore() // applies the persisted theme on mount via its watcher
const store = useGraphStore()
const { loading, error, connected, nodeList } = storeToRefs(store)

useGraphSync() // poll-reconcile cross-process writes, but only while the user is active

onMounted(async () => {
    await store.load()
    store.connect() // instant SSE for pane-originated writes
})

onBeforeUnmount(() => store.disconnect())
</script>

<template>
<div class="app">
    <GraphCanvas class="canvas-layer" />

    <header class="topbar">
        <div class="brand">
            <EngramMark class="brand-mark" />
            <span class="conn" :class="{ live: connected }" :title="connected ? 'Live' : 'Disconnected'">
                {{ connected ? 'live' : 'offline' }}
            </span>
        </div>
        <div class="topbar-search">
            <SearchBar />
        </div>
        <div class="topbar-actions">
            <FilterMenu />
            <ReviewPanel />
            <SettingsMenu />
        </div>
    </header>

    <NodeDetail />
    <MemoryLensPanel />
    <HealthStrip />

    <Transition name="fade">
        <div v-if="loading" class="overlay glass-panel">Loading memory…</div>
    </Transition>

    <Transition name="fade">
        <div v-if="error" class="overlay glass-panel error">
            <p>Can't reach <code>engram serve</code>.</p>
            <p class="overlay-detail">{{ error }}</p>
            <button class="retry" type="button" @click="store.load()">Retry</button>
        </div>
    </Transition>

    <Transition name="fade">
        <div v-if="!loading && !error && nodeList.length === 0" class="overlay glass-panel empty-state">
            <p class="empty-title">No memory yet</p>
            <p>
                This graph fills as your assistant works — decisions and their reasons,
                cautions that bit you, problems and how they were solved.
            </p>
            <p>
                Fast start: ask your assistant to
                <em>“seed the Engram graph from this project's docs and history”</em>
                — it captures the existing canon in one pass, for you to review here.
            </p>
        </div>
    </Transition>
</div>
</template>

<style scoped>
.app {
    position: fixed;
    inset: 0;
    overflow: hidden;
    background-color: var(--canvas-bg);
    color: var(--text-primary);
    font-family: var(--font-sans);
}

.canvas-layer {
    position: absolute;
    inset: 0;
}

.topbar {
    position: absolute;
    top: 1.6rem;
    left: 1.6rem;
    right: 1.6rem;
    z-index: 10;
    /* Equal 1fr side tracks keep the search screen-centered regardless of how
       the brand and actions differ in width; when space runs out the sides
       floor at their content and the search track shrinks instead. */
    display: grid;
    grid-template-columns: minmax(max-content, 1fr) minmax(0, 36rem) minmax(max-content, 1fr);
    align-items: center;
    gap: 1.6rem;
    pointer-events: none;
}

.topbar > * {
    pointer-events: auto;
}

.topbar-search {
    display: flex;
    justify-content: center;
    justify-self: center;
    width: 100%;
}

.topbar-actions {
    display: flex;
    align-items: center;
    justify-self: end;
    gap: 0.8rem;
}

.brand {
    display: flex;
    align-items: center;
    gap: 0.8rem;
}

.brand-mark {
    width: 3.2rem;
    height: 3.2rem;
    color: var(--interactive-primary);
    filter: drop-shadow(0 0 1rem color-mix(in srgb, var(--interactive-primary) 55%, transparent));
}

.conn {
    padding: 0.2rem 0.6rem;
    border-radius: var(--radius-full);
    font-size: var(--text-caption);
    color: var(--text-tertiary);
    background-color: var(--surface-muted);
}

.conn.live {
    color: var(--trust-trusted);
    background-color: color-mix(in srgb, var(--trust-trusted) 16%, transparent);
}

.overlay {
    position: absolute;
    top: 50%;
    left: 50%;
    z-index: 30;
    transform: translate(-50%, -50%);
    padding: 1.6rem 2rem;
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
    text-align: center;
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
}

.overlay.error {
    border-color: var(--node-problem);
}

.empty-state {
    /* Informational, not modal: sit at canvas level so panels (review, search,
       menus) open above it. */
    z-index: 1;
    display: flex;
    flex-direction: column;
    gap: 0.8rem;
    max-width: 44rem;
    text-align: left;
}

.empty-state .empty-title {
    font-size: var(--text-h3);
    font-weight: 600;
    color: var(--text-primary);
}

.empty-state em {
    color: var(--text-primary);
    font-style: italic;
}

.overlay-detail {
    margin-top: 0.6rem;
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-tertiary);
    max-width: 40rem;
}

.retry {
    margin-top: 1rem;
    padding: 0.6rem 1.4rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: var(--interactive-primary);
    color: var(--text-inverse);
    font-weight: 600;
    cursor: pointer;
}

.fade-enter-active,
.fade-leave-active {
    transition: opacity var(--duration-normal) var(--ease-default);
}

.fade-enter-from,
.fade-leave-to {
    opacity: 0;
}
</style>
