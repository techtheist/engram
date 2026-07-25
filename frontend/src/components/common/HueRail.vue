<script setup lang="ts">
import { computed } from 'vue'

/**
 * The one color input a type has (PLAN §7D: hue IS the persistence): a full
 * spectrum rail with a thumb filled by the chosen hue. Native range styling
 * is theme-blind, so the track and thumb are drawn from scratch.
 */
const props = defineProps<{
    modelValue: number
    ariaLabel?: string
}>()

const emit = defineEmits<{ (e: 'update:modelValue', v: number): void }>()

const thumb = computed(() => `hsl(${props.modelValue} 84% 60%)`)

function onInput(e: Event): void {
    emit('update:modelValue', Number((e.target as HTMLInputElement).value))
}
</script>

<template>
<div class="hue-rail">
    <input
        class="rail"
        type="range"
        min="0"
        max="359"
        :value="modelValue"
        :aria-label="ariaLabel"
        :style="{ '--thumb': thumb }"
        @input="onInput"
    />
    <span class="degrees">{{ modelValue }}°</span>
</div>
</template>

<style scoped>
.hue-rail {
    display: flex;
    flex: 1;
    align-items: center;
    gap: 0.8rem;
    min-width: 0;
}

.rail {
    --track: linear-gradient(
        90deg,
        hsl(0deg 84% 60%),
        hsl(60deg 84% 60%),
        hsl(120deg 84% 60%),
        hsl(180deg 84% 60%),
        hsl(240deg 84% 60%),
        hsl(300deg 84% 60%),
        hsl(359deg 84% 60%)
    );

    flex: 1;
    min-width: 0;
    height: 2rem;
    margin: 0;
    appearance: none;
    background: transparent;
    cursor: pointer;
}

.rail::-webkit-slider-runnable-track {
    height: 0.6rem;
    border-radius: var(--radius-full);
    background: var(--track);
}

.rail::-webkit-slider-thumb {
    appearance: none;
    width: 1.8rem;
    height: 1.8rem;
    margin-top: -0.6rem;
    border: 0.3rem solid var(--surface-elevated);
    border-radius: 50%;
    background: var(--thumb);
    box-shadow: 0 0 0 1px var(--border-strong), var(--shadow-sm);
}

.rail::-moz-range-track {
    height: 0.6rem;
    border-radius: var(--radius-full);
    background: var(--track);
}

.rail::-moz-range-thumb {
    width: 1.2rem;
    height: 1.2rem;
    border: 0.3rem solid var(--surface-elevated);
    border-radius: 50%;
    background: var(--thumb);
    box-shadow: 0 0 0 1px var(--border-strong), var(--shadow-sm);
}

.rail:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 2px;
    border-radius: var(--radius-full);
}

.degrees {
    min-width: 3.4rem;
    text-align: right;
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-secondary);
}
</style>
