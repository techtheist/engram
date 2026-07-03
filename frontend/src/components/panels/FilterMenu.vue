<script setup lang="ts">
import { computed, ref, useTemplateRef } from 'vue'
import { onClickOutside } from '@vueuse/core'
import { storeToRefs } from 'pinia'
import { NODE_ACCENT_VAR } from '@/constants/ontology'
import { trustLevel, useGraphStore, type GraphFilters } from '@/stores/graph'
import type { NodeType } from '@/types/graph'

/**
 * Pure client-side canvas filter. Every option list is gathered from the
 * loaded graph — only values that actually occur are offered — and the canvas
 * re-packs to just the matching nodes.
 */
const store = useGraphStore()
const { nodeList, filters, activeFilterCount } = storeToRefs(store)

const open = ref(false)
const root = useTemplateRef<HTMLElement>('root')
onClickOutside(root, () => (open.value = false))

type Group = Exclude<keyof GraphFilters, 'showArchived'>

interface Section {
    group: Group
    label: string
    values: string[]
}

const sections = computed<Section[]>(() => {
    const types = new Set<string>()
    const durabilities = new Set<string>()
    const sources = new Set<string>()
    const statuses = new Set<string>()
    const trust = new Set<string>()
    for (const n of nodeList.value) {
        types.add(n.type)
        durabilities.add(n.durability)
        sources.add(n.source)
        if (n.status) statuses.add(n.status)
        trust.add(trustLevel(n))
    }
    const section = (group: Group, label: string, set: Set<string>): Section => ({
        group,
        label,
        values: [...set].sort(),
    })
    return [
        section('types', 'Type', types),
        section('statuses', 'Status', statuses),
        section('trust', 'Trust', trust),
        section('sources', 'Source', sources),
        section('durabilities', 'Durability', durabilities),
    ].filter((s) => s.values.length > 1)
})

const hasArchived = computed(() => nodeList.value.some((n) => n.valid_until != null))

const isOn = (group: Group, value: string): boolean => filters.value[group].includes(value)

function accentFor(group: Group, value: string): string | undefined {
    return group === 'types' ? NODE_ACCENT_VAR[value as NodeType] : undefined
}
</script>

<template>
<div ref="root" class="filter-root">
    <button
        class="toggle"
        type="button"
        :class="{ active: open || activeFilterCount > 0 }"
        :title="'Filter the canvas'"
        @click="open = !open"
    >
        Filter
        <span v-if="activeFilterCount" class="count">{{ activeFilterCount }}</span>
    </button>

    <Transition name="pop">
        <div v-if="open" class="popover glass-panel">
            <section v-for="s in sections" :key="s.group" class="group">
                <h3 class="group-title">{{ s.label }}</h3>
                <div class="chips">
                    <button
                        v-for="v in s.values"
                        :key="v"
                        class="chip"
                        type="button"
                        :class="{ on: isOn(s.group, v) }"
                        :style="accentFor(s.group, v) ? { '--chip-accent': accentFor(s.group, v) } : undefined"
                        @click="store.toggleFilter(s.group, v)"
                    >
                        {{ v }}
                    </button>
                </div>
            </section>

            <label v-if="hasArchived" class="archived-row">
                <input v-model="filters.showArchived" type="checkbox" />
                <span>Show archived</span>
            </label>

            <button
                v-if="activeFilterCount"
                class="clear"
                type="button"
                @click="store.clearFilters()"
            >
                Clear all filters
            </button>
        </div>
    </Transition>
</div>
</template>

<style scoped>
.filter-root {
    position: relative;
}

.toggle {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.6rem 1.2rem;
    border-radius: var(--radius-full);
    border: 1px solid var(--border-default);
    background-color: var(--surface-glass);
    backdrop-filter: var(--glass-backdrop);
    color: var(--text-secondary);
    font-size: var(--text-label);
    font-weight: 600;
    cursor: pointer;
}

.toggle:hover,
.toggle.active {
    color: var(--text-primary);
    background-color: var(--node-hover-surface);
}

.count {
    min-width: 1.8rem;
    padding: 0.1rem 0.5rem;
    border-radius: var(--radius-full);
    background-color: var(--interactive-primary);
    color: var(--text-inverse);
    font-size: var(--text-caption);
    text-align: center;
}

.popover {
    position: absolute;
    top: calc(100% + 0.8rem);
    right: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    gap: 1.2rem;
    width: 30rem;
    max-height: 70vh;
    overflow-y: auto;
    padding: 1.4rem;
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
}

.pop-enter-active,
.pop-leave-active {
    transition:
        transform var(--duration-fast) var(--ease-default),
        opacity var(--duration-fast) var(--ease-default);
}

.pop-enter-from,
.pop-leave-to {
    transform: translateY(-0.4rem);
    opacity: 0;
}

.group-title {
    margin-bottom: 0.5rem;
    font-size: var(--text-caption);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-tertiary);
}

.chips {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
}

.chip {
    --chip-accent: var(--interactive-primary);
    padding: 0.3rem 0.9rem;
    border-radius: var(--radius-full);
    border: 1px solid var(--border-default);
    background-color: transparent;
    color: var(--text-secondary);
    font-size: var(--text-caption);
    font-weight: 600;
    cursor: pointer;
}

.chip:hover {
    color: var(--text-primary);
    background-color: var(--interactive-ghost-hover);
}

.chip.on {
    color: var(--chip-accent);
    border-color: color-mix(in srgb, var(--chip-accent) 55%, transparent);
    background-color: color-mix(in srgb, var(--chip-accent) 14%, transparent);
}

.archived-row {
    display: flex;
    align-items: center;
    gap: 0.7rem;
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
    cursor: pointer;
}

.clear {
    align-self: flex-start;
    padding: 0.5rem 1rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: transparent;
    color: var(--text-secondary);
    font-size: var(--text-caption);
    font-weight: 600;
    cursor: pointer;
}

.clear:hover {
    color: var(--text-primary);
    background-color: var(--interactive-ghost-hover);
}
</style>
