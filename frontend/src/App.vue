<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, useTemplateRef, watch } from 'vue'
import { onClickOutside } from '@vueuse/core'
import { storeToRefs } from 'pinia'
import FeedView from '@/components/FeedView.vue'
import GraphCanvas from '@/components/GraphCanvas.vue'
import EmptyGraph from '@/components/common/EmptyGraph.vue'
import EngramMark from '@/components/common/EngramMark.vue'
import SegmentedControl from '@/components/common/SegmentedControl.vue'
import AuditPanel from '@/components/panels/AuditPanel.vue'
import CheckupPanel from '@/components/panels/CheckupPanel.vue'
import ConfigPanel from '@/components/panels/ConfigPanel.vue'
import SearchBar from '@/components/panels/SearchBar.vue'
import FilterMenu from '@/components/panels/FilterMenu.vue'
import HealthStrip from '@/components/panels/HealthStrip.vue'
import MemoryLensPanel from '@/components/panels/MemoryLensPanel.vue'
import NodeCreatePanel from '@/components/panels/NodeCreatePanel.vue'
import NodeDetail from '@/components/panels/NodeDetail.vue'
import ProjectSwitcher from '@/components/panels/ProjectSwitcher.vue'
import ReviewPanel from '@/components/panels/ReviewPanel.vue'
import SettingsMenu from '@/components/panels/SettingsMenu.vue'
import SystemInfoPanel from '@/components/panels/SystemInfoPanel.vue'
import { hostChrome } from '@/services/hostChrome'
import { useConfigStore } from '@/stores/config'
import { useGraphStore } from '@/stores/graph'
import { useLayoutStore, type ViewMode } from '@/stores/layout'
import { useThemeStore } from '@/stores/theme'
import { useGraphSync } from '@/composables/useGraphSync'

useThemeStore() // applies the persisted theme on mount via its watcher
const store = useGraphStore()
const config = useConfigStore()
const layout = useLayoutStore()
const { loading, error, connected, nodeList } = storeToRefs(store)

const VIEW_OPTIONS = [
    { value: 'graph', label: 'Graph' },
    { value: 'feed', label: 'Feed' },
]

useGraphSync() // poll-reconcile cross-process writes, but only while the user is active

const creating = ref(false)

// Narrow panes (IDE side panels) fold + New / Filter / Review under a burger.
const menuOpen = ref(false)
const actionsRoot = useTemplateRef<HTMLElement>('actionsRoot')
onClickOutside(actionsRoot, () => (menuOpen.value = false))

// The feed acts on the card at the center, not through a drawer: arriving
// there with the detail pane still open covers the feed with the node you
// just left. The selection survives (the feed centers on it) — Edit reopens
// the drawer when the full form is actually wanted.
watch(
    () => layout.view,
    (view) => {
        if (view === 'feed') store.closeDetail()
    },
)

function startCreate(): void {
    // Claiming the right side auto-dismisses the detail drawer (usePanels).
    creating.value = true
    menuOpen.value = false
}

onMounted(async () => {
    // Config first-ish: colors/labels derive from it; both load in parallel
    // and the ontology CSS lands as soon as it arrives.
    await Promise.all([config.load().catch(() => undefined), store.load()])
    store.connect() // instant SSE for pane-originated writes
})

onBeforeUnmount(() => store.disconnect())
</script>

<template>
<div class="app">
    <!-- v-show keeps the canvas's zoom/positions alive across view switches;
         the feed mounts fresh (its keyboard handling must not run under the
         graph screen). -->
    <GraphCanvas v-show="layout.view === 'graph'" class="canvas-layer" />
    <FeedView v-if="layout.view === 'feed'" />

    <header class="topbar">
        <div class="brand">
            <span class="brand-chip">
                <EngramMark class="brand-mark" />
            </span>
            <ProjectSwitcher />
            <span class="conn" :class="{ live: connected }" :title="connected ? 'Live' : 'Disconnected'">
                {{ connected ? 'live' : 'offline' }}
            </span>
        </div>
        <div class="topbar-search">
            <SearchBar />
        </div>
        <div ref="actionsRoot" class="topbar-actions">
            <SegmentedControl
                class="view-toggle"
                :model-value="layout.view"
                :options="VIEW_OPTIONS"
                aria-label="Screen"
                @update:model-value="layout.setView($event as ViewMode)"
            />
            <button
                class="burger"
                type="button"
                :class="{ active: menuOpen }"
                :title="menuOpen ? 'Close actions' : 'Actions'"
                @click="menuOpen = !menuOpen"
            >
                <svg class="burger-icon" viewBox="0 0 24 24" aria-hidden="true">
                    <line x1="4" y1="6" x2="20" y2="6" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
                    <line x1="4" y1="12" x2="20" y2="12" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
                    <line x1="4" y1="18" x2="20" y2="18" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
                </svg>
            </button>
            <div class="actions-cluster" :class="{ open: menuOpen }">
                <button class="new-node" type="button" title="Create a memory node" @click="startCreate">
                    + New
                </button>
                <FilterMenu />
                <ReviewPanel />
                <CheckupPanel />
            </div>
            <SettingsMenu />
        </div>
    </header>

    <!-- Nothing in the shipped builds; the Pages demo mounts its banner here. -->
    <component :is="hostChrome" v-if="hostChrome" />

    <NodeDetail />
    <NodeCreatePanel v-model="creating" />
    <MemoryLensPanel />
    <AuditPanel />
    <SystemInfoPanel />
    <ConfigPanel />
    <!-- Graph view only: the feed's bottom row belongs to the action bar,
         and the strip earned no other home there. -->
    <HealthStrip v-if="layout.view === 'graph'" />

    <Transition name="fade">
        <div v-if="loading" class="overlay glass-panel">Loading memory…</div>
    </Transition>

    <Transition name="fade">
        <div v-if="error" class="overlay glass-panel error">
            <p>Can't reach <code>engram-alpha serve</code>.</p>
            <p class="overlay-detail">{{ error }}</p>
            <button class="retry" type="button" @click="store.load()">Retry</button>
        </div>
    </Transition>

    <Transition name="fade">
        <!-- Graph-only: the feed carries its own empty state, and two of them
             at once read as a broken screen. -->
        <div
            v-if="!loading && !error && nodeList.length === 0 && layout.view === 'graph'"
            class="overlay glass-panel empty-state"
        >
            <EmptyGraph />
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
    /* Above the side drawers (z 11): the bar itself never overlaps them
       (drawers start below it), but its dropdowns — settings, filters,
       search results, the burger cluster — must stack over open drawers.
       This is the whole subtree's stacking context, so the inner z-20s
       can't win on their own. */
    z-index: 12;
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
    position: relative;
    display: flex;
    align-items: center;
    justify-self: end;
    gap: 0.8rem;
}

/* Inline row on wide panes; the burger + dropdown take over under 600px. */
.actions-cluster {
    display: contents;
}

/* The Graph/Feed switch rides the floating bar — glass like its siblings. */
.view-toggle {
    background-color: var(--surface-glass);
    backdrop-filter: var(--glass-backdrop);
}

.burger {
    display: none;
    align-items: center;
    justify-content: center;
    padding: 0.9rem;
    border-radius: var(--radius-full);
    border: 1px solid var(--border-default);
    background-color: var(--surface-glass);
    backdrop-filter: var(--glass-backdrop);
    color: var(--text-secondary);
    cursor: pointer;
}

.burger:hover,
.burger.active {
    color: var(--text-primary);
    background-color: var(--node-hover-surface);
}

.burger-icon {
    display: block;
    width: 1.6rem;
    height: 1.6rem;
}

@media (width <= 850px) {
    .burger {
        display: flex;
    }

    .actions-cluster {
        position: absolute;
        top: calc(100% + 0.8rem);
        right: 0;
        z-index: 20;
        display: flex;
        flex-direction: column;
        align-items: stretch;
        gap: 0.8rem;
        padding: 0.8rem;
        border-radius: var(--radius-lg);
        border: 1px solid var(--border-default);
        /* Opaque, deliberately NOT glass: a backdrop-filter here would make
           this box the containing block for position:fixed descendants, and
           the Review/Checkup drawers are exactly that — their left:0 would
           mean the dropdown's corner, not the viewport, hiding the drawer
           off-screen. (Glass would also sample nothing over the feed's
           scroller — same Chromium limit as the search bar.) */
        background-color: var(--surface-elevated);
        box-shadow: var(--shadow-lg);
        /* visibility, not display: the Review drawer is a fixed-position
           descendant and must survive the menu closing. */
        visibility: hidden;
    }

    .actions-cluster.open {
        visibility: visible;
    }

    .actions-cluster :deep(.side-panel) {
        visibility: visible;
    }

    .actions-cluster > * {
        width: 100%;
    }

    .actions-cluster :deep(.toggle) {
        width: 100%;
        justify-content: center;
    }
}

/* Too narrow for a useful search pill (IDE side panels) — the grid's middle
   track goes with it, so the bar falls back to flex. */
@media (width <= 530px) {
    .topbar {
        display: flex;
        justify-content: space-between;
    }

    .topbar-search {
        display: none;
    }
}

/* Bare minimum chrome: the connection badge and the actions burger.
   (.brand-qualified so this outranks the base .brand-chip rule, which
   sits later in the file.) */
@media (width <= 400px) {
    .brand .brand-chip,
    .brand .switcher-root {
        display: none;
    }
}

/* The one primary action in the bar — filled, unlike its ghost siblings. */
.new-node {
    padding: 0.6rem 1.2rem;
    border-radius: var(--radius-full);
    border: 1px solid transparent;
    background-color: var(--interactive-primary);
    color: var(--text-inverse);
    font-size: var(--text-label);
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
}

.new-node:hover {
    background-color: var(--interactive-primary-hover);
}

.brand {
    display: flex;
    align-items: center;
    gap: 0.8rem;
}

.brand-chip {
    display: grid;
    flex-shrink: 0;
    place-content: center;
    width: 3.2rem;
    height: 3.2rem;
    border-radius: var(--radius-md);
    background: var(--interactive-primary);
    color: var(--text-inverse);
    box-shadow: 0 0 1.2rem color-mix(in srgb, var(--interactive-primary) 45%, transparent);
}

.brand-mark {
    width: 2.2rem;
    height: 2.2rem;
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
       menus) open above it. The card's own type and layout live in
       EmptyGraph.vue — both screens show the same one. */
    z-index: 1;
    text-align: left;
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
