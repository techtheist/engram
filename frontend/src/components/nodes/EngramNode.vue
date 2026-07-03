<script setup lang="ts">
import { computed } from 'vue'
import { Handle, Position, type NodeProps } from '@vue-flow/core'
import MarkdownView from '@/components/common/MarkdownView.vue'
import { NODE_ACCENT_VAR } from '@/constants/ontology'
import type { GraphNode } from '@/types/graph'

const props = defineProps<NodeProps<GraphNode>>()

const node = computed(() => props.data)
const body = computed(() => node.value.body ?? '')
const accent = computed(() => NODE_ACCENT_VAR[node.value.type])

const archived = computed(() => node.value.valid_until != null)
const trusted = computed(() => (node.value.confidence ?? 0) >= 0.7)
const isUser = computed(() => node.value.source === 'user')
</script>

<template>
<div
    class="engram-node node-appear"
    :class="{ selected: props.selected, archived }"
    :style="{ '--accent': accent }"
>
    <Handle :position="Position.Left" type="target" class="engram-handle" />

    <header class="flex items-center gap-1">
        <span class="type-pill">{{ node.type }}</span>
        <span class="meta-badge">{{ node.durability }}</span>
        <span v-if="node.status" class="meta-badge">{{ node.status }}</span>
        <span class="grow" />
        <span v-if="isUser" class="trust-badge trust-user" title="User-authored — trusted">user</span>
        <span v-else-if="trusted" class="trust-badge trust-ok" title="Reconfirmed — trusted">trusted</span>
        <span v-else class="trust-badge trust-prov" title="Claude-authored, not yet reconfirmed">provisional</span>
    </header>

    <h4 class="node-title">{{ node.title }}</h4>

    <div v-if="body" class="body-clip">
        <MarkdownView :content="body" />
    </div>

    <div v-if="node.code_refs.length" class="refs">
        <span v-for="ref in node.code_refs" :key="ref" class="ref-chip">{{ ref }}</span>
    </div>

    <span v-if="archived" class="archived-flag" title="Archived by decay or superseded">archived</span>

    <Handle :position="Position.Right" type="source" class="engram-handle" />
</div>
</template>

<style scoped>
.engram-node {
    position: relative;
    display: flex;
    width: 34rem;
    min-height: 9rem;
    flex-direction: column;
    gap: 0.6rem;
    padding: 1.4rem 1.6rem;
    border-radius: var(--radius-xl);
    border: 1px solid var(--border-default);
    border-left: 3px solid var(--accent);
    background-color: var(--surface-elevated);
    /* Frosted glass behind the node — resolves to a blur only in engram-purple
       (where --glass-backdrop is a blur); every other theme sets it to `none`.
       Invisible at rest (opaque surface) and revealed on hover, where the
       surface turns translucent. */
    backdrop-filter: var(--glass-backdrop);
    box-shadow: var(--shadow-md);
    /* asymmetric: collapse fast (ease-enter), expand slow (set on :hover) */
    transition:
        margin var(--duration-fast) var(--ease-enter),
        background-color var(--duration-fast) var(--ease-enter),
        box-shadow var(--duration-fast) var(--ease-enter);
}

.engram-node:hover {
    margin-top: -0.6rem;
    background-color: var(--node-hover-surface);
    box-shadow: var(--shadow-lg);
    transition:
        margin var(--duration-slow) var(--ease-default),
        background-color var(--duration-slow) var(--ease-default),
        box-shadow var(--duration-normal) var(--ease-default);
}

.engram-node.selected {
    outline: 0.2rem solid var(--interactive-primary);
    outline-offset: 0.3rem;
}

.engram-node.archived {
    opacity: 0.5;
    filter: saturate(0.6);
}

.node-title {
    font-size: var(--text-body);
    font-weight: 600;
    line-height: var(--leading-tight);
    color: var(--text-primary);
}

.body-clip {
    overflow: hidden;
    max-height: 9rem;
    transition: max-height var(--duration-fast) var(--ease-enter);
}

.engram-node:hover .body-clip {
    max-height: 200rem;
    transition: max-height var(--duration-slow) var(--ease-default);
}

.type-pill {
    padding: 0.2rem 0.7rem;
    border-radius: var(--radius-md);
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--accent);
    background-color: color-mix(in srgb, var(--accent) 16%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent);
}

.meta-badge {
    padding: 0.2rem 0.6rem;
    border-radius: var(--radius-md);
    font-size: var(--text-caption);
    color: var(--text-tertiary);
    background-color: var(--surface-muted);
}

.trust-badge {
    padding: 0.2rem 0.6rem;
    border-radius: var(--radius-md);
    font-size: var(--text-caption);
    font-weight: 600;
    border: 1px solid transparent;
}

.trust-prov {
    color: var(--trust-provisional);
    background-color: color-mix(in srgb, var(--trust-provisional) 16%, transparent);
    border-color: color-mix(in srgb, var(--trust-provisional) 40%, transparent);
}

.trust-ok,
.trust-user {
    color: var(--trust-trusted);
    background-color: color-mix(in srgb, var(--trust-trusted) 16%, transparent);
    border-color: color-mix(in srgb, var(--trust-trusted) 40%, transparent);
}

.refs {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
    /* Collapsed: exactly one chip row; the rest reveals on hover like the body. */
    max-height: 2rem;
    overflow: hidden;
    transition: max-height var(--duration-fast) var(--ease-enter);
}

.engram-node:hover .refs {
    max-height: 40rem;
    transition: max-height var(--duration-slow) var(--ease-default);
}

.ref-chip {
    max-width: 100%;
    padding: 0.1rem 0.6rem;
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-tertiary);
    background-color: var(--surface-sunken);
    /* Long file paths must stay inside the card, not escape it. */
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.archived-flag {
    position: absolute;
    top: 0.8rem;
    right: 1.2rem;
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.grow {
    flex: 1;
}
</style>
