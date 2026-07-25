<script setup lang="ts">
/**
 * One-of-N as adjacent segments in a sunken rail; the active segment fills
 * with the context accent (--check-accent → theme primary). Replaces small
 * <select>s whose whole option list fits on one line (durability, variants).
 */
defineProps<{
    modelValue: string
    options: { value: string; label: string }[]
    ariaLabel?: string
    disabled?: boolean
}>()

const emit = defineEmits<{ (e: 'update:modelValue', v: string): void }>()
</script>

<template>
<div class="segments" role="radiogroup" :aria-label="ariaLabel">
    <button
        v-for="o in options"
        :key="o.value"
        class="segment"
        type="button"
        role="radio"
        :aria-checked="o.value === modelValue"
        :class="{ on: o.value === modelValue }"
        :disabled="disabled"
        @click="emit('update:modelValue', o.value)"
    >
        {{ o.label }}
    </button>
</div>
</template>

<style scoped>
.segments {
    display: inline-flex;
    gap: 0.2rem;
    padding: 0.25rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md);
    background: var(--surface-sunken);
}

.segment {
    padding: 0.35rem 1rem;
    border: none;
    border-radius: calc(var(--radius-md) - 0.25rem);
    background: transparent;
    color: var(--text-secondary);
    font-size: var(--text-caption);
    font-weight: 600;
    white-space: nowrap;
    cursor: pointer;
    transition:
        background-color 120ms ease,
        color 120ms ease;
}

.segment:disabled {
    opacity: 0.5;
    cursor: default;
}

.segment:hover:not(.on, :disabled) {
    color: var(--text-primary);
    background: var(--interactive-ghost-hover);
}

.segment.on {
    background: var(--check-accent, var(--interactive-primary));
    color: var(--text-inverse);
}
</style>
