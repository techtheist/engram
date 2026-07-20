<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { BaseEdge, getBezierPath, type EdgeProps, type Position } from '@vue-flow/core'

/**
 * The default bezier edge plus a native tooltip: when the edge carries a
 * free-text `note`, hovering the label box shows it. The `<title>` is a child
 * of the label `<g>`, so it covers the whole box — rect and text — unlike
 * EdgeText's slot, which only reacts over the glyphs. No note → no `<title>`,
 * so nothing shows on hover, and the label looks exactly as before.
 */
const props = defineProps<
    EdgeProps<{ note: string | null }> & {
        sourcePosition: Position
        targetPosition: Position
    }
>()

const path = computed(() =>
    getBezierPath({
        sourceX: props.sourceX,
        sourceY: props.sourceY,
        sourcePosition: props.sourcePosition,
        targetX: props.targetX,
        targetY: props.targetY,
        targetPosition: props.targetPosition,
    }),
)

const note = computed(() => props.data?.note ?? null)

// Center the label box on the path midpoint, mirroring EdgeText's own
// transform (measure the text, then shift by half its box).
const textEl = ref<SVGTextElement | null>(null)
const box = ref({ width: 0, height: 0 })
function measure(): void {
    if (!textEl.value) return
    const b = textEl.value.getBBox()
    if (b.width !== box.value.width || b.height !== box.value.height) {
        box.value = { width: b.width, height: b.height }
    }
}
onMounted(measure)
watch([() => props.label, () => path.value[1], () => path.value[2]], measure)

const PAD_X = 4
const PAD_Y = 2
const labelTransform = computed(
    () => `translate(${path.value[1] - box.value.width / 2} ${path.value[2] - box.value.height / 2})`,
)
</script>

<template>
<BaseEdge
    :id="id"
    :path="path[0]"
    :marker-end="markerEnd"
    :marker-start="markerStart"
    :style="style"
/>
<g v-if="label" :transform="labelTransform" class="engram-edge-label">
    <title v-if="note">{{ note }}</title>
    <rect
        class="vue-flow__edge-textbg engram-edge-label__bg"
        :width="box.width + 2 * PAD_X"
        :height="box.height + 2 * PAD_Y"
        :x="-PAD_X"
        :y="-PAD_Y"
        rx="2"
        ry="2"
    />
    <text
        ref="textEl"
        class="vue-flow__edge-text engram-edge-label__text"
        :y="box.height / 2"
        dy="0.3em"
    >
        {{ label }}
    </text>
</g>
</template>

<style scoped>
/* The whole label box drives the hover tooltip, not just the glyphs. */
.engram-edge-label {
    pointer-events: all;
}

.engram-edge-label__bg {
    fill: var(--surface-base);
    fill-opacity: 0.85;
}

.engram-edge-label__text {
    fill: var(--text-tertiary);
    font-size: 11px;
    font-family: var(--font-sans);
}
</style>
