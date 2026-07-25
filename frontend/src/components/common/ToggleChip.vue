<script setup lang="ts">
/**
 * A boolean as a small filled chip — on = filled with the context accent
 * (--check-accent, falling back to the theme primary), off = quiet outline.
 * Replaces bare checkboxes wherever the option is a *role* rather than a
 * form field (type/verb roles, brief sections).
 */
defineProps<{
    modelValue: boolean
    label: string
    title?: string
    disabled?: boolean
}>()

const emit = defineEmits<{ (e: 'update:modelValue', v: boolean): void }>()
</script>

<template>
<button
    class="chip"
    type="button"
    :class="{ on: modelValue }"
    :title="title"
    :disabled="disabled"
    :aria-pressed="modelValue"
    @click="emit('update:modelValue', !modelValue)"
>
    {{ label }}
</button>
</template>

<style scoped>
.chip {
    padding: 0.45rem 1.1rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md);
    background: var(--surface-sunken);
    color: var(--text-secondary);
    font-size: var(--text-caption);
    font-weight: 600;
    white-space: nowrap;
    cursor: pointer;
    transition:
        background-color 120ms ease,
        border-color 120ms ease,
        color 120ms ease;
}

.chip:disabled {
    opacity: 0.5;
    cursor: default;
}

.chip:hover:not(:disabled) {
    color: var(--text-primary);
    border-color: var(--check-accent, var(--interactive-primary));
}

.chip.on {
    background: var(--check-accent, var(--interactive-primary));
    border-color: var(--check-accent, var(--interactive-primary));
    color: var(--text-inverse);
}
</style>
