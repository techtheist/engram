<script setup lang="ts">
/**
 * One-of-N as a styled trigger + a floating listbox. Replaces every native
 * <select> in the pane: the native popup is unstyleable and looks foreign on
 * Windows, so the list is our own — teleported to <body> and positioned
 * from the trigger's rect (side panels clip overflow), flipping upward when
 * the space below runs out. Keyboard: arrows move, Enter/Space pick, Escape
 * closes, Home/End jump. Wide lists (SegmentedControl territory) stay where
 * they are; this is for lists that don't fit on one line.
 */
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'

export interface SelectOption {
    value: string
    label: string
    /** Optional text color for the option and the trigger while selected. */
    color?: string
}

defineOptions({ inheritAttrs: false })

const props = withDefaults(
    defineProps<{
        modelValue: string
        options: SelectOption[]
        ariaLabel?: string
        id?: string
        disabled?: boolean
        placeholder?: string
        /** `sm` is the inline (connection verb) size; `md` the form size. */
        size?: 'sm' | 'md'
        /** Trigger text color (the connection list tints by verb). */
        color?: string
        /** Fill the container's width (form fields). */
        block?: boolean
    }>(),
    { size: 'md', block: false, disabled: false, placeholder: '—', ariaLabel: undefined, id: undefined, color: undefined },
)

const emit = defineEmits<{ (e: 'update:modelValue', v: string): void }>()

const trigger = ref<HTMLButtonElement | null>(null)
const list = ref<HTMLUListElement | null>(null)
const open = ref(false)
const active = ref(0)
const menuStyle = ref<Record<string, string>>({})

const selected = computed(() => props.options.find((o) => o.value === props.modelValue))
const triggerColor = computed(() => props.color ?? selected.value?.color)

const listId = `select-menu-${Math.random().toString(36).slice(2, 9)}`

function place(): void {
    const el = trigger.value
    if (!el) return
    const r = el.getBoundingClientRect()
    const gap = 4
    const maxH = 280
    const below = window.innerHeight - r.bottom - gap
    const above = r.top - gap
    const flip = below < Math.min(maxH, 160) && above > below
    const height = Math.min(maxH, flip ? above : below)
    const style: Record<string, string> = {
        left: `${Math.max(4, Math.min(r.left, window.innerWidth - r.width - 4))}px`,
        minWidth: `${r.width}px`,
        maxHeight: `${Math.max(96, height)}px`,
    }
    if (flip) style.bottom = `${window.innerHeight - r.top + gap}px`
    else style.top = `${r.bottom + gap}px`
    menuStyle.value = style
}

async function show(): Promise<void> {
    if (props.disabled || open.value) return
    const idx = props.options.findIndex((o) => o.value === props.modelValue)
    active.value = idx < 0 ? 0 : idx
    place()
    open.value = true
    await nextTick()
    list.value?.focus()
    scrollActiveIntoView()
}

function hide(refocus = false): void {
    if (!open.value) return
    open.value = false
    if (refocus) trigger.value?.focus()
}

function pick(value: string): void {
    if (value !== props.modelValue) emit('update:modelValue', value)
    hide(true)
}

function move(delta: number): void {
    const n = props.options.length
    if (n === 0) return
    active.value = (active.value + delta + n) % n
    scrollActiveIntoView()
}

function scrollActiveIntoView(): void {
    const el = list.value?.children[active.value] as HTMLElement | undefined
    el?.scrollIntoView({ block: 'nearest' })
}

function onListKey(e: KeyboardEvent): void {
    switch (e.key) {
        case 'ArrowDown':
            move(1)
            break
        case 'ArrowUp':
            move(-1)
            break
        case 'Home':
            active.value = 0
            scrollActiveIntoView()
            break
        case 'End':
            active.value = Math.max(0, props.options.length - 1)
            scrollActiveIntoView()
            break
        case 'Enter':
        case ' ': {
            const o = props.options[active.value]
            if (o) pick(o.value)
            break
        }
        case 'Escape':
        case 'Tab':
            hide(e.key === 'Escape')
            return
        default:
            return
    }
    e.preventDefault()
}

function onTriggerKey(e: KeyboardEvent): void {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp' || e.key === 'Enter' || e.key === ' ') {
        e.preventDefault()
        void show()
    }
}

function onDocumentPointer(e: PointerEvent): void {
    const t = e.target as Node | null
    if (t && (trigger.value?.contains(t) || list.value?.contains(t))) return
    hide()
}

function onViewportChange(): void {
    hide()
}

watch(open, (is) => {
    if (is) {
        document.addEventListener('pointerdown', onDocumentPointer, true)
        window.addEventListener('resize', onViewportChange)
        window.addEventListener('scroll', onViewportChange, true)
    } else {
        document.removeEventListener('pointerdown', onDocumentPointer, true)
        window.removeEventListener('resize', onViewportChange)
        window.removeEventListener('scroll', onViewportChange, true)
    }
})

onBeforeUnmount(() => {
    open.value = false
    document.removeEventListener('pointerdown', onDocumentPointer, true)
    window.removeEventListener('resize', onViewportChange)
    window.removeEventListener('scroll', onViewportChange, true)
})
</script>

<template>
<button
    v-bind="$attrs"
    :id="id"
    ref="trigger"
    class="select-trigger"
    :class="[size, { block, open, placeholder: !selected }]"
    type="button"
    role="combobox"
    :aria-label="ariaLabel"
    :aria-expanded="open"
    :aria-controls="listId"
    aria-haspopup="listbox"
    :disabled="disabled"
    :style="triggerColor ? { color: triggerColor } : undefined"
    @click="open ? hide() : show()"
    @keydown="onTriggerKey"
>
    <span class="select-value">{{ selected?.label ?? placeholder }}</span>
    <svg class="select-chevron" viewBox="0 0 12 12" aria-hidden="true">
        <path d="M2.5 4.5 6 8l3.5-3.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
    </svg>
</button>
<Teleport to="body">
    <ul
        v-if="open"
        :id="listId"
        ref="list"
        class="select-menu"
        :class="size"
        role="listbox"
        tabindex="-1"
        :aria-label="ariaLabel"
        :aria-activedescendant="`${listId}-${active}`"
        :style="menuStyle"
        @keydown="onListKey"
    >
        <li
            v-for="(o, i) in options"
            :id="`${listId}-${i}`"
            :key="o.value"
            class="select-option"
            :class="{ active: i === active, selected: o.value === modelValue }"
            role="option"
            :aria-selected="o.value === modelValue"
            :style="o.color ? { color: o.color } : undefined"
            @pointermove="active = i"
            @click="pick(o.value)"
        >
            <span class="option-label">{{ o.label }}</span>
            <svg v-if="o.value === modelValue" class="option-check" viewBox="0 0 12 12" aria-hidden="true">
                <path d="M2.5 6.5 5 9l4.5-6" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
            </svg>
        </li>
        <li v-if="!options.length" class="select-empty">nothing to choose</li>
    </ul>
</Teleport>
</template>

<style scoped>
.select-trigger {
    display: inline-flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    min-width: 0;
    max-width: 100%;
    padding: 0.5rem 0.7rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md);
    background: var(--surface-sunken);
    color: var(--text-primary);
    font: inherit;
    font-size: var(--text-body-sm);
    text-align: left;
    cursor: pointer;
    transition:
        border-color 120ms ease,
        background-color 120ms ease;
}

.select-trigger.block {
    display: flex;
    width: 100%;
}

.select-trigger.sm {
    padding: 0.3rem 0.45rem;
    border-color: var(--border-subtle);
    border-radius: var(--radius-sm);
    background: transparent;
    font-size: var(--text-caption);
    font-weight: 600;
}

.select-trigger:focus-visible {
    outline: 2px solid var(--interactive-primary);
    outline-offset: 1px;
}

.select-trigger:disabled {
    opacity: 0.55;
    cursor: default;
}

.select-trigger:hover:not(:disabled),
.select-trigger.open {
    border-color: var(--interactive-primary);
}

.select-value {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.select-trigger.placeholder .select-value {
    color: var(--text-tertiary);
}

.select-chevron {
    width: 0.75rem;
    height: 0.75rem;
    flex: none;
    color: var(--text-tertiary);
    transition: transform 120ms ease;
}

.select-trigger.open .select-chevron {
    transform: rotate(180deg);
}

.select-menu {
    position: fixed;
    z-index: 1000;
    margin: 0;
    padding: 0.3rem;
    list-style: none;
    overflow-y: auto;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md);
    background: var(--surface-elevated);
    box-shadow: var(--shadow-md);
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    outline: none;
}

.select-menu.sm {
    font-size: var(--text-caption);
    font-weight: 600;
}

.select-option {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.4rem 0.6rem;
    border-radius: calc(var(--radius-md) - 0.2rem);
    cursor: pointer;
    white-space: nowrap;
}

.select-option.active {
    background: var(--interactive-ghost-hover);
}

.select-option.selected {
    font-weight: 600;
}

.option-label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
}

.option-check {
    width: 0.75rem;
    height: 0.75rem;
    flex: none;
    color: var(--interactive-primary);
}

.select-empty {
    padding: 0.4rem 0.6rem;
    color: var(--text-tertiary);
}
</style>
