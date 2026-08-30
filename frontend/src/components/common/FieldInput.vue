<script setup lang="ts">
// One typed input for a custom field value (0.9.0): the control follows the
// definition's kind. An empty/unset value emits `undefined` so callers can
// distinguish "cleared" from a real value.
import { computed } from 'vue'
import type { FieldDef } from '@/types/graph'

const props = defineProps<{ def: FieldDef; modelValue: unknown }>()
const emit = defineEmits<{ (e: 'update:modelValue', v: unknown): void }>()

const label = computed(() => (props.def.label !== '' ? props.def.label : props.def.name))

const text = computed(() => {
    const v = props.modelValue
    if (v == null) return ''
    return typeof v === 'string' ? v : String(v)
})

function onText(e: Event): void {
    const raw = (e.target as HTMLInputElement).value.trim()
    emit('update:modelValue', raw === '' ? undefined : raw)
}

function onNumber(e: Event): void {
    const raw = (e.target as HTMLInputElement).value.trim()
    if (raw === '') {
        emit('update:modelValue', undefined)
        return
    }
    const n = Number(raw)
    emit('update:modelValue', Number.isFinite(n) ? n : undefined)
}

function onBool(e: Event): void {
    emit('update:modelValue', (e.target as HTMLInputElement).checked)
}

function onEnum(e: Event): void {
    const raw = (e.target as HTMLSelectElement).value
    emit('update:modelValue', raw === '' ? undefined : raw)
}
</script>

<template>
<label class="field-input">
    <span class="field-label">
        {{ label }}<span v-if="def.required" class="req" title="required">*</span>
    </span>
    <input
        v-if="def.kind === 'text' || def.kind === 'url'"
        class="control"
        type="text"
        :value="text"
        :placeholder="def.kind === 'url' ? 'https://…' : ''"
        :aria-label="label"
        @change="onText"
    />
    <input
        v-else-if="def.kind === 'number'"
        class="control"
        type="number"
        step="any"
        :value="text"
        :aria-label="label"
        @change="onNumber"
    />
    <input
        v-else-if="def.kind === 'date'"
        class="control"
        type="date"
        :value="text"
        :aria-label="label"
        @change="onText"
    />
    <input
        v-else-if="def.kind === 'bool'"
        class="control check"
        type="checkbox"
        :checked="modelValue === true"
        :aria-label="label"
        @change="onBool"
    />
    <select
        v-else
        class="control"
        :value="text"
        :aria-label="label"
        @change="onEnum"
    >
        <option value="">—</option>
        <option v-for="v in def.values" :key="v" :value="v">{{ v }}</option>
    </select>
</label>
</template>

<style scoped>
.field-input {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: var(--text-caption);
    color: var(--text-secondary);
}

.field-label {
    min-width: 7rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.req {
    color: var(--node-problem, #ef4444);
    margin-left: 0.15rem;
}

.control {
    flex: 1;
    min-width: 0;
    padding: 0.35rem 0.55rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-primary);
    color: var(--text-primary);
    font-size: var(--text-caption);
}

.control.check {
    flex: none;
    width: 1rem;
    height: 1rem;
    accent-color: var(--accent, currentColor);
}
</style>
