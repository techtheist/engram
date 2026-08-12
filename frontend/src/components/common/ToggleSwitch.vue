<script setup lang="ts">
/**
 * A boolean as a real switch — track + sliding knob, label alongside. For
 * the knobs that gate a whole GROUP of settings (record sessions, history
 * in search): a filled chip reads as "a flat button", a switch reads as
 * "this turns something on". ToggleChip stays for role-style multi-selects.
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
    class="switch"
    type="button"
    role="switch"
    :class="{ on: modelValue }"
    :title="title"
    :disabled="disabled"
    :aria-checked="modelValue"
    @click="emit('update:modelValue', !modelValue)"
>
    <span class="track" aria-hidden="true"><span class="knob" /></span>
    <span class="label">{{ label }}</span>
</button>
</template>

<style scoped>
.switch {
    display: inline-flex;
    gap: 0.55rem;
    align-items: center;
    padding: 0.2rem 0;
    border: none;
    background: transparent;
    color: var(--text-secondary);
    font-size: var(--text-caption);
    font-weight: 600;
    white-space: nowrap;
    cursor: pointer;
}

.switch:disabled {
    opacity: 0.5;
    cursor: default;
}

.switch.on {
    color: var(--text-primary);
}

.track {
    position: relative;
    flex: none;
    width: 2rem;
    height: 1.15rem;
    border: 1px solid var(--border-default);
    border-radius: 999px;
    background: var(--surface-sunken);
    transition:
        background-color 140ms ease,
        border-color 140ms ease;
}

.switch.on .track {
    border-color: var(--check-accent, var(--interactive-primary));
    background: var(--check-accent, var(--interactive-primary));
}

.knob {
    position: absolute;
    top: 50%;
    left: 2px;
    width: 0.8rem;
    height: 0.8rem;
    border-radius: 50%;
    background: var(--text-secondary);
    transition:
        left 140ms ease,
        background-color 140ms ease;
    transform: translateY(-50%);
}

.switch.on .knob {
    left: calc(100% - 0.8rem - 2px);
    background: var(--text-inverse);
}

.switch:hover:not(:disabled) .track {
    border-color: var(--check-accent, var(--interactive-primary));
}
</style>
