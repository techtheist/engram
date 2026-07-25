<script setup lang="ts">
import { computed, ref, watch } from 'vue'

/**
 * A number as "− value +": type into the middle or nudge by step. Replaces
 * native number inputs (invisible spinners, theme-blind) everywhere a policy
 * knob or cap is edited. Commits clamped values on blur/Enter; −/+ commit
 * immediately.
 */
const props = withDefaults(
    defineProps<{
        modelValue: number
        min?: number
        max?: number
        step?: number
        /** Fraction digits shown; defaults from the step (0.05 → 2, 1 → 0). */
        decimals?: number
        ariaLabel?: string
    }>(),
    { min: 0, max: Number.MAX_SAFE_INTEGER, step: 1, decimals: undefined, ariaLabel: undefined },
)

const emit = defineEmits<{ (e: 'update:modelValue', v: number): void }>()

const digits = computed(() => props.decimals ?? (props.step < 1 ? 2 : 0))

const fmt = (v: number): string => v.toFixed(digits.value)

const text = ref(fmt(props.modelValue))
watch(
    () => props.modelValue,
    (v) => (text.value = fmt(v)),
)

function commit(raw: number): void {
    if (Number.isNaN(raw)) {
        text.value = fmt(props.modelValue)
        return
    }
    const clamped = Math.min(props.max, Math.max(props.min, raw))
    const rounded = Number(clamped.toFixed(digits.value))
    emit('update:modelValue', rounded)
    text.value = fmt(rounded)
}

const nudge = (dir: 1 | -1) => commit(props.modelValue + dir * props.step)
</script>

<template>
<div class="stepper">
    <button
        class="nudge"
        type="button"
        :aria-label="`decrease ${ariaLabel ?? 'value'}`"
        :disabled="modelValue <= min"
        @click="nudge(-1)"
    >
        −
    </button>
    <input
        v-model="text"
        class="value"
        type="text"
        inputmode="decimal"
        :aria-label="ariaLabel"
        :style="{ width: `calc(${Math.max(text.length, 2)}ch + 0.6rem)` }"
        @blur="commit(Number.parseFloat(text))"
        @keydown.enter="($event.target as HTMLInputElement).blur()"
        @keydown.up.prevent="nudge(1)"
        @keydown.down.prevent="nudge(-1)"
    />
    <button
        class="nudge"
        type="button"
        :aria-label="`increase ${ariaLabel ?? 'value'}`"
        :disabled="modelValue >= max"
        @click="nudge(1)"
    >
        +
    </button>
</div>
</template>

<style scoped>
.stepper {
    display: inline-flex;
    align-items: center;
    gap: 0.1rem;
    padding: 0.15rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md);
    background: var(--surface-sunken);
}

.nudge {
    display: inline-grid;
    place-content: center;
    width: 2.2rem;
    height: 2.2rem;
    border: none;
    border-radius: calc(var(--radius-md) - 0.15rem);
    background: transparent;
    color: var(--text-tertiary);
    font-size: var(--text-body-sm);
    line-height: 1;
    cursor: pointer;
}

.nudge:disabled {
    opacity: 0.35;
    cursor: default;
}

.nudge:hover:not(:disabled) {
    color: var(--text-primary);
    background: var(--interactive-ghost-hover);
}

.value {
    min-width: 2ch;
    border: none;
    background: transparent;
    color: var(--text-primary);
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    text-align: center;
    outline: none;
}
</style>
