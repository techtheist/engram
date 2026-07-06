<script setup lang="ts">
import { computed } from 'vue'
import { storeToRefs } from 'pinia'
import { useGraphStore } from '@/stores/graph'
import type { GraphNode } from '@/types/graph'

/**
 * Graph-health strip (PLAN §10 Phase 1): the pane's at-a-glance answer to
 * "does my memory need attention?" — counts only; the Review drawer is where
 * the work happens.
 */
const store = useGraphStore()
const { nodeList, edgeList, suspects } = storeToRefs(store)

const active = (n: GraphNode): boolean => n.valid_until == null

const activeNodes = computed(() => nodeList.value.filter(active))
const staleCount = computed(() => activeNodes.value.filter((n) => n.stale).length)
const conflictCount = computed(
    () =>
        edgeList.value.filter(
            (e) => e.type === 'conflicts-with' && (e.status == null || e.status === 'active'),
        ).length,
)
const provisionalCount = computed(
    () =>
        activeNodes.value.filter((n) => n.source === 'claude' && n.approved_at == null && !n.stale)
            .length,
)

const attention = computed(
    () => staleCount.value + conflictCount.value + suspects.value.length,
)
</script>

<template>
<div v-if="nodeList.length" class="health glass-panel" :title="'Graph health — review via the Review panel'">
    <span class="stat">{{ activeNodes.length }} nodes</span>
    <span v-if="suspects.length" class="stat warn">{{ suspects.length }} suspected</span>
    <span v-if="conflictCount" class="stat warn">{{ conflictCount }} conflicts</span>
    <span v-if="staleCount" class="stat warn">{{ staleCount }} stale</span>
    <span v-if="provisionalCount" class="stat">{{ provisionalCount }} provisional</span>
    <span v-if="!attention" class="stat ok">healthy</span>
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

.stat {
    color: var(--text-tertiary);
    white-space: nowrap;
}

.stat.warn {
    color: var(--node-problem);
    font-weight: 600;
}

.stat.ok {
    color: var(--trust-trusted);
    font-weight: 600;
}
</style>
