<script setup lang="ts">
import { nextTick, ref } from 'vue'

/**
 * The canvas controls (zoom in / out, fit, heat, lock) — Vue Flow's Controls,
 * rebuilt as plain HTML over the WebGL canvas so they keep their glass:
 * a real backdrop-filter over the canvas pixels in the brand theme. Resting
 * the pointer on a button (or holding it on touch) explains it in a popup
 * beside the bar — centered on the button, but lifted clear of the health
 * strip, which shares the bottom edge and grows when its stats wrap.
 */
const props = defineProps<{ locked: boolean; canZoomIn: boolean; canZoomOut: boolean; heat?: boolean }>()
const emit = defineEmits<{ zoomIn: []; zoomOut: []; fit: []; toggleLock: []; toggleHeat: [] }>()

type Key = 'zoomIn' | 'zoomOut' | 'fit' | 'heat' | 'lock'

function hintOf(key: Key): { title: string; text: string } {
    switch (key) {
        case 'zoomIn':
            return { title: 'Zoom in', text: 'Step in towards the whole graph.' }
        case 'zoomOut':
            return { title: 'Zoom out', text: 'Step back toward the whole graph.' }
        case 'fit':
            return { title: 'Fit view', text: 'Frame every visible note.' }
        case 'heat':
            return {
                title: props.heat ? 'Crowding heat — on' : 'Crowding heat',
                text: 'Glows where notes crowd each other in vector space: yellow has a few close neighbours, red a pile on one subject. Checkup lists the piles to consolidate.',
            }
        case 'lock':
            return {
                title: props.locked ? 'Unlock' : 'Lock',
                text: props.locked ? 'Cards can be dragged and linked again.' : 'Keep cards in place: no dragging or linking; panning and zooming still work.',
            }
    }
}

const DWELL_MS = 450
/** Clearance above the health strip, px. */
const STRIP_GAP = 8
const hint = ref<{ key: Key; top: number } | null>(null)
const hintEl = ref<HTMLElement | null>(null)
const wrap = ref<HTMLElement | null>(null)
let timer = 0

/** Center the popup on the button, then lift it above the health strip if they meet. */
async function place(btn: HTMLElement): Promise<void> {
    await nextTick()
    const el = hintEl.value
    const w = wrap.value
    if (!el || !w || !hint.value) return
    const h = el.offsetHeight
    let top = btn.offsetTop + btn.offsetHeight / 2 - h / 2
    const strip = document.querySelector<HTMLElement>('.health')
    if (strip) {
        const limit = strip.getBoundingClientRect().top - w.getBoundingClientRect().top - STRIP_GAP
        top = Math.min(top, limit - h)
    }
    hint.value = { ...hint.value, top }
}

function arm(key: Key, e: PointerEvent): void {
    window.clearTimeout(timer)
    const btn = e.currentTarget as HTMLElement
    const show = (): void => {
        hint.value = { key, top: btn.offsetTop + btn.offsetHeight / 2 }
        void place(btn)
    }
    // Moving along the bar with a popup open swaps it at once.
    if (hint.value) show()
    else timer = window.setTimeout(show, DWELL_MS)
}

function disarm(): void {
    window.clearTimeout(timer)
    hint.value = null
}

/** A touch hold shows the popup without the click firing a second action. */
function onTouchHold(key: Key, e: PointerEvent): void {
    if (e.pointerType !== 'mouse') arm(key, e)
}
</script>

<template>
<div ref="wrap" class="gl-controls-wrap gl-chrome" @pointerleave="disarm">
    <div class="gl-controls" role="toolbar" aria-label="Canvas controls">
        <button
            type="button"
            class="gl-controls-button"
            aria-label="zoom in"
            :disabled="!canZoomIn"
            @pointerenter="arm('zoomIn', $event)"
            @pointerdown="onTouchHold('zoomIn', $event)"
            @click="emit('zoomIn')"
        >
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><path d="M32 18.133H18.133V32h-4.266V18.133H0v-4.266h13.867V0h4.266v13.867H32z" /></svg>
        </button>
        <button
            type="button"
            class="gl-controls-button"
            aria-label="zoom out"
            :disabled="!canZoomOut"
            @pointerenter="arm('zoomOut', $event)"
            @pointerdown="onTouchHold('zoomOut', $event)"
            @click="emit('zoomOut')"
        >
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 5"><path d="M0 0h32v4.2H0z" /></svg>
        </button>
        <button
            type="button"
            class="gl-controls-button"
            aria-label="fit view"
            @pointerenter="arm('fit', $event)"
            @pointerdown="onTouchHold('fit', $event)"
            @click="emit('fit')"
        >
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 30"><path d="M3.692 4.63c0-.53.4-.938.939-.938h5.215V0H4.708C2.13 0 0 2.054 0 4.63v5.216h3.692V4.631zM27.354 0h-5.2v3.692h5.17c.53 0 .984.4.984.939v5.215H32V4.631A4.624 4.624 0 0 0 27.354 0zm.954 24.83c0 .532-.4.94-.939.94h-5.215v3.768h5.215c2.577 0 4.631-2.13 4.631-4.707v-5.139h-3.692v5.139zm-23.677.94a.919.919 0 0 1-.939-.94v-5.138H0v5.139c0 2.577 2.13 4.707 4.708 4.707h5.138V25.77H4.631z" /></svg>
        </button>
        <button
            type="button"
            class="gl-controls-button"
            :class="{ on: heat }"
            aria-label="toggle crowding heat"
            :aria-pressed="heat ? 'true' : 'false'"
            @pointerenter="arm('heat', $event)"
            @pointerdown="onTouchHold('heat', $event)"
            @click="emit('toggleHeat')"
        >
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 32"><path d="M13.5 0S16 6.5 11 11.5C7.6 14.9 4 18.4 4 23.2 4 28.1 7.6 32 12 32s8-3.9 8-8.8c0-3.5-1.7-6.2-3.4-8.2.1 2.6-1 4.6-2.8 5.4.9-4.6-.1-9.4-1.3-12.1C11.4 4.6 13.5 0 13.5 0z" /></svg>
        </button>
        <button
            type="button"
            class="gl-controls-button"
            aria-label="toggle interactivity"
            :aria-pressed="locked ? 'true' : 'false'"
            @pointerenter="arm('lock', $event)"
            @pointerdown="onTouchHold('lock', $event)"
            @click="emit('toggleLock')"
        >
            <svg v-if="locked" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 25 32"><path d="M21.333 10.667H19.81V7.619C19.81 3.429 16.38 0 12.19 0 8 0 4.571 3.429 4.571 7.619v3.048H3.048A3.056 3.056 0 0 0 0 13.714v15.238A3.056 3.056 0 0 0 3.048 32h18.285a3.056 3.056 0 0 0 3.048-3.048V13.714a3.056 3.056 0 0 0-3.048-3.047zM12.19 24.533a3.056 3.056 0 0 1-3.047-3.047 3.056 3.056 0 0 1 3.047-3.048 3.056 3.056 0 0 1 3.048 3.048 3.056 3.056 0 0 1-3.048 3.047zm4.724-13.866H7.467V7.619c0-2.59 2.133-4.724 4.723-4.724 2.591 0 4.724 2.133 4.724 4.724v3.048z" /></svg>
            <svg v-else xmlns="http://www.w3.org/2000/svg" viewBox="0 0 25 32"><path d="M21.333 10.667H19.81V7.619C19.81 3.429 16.38 0 12.19 0c-4.114 1.828-1.37 2.133.305 2.438 1.676.305 4.42 2.59 4.42 5.181v3.048H3.047A3.056 3.056 0 0 0 0 13.714v15.238A3.056 3.056 0 0 0 3.048 32h18.285a3.056 3.056 0 0 0 3.048-3.048V13.714a3.056 3.056 0 0 0-3.048-3.047zM12.19 24.533a3.056 3.056 0 0 1-3.047-3.047 3.056 3.056 0 0 1 3.047-3.048 3.056 3.056 0 0 1 3.048 3.048 3.056 3.056 0 0 1-3.048 3.047z" /></svg>
        </button>
    </div>
    <div v-if="hint" ref="hintEl" class="gl-controls-hint" role="tooltip" :style="{ top: `${hint.top}px` }">
        <strong>{{ hintOf(hint.key).title }}</strong>
        <span>{{ hintOf(hint.key).text }}</span>
    </div>
</div>
</template>

<style scoped>
.gl-controls-wrap {
    position: absolute;
    bottom: 0;
    left: 0;
    z-index: 5;
    margin: 15px;
}

.gl-controls {
    overflow: hidden;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-lg);
    background-color: var(--surface-glass);
    backdrop-filter: var(--glass-backdrop);
    box-shadow: var(--shadow-md);
}

.gl-controls-hint {
    position: absolute;
    left: calc(100% + 0.8rem);
    display: flex;
    width: max-content;
    max-width: 26rem;
    flex-direction: column;
    gap: 0.2rem;
    padding: 0.6rem 0.9rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md);
    background-color: var(--surface-glass);
    backdrop-filter: var(--glass-backdrop);
    box-shadow: var(--shadow-md);
    color: var(--text-secondary);
    font-size: var(--text-caption);
    line-height: 1.4;
    pointer-events: none;
}

.gl-controls-hint strong {
    color: var(--text-primary);
    font-weight: 600;
}

.gl-controls-button {
    display: flex;
    box-sizing: content-box;
    width: 16px;
    height: 16px;
    align-items: center;
    justify-content: center;
    padding: 5px;
    border: none;
    border-bottom: 1px solid var(--border-subtle);
    background-color: transparent;
    fill: var(--text-secondary);
    cursor: pointer;
    user-select: none;
}

.gl-controls-button svg {
    width: 100%;
    max-width: 12px;
    max-height: 12px;
}

.gl-controls-button:last-child {
    border-bottom: none;
}

.gl-controls-button:hover {
    background-color: var(--interactive-ghost-hover);
    fill: var(--text-primary);
}

.gl-controls-button.on {
    fill: rgb(249 115 22);
}

.gl-controls-button.on:hover {
    fill: rgb(220 38 38);
}

.gl-controls-button:disabled {
    cursor: default;
}

.gl-controls-button:disabled svg {
    fill-opacity: 0.4;
}
</style>
