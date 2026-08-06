import type { Component } from 'vue'

/**
 * Extra chrome contributed by the build that hosts the pane. The daemon,
 * VSCode and JetBrains builds contribute none — the pane is the whole
 * surface there. The GitHub Pages demo aliases this module (see
 * `vite.config.ts`) to mount its "you are in a demo" banner without any
 * demo-awareness leaking into App.vue.
 */
export const hostChrome: Component | null = null
