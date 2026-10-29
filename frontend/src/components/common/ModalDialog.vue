<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, useTemplateRef } from 'vue'

/**
 * The pane's modal shell (0.10.3): a centered panel over a scrim, teleported
 * to <body> so it escapes the inert, transformed screens and side panels.
 * It replaces window.confirm, which IDE webviews sandbox into a silent
 * `false` (issue #14). Esc or a scrim click closes it (unless `locked`), Tab
 * stays inside, focus starts on the `data-autofocus` element (else the first
 * control) and returns where it was on close. Keys never leak to the screen
 * underneath — the feed's j/k and arrows listen on window.
 */
const props = withDefaults(
    defineProps<{
        title: string
        /** Danger tints the title rule — for destructive questions. */
        tone?: 'default' | 'danger'
        /** Ignore Esc and scrim clicks (while a request is in flight). */
        locked?: boolean
        width?: string
    }>(),
    { tone: 'default', locked: false, width: '46rem' },
)
const emit = defineEmits<{ close: [] }>()

const panel = useTemplateRef<HTMLElement>('panel')
let returnFocus: HTMLElement | null = null

const FOCUSABLE =
    'button:not([disabled]), [href], input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])'

function focusables(): HTMLElement[] {
    return panel.value ? [...panel.value.querySelectorAll<HTMLElement>(FOCUSABLE)] : []
}

function dismiss(): void {
    if (!props.locked) emit('close')
}

function onKey(e: KeyboardEvent): void {
    e.stopPropagation()
    if (e.key === 'Escape') {
        e.preventDefault()
        dismiss()
        return
    }
    if (e.key !== 'Tab') return
    const items = focusables()
    if (!items.length) return
    const first = items[0]!
    const last = items[items.length - 1]!
    if (e.shiftKey && document.activeElement === first) {
        e.preventDefault()
        last.focus()
    } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault()
        first.focus()
    }
}

onMounted(async () => {
    returnFocus = document.activeElement as HTMLElement | null
    await nextTick()
    const start = panel.value?.querySelector<HTMLElement>('[data-autofocus]') ?? focusables()[0]
    ;(start ?? panel.value)?.focus()
})

onBeforeUnmount(() => {
    if (returnFocus?.isConnected) returnFocus.focus()
})
</script>

<template>
<Teleport to="body">
    <Transition name="modal" appear>
        <div class="modal-scrim" @click.self="dismiss" @keydown="onKey">
            <section
                ref="panel"
                class="modal glass-panel"
                :class="`modal--${tone}`"
                role="dialog"
                aria-modal="true"
                :aria-label="title"
                tabindex="-1"
                :style="{ width: `min(${width}, calc(100vw - 3.2rem))` }"
            >
                <h2 class="modal-title">{{ title }}</h2>
                <div class="modal-body">
                    <slot />
                </div>
                <footer v-if="$slots.actions" class="modal-actions">
                    <slot name="actions" />
                </footer>
            </section>
        </div>
    </Transition>
</Teleport>
</template>

<style scoped>
.modal-scrim {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 1.6rem;
    background-color: var(--surface-overlay);
}

.modal {
    display: flex;
    flex-direction: column;
    gap: 1.4rem;
    max-height: calc(100vh - 3.2rem);
    padding: 2rem 2.2rem 1.8rem;
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-lg);
    outline: none;
}

.modal-title {
    margin: 0;
    font-size: var(--text-body);
    font-weight: 600;
    color: var(--text-primary);
}

.modal--danger .modal-title {
    color: var(--node-problem);
}

.modal-body {
    display: flex;
    flex-direction: column;
    gap: 1.2rem;
    min-height: 0;
    overflow-y: auto;
}

.modal-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 0.8rem;
}

.modal-enter-active,
.modal-leave-active {
    transition: opacity 0.16s ease;
}

.modal-enter-active .modal,
.modal-leave-active .modal {
    transition: transform 0.18s cubic-bezier(0.2, 0.8, 0.2, 1);
}

.modal-enter-from,
.modal-leave-to {
    opacity: 0;
}

.modal-enter-from .modal,
.modal-leave-to .modal {
    transform: translateY(0.8rem) scale(0.98);
}

@media (prefers-reduced-motion: reduce) {
    .modal-enter-active,
    .modal-leave-active,
    .modal-enter-active .modal,
    .modal-leave-active .modal {
        transition: none;
    }
}
</style>
