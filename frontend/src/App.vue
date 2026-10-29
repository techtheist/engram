<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref, useTemplateRef, watch } from 'vue'
import { onClickOutside } from '@vueuse/core'
import { storeToRefs } from 'pinia'
import FeedView from '@/components/FeedView.vue'
import GraphCanvas from '@/components/GraphCanvas.vue'
import HistoryView from '@/components/HistoryView.vue'
import TasksView from '@/components/TasksView.vue'
import ScreenSkeleton from '@/components/common/ScreenSkeleton.vue'
import ConfirmHost from '@/components/common/ConfirmHost.vue'
import EmptyGraph from '@/components/common/EmptyGraph.vue'
import EngramMark from '@/components/common/EngramMark.vue'
import ViewSwitcher from '@/components/common/ViewSwitcher.vue'
import AuditPanel from '@/components/panels/AuditPanel.vue'
import CheckupPanel from '@/components/panels/CheckupPanel.vue'
import ConfigPanel from '@/components/panels/ConfigPanel.vue'
import SearchBar from '@/components/panels/SearchBar.vue'
import FilterMenu from '@/components/panels/FilterMenu.vue'
import HealthStrip from '@/components/panels/HealthStrip.vue'
import HistoryNotice from '@/components/panels/HistoryNotice.vue'
import MemoryLensPanel from '@/components/panels/MemoryLensPanel.vue'
import NodeCreatePanel from '@/components/panels/NodeCreatePanel.vue'
import NodeDetail from '@/components/panels/NodeDetail.vue'
import ProjectSwitcher from '@/components/panels/ProjectSwitcher.vue'
import ReviewPanel from '@/components/panels/ReviewPanel.vue'
import SettingsMenu from '@/components/panels/SettingsMenu.vue'
import SystemInfoPanel from '@/components/panels/SystemInfoPanel.vue'
import TaskCreatePanel from '@/components/panels/TaskCreatePanel.vue'
import TaskDrawer from '@/components/panels/TaskDrawer.vue'
import { isDemo } from '@/services/api'
import { hostChrome } from '@/services/hostChrome'
import { useConfigStore } from '@/stores/config'
import { useGraphStore } from '@/stores/graph'
import { useLayoutStore, type ViewMode } from '@/stores/layout'
import { useProjectsStore } from '@/stores/projects'
import { useTasksStore } from '@/stores/tasks'
import { useThemeStore } from '@/stores/theme'
import { useGraphSync } from '@/composables/useGraphSync'

const theme = useThemeStore() // applies the persisted theme on mount via its watcher
// The top-bar icon (0.9.7): the brand theme shows the boxed purple icon as
// drawn (64px source for 32px at 2x); IDE skins draw the mark in their own
// accent + text colors so it never looks pasted on from another product.
const brandIcon = `${import.meta.env.BASE_URL}engram-light-64.png`
const brandTheme = computed(() => theme.current === 'engram-purple')
const store = useGraphStore()
const config = useConfigStore()
const layout = useLayoutStore()
const projects = useProjectsStore()
const { loading, error, connected, nodeList } = storeToRefs(store)

const tasks = useTasksStore()
// The Tasks screen exists only while the graph keeps tasks (0.10.0).
const VIEW_OPTIONS = computed(() =>
    [
        { value: 'graph', label: 'Graph' },
        { value: 'feed', label: 'Feed' },
        { value: 'tasks', label: 'Tasks' },
        { value: 'history', label: 'History' },
    ].filter((o) => o.value !== 'tasks' || config.tasksEnabled),
)

// Turned off while on the Tasks screen: back to the graph.
watch(
    () => config.tasksEnabled,
    (on) => {
        if (!on && layout.view === 'tasks') layout.setView('graph')
        void tasks.load()
    },
)

useGraphSync() // poll-reconcile cross-process writes, but only while the user is active

const creating = ref(false)

// Narrow panes (IDE side panels) fold + New / Filter / Review under a burger.
const menuOpen = ref(false)
const actionsRoot = useTemplateRef<HTMLElement>('actionsRoot')
onClickOutside(actionsRoot, () => (menuOpen.value = false))

// The feed acts on the card at the center, not through a drawer: arriving
// there with the detail pane still open covers the feed with the node you
// just left — so entering the feed closes it (the selection survives; the
// feed centers on it). Every other switch leaves the drawer as it was: open
// on the graph stays open over tasks and history and is still open back on
// the graph; closed stays closed. The feed itself decides on the way out
// (FeedView's unmount): to the graph it opens the drawer, anywhere else not.
watch(
    () => layout.view,
    (view) => {
        if (view === 'feed') store.closeDetail()
    },
)

// ---- screens: kept alive, switched by sliding (0.10.0) ---------------------
//
// Every screen, once visited, stays in the page; the inactive ones are
// `content-visibility: hidden` + inert, so the browser keeps their layout and
// skips painting them — coming back costs a paint, not a rebuild (Feed was
// ~800 ms per visit on a 640-note graph, the canvas 400–750 ms to re-measure
// after display:none). A switch slides the new screen in from the side of
// its position in the toggle and the old one out, both fading, so the
// backgrounds crossfade while the content moves; the top bar stays put.
const SCREENS: ViewMode[] = ['graph', 'feed', 'tasks', 'history']
/** Matches the slide keyframes' duration in the styles below. */
const SWITCH_MS = 220
const visited = reactive(new Set<ViewMode>([layout.view]))
const leaving = ref<ViewMode | null>(null)
const forward = ref(true)
let switchTimer: number | undefined

watch(
    () => layout.view,
    (view, prev) => {
        visited.add(view)
        forward.value = SCREENS.indexOf(view) > SCREENS.indexOf(prev)
        leaving.value = prev
        window.clearTimeout(switchTimer)
        switchTimer = window.setTimeout(() => (leaving.value = null), SWITCH_MS + 40)
    },
)

/**
 * Build the other screens hidden once the browser is idle after load, so a
 * first visit costs what a return does (Feed's first build was ~950 ms on a
 * 640-note graph). A screen asked for before this ran shows its skeleton
 * through the slide and builds right after (see `warm`).
 */
function prewarm(): void {
    const run = () => SCREENS.forEach((s) => warm.add(s))
    if ('requestIdleCallback' in window) window.requestIdleCallback(run, { timeout: 4000 })
    else globalThis.setTimeout(run, 1500) // Safari
}

/** Screens whose real component is mounted (the rest show a skeleton). */
const warm = reactive(new Set<ViewMode>([layout.view]))
watch(
    () => layout.view,
    (view) => {
        if (warm.has(view)) return
        // Cold: slide the skeleton in, then build the real screen.
        window.setTimeout(() => warm.add(view), SWITCH_MS + 20)
    },
)

function screenClass(s: ViewMode): Record<string, boolean> {
    const switching = leaving.value != null
    return {
        active: layout.view === s,
        leaving: leaving.value === s,
        entering: switching && layout.view === s,
        back: !forward.value,
    }
}

/** New makes what the screen is about: a task on Tasks, a memory elsewhere. */
const newTitle = computed(() => (layout.view === 'tasks' ? 'Create a task' : 'Create a memory node'))

function startCreate(): void {
    menuOpen.value = false
    if (layout.view === 'tasks') {
        tasks.startCreate()
        return
    }
    // Claiming the right side auto-dismisses the detail drawer (usePanels).
    creating.value = true
}

onMounted(async () => {
    // Which graph first: the `?project=` deep link / remembered selection
    // must scope the API before anything project-scoped loads.
    await projects.restore().catch(() => undefined)
    // Config first-ish: colors/labels derive from it; both load in parallel
    // and the ontology CSS lands as soon as it arrives.
    await Promise.all([config.load().catch(() => undefined), store.load(), tasks.load()])
    store.connect() // instant SSE for pane-originated writes
    prewarm()
})

onBeforeUnmount(() => store.disconnect())
</script>

<template>
<div class="app" :data-view="layout.view">
    <!-- Screens stay mounted once visited (see screenClass): the canvas
         keeps its zoom and positions, the feed its cards, and switching
         back is a paint, not a rebuild. -->
    <div class="screen" :class="screenClass('graph')" :inert="layout.view !== 'graph'">
        <GraphCanvas class="canvas-layer" />
    </div>
    <div v-if="visited.has('feed') || warm.has('feed')" class="screen" :class="screenClass('feed')" :inert="layout.view !== 'feed'">
        <FeedView v-if="warm.has('feed')" />
        <ScreenSkeleton v-else kind="feed" />
    </div>
    <div v-if="visited.has('tasks') || warm.has('tasks')" class="screen" :class="screenClass('tasks')" :inert="layout.view !== 'tasks'">
        <TasksView v-if="warm.has('tasks')" />
        <ScreenSkeleton v-else kind="tasks" />
    </div>
    <div v-if="visited.has('history') || warm.has('history')" class="screen" :class="screenClass('history')" :inert="layout.view !== 'history'">
        <HistoryView v-if="warm.has('history')" />
        <ScreenSkeleton v-else kind="history" />
    </div>

    <header class="topbar">
        <div class="brand">
            <span class="brand-chip" :class="{ boxed: !brandTheme }">
                <img v-if="brandTheme" class="brand-icon" :src="brandIcon" alt="" width="32" height="32" />
                <EngramMark v-else class="brand-mark" />
            </span>
            <ProjectSwitcher />
            <span class="conn" :class="{ live: connected }" :title="connected ? (isDemo ? 'Demo' : 'Live') : 'Disconnected'">
                {{ connected ? (isDemo ? 'demo' : '●') : 'offline' }}
            </span>
        </div>
        <div class="topbar-search">
            <SearchBar />
        </div>
        <div ref="actionsRoot" class="topbar-actions">
            <ViewSwitcher
                :model-value="layout.view"
                :options="VIEW_OPTIONS"
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
                <button class="new-node" type="button" :title="newTitle" @click="startCreate">
                    New
                </button>
                <!-- One Filter in one place; it filters whatever the screen shows. -->
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
    <TaskDrawer v-if="layout.view === 'tasks'" />
    <TaskCreatePanel v-if="layout.view === 'tasks'" />
    <NodeCreatePanel v-model="creating" />
    <MemoryLensPanel />
    <AuditPanel />
    <SystemInfoPanel />
    <ConfigPanel />
    <!-- Graph view only: the feed's bottom row belongs to the action bar,
         and the strip earned no other home there. -->
    <HealthStrip v-if="layout.view === 'graph'" />
    <HistoryNotice />
    <ConfirmHost />

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
    /* clip, not hidden: a hidden box is still a scroll container, and a
       scrollIntoView aimed at sliding content (it sits 28% off to the side
       mid-switch) scrolled the whole app sideways — top bar included. */
    overflow: clip;
    background-color: var(--canvas-bg);
    color: var(--text-primary);
    font-family: var(--font-sans);
}

.canvas-layer {
    position: absolute;
    inset: 0;
}

/* One layer per screen. Inactive ones keep their layout but skip painting
   (content-visibility), so a return is cheap; only the active screen and
   the one sliding out are rendered. */
.screen {
    --slide: 28%;
    position: absolute;
    inset: 0;
    content-visibility: hidden;
    /* Not painted, and not composited either: Safari skipped painting a
       content-visibility:hidden screen but still composited the graph's
       WebGL canvas (an opaque layer), so Tasks and History showed it as a
       black or stale frame behind them — ghosts of menus included. */
    visibility: hidden;
}

.screen.active,
.screen.leaving {
    content-visibility: visible;
    visibility: visible;
}

/* A switch is two motions: the whole screen (its backdrop — the canvas
   color, glow and dot grid, the feed's gradient) only fades, while its
   content (`.screen-content`, and the canvas's node/edge layer) slides.
   Content moves with the `translate` property, which composes with the
   transforms components already own (Vue Flow's pan/zoom, centered bars). */
.screen.leaving {
    pointer-events: none;
    animation: screen-fade-out 220ms cubic-bezier(0.3, 0.7, 0.2, 1) both;
}

.screen.entering {
    animation: screen-fade-in 220ms cubic-bezier(0.3, 0.7, 0.2, 1) both;
}

.screen.entering :deep(.screen-content),
.screen.entering :deep(.vue-flow__viewport),
.screen.entering :deep(.gl-slide) {
    animation: content-in-fwd 220ms cubic-bezier(0.3, 0.7, 0.2, 1) both;
}

.screen.leaving :deep(.screen-content),
.screen.leaving :deep(.vue-flow__viewport),
.screen.leaving :deep(.gl-slide) {
    animation: content-out-fwd 220ms cubic-bezier(0.3, 0.7, 0.2, 1) both;
}

.screen.entering.back :deep(.screen-content),
.screen.entering.back :deep(.vue-flow__viewport),
.screen.entering.back :deep(.gl-slide) {
    animation-name: content-in-back;
}

.screen.leaving.back :deep(.screen-content),
.screen.leaving.back :deep(.vue-flow__viewport),
.screen.leaving.back :deep(.gl-slide) {
    animation-name: content-out-back;
}

@keyframes screen-fade-in {
    from {
        opacity: 0;
    }
}

@keyframes screen-fade-out {
    to {
        opacity: 0;
    }
}

/* Forward (to a screen right of this one in the toggle): content comes in
   from the right and leaves to the left; back mirrors it. */
@keyframes content-in-fwd {
    from {
        translate: var(--slide) 0;
    }
}

@keyframes content-in-back {
    from {
        translate: calc(-1 * var(--slide)) 0;
    }
}

@keyframes content-out-fwd {
    to {
        translate: calc(-1 * var(--slide)) 0;
    }
}

@keyframes content-out-back {
    to {
        translate: var(--slide) 0;
    }
}

/* Reduced motion: no travel, just the crossfade. */
@media (prefers-reduced-motion: reduce) {
    .screen {
        --slide: 0%;
    }
}


.topbar {
    position: absolute;
    top: 1.6rem;
    left: 1.6rem;
    right: 1.6rem;
    z-index: 12;
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

/* The screen switch rides the floating bar — glass like its siblings. */
.topbar-actions :deep(.view-toggle) {
    background-color: var(--surface-glass);
    backdrop-filter: var(--glass-backdrop);
}

.burger {
    display: none;
    align-items: center;
    justify-content: center;
    padding: 0.9rem;
    border-radius: var(--radius-control);
    border: 1px solid var(--border-default);
    background-color: var(--surface-glass);
    backdrop-filter: var(--glass-backdrop);
    color: var(--control-text);
    cursor: pointer;
}

.burger:hover,
.burger.active {
    color: var(--text-primary);
    /* The hover tint is painted OVER the button's own surface: an IDE's
       hover color is often translucent, and as a plain background it let
       the canvas show through and the button all but vanished. */
    background-image: linear-gradient(var(--node-hover-surface), var(--node-hover-surface));
}

.burger-icon {
    display: block;
    width: 1.6rem;
    height: 1.6rem;
}

@media (width <= 1100px) {
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
@media (width <= 600px) {
    .topbar {
        display: flex;
        justify-content: space-between;
    }

    .topbar-search {
        display: none;
    }
}

@media (width <= 470px) {
    .brand .brand-chip,
    .brand .live {
        display: none;
    }
}

@media (width <= 345px) {
    .brand .switcher-root {
        display: none;
    }
}

@media (width <= 450px) {
    .topbar {
        left: 0.8rem;
        right: 0.8rem;
        gap: 0;
    }

    .topbar-actions {
        gap: 0.4rem;
    }

    .topbar-actions :deep(.segment) {
        padding: 0.35rem 0.7rem;
    }
}

/* Phone-narrow (300px pane): the view switch, burger and gear ARE the bar. */
@media (width <= 360px) {
    .brand {
        display: none;
    }
}

@media (width <= 300px) {
    .topbar-actions :deep(.segment) {
        padding: 0.35rem 0.55rem;
    }
}

/* Sliver panes (≤ 250px): the view switch alone outgrows the bar, so it
   drops onto a second row under the burger and the gear — both stay pinned
   to the edge, and the gear stays the reachable path to Settings.
   (SidePanel lowers its top for the same width.) */
@media (width <= 250px) {
    .topbar-actions {
        flex-wrap: wrap;
        justify-content: flex-end;
    }

    .topbar-actions > :first-child {
        order: 1;
    }
}

/* The one primary action in the bar — filled, unlike its ghost siblings. */
.new-node {
    padding: 0.6rem 1.2rem;
    border-radius: var(--radius-control);
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
    box-shadow: var(--shadow-md);
}

/* IDE skins: the chip is the box, in the theme accent like the buttons on
   this row; the "E" (the bars) is black at 0.8 so a little accent shows
   through, the "A" (the face) is white. */
.brand-chip.boxed {
    background: var(--interactive-primary);
    color: #0a0a14;
    --mark-face: #fff;
    --mark-bars-opacity: 0.8;
}

.brand-icon {
    display: block;
    width: 3.2rem;
    height: 3.2rem;
    border-radius: var(--radius-md);
}

.brand-mark {
    width: 2.4rem;
    height: 2.4rem;
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
