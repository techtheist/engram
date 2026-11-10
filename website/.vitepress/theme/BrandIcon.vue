<script setup lang="ts">
/**
 * The pane's top-bar icon rules (App.vue, 0.9.7): the Engram skin shows the
 * boxed purple icon as drawn; the other skins draw the mark on a box in
 * their own accent — bars black at 0.8, face white — so it never looks
 * pasted on from another product.
 */
import { withBase } from 'vitepress'
import { skin } from './theme'

defineProps<{ size: number }>()
</script>

<template>
  <span class="brand-icon" :style="{ width: `${size}px`, height: `${size}px` }">
    <img
      v-if="skin === 'engram-purple'"
      :src="withBase(size > 64 ? '/engram-light-512.png' : '/engram-light-64.png')"
      alt=""
      :width="size"
      :height="size"
    />
    <span v-else class="boxed">
      <svg viewBox="0 0 120 120" aria-hidden="true">
        <g fill-rule="evenodd">
          <path class="face" d="M110 10L110 110 30 110 100 10 110 10Z" />
          <path class="bar" d="M10 35.1039L10 10 110 10 110 10 90 10 72.4273 35.1039 10 35.1039Z" />
          <path class="bar" d="M63.7139 47.5517L46.1411 72.6557 10 72.6557 10 47.5517 63.7139 47.5517Z" />
          <path class="bar" d="M10 84.8961L10 110 110 110 110 110 90 110 72.4273 84.8961 10 84.8961Z" />
        </g>
      </svg>
    </span>
  </span>
</template>

<style scoped>
/* The pane's chip: 32px with an 8px corner, so the corner scales at 1/4. */
.brand-icon {
  display: inline-grid;
  flex-shrink: 0;
  border-radius: 25%;
  box-shadow: 0 8px 24px rgb(0 0 0 / 28%), 0 2px 8px rgb(0 0 0 / 18%);
  overflow: hidden;
}

.brand-icon img {
  display: block;
  width: 100%;
  height: 100%;
}

.boxed {
  display: grid;
  place-items: center;
  background: var(--interactive-primary);
}

.boxed svg {
  width: 75%;
  height: 75%;
}

.face {
  fill: #fff;
}

.bar {
  fill: #0a0a14;
  fill-opacity: 0.8;
}
</style>
