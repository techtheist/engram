<script setup lang="ts">
import { SKINS, setSkin, skin } from './theme'

defineProps<{ wide?: boolean }>()
</script>

<template>
  <div class="theme-switch" :class="{ wide }" role="radiogroup" aria-label="Theme">
    <button
      v-for="t in SKINS"
      :key="t.id"
      type="button"
      role="radio"
      class="opt"
      :class="{ current: t.id === skin }"
      :aria-checked="t.id === skin"
      :title="t.label"
      @click="setSkin(t.id)"
    >
      <span class="swatch" :data-theme="t.id" aria-hidden="true" />
      <span v-if="wide" class="name">{{ t.label }}</span>
    </button>
  </div>
</template>

<style scoped>
.theme-switch {
  display: flex;
  align-items: center;
  gap: 2px;
  margin-left: 12px;
}

.theme-switch.wide {
  margin: 24px 0 0;
  gap: 8px;
}

.opt {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px;
  border-radius: 6px;
  color: var(--vp-c-text-2);
  font-size: 14px;
}

.opt:hover {
  background: var(--interactive-ghost-hover);
}

.wide .opt {
  padding: 6px 10px 6px 6px;
}

.opt.current {
  color: var(--interactive-primary);
}

/* The pane's settings swatch: each paints its own skin's surface and accent. */
.swatch {
  width: 14px;
  height: 14px;
  border-radius: 4px;
  border: 1px solid var(--border-default);
  background: linear-gradient(135deg, var(--surface-base) 50%, var(--interactive-primary) 50%);
}

.opt.current .swatch {
  outline: 1px solid var(--interactive-primary);
  outline-offset: 2px;
}
</style>
