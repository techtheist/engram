<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { usePanels, type PanelSide } from '@/composables/usePanels'

/**
 * Shared shell for the edge drawers. It owns three behaviors the drawers used
 * to hand-roll: exclusive side occupancy (opening a drawer dismisses the one
 * already on that side), a drag-resizable width kept for the UI session, and
 * a two-layer scroll structure — the frame carries border/shadow/radius and
 * clips, the inner area scrolls with contained overscroll — so macOS elastic
 * bounce moves the content inside the frame instead of out from under it.
 */
const props = defineProps<{
    open: boolean
    side: PanelSide
    panelId: string
    /** Width in rem before the user ever drags the grip. */
    defaultRem: number
    /** Content floor in rem — wins over the half-screen cap on narrow panes. */
    minRem: number
    /**
     * Called when another drawer claims this side, and by the built-in
     * header's close button when `title` is used.
     */
    dismiss: () => void
    /** Inner-edge accent border color (the detail/create drawers). */
    accent?: string
    /**
     * Renders the standard drawer header (heading + close button, extra
     * controls via the `actions` slot). Drawers with a fully custom header
     * use the `header` slot instead.
     */
    title?: string
}>()

const { widths, claim, release } = usePanels()

watch(
    () => props.open,
    (open) => {
        if (open) claim(props.side, props.panelId, props.dismiss)
        else release(props.side, props.panelId)
    },
    { immediate: true },
)

onBeforeUnmount(() => release(props.side, props.panelId))

const width = computed(() => {
    const px = widths[props.panelId]
    return px == null ? `${props.defaultRem}rem` : `${px}px`
})

const resizing = ref(false)

function startResize(down: PointerEvent): void {
    down.preventDefault()
    const grip = down.currentTarget as HTMLElement
    grip.setPointerCapture(down.pointerId)
    resizing.value = true
    const rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 10
    const min = props.minRem * rem
    const onMove = (e: PointerEvent) => {
        const raw = props.side === 'left' ? e.clientX : window.innerWidth - e.clientX
        const half = Math.max(window.innerWidth / 2, min)
        widths[props.panelId] = Math.round(Math.min(Math.max(raw, min), half))
    }
    // pointercancel too: touch interruptions and OS gestures end a drag
    // without ever firing pointerup, and must not leave onMove attached.
    const stop = (e: PointerEvent) => {
        if (grip.hasPointerCapture(e.pointerId)) grip.releasePointerCapture(e.pointerId)
        grip.removeEventListener('pointermove', onMove)
        grip.removeEventListener('pointerup', stop)
        grip.removeEventListener('pointercancel', stop)
        resizing.value = false
    }
    grip.addEventListener('pointermove', onMove)
    grip.addEventListener('pointerup', stop)
    grip.addEventListener('pointercancel', stop)
}
</script>

<template>
<Transition :name="side === 'left' ? 'slide-left' : 'slide-right'">
    <aside
        v-if="open"
        class="side-panel glass-panel"
        :class="[side, { 'has-accent': accent }]"
        :style="{ '--panel-width': width, '--panel-min': `${minRem}rem`, '--panel-accent': accent }"
    >
        <div v-if="title || $slots.header" class="panel-head">
            <header v-if="title" class="head">
                <h2 class="heading">{{ title }}</h2>
                <div class="head-actions">
                    <slot name="actions" />
                    <button class="close" type="button" aria-label="Close" @click="dismiss">
                        ×
                    </button>
                </div>
            </header>
            <slot v-else name="header" />
        </div>
        <div class="scroll">
            <slot />
        </div>
        <div
            class="grip"
            :class="{ resizing }"
            title="Drag to resize"
            @pointerdown="startResize"
        />
    </aside>
</Transition>
</template>

<style scoped>
.side-panel {
    position: fixed;
    top: 6.4rem;
    bottom: 0;
    z-index: 11;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    width: min(var(--panel-width), 100vw);
    /* The content floor wins over the half-screen cap (min-width beats
       max-width in CSS), but never exceeds the pane itself. */
    min-width: min(var(--panel-min), 100vw);
    max-width: clamp(var(--panel-min), 50vw, 100vw);
    box-shadow: var(--shadow-lg);
}

.side-panel.right {
    right: 0;
    border-top-left-radius: var(--radius-xl);
    border-bottom-left-radius: var(--radius-xl);
}

.side-panel.left {
    left: 0;
    border-top-right-radius: var(--radius-xl);
    border-bottom-right-radius: var(--radius-xl);
}

.side-panel.right.has-accent {
    border-left: 3px solid var(--panel-accent);
}

.side-panel.left.has-accent {
    border-right: 3px solid var(--panel-accent);
}

/* The top bar lives outside the scroller, so close/actions stay reachable
 * however far the list is scrolled. */
.panel-head {
    flex: none;
    padding: 1.8rem 1.8rem 1.2rem;
    border-bottom: 1px solid var(--border-subtle);
}

.scroll {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: var(--panel-gap, 1.2rem);
    min-width: 0;
    min-height: 0;
    padding: 1.8rem;
    overflow-y: auto;
    /* Keep macOS elastic overscroll (and scroll chaining) inside the drawer. */
    overscroll-behavior: contain;
}

.panel-head + .scroll {
    padding-top: 1.2rem;
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

.head-actions {
    display: flex;
    align-items: center;
    gap: 0.8rem;
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

.grip {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1rem;
    cursor: col-resize;
    touch-action: none;
    user-select: none;
}

.side-panel.right .grip {
    left: 0;
}

.side-panel.left .grip {
    right: 0;
}

.grip::after {
    content: '';
    position: absolute;
    top: 0;
    bottom: 0;
    width: 3px;
    background-color: var(--interactive-primary);
    opacity: 0;
    transition: opacity var(--duration-fast) var(--ease-default);
}

.grip:hover::after,
.grip.resizing::after {
    opacity: 0.6;
}

.side-panel.right .grip::after {
    left: 0;
}

.side-panel.left .grip::after {
    right: 0;
}

.slide-left-enter-active,
.slide-left-leave-active,
.slide-right-enter-active,
.slide-right-leave-active {
    transition:
        transform var(--duration-normal) var(--ease-default),
        opacity var(--duration-normal) var(--ease-default);
}

.slide-left-enter-from,
.slide-left-leave-to {
    transform: translateX(-100%);
    opacity: 0;
}

.slide-right-enter-from,
.slide-right-leave-to {
    transform: translateX(100%);
    opacity: 0;
}
</style>
