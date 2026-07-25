<script setup lang="ts">
import { computed } from 'vue'
import { storeToRefs } from 'pinia'
import { useGraphStore } from '@/stores/graph'
import { useConfigStore } from '@/stores/config'
import type { GraphNode } from '@/types/graph'

/**
 * Graph-health strip (PLAN §10 Phase 1): the pane's at-a-glance answer to
 * "does my memory need attention?" — counts only; the Review drawer is where
 * the work happens.
 */
const store = useGraphStore()
const config = useConfigStore()
const { nodeList, edgeList, suspects, drift } = storeToRefs(store)

const active = (n: GraphNode): boolean => n.valid_until == null

const activeNodes = computed(() => nodeList.value.filter(active))
const staleCount = computed(() => activeNodes.value.filter((n) => n.stale).length)
const conflictCount = computed(
    () =>
        edgeList.value.filter(
            (e) => config.isActiveConflict(e),
        ).length,
)
const provisionalCount = computed(
    () =>
        activeNodes.value.filter(
            (n) =>
                n.source === 'claude' &&
                n.approved_at == null &&
                n.trust_override == null &&
                !n.stale,
        ).length,
)

const attention = computed(
    () => staleCount.value + conflictCount.value + suspects.value.length + drift.value.length,
)

/* Mean computed trust across active nodes — the graph's one-glance pulse. */
const trustIndex = computed(() => {
    const list = activeNodes.value
    if (!list.length) return 0
    return list.reduce((sum, n) => sum + n.trust, 0) / list.length
})

const trustTone = computed(() =>
    trustIndex.value >= 0.6 ? 'ok' : trustIndex.value >= 0.4 ? 'mid' : 'low',
)
</script>

<template>
<div
    v-if="nodeList.length"
    class="health glass-panel"
    :title="'Graph health — review via the Review panel'"
>
    <span class="stat">{{ activeNodes.length }} nodes</span>
    <span v-if="suspects.length" class="stat warn">{{ suspects.length }} suspected</span>
    <span v-if="conflictCount" class="stat warn">{{ conflictCount }} conflicts</span>
    <span v-if="staleCount" class="stat warn">{{ staleCount }} stale</span>
    <span v-if="drift.length" class="stat warn">{{ drift.length }} drifted</span>
    <span v-if="provisionalCount" class="stat soft">{{ provisionalCount }} provisional</span>
    <span v-if="!attention" class="stat ok">healthy</span>
    <span class="stat meter-stat" :title="`Mean trust across active nodes: ${Math.round(trustIndex * 100)}%`">
        <span class="meter"><span class="meter-fill" :class="trustTone" :style="{ width: `${Math.round(trustIndex * 100)}%` }" /></span>
        trust index
    </span>
</div>
</template>

<style scoped>
.health {
    position: absolute;
    bottom: 1.6rem;
    /* Clear of the Vue Flow zoom controls, which own the bottom-left corner. */
    left: 6.4rem;
    z-index: 8;
    display: flex;
    align-items: center;
    gap: 0.9rem;
    padding: 0.5rem 1.1rem;
    border-radius: var(--radius-full);
    font-size: var(--text-caption);
}

/* Panes thinner than 350px: no room even for the folded strip. */
@media (width <= 350px) {
    .health {
        display: none;
    }
}

/* Panes thinner than 608px: the strip would run under the minimap — fold
   the stats into a column instead. */
@media (width <= 608px) {
    .health {
        flex-direction: column;
        align-items: flex-start;
        gap: 0.4rem;
        padding: 0.8rem 1.2rem;
        border-radius: var(--radius-lg);
    }
}

.stat {
    color: var(--text-tertiary);
    font-family: var(--font-mono);
    white-space: nowrap;
}

.stat + .stat {
    padding-left: 0.9rem;
    border-left: 1px solid var(--border-subtle);
}

.stat.warn {
    color: var(--node-problem);
    font-weight: 600;
}

.stat.soft {
    color: var(--trust-provisional);
}

.stat.ok {
    color: var(--trust-trusted);
    font-weight: 600;
}

.meter-stat {
    display: inline-flex;
    align-items: center;
    gap: 0.6rem;
}

.meter {
    display: inline-block;
    overflow: hidden;
    width: 5.4rem;
    height: 0.5rem;
    border-radius: var(--radius-full);
    background: var(--surface-muted);
}

.meter-fill {
    display: block;
    height: 100%;
    border-radius: var(--radius-full);
    transition: width var(--duration-slow) var(--ease-default);
}

.meter-fill.ok {
    background: var(--trust-trusted);
}

.meter-fill.mid {
    background: var(--trust-provisional);
}

.meter-fill.low {
    background: var(--node-problem);
}

/* The column fold has no room for row dividers. */
@media (width <= 426px) {
    .stat + .stat {
        padding-left: 0;
        border-left: none;
    }
}
</style>
