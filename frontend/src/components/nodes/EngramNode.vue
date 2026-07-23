<script setup lang="ts">
import { computed } from 'vue'
import { Handle, Position, type NodeProps } from '@vue-flow/core'
import MarkdownView from '@/components/common/MarkdownView.vue'
import { useConfigStore } from '@/stores/config'
import { BADGE_TIPS } from '@/constants/trust'
import type { GraphNode } from '@/types/graph'

const props = defineProps<NodeProps<GraphNode>>()

const node = computed(() => props.data)
const body = computed(() => node.value.body ?? '')
const config = useConfigStore()
const accent = computed(() => config.accent(node.value.type))

const archived = computed(() => node.value.valid_until != null)
const pinned = computed(() => node.value.trust_override != null)
const trusted = computed(() => node.value.approved_at != null || node.value.trust >= 0.7)
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
        <span v-if="pinned" class="trust-badge trust-pinned" :title="BADGE_TIPS.pinned">📌 pinned</span>
        <span v-else-if="node.stale" class="trust-badge trust-stale" :title="BADGE_TIPS.stale">stale</span>
        <span v-else-if="isUser" class="trust-badge trust-user" :title="BADGE_TIPS.user">user</span>
        <span v-else-if="trusted" class="trust-badge trust-ok" :title="BADGE_TIPS.trusted">trusted</span>
        <span v-else class="trust-badge trust-prov" :title="BADGE_TIPS.provisional">provisional</span>
    </header>

    <h4 class="node-title">{{ node.title }}</h4>

    <div v-if="body" class="body-clip">
        <MarkdownView :content="body" />
    </div>

    <div v-if="node.code_refs.length" class="refs">
        <span v-for="ref in node.code_refs" :key="ref" class="ref-chip">{{ ref }}</span>
    </div>

    <div v-if="node.tags.length" class="refs tags">
        <span v-for="t in node.tags" :key="t" class="tag-chip">#{{ t }}</span>
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
    /* Collapsed cards never exceed 216px: the title clamp below plus the
       body's flex-shrink keep the content inside, so no overflow:hidden is
       needed (it would clip the edge handles). */
    max-height: 21.6rem;
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
    max-height: none;
    margin-top: -0.6rem;
    background-color: var(--node-hover-surface);
    box-shadow: var(--shadow-lg);
    transition:
        margin var(--duration-slow) var(--ease-default),
        background-color var(--duration-slow) var(--ease-default),
        box-shadow var(--duration-normal) var(--ease-default);
}

.engram-node.selected {
    /* Width/offset are zoom-compensated by GraphCanvas so the selection ring
       stays readable at any zoom (fixed rem would vanish under the canvas
       transform at 0.1×). */
    outline: var(--selected-outline-w, 0.2rem) solid var(--interactive-primary);
    outline-offset: var(--selected-outline-o, 0.3rem);
}

.engram-node.archived {
    opacity: 0.5;
    filter: saturate(0.6);
}

/* The body is the only flex child allowed to shrink under the height cap. */
.engram-node > header,
.refs {
    flex-shrink: 0;
}

.node-title {
    font-size: var(--text-body);
    font-weight: 600;
    line-height: var(--leading-tight);
    color: var(--text-primary);
    /* A long title takes space from the body (which flex-shrinks first), but
       may never blow the card past its 21.6rem cap on its own: 5 clamped
       lines + header + refs still fit. Unclamped on hover. */
    flex-shrink: 0;
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 5;
    line-clamp: 5;
    overflow: hidden;
}

.engram-node:hover .node-title {
    -webkit-line-clamp: unset;
    line-clamp: unset;
}

.body-clip {
    overflow: hidden;
    max-height: 9rem;
    /* Shrinks below its content before anything else when the title is tall
       and the card hits its height cap. */
    flex-shrink: 1;
    min-height: 0;
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

.trust-stale {
    color: var(--node-problem);
    background-color: color-mix(in srgb, var(--node-problem) 16%, transparent);
    border-color: color-mix(in srgb, var(--node-problem) 40%, transparent);
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

.trust-pinned {
    color: var(--interactive-primary);
    background-color: color-mix(in srgb, var(--interactive-primary) 14%, transparent);
    border-color: color-mix(in srgb, var(--interactive-primary) 40%, transparent);
    white-space: nowrap;
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

.tag-chip {
    padding: 0.1rem 0.6rem;
    border-radius: var(--radius-full);
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--interactive-primary);
    background-color: color-mix(in srgb, var(--interactive-primary) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--interactive-primary) 35%, transparent);
    white-space: nowrap;
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
